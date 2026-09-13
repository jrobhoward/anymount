//! Mounting over loopback TCP by running `mount_nfs`.
//!
//! The fallback path, on documented interfaces only. It costs a disclosure the
//! local-socket path does not have: `mount_nfs` records the export path in the
//! system mount table, where `nfsstat -m` reprints it for any local account, so
//! the value in that path is public from the moment the mount succeeds.
//!
//! Two independent values keep that from granting anything. The public one
//! authorizes `MNT` and nothing else, and `MNT` stops being answered the
//! moment `mount_nfs` exits — so by the time the value can be read, there is
//! no call left to spend it on. The value that prefixes file handles is never
//! written anywhere a reader can reach.

use std::net::TcpListener;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::error::{FsError, Result};
use crate::fs::ReadOnlyFs;
use crate::mount::MountBuilder;

use super::handle::{ExportSecret, FileHandle3};
use super::server::{self, Config};
use super::{NfsHandle, Transport, volume_label};

pub(super) fn mount<F: ReadOnlyFs>(builder: &MountBuilder, fs: Arc<F>) -> Result<NfsHandle> {
    let handle = Arc::new(FileHandle3::new_random());
    let export = ExportSecret::new_random();

    let listener = TcpListener::bind("127.0.0.1:0")
        .map_err(|e| FsError::Io(e).context("binding NFS server socket"))?;
    let port = listener
        .local_addr()
        .map_err(|e| FsError::Io(e).context("reading NFS server socket's assigned port"))?
        .port();

    // `/export/<secret>/<label>`, not `/export/<secret>`. macOS titles a
    // Finder window, and names the volume, from the last component of the
    // remote path, so a bare secret export shows up throughout the UI as 32
    // hex characters. The label is decorative — `mount_proto` checks the
    // secret segment and ignores this one.
    let export_path = format!(
        "/export/{}/{}",
        export.hex(),
        volume_label(&builder.fs_name)
    );

    let serve_mount = Arc::new(AtomicBool::new(true));
    let config = Arc::new(Config {
        serve_mount: Arc::clone(&serve_mount),
        export,
    });

    let stop = Arc::new(AtomicBool::new(false));
    let server_thread = {
        let handle = Arc::clone(&handle);
        let stop = Arc::clone(&stop);
        std::thread::spawn(move || server::run(listener, fs, handle, config, stop))
    };

    // Both ways this can fail — `mount_nfs` not spawning at all, and
    // `mount_nfs` running but refusing the mount — have to stop and join the
    // server thread before returning. Otherwise the thread outlives the failed
    // `mount()` call, holding its listener bound and the filesystem alive,
    // with nothing left to shut it down. Resolving the outcome first and
    // handling the error once keeps the two paths from drifting apart.
    let outcome = Command::new("mount_nfs")
        .arg("-o")
        .arg(format!(
            "vers=3,tcp,port={port},mountport={port},noresvport,ro,soft,timeo=20,retrans=2"
        ))
        .arg(format!("127.0.0.1:{export_path}"))
        .arg(&builder.mountpoint)
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

    // `MNT` is issued exactly once per mount lifetime, so refusing it from here
    // on costs nothing: a soft mount that loses its connection reconnects and
    // carries on serving without asking again. This runs whether the mount
    // succeeded or not — on the failure path the server is about to stop
    // anyway, and leaving the window open while tearing down would be the one
    // case nobody tests.
    serve_mount.store(false, Ordering::Relaxed);

    if let Err(e) = outcome {
        stop.store(true, Ordering::Relaxed);
        let _ = server_thread.join();
        return Err(e);
    }

    Ok(NfsHandle::new(
        builder.mountpoint.clone(),
        stop,
        server_thread,
        Transport::Tcp,
        None,
    ))
}
