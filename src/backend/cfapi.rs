//! Windows Cloud Files (cfapi) backend — the only Windows backend `anymount`
//! ships. `docs/PLAN.md` (Phase 0) covers why ProjFS was evaluated and cut.
//!
//! # Status: Phase 0 stub
//!
//! [`probe`] is real; [`mount`] is not implemented yet — see `docs/PLAN.md`,
//! Phase 2. `CldApi.dll` ships enabled on every Windows 10 1709+ install, so
//! there is no admin feature-enable step.
//!
//! Sync root registration was the open question the Phase 0 spike settled:
//!
//! * WinRT `StorageProviderSyncRootManager::Register` is **package-identity
//!   gated** — it needs MSIX or a sparse package. This is the path the
//!   `cloud-filter` crate takes.
//! * Win32 `CfRegisterSyncRoot` documents **no identity requirement**, only
//!   `WRITE_DATA`/`WRITE_DAC` on the directory.
//!
//! **Confirmed:** the Win32 path works from an unpackaged binary. A throwaway
//! spike called `CfRegisterSyncRoot` directly from a plain `cargo run` binary
//! — no MSIX, no sparse package, no app identity — and it registered and
//! unregistered a real sync root cleanly, repeatedly.

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
    // A load-time import of `CldApi.dll` is safe: it ships with every Windows
    // 10 1709+ install and is not an optional feature.
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
        "the cfapi backend is not implemented yet (Phase 2)",
    ))
}
