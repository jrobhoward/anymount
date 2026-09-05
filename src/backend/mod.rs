//! Per-OS backends and the dispatch that picks one.
//!
//! One mechanism per platform: FUSE on Linux, cfapi on Windows. macOS's
//! decided backend is NFS (`docs/PLAN.md`), not yet built as a real
//! `backend/nfs.rs` — macOS currently compiles in no backend at all, and
//! `auto_mount` reports [`FsError::Unsupported`] there.
//!
//! Every backend is compiled only for its own platform *and* gated behind a
//! cargo feature. Because cargo cannot express per-OS defaults, `fuse` and
//! `cfapi` are both on by default and simply compile to nothing off-platform;
//! the dependencies themselves live under `[target.'cfg(...)'.dependencies]`,
//! so a Linux build never fetches the `windows` crate and vice versa.

use crate::error::{FsError, Result};
use crate::fs::ReadOnlyFs;
use crate::mount::{Backend, Mount, MountBuilder};

#[cfg(all(target_os = "linux", feature = "fuse"))]
pub(crate) mod fuse;

#[cfg(all(target_os = "macos", feature = "nfs"))]
pub(crate) mod nfs;

pub(crate) mod readdir_cookie;

#[cfg(all(windows, feature = "cfapi"))]
pub(crate) mod cfapi;

/// Backend-specific live-mount state.
pub(crate) enum MountHandle {
    #[cfg(all(target_os = "linux", feature = "fuse"))]
    Fuse(fuse::FuseHandle),
    #[cfg(all(target_os = "macos", feature = "nfs"))]
    Nfs(nfs::NfsHandle),
    #[cfg(all(windows, feature = "cfapi"))]
    CfApi(cfapi::CfApiHandle),
    /// Keeps the enum inhabited when every backend is cfg'd or featured out.
    #[allow(dead_code)]
    None,
}

impl MountHandle {
    pub(crate) fn unmount(self) -> Result<()> {
        match self {
            #[cfg(all(target_os = "linux", feature = "fuse"))]
            Self::Fuse(h) => h.unmount(),
            #[cfg(all(target_os = "macos", feature = "nfs"))]
            Self::Nfs(h) => h.unmount(),
            #[cfg(all(windows, feature = "cfapi"))]
            Self::CfApi(h) => h.unmount(),
            Self::None => Ok(()),
        }
    }
}

/// Resolve [`Backend::Auto`] and hand off to the chosen backend.
pub(crate) fn mount<F: ReadOnlyFs>(builder: MountBuilder, fs: F) -> Result<Mount> {
    let requested = builder.backend;
    let mountpoint = builder.mountpoint.clone();

    let inner = match requested {
        Backend::Auto => auto_mount(builder, fs)?,

        #[cfg(all(target_os = "linux", feature = "fuse"))]
        Backend::Fuse => MountHandle::Fuse(fuse::mount(builder, fs)?),

        #[cfg(all(target_os = "macos", feature = "nfs"))]
        Backend::Nfs => MountHandle::Nfs(nfs::mount(builder, fs)?),

        #[cfg(all(windows, feature = "cfapi"))]
        Backend::CfApi => MountHandle::CfApi(cfapi::mount(builder, fs)?),

        #[allow(unreachable_patterns)]
        other => return Err(unavailable(other)),
    };

    Ok(Mount { inner, mountpoint })
}

/// Pick the best backend for this platform.
///
/// Windows uses cfapi: it needs no one-time admin feature enable, can stream
/// without persisting data to disk, dehydrates automatically, and
/// `CfRegisterSyncRoot` is confirmed to work unpackaged (Phase 0,
/// `docs/PLAN.md`). ProjFS was evaluated and is not used — see `docs/GAPS.md`.
/// macOS uses the NFSv3 backend, mounted with the OS's built-in `mount_nfs`
/// client.
#[allow(unused_variables, unused_mut)]
fn auto_mount<F: ReadOnlyFs>(builder: MountBuilder, fs: F) -> Result<MountHandle> {
    #[cfg(all(target_os = "linux", feature = "fuse"))]
    {
        return Ok(MountHandle::Fuse(fuse::mount(builder, fs)?));
    }

    #[cfg(all(target_os = "macos", feature = "nfs"))]
    {
        return Ok(MountHandle::Nfs(nfs::mount(builder, fs)?));
    }

    #[cfg(all(windows, feature = "cfapi"))]
    {
        return Ok(MountHandle::CfApi(cfapi::mount(builder, fs)?));
    }

    #[allow(unreachable_code)]
    Err(FsError::Unsupported(
        "no mount backend compiled in for this platform; \
         enable the `fuse` feature on Linux, `nfs` on macOS, or `cfapi` on \
         Windows",
    ))
}

#[allow(dead_code)]
fn unavailable(backend: Backend) -> FsError {
    match backend {
        Backend::Fuse => {
            FsError::Unsupported("the `fuse` backend requires Linux and the `fuse` feature")
        }
        Backend::Nfs => {
            FsError::Unsupported("the `nfs` backend requires macOS and the `nfs` feature")
        }
        Backend::CfApi => {
            FsError::Unsupported("the `cfapi` backend requires Windows and the `cfapi` feature")
        }
        Backend::Auto => FsError::Unsupported("no mount backend available for this platform"),
    }
}
