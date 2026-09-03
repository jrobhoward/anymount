//! Windows Projected File System backend.
//!
//! # Status: Phase 0 stub
//!
//! [`probe`] is real and is the Windows spike's first check. [`mount`] is not
//! implemented yet — see `docs/PLAN.md`, Phase 2.
//!
//! # ProjFS must be resolved dynamically, not linked
//!
//! ProjFS ships with Windows 10 1809+ but is **not enabled by default**; it
//! needs a one-time, no-reboot admin step:
//!
//! ```powershell
//! Enable-WindowsOptionalFeature -Online -FeatureName Client-ProjFS -NoRestart
//! ```
//!
//! When the feature is off, `ProjectedFSLib.dll` is absent. A *load-time*
//! import of any `Prj*` function would then stop the whole process from
//! starting — the loader fails before `main` runs, so there is no opportunity
//! to report a friendly error. This module therefore makes no static reference
//! to any `Prj*` symbol, and [`probe`] uses `LoadLibraryW`.
//!
//! Phase 2 must keep that property: resolve every ProjFS entry point through
//! `GetProcAddress` (or delay-loading) rather than calling it directly.
//!
//! Bindings come from Microsoft's own `windows` crate (MIT OR Apache-2.0). The
//! third-party `windows-projfs` crate is GPL-2.0 and is not used.

use crate::error::{FsError, Result};
use crate::fs::ReadOnlyFs;
use crate::mount::MountBuilder;

pub(crate) struct ProjFsHandle {
    _private: (),
}

impl ProjFsHandle {
    pub(crate) fn unmount(self) -> Result<()> {
        Ok(())
    }
}

/// Report whether ProjFS is present and enabled on this machine.
///
/// Attempts to load `ProjectedFSLib.dll`, which exists only once the
/// `Client-ProjFS` optional feature is enabled. This cannot crash on a machine
/// without the feature, which a static import would.
pub fn probe() -> bool {
    use windows::Win32::Foundation::FreeLibrary;
    use windows::Win32::System::LibraryLoader::LoadLibraryW;
    use windows::core::w;

    // SAFETY: the argument is a valid NUL-terminated wide string literal. On
    // success the returned module is freed immediately; the handle is not used
    // for anything else, so no dangling reference can escape.
    unsafe {
        match LoadLibraryW(w!("ProjectedFSLib.dll")) {
            Ok(module) if !module.is_invalid() => {
                let _ = FreeLibrary(module);
                true
            }
            _ => false,
        }
    }
}

pub(crate) fn mount<F: ReadOnlyFs>(_builder: MountBuilder, _fs: F) -> Result<ProjFsHandle> {
    if !probe() {
        return Err(FsError::Unsupported(
            "ProjFS is unavailable: enable it with \
             `Enable-WindowsOptionalFeature -Online -FeatureName Client-ProjFS -NoRestart` \
             (requires administrator, no reboot)",
        ));
    }
    Err(FsError::Unsupported(
        "the ProjFS backend is not implemented yet (Phase 2)",
    ))
}
