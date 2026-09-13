//! Mounting over an `AF_UNIX` socket, with the root file handle handed to
//! `mount(2)` directly.
//!
//! The preferred path. Nothing listens on the network, the MOUNT protocol is
//! never served, and no credential reaches the system mount table — access is
//! decided by ordinary file permissions on the socket, which lives mode 0600
//! inside a directory created mode 0700.
//!
//! Peer credentials contribute nothing here and are not consulted: the kernel
//! makes the connection on the mounting process's behalf, and `LOCAL_PEERCRED`
//! reports uid 0 whoever mounted. The directory permissions are the whole
//! access control story.

use std::ffi::CString;
use std::io;
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use crate::error::{FsError, Result};
use crate::fs::ReadOnlyFs;
use crate::mount::MountBuilder;
use crate::types::ROOT_INO;

use super::handle::{FileHandle3, random_bytes};
use super::mount_args::{self, LocalMountArgs, MNT_RDONLY, SUN_PATH_MAX};
use super::server::{self, Config};
use super::{NfsHandle, Transport, volume_label};

/// Where the socket directory is created.
///
/// `sun_path` is 104 bytes, and macOS's per-user temporary directory is a
/// `/var/folders/xx/…` path long enough to leave little room for anything
/// else. `/tmp` is world-writable, which is why the directory below is created
/// fresh with mode 0700 and never reused: the socket's protection comes from
/// the directory, not from the socket file's own mode.
const SOCKET_PARENT: &str = "/tmp";

/// Matches the TCP path's `timeo=20,retrans=2`, in the units `mount_nfs`
/// uses: `timeo` is tenths of a second.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(2);
const SOFT_RETRY_COUNT: u32 = 2;

/// A socket directory, removed when the mount that owns it goes away.
#[derive(Debug)]
pub(crate) struct SocketDir(PathBuf);

impl SocketDir {
    /// Create a fresh directory nobody else can enter.
    ///
    /// `create` rather than `create_all`, so a name that already exists is an
    /// error rather than a directory this process did not make and cannot
    /// vouch for. `mkdir(2)` applies the mode at creation, and a umask can
    /// only clear bits, so the result is never more permissive than 0700.
    fn create() -> Result<Self> {
        let suffix: [u8; 8] = random_bytes();
        let hex: String = suffix.iter().map(|b| format!("{b:02x}")).collect();
        let path = Path::new(SOCKET_PARENT).join(format!(".anymount-{hex}"));

        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .map_err(|e| {
                FsError::Io(e).context(format!(
                    "creating the socket directory {} for the local NFS transport",
                    path.display()
                ))
            })?;

        Ok(Self(path))
    }

    fn socket_path(&self) -> PathBuf {
        self.0.join("s")
    }

    /// Remove the socket and its directory. Best effort: a mount that is
    /// otherwise torn down cleanly should not report a failure because a
    /// temporary directory could not be removed.
    pub(crate) fn remove(&self) {
        let _ = std::fs::remove_file(self.socket_path());
        let _ = std::fs::remove_dir(&self.0);
    }
}

pub(super) fn mount<F: ReadOnlyFs>(builder: &MountBuilder, fs: Arc<F>) -> Result<NfsHandle> {
    let dir = SocketDir::create()?;
    match mount_in(builder, fs, &dir) {
        Ok(handle) => Ok(handle),
        Err(e) => {
            dir.remove();
            Err(e)
        }
    }
}

fn mount_in<F: ReadOnlyFs>(
    builder: &MountBuilder,
    fs: Arc<F>,
    dir: &SocketDir,
) -> Result<NfsHandle> {
    let socket_path = dir.socket_path();
    let socket_str = socket_path.to_str().ok_or_else(|| {
        FsError::Other(format!(
            "socket path {} is not valid UTF-8",
            socket_path.display()
        ))
    })?;
    if socket_str.len() > SUN_PATH_MAX {
        return Err(FsError::Other(format!(
            "socket path {socket_str} is {} bytes, over the {SUN_PATH_MAX}-byte \
             sun_path limit",
            socket_str.len()
        )));
    }

    let listener = UnixListener::bind(&socket_path)
        .map_err(|e| FsError::Io(e).context(format!("binding the NFS socket at {socket_str}")))?;
    // Belt and braces: the 0700 directory already keeps everyone else out, but
    // a socket left at whatever the umask allows would become reachable the
    // moment anything widened the directory.
    std::fs::set_permissions(
        &socket_path,
        std::os::unix::fs::PermissionsExt::from_mode(0o600),
    )
    .map_err(|e| {
        FsError::Io(e).context(format!(
            "restricting permissions on the NFS socket {socket_str}"
        ))
    })?;

    let handle = Arc::new(FileHandle3::new_random());
    let label = volume_label(&builder.fs_name);

    // The MOUNT protocol is never answered on this path: the root handle goes
    // to `mount(2)` in the argument buffer, so there is no `MNT` to make and
    // answering one would only add a second way to obtain a root handle.
    let config = Arc::new(Config {
        #[cfg(feature = "nfs-tcp")]
        serve_mount: Arc::new(AtomicBool::new(false)),
        #[cfg(feature = "nfs-tcp")]
        export: super::handle::ExportSecret::from_secret([0; 16]),
    });

    let stop = Arc::new(AtomicBool::new(false));
    let server_thread = {
        let handle = Arc::clone(&handle);
        let stop = Arc::clone(&stop);
        std::thread::spawn(move || server::run(listener, fs, handle, config, stop))
    };

    let outcome =
        call_mount(builder, socket_str, &handle, &label).and_then(|()| verify(&builder.mountpoint));

    if let Err(e) = outcome {
        // `verify` failing means a mount may be in place, so take it down
        // before the server behind it stops answering.
        let _ = NfsHandle::unmount_path(&builder.mountpoint);
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        let _ = server_thread.join();
        return Err(e);
    }

    Ok(NfsHandle::new(
        builder.mountpoint.clone(),
        stop,
        server_thread,
        Transport::LocalSocket,
        Some(SocketDir(dir.0.clone())),
    ))
}

fn call_mount(
    builder: &MountBuilder,
    socket_path: &str,
    handle: &FileHandle3,
    label: &str,
) -> Result<()> {
    let root = handle.encode(ROOT_INO);
    let mut args = mount_args::encode(&LocalMountArgs {
        socket_path,
        root_handle: &root,
        label,
        request_timeout: REQUEST_TIMEOUT,
        soft_retry_count: SOFT_RETRY_COUNT,
    });

    let fstype = CString::new("nfs").map_err(|e| FsError::Other(format!("fstype: {e}")))?;
    let mountpoint = CString::new(builder.mountpoint.as_os_str().as_encoded_bytes())
        .map_err(|e| FsError::Other(format!("mountpoint has an interior NUL: {e}")))?;

    // SAFETY: `fstype` and `mountpoint` are NUL-terminated C strings, and
    // `args` is a byte buffer, all three owned by this frame and live for the
    // whole call. `mount(2)` reads the argument buffer during the call and
    // does not retain the pointer: the NFS client copies what it needs into
    // its own mount structure before returning. The buffer is passed as
    // `*mut` because the prototype says so; nothing writes through it.
    let rc = unsafe {
        libc::mount(
            fstype.as_ptr(),
            mountpoint.as_ptr(),
            MNT_RDONLY as libc::c_int,
            args.as_mut_ptr().cast(),
        )
    };

    if rc == 0 {
        return Ok(());
    }

    let err = io::Error::last_os_error();
    Err(FsError::Io(err).context(format!(
        "mount(2) refused the local-socket NFS arguments for {}",
        builder.mountpoint.display()
    )))
}

/// Check the mount rather than trusting the return code.
///
/// A wrong argument buffer is rejected loudly — a length in the wrong byte
/// order is `ENOMEM`, an over-long buffer is `E2BIG` — but "loud" is an
/// observation, not a guarantee, and a mount that returns success while
/// serving the wrong thing is the one failure that observation does not
/// cover. Reading the root back through the mountpoint and comparing it
/// against the inode this crate defines closes that.
fn verify(mountpoint: &Path) -> Result<()> {
    use std::os::unix::fs::MetadataExt;

    let meta = std::fs::metadata(mountpoint).map_err(|e| {
        FsError::Io(e).context(format!(
            "reading {} back after mounting; mount(2) reported success",
            mountpoint.display()
        ))
    })?;

    if meta.ino() != ROOT_INO.0 {
        return Err(FsError::Other(format!(
            "{} mounted but its root has inode {} rather than {}; the mount is \
             not this filesystem",
            mountpoint.display(),
            meta.ino(),
            ROOT_INO.0
        )));
    }

    Ok(())
}
