//! NFSv3 backend. Mounting is macOS only; the wire layer builds on any Unix.
//!
//! A from-scratch, unprivileged NFSv3 server (RFC 1813), mounted by the OS's
//! own NFS client — no macFUSE, no kernel extension, no Reduced Security boot
//! policy, no root. See `docs/ARCHITECTURE.md` for how this mechanism was
//! chosen.
//!
//! Single-export, single-connection-set server: one [`handle::FileHandle3`]
//! secret per
//! mount authorizes every handle this server ever hands out, and every handle
//! a client can present back. v1 serves only single-fragment RPC messages and
//! opens/reads/releases a fresh [`crate::ReadOnlyFs`] handle on every `READ3`
//! rather than caching one per inode — see `docs/GAPS.md`.
//!
//! # Two transports
//!
//! [`local`] serves an `AF_UNIX` socket and hands the root file handle to
//! `mount(2)` directly. Nothing listens on the network, the MOUNT protocol is
//! never served, and access is decided by file permissions on the socket. It
//! needs an argument buffer whose layout macOS does not document, so it is
//! tried first rather than depended on.
//!
//! [`tcp`] serves loopback and runs `mount_nfs`, on documented interfaces
//! only. It publishes a value to the system mount table, where any local
//! account can read it, which is what makes it the fallback rather than the
//! default. [`MountBuilder::nfs_require_local_socket`] turns the fallback into
//! an error for a caller who would rather not mount at all.
//!
//! # What is compiled where
//!
//! The wire layer — [`xdr`], [`rpc`], [`handle`], [`mount_proto`],
//! [`nfs_proto`], [`transport`], [`server`] — and the [`mount_args`] encoder
//! are byte manipulation with no platform API in them, so they compile and
//! test on every Unix. Only [`local`], [`tcp`] and [`NfsHandle`] are
//! macOS-gated: those are the parts that call `mount(2)`, run `mount_nfs` and
//! call `libc::unmount`. Off macOS nothing calls the wire layer, hence the
//! scoped `dead_code` allow on each module rather than a blanket one.

#[cfg(target_os = "macos")]
use std::io;
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
#[cfg(feature = "nfs-local-socket")]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
mod mount_args;
#[cfg(feature = "nfs-tcp")]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
mod mount_proto;
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
mod nfs_proto;
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
mod rpc;
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
mod server;
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
mod transport;
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
mod xdr;

#[cfg(all(target_os = "macos", feature = "nfs-local-socket"))]
mod local;
#[cfg(all(target_os = "macos", feature = "nfs-tcp"))]
mod tcp;

/// ONC RPC program number for the MOUNT protocol (RFC 1813 Appendix I).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const MOUNT_PROG: u32 = 100_005;
/// ONC RPC program number for NFS (RFC 1813 §2).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const NFS_PROG: u32 = 100_003;

/// `allow_other` and `auto_unmount` are FUSE mount options with no NFS
/// counterpart: this server authorizes with the handle secret rather than by
/// uid, and teardown is owned by [`Mounted`].
#[cfg(target_os = "macos")]
const CAPS: Caps = Caps {
    name: "nfs",
    allow_other: false,
    auto_unmount: false,
    empty_mountpoint: false,
    threads: false,
    nfs_local_socket: cfg!(feature = "nfs-local-socket"),
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

/// Which transport a live mount is being served over.
#[cfg(target_os = "macos")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Transport {
    /// `AF_UNIX`, root handle passed to `mount(2)`, no MOUNT protocol.
    #[cfg(feature = "nfs-local-socket")]
    LocalSocket,
    /// Loopback TCP, mounted by `mount_nfs` after a `MNT` call.
    #[cfg(feature = "nfs-tcp")]
    Tcp,
}

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

/// Whether `path` is still the root of a mount.
///
/// A mounted directory and the directory holding it sit on different
/// filesystems, so their device numbers differ; once the mount is gone the two
/// match again. That distinguishes "the unmount already happened" from "the
/// unmount failed", which `unmount(2)` reports as `EINVAL` either way.
///
/// Errs toward `true`: a path that cannot be inspected produces the underlying
/// unmount error rather than a silent success.
///
/// Compiled on every Unix rather than only macOS so it can be tested without a
/// Mac, the same reasoning that keeps the wire layer unconditional.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn still_mounted(path: &std::path::Path) -> bool {
    use std::os::unix::fs::MetadataExt;

    let Ok(meta) = std::fs::metadata(path) else {
        // The path itself is gone, so nothing is mounted on it.
        return false;
    };
    let parent = match path.parent() {
        // The filesystem root is always a mount point.
        None => return true,
        // A bare relative path: the parent is the working directory.
        Some(p) if p.as_os_str().is_empty() => std::path::Path::new("."),
        Some(p) => p,
    };
    match std::fs::metadata(parent) {
        Ok(parent_meta) => parent_meta.dev() != meta.dev(),
        Err(_) => true,
    }
}

/// A live NFS mount: the client-side mount plus the server thread behind it.
#[cfg(target_os = "macos")]
#[derive(Debug)]
pub(crate) struct NfsHandle {
    mountpoint: std::path::PathBuf,
    stop: Arc<AtomicBool>,
    server_thread: JoinHandle<()>,
    transport: Transport,
    /// Present only on the local-socket path, which has a directory to clear
    /// up after itself.
    #[cfg(feature = "nfs-local-socket")]
    socket_dir: Option<local::SocketDir>,
}

#[cfg(target_os = "macos")]
impl NfsHandle {
    fn new(
        mountpoint: std::path::PathBuf,
        stop: Arc<AtomicBool>,
        server_thread: JoinHandle<()>,
        transport: Transport,
        #[cfg(feature = "nfs-local-socket")] socket_dir: Option<local::SocketDir>,
        #[cfg(not(feature = "nfs-local-socket"))] _socket_dir: Option<()>,
    ) -> Self {
        Self {
            mountpoint,
            stop,
            server_thread,
            transport,
            #[cfg(feature = "nfs-local-socket")]
            socket_dir,
        }
    }

    /// Unmount a path without owning a handle for it, for the failure paths
    /// that have to take down a mount they just made.
    fn unmount_path(mountpoint: &std::path::Path) -> Result<()> {
        let path = std::ffi::CString::new(mountpoint.as_os_str().as_encoded_bytes())
            .map_err(|e| FsError::Other(format!("mountpoint has an interior NUL: {e}")))?;
        // SAFETY: `path` is a valid NUL-terminated C string for the duration
        // of this call; `unmount(2)` does not retain the pointer afterward.
        // No flags are passed, matching what `/sbin/umount` itself does for
        // a user unmounting their own mount.
        let rc = unsafe { libc::unmount(path.as_ptr(), 0) };
        if rc == 0 {
            return Ok(());
        }

        // Errno first: the check below makes syscalls of its own.
        let err = io::Error::last_os_error();

        // Ejecting the volume in Finder, or running `umount`, takes the mount
        // down without telling this process, and `unmount(2)` then reports
        // `EINVAL` for a path that is no longer a mount point. That is the
        // state this call was asking for, not a failure, so teardown stays
        // idempotent — the same answer `fuser` gives on Linux, where it checks
        // whether the FUSE device is still mounted before unmounting again.
        if !still_mounted(mountpoint) {
            crate::backend::trace::backend_info!(
                "anymount/nfs: {} was already unmounted from outside this process",
                mountpoint.display()
            );
            return Ok(());
        }

        Err(FsError::Io(err).context(format!("unmount failed for {}", mountpoint.display())))
    }
}

#[cfg(target_os = "macos")]
impl Mounted for NfsHandle {
    /// The client-side mount is torn down first, so no new request can arrive;
    /// only then is the server stopped, so nothing is left in-flight to hang
    /// on. The socket directory goes last, once nothing can still connect
    /// through it.
    fn unmount(self: Box<Self>) -> Result<()> {
        let unmounted = Self::unmount_path(&self.mountpoint);

        // Stop and join the server even if the client-side unmount failed,
        // so a failure cannot leak the thread.
        self.stop.store(true, Ordering::Relaxed);
        if self.server_thread.join().is_err() {
            crate::backend::trace::backend_warn!(
                "anymount/nfs: the server thread for {} panicked",
                self.mountpoint.display()
            );
        }

        #[cfg(feature = "nfs-local-socket")]
        if let Some(dir) = &self.socket_dir {
            dir.remove();
        }

        unmounted
    }

    fn backend(&self) -> Backend {
        Backend::Nfs
    }

    fn uses_local_socket(&self) -> bool {
        match self.transport {
            #[cfg(feature = "nfs-local-socket")]
            Transport::LocalSocket => true,
            #[cfg(feature = "nfs-tcp")]
            Transport::Tcp => false,
        }
    }
}

/// Mount, preferring the local socket and falling back to loopback TCP.
///
/// The order is not a preference between equals. The local-socket path closes
/// both disclosure and reachability; the TCP path closes neither completely,
/// and exists so that a macOS release which changes the private argument
/// encoding degrades to a working mount with weaker properties rather than to
/// no mount at all. A caller who would rather have no mount says so with
/// [`MountBuilder::nfs_require_local_socket`].
#[cfg(target_os = "macos")]
pub(crate) fn mount<F: ReadOnlyFs>(builder: MountBuilder, fs: F) -> Result<NfsHandle> {
    preflight::check(&builder, &CAPS)?;
    let fs = Arc::new(fs);

    #[cfg(feature = "nfs-local-socket")]
    let local_err = match local::mount(&builder, Arc::clone(&fs)) {
        Ok(handle) => return Ok(handle),
        Err(e) if builder.nfs_require_local_socket => {
            return Err(e.context(
                "the local-socket transport failed and nfs_require_local_socket \
                 forbids falling back to loopback TCP",
            ));
        }
        Err(e) => e,
    };

    #[cfg(feature = "nfs-tcp")]
    {
        #[cfg(feature = "nfs-local-socket")]
        crate::backend::trace::backend_warn!(
            "anymount/nfs: the local-socket transport failed ({local_err}); \
             falling back to loopback TCP, where this mount is reachable by \
             other local accounts. Set nfs_require_local_socket to refuse this"
        );
        tcp::mount(&builder, fs)
    }

    #[cfg(not(feature = "nfs-tcp"))]
    {
        drop(fs);
        Err(local_err)
    }
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod mod_tests;
