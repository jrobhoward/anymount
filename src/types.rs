//! Core value types: inodes, handles, attributes, directory entries.
//!
//! # Why nothing here is `#[non_exhaustive]`
//!
//! Marking these types `#[non_exhaustive]` was considered for 1.0 and not
//! adopted. It would reserve the right to add a [`FileKind`] variant or a
//! [`FileAttr`] field later without a major version, but it would also stop
//! implementors constructing a `FileAttr` or a `DirEntry` with a struct
//! literal — which is how the trait is meant to be used, and how
//! `examples/memfs.rs` uses it. The shapes below cover what the crate needs
//! and are frozen as they stand: adding to them means a major version. That
//! is a deliberate trade, not an oversight, and there is no plan to revisit
//! it.

use std::ffi::OsString;
use std::time::SystemTime;

/// Inode number. Stable for the lifetime of a mount.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Ino(pub u64);

impl std::fmt::Display for Ino {
    /// The bare number, so a log line reads `ino 42` rather than `ino Ino(42)`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, f)
    }
}

/// The root of every mount.
pub const ROOT_INO: Ino = Ino(1);

/// An open-file token handed back by [`crate::ReadOnlyFs::open`].
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct FileHandle(pub u64);

impl std::fmt::Display for FileHandle {
    /// The bare number, matching [`Ino`]'s rendering.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, f)
    }
}

/// What an inode is.
///
/// Deliberately only two variants: symlinks are not represented because the
/// first consumer (ciphercask) does not back them up, and cfapi does not model
/// them the way FUSE does. See `docs/GAPS.md`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum FileKind {
    /// A regular file, whose contents come from
    /// [`read_at`](crate::ReadOnlyFs::read_at).
    File,
    /// A directory, whose contents come from
    /// [`readdir`](crate::ReadOnlyFs::readdir).
    Directory,
}

/// Metadata for one inode.
#[derive(Clone, Debug)]
pub struct FileAttr {
    /// Which inode this describes. Must match the [`Ino`] it was requested
    /// for.
    pub ino: Ino,
    /// File or directory.
    pub kind: FileKind,
    /// Logical size in bytes. Zero for directories.
    ///
    /// A backend reads exactly this many bytes: cfapi requests the whole file
    /// at once, and the NFS backend reports end-of-file from it, so a size
    /// that disagrees with what [`read_at`](crate::ReadOnlyFs::read_at) can
    /// produce shows up as a truncated or failed read.
    pub size: u64,
    /// Unix permission bits (e.g. `0o644`). Advisory on Windows.
    pub perm: u16,
    /// Hard link count. One for a file; two for a directory, counting its own
    /// `.` entry, is the conventional value for a synthesised tree.
    pub nlink: u32,
    /// Owning user id. Ignored on Windows.
    pub uid: u32,
    /// Owning group id. Ignored on Windows.
    pub gid: u32,
    /// Last access time.
    pub atime: SystemTime,
    /// Last modification time.
    pub mtime: SystemTime,
    /// Inode change time. Also reported as the creation time on Windows,
    /// which has no separate ctime.
    pub ctime: SystemTime,
}

impl FileAttr {
    /// A plain read-only regular file owned by the current user.
    pub fn file(ino: Ino, size: u64) -> Self {
        Self {
            ino,
            kind: FileKind::File,
            size,
            perm: 0o444,
            nlink: 1,
            uid: current_uid(),
            gid: current_gid(),
            atime: SystemTime::UNIX_EPOCH,
            mtime: SystemTime::UNIX_EPOCH,
            ctime: SystemTime::UNIX_EPOCH,
        }
    }

    /// A read-and-traverse directory owned by the current user.
    pub fn dir(ino: Ino) -> Self {
        Self {
            ino,
            kind: FileKind::Directory,
            size: 0,
            perm: 0o555,
            nlink: 2,
            uid: current_uid(),
            gid: current_gid(),
            atime: SystemTime::UNIX_EPOCH,
            mtime: SystemTime::UNIX_EPOCH,
            ctime: SystemTime::UNIX_EPOCH,
        }
    }
}

/// One entry returned by [`crate::ReadOnlyFs::readdir`].
#[derive(Clone, Debug)]
pub struct DirEntry {
    /// The entry's inode, which must answer
    /// [`getattr`](crate::ReadOnlyFs::getattr).
    pub ino: Ino,
    /// The entry's name within its directory, with no path separators.
    /// Never `.` or `..`; the backend synthesises those.
    pub name: OsString,
    /// File or directory, so a caller listing a tree needs no second call.
    pub kind: FileKind,
}

/// Filesystem-wide statistics, reported to `statfs(2)`.
///
/// The defaults report an empty filesystem, which is what `df` will show
/// unless [`statfs`](crate::ReadOnlyFs::statfs) is overridden.
#[derive(Clone, Debug)]
pub struct StatFs {
    /// Total blocks of [`frsize`](Self::frsize) bytes.
    pub blocks: u64,
    /// Free blocks.
    pub bfree: u64,
    /// Free blocks available to an unprivileged user.
    pub bavail: u64,
    /// Total inodes.
    pub files: u64,
    /// Free inodes.
    pub ffree: u64,
    /// Preferred I/O block size in bytes.
    pub bsize: u32,
    /// Longest name a single path component may have.
    pub namelen: u32,
    /// Fragment size in bytes: the unit [`blocks`](Self::blocks),
    /// [`bfree`](Self::bfree) and [`bavail`](Self::bavail) are counted in.
    pub frsize: u32,
}

impl Default for StatFs {
    fn default() -> Self {
        Self {
            blocks: 0,
            bfree: 0,
            bavail: 0,
            files: 0,
            ffree: 0,
            bsize: 512,
            namelen: 255,
            frsize: 512,
        }
    }
}

#[cfg(unix)]
fn current_uid() -> u32 {
    // SAFETY: getuid() is always safe; it cannot fail and touches no memory.
    unsafe { libc::getuid() }
}

#[cfg(unix)]
fn current_gid() -> u32 {
    // SAFETY: getgid() is always safe; it cannot fail and touches no memory.
    unsafe { libc::getgid() }
}

#[cfg(not(unix))]
fn current_uid() -> u32 {
    0
}

#[cfg(not(unix))]
fn current_gid() -> u32 {
    0
}

#[cfg(test)]
#[path = "types_tests.rs"]
mod types_tests;
