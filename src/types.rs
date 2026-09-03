//! Core value types: inodes, handles, attributes, directory entries.

use std::ffi::OsString;
use std::time::SystemTime;

/// Inode number. Stable for the lifetime of a mount.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Ino(pub u64);

/// The root of every mount.
pub const ROOT_INO: Ino = Ino(1);

/// An open-file token handed back by [`crate::ReadOnlyFs::open`].
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct FileHandle(pub u64);

/// What an inode is.
///
/// Deliberately only two variants: symlinks are not represented because the
/// first consumer (ciphercask) does not back them up, and neither ProjFS nor
/// cfapi models them the way FUSE does. See `docs/GAPS.md`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum FileKind {
    File,
    Directory,
}

/// Metadata for one inode.
#[derive(Clone, Debug)]
pub struct FileAttr {
    pub ino: Ino,
    pub kind: FileKind,
    /// Logical size in bytes. Zero for directories.
    pub size: u64,
    /// Unix permission bits (e.g. `0o644`). Advisory on Windows.
    pub perm: u16,
    pub nlink: u32,
    pub uid: u32,
    pub gid: u32,
    pub atime: SystemTime,
    pub mtime: SystemTime,
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
    pub ino: Ino,
    pub name: OsString,
    pub kind: FileKind,
}

/// Filesystem-wide statistics, reported to `statfs(2)`.
#[derive(Clone, Debug)]
pub struct StatFs {
    pub blocks: u64,
    pub bfree: u64,
    pub bavail: u64,
    pub files: u64,
    pub ffree: u64,
    pub bsize: u32,
    pub namelen: u32,
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
