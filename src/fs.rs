//! The filesystem trait implementors provide.

use std::ffi::{OsStr, OsString};

use crate::error::{FsError, Result};
use crate::types::{DirEntry, FileAttr, FileHandle, Ino, StatFs};

/// A read-only filesystem.
///
/// The operation set is the FUSE *lowlevel* intersection, chosen because cfapi
/// maps onto it but not the reverse.
///
/// # Read patterns differ by backend
///
/// [`read_at`](Self::read_at) is called differently by each backend. FUSE issues
/// random reads driven by the application. cfapi calls it sequentially, during
/// hydration, because it materialises a whole file before handing it to the
/// application. An implementation that can only decode sequentially is
/// therefore already efficient on Windows, and needs a cache only for the FUSE
/// path.
///
/// # Concurrency
///
/// Methods take `&self` and the trait requires `Send + Sync`: the backend may
/// call into it from several threads at once. Implementors must do their own
/// interior locking, including for concurrent [`read_at`](Self::read_at) calls
/// against the same [`FileHandle`] — a backend serving several worker threads
/// may issue two reads on one handle at once.
///
/// # Inode lifetime
///
/// An [`Ino`] returned from [`lookup`](Self::lookup) or
/// [`readdir`](Self::readdir) must answer [`getattr`](Self::getattr) correctly
/// for as long as the mount is live — there is no eviction contract. FUSE's
/// kernel client tracks a per-inode lookup count and sends `forget` once it
/// drops to zero, normally so a filesystem can free cached state; this trait
/// does not require that bookkeeping; implementations that hold no
/// inode-keyed cache can ignore [`forget`](Self::forget) entirely (its default
/// does nothing). It exists only so an implementor *choosing* to cache — for
/// example a `readdir`-built tree that is expensive to reconstruct — has a
/// hook to evict entries the kernel no longer references. cfapi has no
/// equivalent notification, so `forget` is never called on Windows.
///
/// # `.` and `..`
///
/// FUSE's kernel client never sends these names to [`lookup`](Self::lookup);
/// it resolves them from its own dentry cache. NFS clients have no such
/// cache and issue real wire `LOOKUP` calls for both. Implementations
/// intended to work under the NFS backend should answer
/// `lookup(dir, ".")` with `dir`'s own attributes and `lookup(dir, "..")`
/// with the parent's, the way `examples/memfs.rs` does.
/// [`readdir`](Self::readdir) must still never return either — the backend
/// synthesizes them, obtaining `..`'s target [`Ino`] the same way, via
/// `lookup(dir, "..")`.
///
/// # Handle lifetime
///
/// [`open`](Self::open) may be called more than once for the same `ino`,
/// each call returning a distinct [`FileHandle`] with its own lifetime; the
/// handles need not be released in the order they were opened, and a handle
/// is never reused by [`open`](Self::open) after its
/// [`release`](Self::release). Implementors that key a per-open cursor or
/// cache off the handle should scope it to that handle, not to `ino`.
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

    /// The kernel no longer needs `ino` cached; see "Inode lifetime" above.
    /// `nlookup` is how many outstanding lookups this covers, matching FUSE's
    /// own `forget` semantics. Defaults to doing nothing, which is correct
    /// for any implementation that keeps no inode-keyed cache. Never called
    /// by the cfapi backend.
    fn forget(&self, _ino: Ino, _nlookup: u64) {}

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
