//! Validation every backend runs before any platform-specific code.
//!
//! Two jobs. First, the mountpoint has to be usable: it must exist, be a
//! directory, and — for a backend that projects into it rather than covering
//! it — be empty. Without this each platform reports whatever its helper
//! binary prints, which is three different messages for one mistake. Second,
//! [`MountBuilder`] carries options only some backends can honor —
//! `allow_other` and `auto_unmount` are FUSE mount options with no NFS or
//! cfapi counterpart — and a request for one that cannot be honored is an
//! error naming the backend, not a silent no-op.
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
    /// Whether this backend has a worker-thread count to set.
    pub(crate) threads: bool,
    /// Whether this backend requires the mountpoint to be empty.
    ///
    /// A Unix mount covers the mountpoint: whatever was underneath is hidden
    /// for the mount's life and comes back on unmount, so a non-empty
    /// directory is harmless. cfapi instead projects placeholders *into* the
    /// directory and clears them again on unmount, so mounting over existing
    /// files would destroy them. The check belongs here rather than in the
    /// backend so the refusal reads the same as every other option error.
    pub(crate) empty_mountpoint: bool,
}

/// Validate a [`MountBuilder`] against what the chosen backend can do.
pub(crate) fn check(builder: &MountBuilder, caps: &Caps) -> Result<()> {
    check_mountpoint(&builder.mountpoint)?;

    if caps.empty_mountpoint {
        check_mountpoint_empty(&builder.mountpoint, caps.name)?;
    }

    if builder.allow_other && !caps.allow_other {
        return Err(FsError::InvalidArgument.context(format!(
            "the {} backend cannot honor allow_other; it is a FUSE mount option \
             with no equivalent here. Leave it off, or mount with Backend::Fuse \
             on Linux",
            caps.name
        )));
    }

    if builder.threads.is_some() && !caps.threads {
        return Err(FsError::InvalidArgument.context(format!(
            "the {} backend cannot honor threads; it does not own its own \
             worker pool. Leave it unset, or mount with Backend::Fuse on Linux",
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

/// Reject a non-empty mountpoint for a backend that projects into it.
///
/// The error says what would have happened rather than only what is wrong:
/// the destructive part of the contract — that unmounting clears the
/// directory — is the reason the check exists, and a caller who only reads
/// the error should still learn it.
fn check_mountpoint_empty(path: &Path, backend: &str) -> Result<()> {
    let mut entries = std::fs::read_dir(path).map_err(|e| {
        FsError::Io(e).context(format!(
            "reading mountpoint {} to check that it is empty",
            path.display()
        ))
    })?;

    if entries.next().is_some() {
        return Err(FsError::InvalidArgument.context(format!(
            "mountpoint {} is not empty; the {backend} backend projects its \
             entries into the directory rather than covering it, and clears \
             them again on unmount, so mounting here would destroy the \
             existing contents. Use an empty directory",
            path.display()
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
