//! Per-OS backends and the dispatch that picks one.
//!
//! One mechanism per platform: FUSE on Linux, NFS on macOS, cfapi on Windows.
//! Every backend is compiled only for its own platform *and* gated behind a
//! cargo feature. Because cargo cannot express per-OS defaults, `fuse`, `nfs`
//! and `cfapi` are all on by default and simply compile to nothing
//! off-platform; the dependencies themselves live under
//! `[target.'cfg(...)'.dependencies]`, so a Linux build never fetches the
//! `windows` crate and vice versa.
//!
//! # What a backend supplies
//!
//! Three things, and nothing else:
//!
//! 1. A `mount(builder, fs)` function returning a handle.
//! 2. That handle's [`Mounted`] impl, which owns teardown.
//! 3. A [`preflight::Caps`] describing which [`MountBuilder`] options it can
//!    honor.
//!
//! Everything else shared lives in [`preflight`] (option validation and
//! mountpoint checks), [`readdir`] (the `.`/`..` listing driver) and here
//! (unmount-on-drop). A new backend adds one arm to [`mount`] and one to
//! [`auto_mount`], not a variant threaded through several `match`es.

use crate::error::{FsError, Result};
use crate::fs::ReadOnlyFs;
use crate::mount::{Backend, Mount, MountBuilder};

#[cfg(all(target_os = "linux", feature = "fuse"))]
pub(crate) mod fuse;

#[cfg(all(target_os = "macos", feature = "nfs"))]
pub(crate) mod nfs;

// Dead when every backend is cfg'd or featured out — the only configuration
// in which nothing supplies a `Caps` — so the allow is scoped to exactly that
// build rather than blanketed on.
#[cfg_attr(
    not(any(
        all(target_os = "linux", feature = "fuse"),
        all(target_os = "macos", feature = "nfs"),
        all(windows, feature = "cfapi"),
    )),
    allow(dead_code)
)]
pub(crate) mod preflight;
pub(crate) mod readdir;
#[macro_use]
pub(crate) mod trace;

#[cfg(all(windows, feature = "cfapi"))]
pub(crate) mod cfapi;

/// A live mount, owned by whichever backend created it.
///
/// # Teardown contract
///
/// [`unmount`](Mounted::unmount) consumes the handle, so [`Mount`] can call it
/// from both [`Mount::unmount`] and its `Drop` and have it run exactly once.
/// Implementors therefore need no idempotence flag and no `Drop` of their own —
/// unmount-on-drop is a guarantee this crate makes uniformly rather than one
/// each backend re-derives from whatever its underlying library happens to do.
///
/// An implementation must leave nothing behind that would outlive the process:
/// no serving thread still running, no registration still held. Joining a
/// worker is part of teardown, not something left to the runtime.
pub(crate) trait Mounted: Send + Sync + std::fmt::Debug {
    /// Tear the mount down, surfacing whatever `drop` would have to swallow.
    fn unmount(self: Box<Self>) -> Result<()>;

    /// Which mechanism this handle came from, for diagnostics.
    fn backend(&self) -> Backend;
}

/// Resolve [`Backend::Auto`] and hand off to the chosen backend.
pub(crate) fn mount<F: ReadOnlyFs>(builder: MountBuilder, fs: F) -> Result<Mount> {
    let mountpoint = builder.mountpoint.clone();

    let inner: Box<dyn Mounted> = match builder.backend {
        Backend::Auto => auto_mount(builder, fs)?,

        #[cfg(all(target_os = "linux", feature = "fuse"))]
        Backend::Fuse => Box::new(fuse::mount(builder, fs)?),

        #[cfg(all(target_os = "macos", feature = "nfs"))]
        Backend::Nfs => Box::new(nfs::mount(builder, fs)?),

        #[cfg(all(windows, feature = "cfapi"))]
        Backend::CfApi => Box::new(cfapi::mount(builder, fs)?),

        #[allow(unreachable_patterns)]
        other => return Err(unavailable(other)),
    };

    trace::backend_info!(
        "anymount: mounted {} with the {:?} backend",
        mountpoint.display(),
        inner.backend()
    );

    Ok(Mount::new(inner, mountpoint))
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
fn auto_mount<F: ReadOnlyFs>(builder: MountBuilder, fs: F) -> Result<Box<dyn Mounted>> {
    #[cfg(all(target_os = "linux", feature = "fuse"))]
    {
        return Ok(Box::new(fuse::mount(builder, fs)?));
    }

    #[cfg(all(target_os = "macos", feature = "nfs"))]
    {
        return Ok(Box::new(nfs::mount(builder, fs)?));
    }

    #[cfg(all(windows, feature = "cfapi"))]
    {
        return Ok(Box::new(cfapi::mount(builder, fs)?));
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
