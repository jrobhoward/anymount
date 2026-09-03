//! The filesystem trait implementors provide.

use std::ffi::{OsStr, OsString};

use crate::error::{FsError, Result};
use crate::types::{DirEntry, FileAttr, FileHandle, Ino, StatFs};

/// A read-only filesystem.
///
/// The operation set is the FUSE *lowlevel* intersection, chosen because ProjFS
/// and cfapi map onto it but not the reverse.
///
/// # Read patterns differ by backend
///
/// [`read_at`](Self::read_at) is called differently by each backend. FUSE issues
/// random reads driven by the application. ProjFS and cfapi call it
/// sequentially, during hydration, because both materialise a whole file before
/// handing it to the application. An implementation that can only decode
/// sequentially is therefore already efficient on Windows, and needs a cache
/// only for the FUSE path.
///
/// # Concurrency
///
/// Methods take `&self` and the trait requires `Send + Sync`: the backend may
/// call into it from several threads at once. Implementors must do their own
/// interior locking.
pub trait ReadOnlyFs: Send + Sync + 'static {
    /// Resolve `name` within directory `parent`.
    fn lookup(&self, parent: Ino, name: &OsStr) -> Result<FileAttr>;

    /// Fetch attributes for `ino`.
    fn getattr(&self, ino: Ino) -> Result<FileAttr>;

    /// List directory `ino`, skipping the first `offset` entries.
    ///
    /// `.` and `..` are synthesised by the backend and must not be returned.
    fn readdir(&self, ino: Ino, offset: u64) -> Result<Vec<DirEntry>>;

    /// Open file `ino`, returning a handle for subsequent reads.
    fn open(&self, ino: Ino) -> Result<FileHandle>;

    /// Read into `buf` starting at `offset`, returning the byte count.
    ///
    /// A short read is only valid at end-of-file.
    fn read_at(&self, fh: FileHandle, offset: u64, buf: &mut [u8]) -> Result<usize>;

    /// Release a handle from [`open`](Self::open). Errors are logged, not propagated.
    fn release(&self, fh: FileHandle) -> Result<()>;

    /// List extended attribute names for `ino`. Defaults to none.
    fn listxattr(&self, _ino: Ino) -> Result<Vec<OsString>> {
        Ok(Vec::new())
    }

    /// Read one extended attribute. Defaults to "no such attribute".
    fn getxattr(&self, _ino: Ino, _name: &OsStr) -> Result<Vec<u8>> {
        Err(FsError::NoXattr)
    }

    /// Filesystem-wide statistics. Defaults to all-zero counters.
    fn statfs(&self) -> Result<StatFs> {
        Ok(StatFs::default())
    }
}
