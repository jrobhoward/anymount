//! Mounting: [`MountBuilder`] to configure, [`Mount`] as the live handle.

use std::path::{Path, PathBuf};

use crate::backend;
use crate::error::Result;
use crate::fs::ReadOnlyFs;

/// Which platform mechanism to mount with.
///
/// `#[non_exhaustive]` because a fourth backend would otherwise be a breaking
/// change. The value types — [`FileAttr`](crate::FileAttr),
/// [`FileKind`](crate::FileKind), [`DirEntry`](crate::DirEntry) and
/// [`StatFs`](crate::StatFs) — deliberately are not; that was considered for
/// 1.0 and dropped so implementors can keep building them with struct
/// literals.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum Backend {
    /// Pick the best available for this OS.
    #[default]
    Auto,
    /// FUSE, via `fusermount3`. Linux only.
    Fuse,
    /// NFSv3 via the built-in `mount_nfs` client. macOS only.
    ///
    /// The server binds to loopback, and the per-mount secret authorizing it
    /// is published in the system mount table, so a mount is readable by any
    /// local process rather than only by the user that created it. See
    /// [`docs/GAPS.md`](https://github.com/jrobhoward/anymount/blob/main/docs/GAPS.md).
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
    pub(crate) threads: Option<usize>,
    pub(crate) nfs_require_local_socket: bool,
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
            threads: None,
            nfs_require_local_socket: false,
        }
    }

    /// Force a specific backend instead of [`Backend::Auto`].
    pub fn backend(mut self, backend: Backend) -> Self {
        self.backend = backend;
        self
    }

    /// Name shown in `mount(8)` output, Finder and Explorer. Defaults to
    /// `anymount`.
    ///
    /// On macOS this is also the volume name, so it is what titles a Finder
    /// window and labels the mount in a file dialog. Characters that cannot
    /// appear in that position, `/` and control characters, are replaced with
    /// `-`, and the name is truncated for display; a name left with nothing
    /// usable falls back to `anymount`. The name is unchanged everywhere else
    /// it appears.
    pub fn fs_name(mut self, name: impl Into<String>) -> Self {
        self.fs_name = name.into();
        self
    }

    /// Let users other than the mounter see the mount (FUSE `allow_other`).
    ///
    /// Requires `user_allow_other` in `/etc/fuse.conf`. Off by default because
    /// a mount is normally private to one user.
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

    /// Refuse to mount rather than fall back to loopback TCP (macOS).
    ///
    /// The NFS backend prefers an `AF_UNIX` socket, passing the root file
    /// handle to `mount(2)` directly: nothing listens on the network, no
    /// credential reaches the system mount table, and access is decided by the
    /// socket's file permissions. That path needs an argument buffer whose
    /// layout macOS does not document, so when it fails the backend falls back
    /// to running `mount_nfs` over loopback, where a value in the export path
    /// is readable by every local account for as long as the mount is being
    /// established.
    ///
    /// Setting this turns that fallback into an error. Use it when the content
    /// being served must not be reachable by other local users, and a failed
    /// mount is the better outcome. [`Mount::nfs_uses_local_socket`] reports
    /// which path a mount actually took.
    ///
    /// Off by default. NFS only; the FUSE and cfapi backends reject the
    /// request at [`mount`](Self::mount) time rather than ignoring it, and so
    /// does a build with the `nfs-local-socket` feature turned off, which has
    /// no such path to require.
    pub fn nfs_require_local_socket(mut self, yes: bool) -> Self {
        self.nfs_require_local_socket = yes;
        self
    }

    /// Serve kernel requests on `n` worker threads.
    ///
    /// Concurrency comes from serving several requests at once, not from
    /// async, so this is the knob that decides how many
    /// [`ReadOnlyFs`](crate::ReadOnlyFs) calls can be in flight together. The
    /// default is four, enough that a single reader and a directory walk do
    /// not queue behind each other without making an implementor's own
    /// locking the bottleneck. Raising it helps only if reads are slow and
    /// the implementation is genuinely concurrent.
    ///
    /// FUSE only. The NFS backend sizes itself from the connections its
    /// client opens, and cfapi's callbacks are dispatched by the platform, so
    /// neither has a thread count to set; asking for one is rejected at
    /// [`mount`](Self::mount) time rather than ignored.
    ///
    /// `n` is clamped to at least one.
    pub fn threads(mut self, n: usize) -> Self {
        self.threads = Some(n.max(1));
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
/// The OS can unmount first, and nothing here prevents it: ejecting the volume
/// in Finder, or running `umount` or `fusermount3 -u`, takes the mount down
/// while this handle still exists. That is not reported — the handle has no
/// way to learn of it — and it takes down the client side alone, leaving the
/// server to answer requests that can no longer arrive until teardown runs.
/// Teardown is what stops the server and clears up after it, and it succeeds
/// whether or not the mount is still there. See
/// [`docs/GAPS.md`](https://github.com/jrobhoward/anymount/blob/main/docs/GAPS.md).
///
/// [`unmount`]: Mount::unmount
pub struct Mount {
    /// `None` once teardown has run.
    inner: Option<Box<dyn backend::Mounted>>,
    mountpoint: PathBuf,
    /// Cached from the handle, so it stays reportable after teardown.
    backend: Backend,
    /// Cached from the handle, for the same reason.
    local_socket: bool,
}

impl Mount {
    pub(crate) fn new(inner: Box<dyn backend::Mounted>, mountpoint: PathBuf) -> Self {
        Self {
            backend: inner.backend(),
            local_socket: inner.uses_local_socket(),
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

    /// Whether this mount is served over an `AF_UNIX` socket (macOS).
    ///
    /// True only for an NFS mount that took the local-socket path, where the
    /// root file handle went to `mount(2)` directly, nothing listens on the
    /// network, and no credential reaches the system mount table. False for a
    /// mount that fell back to loopback TCP, and false on every other
    /// platform.
    ///
    /// [`MountBuilder::nfs_require_local_socket`] turns that fallback into a
    /// mount error, for a caller who would rather not mount than mount on the
    /// weaker terms.
    pub fn nfs_uses_local_socket(&self) -> bool {
        self.local_socket
    }

    /// Unmount explicitly, surfacing errors that `drop` would swallow.
    ///
    /// A mount the OS has already taken down is not an error: the call stops
    /// the server, releases what the backend allocated, and reports success,
    /// because the state it was asked for is the state that holds. An unmount
    /// that genuinely fails — a file still open on the mount, say — is still
    /// reported.
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
