//! Windows Cloud Files (cfapi) backend.
//!
//! # Status: Phase 0 stub
//!
//! [`probe`] is real and answers the central Windows spike question. [`mount`]
//! is not implemented yet — see `docs/PLAN.md`, Phase 3.
//!
//! Unlike ProjFS, `CldApi.dll` ships enabled on every Windows 10 1709+ install,
//! so there is no admin feature-enable step. The open question is *sync root
//! registration*:
//!
//! * WinRT `StorageProviderSyncRootManager::Register` is **package-identity
//!   gated** — it needs MSIX or a sparse package. This is the path the
//!   `cloud-filter` crate takes.
//! * Win32 `CfRegisterSyncRoot` documents **no identity requirement**, only
//!   `WRITE_DATA`/`WRITE_DAC` on the directory.
//!
//! Whether the Win32 path really works from an unpackaged binary is unverified
//! in public sources, and deciding it is the point of the Windows spike.

use crate::error::{FsError, Result};
use crate::fs::ReadOnlyFs;
use crate::mount::MountBuilder;

pub(crate) struct CfApiHandle {
    _private: (),
}

impl CfApiHandle {
    pub(crate) fn unmount(self) -> Result<()> {
        Ok(())
    }
}

/// Platform version reported by `CfGetPlatformInfo`, proving `CldApi.dll` loads.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct PlatformInfo {
    pub build: u32,
    pub revision: u32,
    pub integration: u32,
}

/// Query the Cloud Files platform version.
///
/// `integration` is the value gating newer features: the unrestricted
/// placeholder-management policies need `0x310` or higher.
pub fn probe() -> Option<PlatformInfo> {
    use windows::Win32::Storage::CloudFilters::CfGetPlatformInfo;

    // SAFETY: the binding allocates and initialises the out-parameter itself
    // and returns it by value; there is nothing for the caller to keep alive.
    //
    // Unlike ProjFS, a load-time import of `CldApi.dll` is safe: it ships with
    // every Windows 10 1709+ install and is not an optional feature.
    let info = unsafe { CfGetPlatformInfo() }.ok()?;
    Some(PlatformInfo {
        build: info.BuildNumber,
        revision: info.RevisionNumber,
        integration: info.IntegrationNumber,
    })
}

pub(crate) fn mount<F: ReadOnlyFs>(_builder: MountBuilder, _fs: F) -> Result<CfApiHandle> {
    if probe().is_none() {
        return Err(FsError::Unsupported(
            "Cloud Files API unavailable: requires Windows 10 1709 or later",
        ));
    }
    Err(FsError::Unsupported(
        "the cfapi backend is not implemented yet (Phase 3)",
    ))
}
