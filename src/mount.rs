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
    /// FUSE, via `fusermount3`. Linux only.
    Fuse,
    /// NFSv3 via the built-in `mount_nfs` client. macOS only.
    Nfs,
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
    /// virtualisation root; cfapi projects into a directory rather than
    /// assigning a drive letter.
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
    ///
    /// FUSE only. The NFS and cfapi backends have no equivalent and reject the
    /// request at [`mount`](Self::mount) time rather than ignoring it.
    pub fn allow_other(mut self, yes: bool) -> Self {
        self.allow_other = yes;
        self
    }

    /// Unmount if the process dies.
    ///
    /// Off by default, and requires [`allow_other`](Self::allow_other). FUSE
    /// implements `auto_unmount` inside the `fusermount3` helper, which refuses
    /// it for an owner-private mount; enabling one without the other is
    /// rejected at [`mount`](Self::mount) time. A private mount left behind by a
    /// crash is cleared with `fusermount3 -u <mountpoint>`.
    ///
    /// FUSE only, on the same terms as [`allow_other`](Self::allow_other). An
    /// orderly exit needs it on no backend: dropping the [`Mount`] unmounts.
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
///
/// Teardown runs exactly once, from whichever comes first — [`unmount`] or
/// `drop` — because the handle is consumed. Every backend routes through the
/// same path, so "unmounts on drop" is a promise this type makes rather than a
/// side effect of the platform library underneath.
///
/// [`unmount`]: Mount::unmount
pub struct Mount {
    /// `None` once teardown has run.
    inner: Option<Box<dyn backend::Mounted>>,
    mountpoint: PathBuf,
    /// Cached from the handle, so it stays reportable after teardown.
    backend: Backend,
}

impl Mount {
    pub(crate) fn new(inner: Box<dyn backend::Mounted>, mountpoint: PathBuf) -> Self {
        Self {
            backend: inner.backend(),
            inner: Some(inner),
            mountpoint,
        }
    }

    /// Where this filesystem is mounted.
    pub fn mountpoint(&self) -> &Path {
        &self.mountpoint
    }

    /// Which platform mechanism is serving this mount.
    ///
    /// Never [`Backend::Auto`]: that is resolved to a concrete backend at
    /// mount time.
    pub fn backend(&self) -> Backend {
        self.backend
    }

    /// Unmount explicitly, surfacing errors that `drop` would swallow.
    pub fn unmount(mut self) -> Result<()> {
        match self.inner.take() {
            Some(handle) => handle.unmount(),
            None => Ok(()),
        }
    }
}

impl Drop for Mount {
    fn drop(&mut self) {
        if let Some(handle) = self.inner.take()
            && let Err(e) = handle.unmount()
        {
            backend::trace::backend_warn!(
                "anymount: unmounting {} during drop failed: {e}",
                self.mountpoint.display()
            );
        }
    }
}

impl std::fmt::Debug for Mount {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Mount")
            .field("mountpoint", &self.mountpoint)
            .field("backend", &self.backend)
            .finish_non_exhaustive()
    }
}
