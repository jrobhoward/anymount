//! FUSE backend, used on Linux and macOS.
//!
//! On Linux `fuser` is built with `default-features = false`, so mounting goes
//! through the `fusermount3` helper binary rather than linking libfuse. That
//! keeps LGPL code out of the link *and* permits unprivileged mounts.
//!
//! On macOS the libfuse path is required, because macFUSE supplies the mount
//! helper. The dylib is resolved at runtime and never vendored.

use std::ffi::OsStr;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use fuser::{
    BackgroundSession, Config, Errno, FopenFlags, Generation, INodeNo, LockOwner, MountOption,
    OpenFlags, ReplyAttr, ReplyData, ReplyDirectory, ReplyEmpty, ReplyEntry, ReplyOpen,
    ReplyStatfs, Request, SessionACL,
};

use crate::error::{FsError, Result};
use crate::fs::ReadOnlyFs;
use crate::mount::MountBuilder;
use crate::types::{FileAttr, FileHandle, FileKind, Ino};

/// How long the kernel may cache attributes and lookups.
///
/// A backup snapshot is immutable for the life of the mount, so a long TTL is
/// safe and removes most of the round-trips a shorter value would cost.
const TTL: Duration = Duration::from_secs(60);

/// Inodes are stable for the life of a mount, so generations are never reused.
const GENERATION: Generation = Generation(0);

/// Live FUSE session. Unmounts when dropped.
pub(crate) struct FuseHandle {
    session: Option<BackgroundSession>,
}

impl FuseHandle {
    pub(crate) fn unmount(mut self) -> Result<()> {
        if let Some(session) = self.session.take() {
            session.umount_and_join()?;
        }
        Ok(())
    }
}

pub(crate) fn mount<F: ReadOnlyFs>(builder: MountBuilder, fs: F) -> Result<FuseHandle> {
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
    // Serve requests on several threads so one slow read cannot stall the
    // whole mount; `clone_fd` gives each worker its own fd (Linux 4.5+).
    config.n_threads = Some(4);
    config.clone_fd = cfg!(target_os = "linux");

    let adapter = FuseAdapter { fs: Arc::new(fs) };
    let session = fuser::spawn_mount(adapter, &builder.mountpoint, &config)?;

    Ok(FuseHandle {
        session: Some(session),
    })
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
        let mut buf = vec![0u8; size as usize];
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
        // the error is dropped rather than turned into a spurious errno.
        let _ = self.fs.release(FileHandle(fh.0));
        reply.ok();
    }

    fn readdir(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: fuser::FileHandle,
        offset: u64,
        mut reply: ReplyDirectory,
    ) {
        // `.` and `..` occupy the first two cookies; the trait never returns
        // them, so entry N from the trait is cookie N + 3.
        let mut next = offset;
        if next == 0 {
            if reply.add(ino, 1, fuser::FileType::Directory, ".") {
                reply.ok();
                return;
            }
            next = 1;
        }
        if next == 1 {
            if reply.add(ino, 2, fuser::FileType::Directory, "..") {
                reply.ok();
                return;
            }
            next = 2;
        }

        match self.fs.readdir(Ino(ino.0), next - 2) {
            Ok(entries) => {
                for (i, entry) in entries.iter().enumerate() {
                    let cookie = next + i as u64 + 1;
                    if reply.add(
                        INodeNo(entry.ino.0),
                        cookie,
                        to_fuser_kind(entry.kind),
                        &entry.name,
                    ) {
                        break;
                    }
                }
                reply.ok();
            }
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
}

fn errno(e: &crate::error::FsError) -> Errno {
    Errno::from_i32(e.to_errno())
}

fn to_fuser_kind(kind: FileKind) -> fuser::FileType {
    match kind {
        FileKind::File => fuser::FileType::RegularFile,
        FileKind::Directory => fuser::FileType::Directory,
    }
}

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
