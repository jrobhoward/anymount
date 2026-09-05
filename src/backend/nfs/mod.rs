//! NFSv3 backend. Mounting is macOS only; the wire layer builds on any Unix.
//!
//! A from-scratch, unprivileged NFSv3 server (RFC 1813) plus the MOUNT
//! protocol (RFC 1813 Appendix I) it needs, hand-rolled over `std::net`, with
//! the OS's own `mount_nfs` client doing the actual mounting — no macFUSE, no
//! kernel extension, no Reduced Security boot policy, no root. See
//! `docs/PLAN.md`'s Phase 0.6 for how this mechanism was chosen and proven.
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

    let export = format!("/export/{}", handle.secret_hex());
    let mountpoint = builder.mountpoint.clone();
    let output = Command::new("mount_nfs")
        .arg("-o")
        .arg(format!(
            "vers=3,tcp,port={port},mountport={port},noresvport,soft,timeo=20,retrans=2"
        ))
        .arg(format!("127.0.0.1:{export}"))
        .arg(&mountpoint)
        .output()
        .map_err(|e| FsError::Io(e).context("spawning mount_nfs"))?;

    if !output.status.success() {
        stop.store(true, Ordering::Relaxed);
        let _ = server_thread.join();
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(FsError::Other(format!(
            "mount_nfs exited with {}: {}",
            output.status, stderr
        )));
    }

    Ok(NfsHandle {
        mountpoint,
        stop,
        server_thread,
    })
}
