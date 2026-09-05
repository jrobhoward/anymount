//! FUSE backend, Linux only.
//!
//! `fuser` is built with `default-features = false`, so mounting goes through
//! the `fusermount3` helper binary rather than linking libfuse. That keeps
//! LGPL code out of the link *and* permits unprivileged mounts.
//!
//! macOS's decided backend is NFS, not FUSE — see `docs/PLAN.md` — so this
//! module is gated to Linux only (`backend/mod.rs`). There is no macFUSE
//! fallback in the tree.

use std::ffi::OsStr;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use fuser::{
    BackgroundSession, Config, Errno, FopenFlags, Generation, INodeNo, LockOwner, MountOption,
    OpenFlags, ReplyAttr, ReplyData, ReplyDirectory, ReplyEmpty, ReplyEntry, ReplyOpen,
    ReplyStatfs, ReplyXattr, Request, SessionACL,
};

use crate::backend::Mounted;
use crate::backend::preflight::{self, Caps};
use crate::backend::readdir::{self, Dots, Sink};
use crate::backend::trace::backend_warn;
use crate::error::{FsError, Result};
use crate::fs::ReadOnlyFs;
use crate::mount::{Backend, MountBuilder};
use crate::types::{FileAttr, FileHandle, FileKind, Ino};

/// How long the kernel may cache attributes and lookups.
///
/// A backup snapshot is immutable for the life of the mount, so a long TTL is
/// safe and removes most of the round-trips a shorter value would cost.
const TTL: Duration = Duration::from_secs(60);

/// Inodes are stable for the life of a mount, so generations are never reused.
const GENERATION: Generation = Generation(0);

/// Worker threads serving kernel requests, so one slow read cannot stall the
/// whole mount. Four is enough to keep a single reader plus a directory walk
/// from queueing behind each other without making an implementor's locking a
/// bottleneck.
const WORKER_THREADS: usize = 4;

/// Upper bound on a single `read` allocation.
///
/// The kernel negotiates its own maximum and does not exceed it, so this is a
/// belt-and-braces cap on trusting a `size` field, mirroring the NFS backend's
/// `RTMAX`. A short read is valid at end-of-file, and the kernel reissues for
/// the remainder, so capping cannot lose data.
const MAX_READ: u32 = 16 * 1024 * 1024;

const CAPS: Caps = Caps {
    name: "fuse",
    allow_other: true,
    auto_unmount: true,
};

/// Live FUSE session.
#[derive(Debug)]
pub(crate) struct FuseHandle {
    session: BackgroundSession,
}

impl Mounted for FuseHandle {
    /// `umount_and_join` both unmounts and joins the serving thread. Dropping a
    /// [`BackgroundSession`] would unmount too — `fuser::Mount`'s own `Drop`
    /// does that — but would leave the thread detached, so teardown always goes
    /// through here rather than through the drop glue.
    fn unmount(self: Box<Self>) -> Result<()> {
        self.session.umount_and_join()?;
        Ok(())
    }

    fn backend(&self) -> Backend {
        Backend::Fuse
    }
}

pub(crate) fn mount<F: ReadOnlyFs>(builder: MountBuilder, fs: F) -> Result<FuseHandle> {
    preflight::check(&builder, &CAPS)?;

    // Not a capability gap — FUSE supports both options, just not together —
    // so this stays here rather than in `Caps`.
    if builder.auto_unmount && !builder.allow_other {
        return Err(FsError::InvalidArgument.context(
            "auto_unmount requires allow_other: fusermount3 refuses to arm \
             auto-unmount on an owner-private mount. Either call \
             .allow_other(true) (needs user_allow_other in /etc/fuse.conf) or \
             leave auto_unmount off and clean up with `fusermount3 -u`",
        ));
    }

    let mut mount_options = vec![
        MountOption::RO,
        MountOption::FSName(builder.fs_name.clone()),
    ];
    if builder.auto_unmount {
        mount_options.push(MountOption::AutoUnmount);
    }

    let mut config = Config::default();
    config.mount_options = mount_options;
    config.acl = if builder.allow_other {
        SessionACL::All
    } else {
        SessionACL::Owner
    };
    config.n_threads = Some(WORKER_THREADS);
    // Each worker gets its own fd (Linux 4.5+). This module is Linux-only, so
    // there is no platform to condition on.
    config.clone_fd = true;

    let adapter = FuseAdapter { fs: Arc::new(fs) };
    let session = fuser::spawn_mount(adapter, &builder.mountpoint, &config)?;

    Ok(FuseHandle { session })
}

/// Translates `fuser::Filesystem` callbacks into [`ReadOnlyFs`] calls.
///
/// `fuser` 0.18 takes `&self` on every callback, which lines up exactly with
/// [`ReadOnlyFs`] being `Send + Sync` — no interior mutability is needed here.
struct FuseAdapter<F: ReadOnlyFs> {
    fs: Arc<F>,
}

impl<F: ReadOnlyFs> fuser::Filesystem for FuseAdapter<F> {
    fn lookup(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEntry) {
        match self.fs.lookup(Ino(parent.0), name) {
            Ok(attr) => reply.entry(&TTL, &to_fuser_attr(&attr), GENERATION),
            Err(e) => reply.error(errno(&e)),
        }
    }

    fn getattr(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: Option<fuser::FileHandle>,
        reply: ReplyAttr,
    ) {
        match self.fs.getattr(Ino(ino.0)) {
            Ok(attr) => reply.attr(&TTL, &to_fuser_attr(&attr)),
            Err(e) => reply.error(errno(&e)),
        }
    }

    fn open(&self, _req: &Request, ino: INodeNo, _flags: OpenFlags, reply: ReplyOpen) {
        match self.fs.open(Ino(ino.0)) {
            Ok(fh) => reply.opened(fuser::FileHandle(fh.0), FopenFlags::empty()),
            Err(e) => reply.error(errno(&e)),
        }
    }

    fn read(
        &self,
        _req: &Request,
        _ino: INodeNo,
        fh: fuser::FileHandle,
        offset: u64,
        size: u32,
        _flags: OpenFlags,
        _lock_owner: Option<LockOwner>,
        reply: ReplyData,
    ) {
        let mut buf = vec![0u8; read_buffer_len(size)];
        match self.fs.read_at(FileHandle(fh.0), offset, &mut buf) {
            Ok(n) => {
                buf.truncate(n);
                reply.data(&buf);
            }
            Err(e) => reply.error(errno(&e)),
        }
    }

    fn release(
        &self,
        _req: &Request,
        _ino: INodeNo,
        fh: fuser::FileHandle,
        _flags: OpenFlags,
        _lock_owner: Option<LockOwner>,
        _flush: bool,
        reply: ReplyEmpty,
    ) {
        // A failed release cannot be reported usefully to the application, so
        // it is logged rather than turned into a spurious errno.
        if let Err(e) = self.fs.release(FileHandle(fh.0)) {
            backend_warn!("anymount/fuse: release of handle {} failed: {e}", fh.0);
        }
        reply.ok();
    }

    // `batch_forget`'s default already forwards each node to `forget`, so no
    // separate override is needed here.
    fn forget(&self, _req: &Request, ino: INodeNo, nlookup: u64) {
        self.fs.forget(Ino(ino.0), nlookup);
    }

    fn readdir(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: fuser::FileHandle,
        offset: u64,
        mut reply: ReplyDirectory,
    ) {
        // `ReplyDirectory::add` returns `true` when the entry did *not* fit,
        // which is exactly `Sink::Full`; everything else about the listing —
        // synthesizing `.`/`..`, the cookie arithmetic, resuming mid-directory
        // — lives in `backend::readdir` and is shared with the NFS backend.
        let outcome = readdir::emit(
            self.fs.as_ref(),
            Ino(ino.0),
            offset,
            Dots::Synthesize,
            |e| {
                if reply.add(INodeNo(e.ino.0), e.cookie, to_fuser_kind(e.kind), e.name) {
                    Sink::Full
                } else {
                    Sink::Accepted
                }
            },
        );

        match outcome {
            Ok(_) => reply.ok(),
            Err(e) => reply.error(errno(&e)),
        }
    }

    fn statfs(&self, _req: &Request, _ino: INodeNo, reply: ReplyStatfs) {
        match self.fs.statfs() {
            Ok(s) => reply.statfs(
                s.blocks, s.bfree, s.bavail, s.files, s.ffree, s.bsize, s.namelen, s.frsize,
            ),
            Err(e) => reply.error(errno(&e)),
        }
    }

    fn listxattr(&self, _req: &Request, ino: INodeNo, size: u32, reply: ReplyXattr) {
        match self.fs.listxattr(Ino(ino.0)) {
            Ok(names) => {
                let mut buf = Vec::new();
                for name in &names {
                    buf.extend_from_slice(name.as_encoded_bytes());
                    buf.push(0);
                }
                reply_xattr(&buf, size, reply);
            }
            Err(e) => reply.error(errno(&e)),
        }
    }

    fn getxattr(&self, _req: &Request, ino: INodeNo, name: &OsStr, size: u32, reply: ReplyXattr) {
        match self.fs.getxattr(Ino(ino.0), name) {
            Ok(value) => reply_xattr(&value, size, reply),
            Err(e) => reply.error(errno(&e)),
        }
    }
}

/// How a `getxattr`/`listxattr` call should be answered, decided separately
/// from answering it so the convention can be unit tested without a live
/// session — `ReplyXattr` can only be constructed by `fuser` itself.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum XattrReply {
    /// The caller passed `size == 0`, asking for the length only.
    Size(u32),
    /// The value fits in the caller's buffer.
    Data,
    /// The value is larger than the buffer the caller offered.
    TooLarge,
}

/// FUSE's size-query convention for `getxattr`/`listxattr`: `size == 0` asks
/// for the value's length only; otherwise the value is returned in full, or
/// the request is rejected with `ERANGE` rather than silently truncated.
fn xattr_reply(len: usize, size: u32) -> XattrReply {
    if size == 0 {
        XattrReply::Size(len as u32)
    } else if len > size as usize {
        XattrReply::TooLarge
    } else {
        XattrReply::Data
    }
}

fn reply_xattr(data: &[u8], size: u32, reply: ReplyXattr) {
    match xattr_reply(data.len(), size) {
        XattrReply::Size(len) => reply.size(len),
        XattrReply::TooLarge => reply.error(Errno::ERANGE),
        XattrReply::Data => reply.data(data),
    }
}

/// Bytes to allocate for a `read` of `size`, capped at [`MAX_READ`].
fn read_buffer_len(size: u32) -> usize {
    size.min(MAX_READ) as usize
}

fn errno(e: &FsError) -> Errno {
    Errno::from_i32(e.to_errno())
}

fn to_fuser_kind(kind: FileKind) -> fuser::FileType {
    match kind {
        FileKind::File => fuser::FileType::RegularFile,
        FileKind::Directory => fuser::FileType::Directory,
    }
}

#[cfg(test)]
#[path = "fuse_tests.rs"]
mod fuse_tests;

fn to_fuser_attr(attr: &FileAttr) -> fuser::FileAttr {
    fuser::FileAttr {
        ino: INodeNo(attr.ino.0),
        size: attr.size,
        blocks: attr.size.div_ceil(512),
        atime: attr.atime,
        mtime: attr.mtime,
        ctime: attr.ctime,
        crtime: SystemTime::UNIX_EPOCH,
        kind: to_fuser_kind(attr.kind),
        perm: attr.perm,
        nlink: attr.nlink,
        uid: attr.uid,
        gid: attr.gid,
        rdev: 0,
        blksize: 512,
        flags: 0,
    }
}
