//! Validation every backend runs before any platform-specific code.
//!
//! Two jobs. First, the mountpoint has to exist and be a directory; without
//! this each platform reports whatever its helper binary prints, which is three
//! different messages for one mistake. Second, [`MountBuilder`] carries options
//! only some backends can honor — `allow_other` and `auto_unmount` are FUSE
//! mount options with no NFS or cfapi counterpart — and a request for one that
//! cannot be honored is an error naming the backend, not a silent no-op.
//!
//! A backend supplies a [`Caps`] and calls [`check`]. That is the whole
//! contract; there is no per-backend policy to re-derive.

use std::path::Path;

use crate::error::{FsError, Result};
use crate::mount::MountBuilder;

/// Which [`MountBuilder`] options a backend is able to honor.
pub(crate) struct Caps {
    /// Name used in error messages, matching the `Backend` variant's docs.
    pub(crate) name: &'static str,
    pub(crate) allow_other: bool,
    pub(crate) auto_unmount: bool,
}

/// Validate a [`MountBuilder`] against what the chosen backend can do.
pub(crate) fn check(builder: &MountBuilder, caps: &Caps) -> Result<()> {
    check_mountpoint(&builder.mountpoint)?;

    if builder.allow_other && !caps.allow_other {
        return Err(FsError::InvalidArgument.context(format!(
            "the {} backend cannot honor allow_other; it is a FUSE mount option \
             with no equivalent here. Leave it off, or mount with Backend::Fuse \
             on Linux",
            caps.name
        )));
    }

    if builder.auto_unmount && !caps.auto_unmount {
        return Err(FsError::InvalidArgument.context(format!(
            "the {} backend cannot honor auto_unmount; it is a FUSE mount option \
             with no equivalent here. The mount is torn down when the Mount is \
             dropped, which covers an orderly exit",
            caps.name
        )));
    }

    Ok(())
}

fn check_mountpoint(path: &Path) -> Result<()> {
    let meta = std::fs::metadata(path).map_err(|e| {
        FsError::Io(e).context(format!(
            "mountpoint {} is not usable; it must exist and be a directory",
            path.display()
        ))
    })?;

    if !meta.is_dir() {
        return Err(FsError::NotADirectory
            .context(format!("mountpoint {} is not a directory", path.display())));
    }

    Ok(())
}

#[cfg(test)]
#[path = "preflight_tests.rs"]
mod preflight_tests;
