//! Mounting: [`MountBuilder`] to configure, [`Mount`] as the live handle.

use std::path::{Path, PathBuf};

use crate::backend;
use crate::error::Result;
use crate::fs::ReadOnlyFs;

/// Which platform mechanism to mount with.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum Backend {
    /// Pick the best available for this OS.
    #[default]
    Auto,
    /// FUSE. Linux and macOS (via macFUSE).
    Fuse,
    /// Windows Projected File System.
    ProjFs,
    /// Windows Cloud Files API.
    CfApi,
}

/// Configuration for a mount.
#[derive(Debug, Clone)]
pub struct MountBuilder {
    pub(crate) mountpoint: PathBuf,
    pub(crate) backend: Backend,
    pub(crate) fs_name: String,
    pub(crate) allow_other: bool,
    pub(crate) auto_unmount: bool,
}

impl MountBuilder {
    /// Start configuring a mount at `mountpoint`.
    ///
    /// On Unix this is a directory that must already exist. On Windows it is the
    /// virtualisation root; both Windows backends project into a directory
    /// rather than assigning a drive letter.
    pub fn new(mountpoint: impl AsRef<Path>) -> Self {
        Self {
            mountpoint: mountpoint.as_ref().to_path_buf(),
            backend: Backend::Auto,
            fs_name: "anymount".to_owned(),
            allow_other: false,
            auto_unmount: false,
        }
    }

    /// Force a specific backend instead of [`Backend::Auto`].
    pub fn backend(mut self, backend: Backend) -> Self {
        self.backend = backend;
        self
    }

    /// Name shown in `mount(8)` output and Explorer. Defaults to `anymount`.
    pub fn fs_name(mut self, name: impl Into<String>) -> Self {
        self.fs_name = name.into();
        self
    }

    /// Let users other than the mounter see the mount (FUSE `allow_other`).
    ///
    /// Requires `user_allow_other` in `/etc/fuse.conf`. Off by default because
    /// a backup mount is normally private to one user.
    pub fn allow_other(mut self, yes: bool) -> Self {
        self.allow_other = yes;
        self
    }

    /// Unmount if the process dies.
    ///
    /// **Off by default, and requires [`allow_other`](Self::allow_other).** FUSE
    /// implements `auto_unmount` inside the `fusermount3` helper, which refuses
    /// it for an owner-private mount; enabling one without the other is
    /// rejected at [`mount`](Self::mount) time. A private mount left behind by a
    /// crash is cleared with `fusermount3 -u <mountpoint>`.
    pub fn auto_unmount(mut self, yes: bool) -> Self {
        self.auto_unmount = yes;
        self
    }

    /// Mount `fs` and return immediately, serving in the background.
    pub fn mount<F: ReadOnlyFs>(self, fs: F) -> Result<Mount> {
        backend::mount(self, fs)
    }
}

/// A live mount. Unmounts on drop.
pub struct Mount {
    pub(crate) inner: backend::MountHandle,
    pub(crate) mountpoint: PathBuf,
}

impl Mount {
    /// Where this filesystem is mounted.
    pub fn mountpoint(&self) -> &Path {
        &self.mountpoint
    }

    /// Unmount explicitly, surfacing errors that `drop` would swallow.
    pub fn unmount(self) -> Result<()> {
        self.inner.unmount()
    }
}

impl std::fmt::Debug for Mount {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Mount")
            .field("mountpoint", &self.mountpoint)
            .finish_non_exhaustive()
    }
}
