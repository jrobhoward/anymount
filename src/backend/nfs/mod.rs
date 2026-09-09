//! NFSv3 backend. Mounting is macOS only; the wire layer builds on any Unix.
//!
//! A from-scratch, unprivileged NFSv3 server (RFC 1813) plus the MOUNT
//! protocol (RFC 1813 Appendix I) it needs, hand-rolled over `std::net`, with
//! the OS's own `mount_nfs` client doing the actual mounting — no macFUSE, no
//! kernel extension, no Reduced Security boot policy, no root. See
//! `docs/ARCHITECTURE.md` for how this mechanism was chosen.
//!
//! Single-export, single-connection-set server: one [`FileHandle3`] secret
//! per mount authorizes every handle this server ever hands out, and every
//! handle a client can present back. v1 serves only single-fragment TCP RPC
//! messages and opens/reads/releases a fresh [`crate::ReadOnlyFs`] handle on
//! every `READ3` rather than caching one per inode — see `docs/GAPS.md`.
//!
//! # What is compiled where
//!
//! The wire layer below — [`xdr`], [`rpc`], [`handle`], [`mount_proto`],
//! [`nfs_proto`] and [`server`] — is byte manipulation and `std::net`, with no
//! macOS API in it, so it compiles and tests on every Unix. Only [`mount`] and
//! [`NfsHandle`] are macOS-gated: those are the parts that run `mount_nfs` and
//! call `libc::unmount`. Off macOS nothing calls the wire layer, hence the
//! scoped `dead_code` allow on each module rather than a blanket one.

#[cfg(target_os = "macos")]
use std::io;
#[cfg(target_os = "macos")]
use std::net::TcpListener;
#[cfg(target_os = "macos")]
use std::process::Command;
#[cfg(target_os = "macos")]
use std::sync::Arc;
#[cfg(target_os = "macos")]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(target_os = "macos")]
use std::thread::JoinHandle;

#[cfg(target_os = "macos")]
use crate::backend::Mounted;
#[cfg(target_os = "macos")]
use crate::backend::preflight::{self, Caps};
#[cfg(target_os = "macos")]
use crate::error::{FsError, Result};
#[cfg(target_os = "macos")]
use crate::fs::ReadOnlyFs;
#[cfg(target_os = "macos")]
use crate::mount::{Backend, MountBuilder};

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
mod handle;
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
mod mount_proto;
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
mod nfs_proto;
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
mod rpc;
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
mod server;
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
mod xdr;

#[cfg(target_os = "macos")]
use handle::FileHandle3;

/// ONC RPC program number for the MOUNT protocol (RFC 1813 Appendix I).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const MOUNT_PROG: u32 = 100_005;
/// ONC RPC program number for NFS (RFC 1813 §2).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const NFS_PROG: u32 = 100_003;

/// `allow_other` and `auto_unmount` are FUSE mount options with no NFS
/// counterpart: this server binds to loopback and authorizes with the handle
/// secret rather than by uid, and teardown is owned by [`Mounted`].
#[cfg(target_os = "macos")]
const CAPS: Caps = Caps {
    name: "nfs",
    allow_other: false,
    auto_unmount: false,
    empty_mountpoint: false,
    threads: false,
};

/// Longest volume label appended to an export path.
///
/// Finder truncates a long volume name in the places it matters anyway, and
/// the whole `dirpath` has to stay inside `MNTPATHLEN` (1024) alongside the
/// prefix and the 32-character secret.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const MAX_LABEL_LEN: usize = 64;

/// Fallback when [`MountBuilder::fs_name`] has no characters a label can use.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const DEFAULT_LABEL: &str = "anymount";

/// Reduce `fs_name` to something usable as the last segment of an export path,
/// which is what macOS shows as the volume name.
///
/// `fs_name` is arbitrary caller input, so two classes of character are
/// filtered rather than trusted. A `/` would add a path segment, changing
/// which text the `MNT` handler checks against the secret. A control
/// character would render as a box in Finder and could garble a terminal
/// printing `mount` output. Both become `-`, runs of them collapse, and the
/// result is trimmed of leading and trailing separators so a name like `../..`
/// cannot produce a label of nothing but dashes.
///
/// Everything else is kept, accented and non-Latin names included: the label
/// is decorative, and nothing ever compares it, so the Unicode normalisation
/// macOS applies to names it displays has nothing to break here.
///
/// Compiled on every Unix rather than only macOS so the filtering can be
/// tested without a Mac, the same reasoning that keeps the wire layer
/// unconditional.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn volume_label(fs_name: &str) -> String {
    let mut out = String::new();
    for ch in fs_name.chars().take(MAX_LABEL_LEN) {
        match (ch == '/' || ch.is_control(), out.ends_with('-')) {
            (false, _) => out.push(ch),
            // Collapse a run of rejected characters into one separator.
            (true, false) => out.push('-'),
            (true, true) => {}
        }
    }

    let trimmed = out.trim_matches(|c: char| c == '-' || c == '.' || c.is_whitespace());
    if trimmed.is_empty() {
        DEFAULT_LABEL.to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// A live NFS mount: the client-side mount plus the server thread behind it.
#[cfg(target_os = "macos")]
#[derive(Debug)]
pub(crate) struct NfsHandle {
    mountpoint: std::path::PathBuf,
    stop: Arc<AtomicBool>,
    server_thread: JoinHandle<()>,
}

#[cfg(target_os = "macos")]
impl Mounted for NfsHandle {
    /// The client-side mount is torn down first, so no new request can arrive;
    /// only then is the server stopped, so nothing is left in-flight to hang
    /// on.
    fn unmount(self: Box<Self>) -> Result<()> {
        let path = std::ffi::CString::new(self.mountpoint.as_os_str().as_encoded_bytes())
            .map_err(|e| FsError::Other(format!("mountpoint has an interior NUL: {e}")))?;
        // SAFETY: `path` is a valid NUL-terminated C string for the duration
        // of this call; `unmount(2)` does not retain the pointer afterward.
        // No flags are passed, matching what `/sbin/umount` itself does for
        // a user unmounting their own mount.
        let rc = unsafe { libc::unmount(path.as_ptr(), 0) };
        let unmounted = if rc == 0 {
            Ok(())
        } else {
            Err(FsError::Io(io::Error::last_os_error())
                .context(format!("unmount failed for {}", self.mountpoint.display())))
        };

        // Stop and join the server even if the client-side unmount failed,
        // so a failure cannot leak the thread.
        self.stop.store(true, Ordering::Relaxed);
        if self.server_thread.join().is_err() {
            crate::backend::trace::backend_warn!(
                "anymount/nfs: the server thread for {} panicked",
                self.mountpoint.display()
            );
        }

        unmounted
    }

    fn backend(&self) -> Backend {
        Backend::Nfs
    }
}

#[cfg(target_os = "macos")]
pub(crate) fn mount<F: ReadOnlyFs>(builder: MountBuilder, fs: F) -> Result<NfsHandle> {
    preflight::check(&builder, &CAPS)?;

    let handle = Arc::new(FileHandle3::new_random());
    let fs = Arc::new(fs);

    let listener = TcpListener::bind("127.0.0.1:0")
        .map_err(|e| FsError::Io(e).context("binding NFS server socket"))?;
    let port = listener
        .local_addr()
        .map_err(|e| FsError::Io(e).context("reading NFS server socket's assigned port"))?
        .port();

    let stop = Arc::new(AtomicBool::new(false));

    let server_thread = {
        let fs = Arc::clone(&fs);
        let handle = Arc::clone(&handle);
        let stop = Arc::clone(&stop);
        std::thread::spawn(move || server::run(listener, fs, handle, stop))
    };

    // `/export/<secret>/<label>`, not `/export/<secret>`. macOS titles a
    // Finder window, and names the volume, from the last component of the
    // remote path, so a bare secret export shows up throughout the UI as 32
    // hex characters. The label is decorative — `mount_proto` checks the
    // secret segment and ignores this one.
    let export = format!(
        "/export/{}/{}",
        handle.secret_hex(),
        volume_label(&builder.fs_name)
    );
    let mountpoint = builder.mountpoint.clone();

    // Both ways this can fail — `mount_nfs` not spawning at all, and
    // `mount_nfs` running but refusing the mount — have to stop and join the
    // server thread before returning. Otherwise the thread outlives the failed
    // `mount()` call, holding its listener bound and the filesystem alive,
    // with nothing left to shut it down. Resolving the outcome first and
    // handling the error once keeps the two paths from drifting apart.
    let outcome = Command::new("mount_nfs")
        .arg("-o")
        .arg(format!(
            "vers=3,tcp,port={port},mountport={port},noresvport,soft,timeo=20,retrans=2"
        ))
        .arg(format!("127.0.0.1:{export}"))
        .arg(&mountpoint)
        .output()
        .map_err(|e| FsError::Io(e).context("spawning mount_nfs"))
        .and_then(|output| {
            if output.status.success() {
                Ok(())
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr);
                Err(FsError::Other(format!(
                    "mount_nfs exited with {}: {}",
                    output.status, stderr
                )))
            }
        });

    if let Err(e) = outcome {
        stop.store(true, Ordering::Relaxed);
        let _ = server_thread.join();
        return Err(e);
    }

    Ok(NfsHandle {
        mountpoint,
        stop,
        server_thread,
    })
}
