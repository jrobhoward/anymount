//! NFS program (100003, version 3) — RFC 1813 §3.
//!
//! Every write/mutating procedure answers with a syntactically valid result
//! (`NFS3ERR_ROFS` or `NFS3ERR_NOTSUPP`) rather than `PROC_UNAVAIL`, matching
//! a real read-only NFS server rather than one that simply doesn't implement
//! the protocol.

use std::ffi::OsStr;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::backend::readdir_cookie;
use crate::error::FsError;
use crate::fs::ReadOnlyFs;
use crate::types::{FileAttr, FileKind, Ino};

use super::handle::FileHandle3;
use super::rpc::ProcOutcome;
use super::xdr::{Reader, Writer};

const NFS3_OK: u32 = 0;
const NFS3ERR_NOENT: u32 = 2;
const NFS3ERR_ROFS: u32 = 30;
const NFS3ERR_NOTSUPP: u32 = 10_004;

const FTYPE_REG: u32 = 1;
const FTYPE_DIR: u32 = 2;

/// `FSINFO3`'s `rtmax`/`rtpref`/`wtmax`/`wtpref`, and the cap applied to a
/// client's requested `READ3` `count` before allocating a buffer.
const RTMAX: u32 = 1_048_576;

const FSF3_HOMOGENEOUS: u32 = 0x0008;

/// Conservative estimate of `dirlist3`'s own overhead (`post_op_attr`,
/// `cookieverf3`, the linked-list terminator) subtracted from a client's
/// declared budget before packing entries, so a full reply never exceeds
/// what the client asked for.
const DIRLIST_OVERHEAD: u32 = 128;

/// Per-call context: the filesystem being served and this mount's handle
/// codec.
pub(super) struct Ctx<'a, F: ReadOnlyFs> {
    pub(super) fs: &'a F,
    pub(super) handle: &'a FileHandle3,
    /// Constant per mount; there is exactly one export.
    pub(super) fsid: u64,
}

/// Route one NFS-program call to its procedure handler.
pub(super) fn dispatch<F: ReadOnlyFs>(
    proc_: u32,
    r: &mut Reader<'_>,
    ctx: &Ctx<'_, F>,
) -> ProcOutcome {
    match proc_ {
        0 => ProcOutcome::Success(Writer::new()), // NULL
        1 => getattr(r, ctx),
        2 => reject_rofs_wcc(r), // SETATTR3
        3 => lookup(r, ctx),
        4 => access(r, ctx),
        5 => reject_notsupp_readlink(r),
        6 => read(r, ctx),
        7 => reject_rofs_wcc(r),     // WRITE3
        8 => reject_rofs_wcc(r),     // CREATE3
        9 => reject_rofs_wcc(r),     // MKDIR3
        10 => reject_notsupp_wcc(r), // SYMLINK3
        11 => reject_rofs_wcc(r),    // MKNOD3
        12 => reject_rofs_wcc(r),    // REMOVE3
        13 => reject_rofs_wcc(r),    // RMDIR3
        14 => reject_rofs_rename(r),
        15 => reject_rofs_link(r),
        16 => readdir_common(r, ctx, false),
        17 => readdir_common(r, ctx, true),
        18 => fsstat(r, ctx),
        19 => fsinfo(r, ctx),
        20 => pathconf(r, ctx),
        21 => reject_rofs_wcc(r), // COMMIT3
        _ => ProcOutcome::ProcUnavail,
    }
}

fn write_nfstime3(w: &mut Writer, t: SystemTime) {
    let dur = t.duration_since(UNIX_EPOCH).unwrap_or_default();
    w.write_u32(dur.as_secs() as u32);
    w.write_u32(dur.subsec_nanos());
}

pub(super) fn write_fattr3(w: &mut Writer, attr: &FileAttr, fsid: u64) {
    let ftype = match attr.kind {
        FileKind::File => FTYPE_REG,
        FileKind::Directory => FTYPE_DIR,
    };
    w.write_u32(ftype);
    w.write_u32(u32::from(attr.perm));
    w.write_u32(attr.nlink);
    w.write_u32(attr.uid);
    w.write_u32(attr.gid);
    w.write_u64(attr.size);
    w.write_u64(attr.size); // used: no sparse-file concept
    w.write_u32(0); // specdata3.major
    w.write_u32(0); // specdata3.minor
    w.write_u64(fsid);
    w.write_u64(attr.ino.0);
    write_nfstime3(w, attr.atime);
    write_nfstime3(w, attr.mtime);
    write_nfstime3(w, attr.ctime);
}

fn write_post_op_attr_ok(w: &mut Writer, attr: &FileAttr, fsid: u64) {
    w.write_bool(true);
    write_fattr3(w, attr, fsid);
}

fn write_post_op_attr_absent(w: &mut Writer) {
    w.write_bool(false);
}

fn write_post_op_attr_best_effort<F: ReadOnlyFs>(w: &mut Writer, ctx: &Ctx<'_, F>, ino: Ino) {
    match ctx.fs.getattr(ino) {
        Ok(attr) => write_post_op_attr_ok(w, &attr, ctx.fsid),
        Err(_) => write_post_op_attr_absent(w),
    }
}

/// `wcc_data`: `pre_op_attr` (always absent — this server never mutates) plus
/// `post_op_attr` (always absent here too, since the rejection table needs no
/// real attributes).
fn write_wcc_data_zero(w: &mut Writer) {
    w.write_bool(false); // pre_op_attr
    w.write_bool(false); // post_op_attr
}

fn reject_rofs_wcc(_r: &mut Reader<'_>) -> ProcOutcome {
    let mut w = Writer::new();
    w.write_u32(NFS3ERR_ROFS);
    write_wcc_data_zero(&mut w);
    ProcOutcome::Success(w)
}

fn reject_notsupp_wcc(_r: &mut Reader<'_>) -> ProcOutcome {
    let mut w = Writer::new();
    w.write_u32(NFS3ERR_NOTSUPP);
    write_wcc_data_zero(&mut w);
    ProcOutcome::Success(w)
}

fn reject_rofs_rename(_r: &mut Reader<'_>) -> ProcOutcome {
    let mut w = Writer::new();
    w.write_u32(NFS3ERR_ROFS);
    write_wcc_data_zero(&mut w);
    write_wcc_data_zero(&mut w);
    ProcOutcome::Success(w)
}

fn reject_rofs_link(_r: &mut Reader<'_>) -> ProcOutcome {
    let mut w = Writer::new();
    w.write_u32(NFS3ERR_ROFS);
    write_post_op_attr_absent(&mut w);
    write_wcc_data_zero(&mut w);
    ProcOutcome::Success(w)
}

fn reject_notsupp_readlink(_r: &mut Reader<'_>) -> ProcOutcome {
    let mut w = Writer::new();
    w.write_u32(NFS3ERR_NOTSUPP);
    write_post_op_attr_absent(&mut w);
    ProcOutcome::Success(w)
}

fn getattr<F: ReadOnlyFs>(r: &mut Reader<'_>, ctx: &Ctx<'_, F>) -> ProcOutcome {
    let Some(fh) = r.read_opaque_var(64) else {
        return ProcOutcome::GarbageArgs;
    };
    let mut w = Writer::new();
    match ctx.handle.resolve(&fh) {
        Some(ino) => match ctx.fs.getattr(ino) {
            Ok(attr) => {
                w.write_u32(NFS3_OK);
                write_fattr3(&mut w, &attr, ctx.fsid);
            }
            Err(e) => w.write_u32(e.to_nfsstat3()),
        },
        None => w.write_u32(NFS3ERR_NOENT),
    }
    ProcOutcome::Success(w)
}

fn lookup<F: ReadOnlyFs>(r: &mut Reader<'_>, ctx: &Ctx<'_, F>) -> ProcOutcome {
    let Some(dirfh) = r.read_opaque_var(64) else {
        return ProcOutcome::GarbageArgs;
    };
    let Some(name) = r.read_string(1024) else {
        return ProcOutcome::GarbageArgs;
    };
    let mut w = Writer::new();
    let Some(dir_ino) = ctx.handle.resolve(&dirfh) else {
        w.write_u32(NFS3ERR_NOENT);
        write_post_op_attr_absent(&mut w);
        return ProcOutcome::Success(w);
    };
    match ctx.fs.lookup(dir_ino, &name) {
        Ok(attr) => {
            w.write_u32(NFS3_OK);
            w.write_opaque_var(&ctx.handle.encode(attr.ino));
            write_post_op_attr_ok(&mut w, &attr, ctx.fsid);
            write_post_op_attr_best_effort(&mut w, ctx, dir_ino);
        }
        Err(e) => {
            w.write_u32(e.to_nfsstat3());
            write_post_op_attr_best_effort(&mut w, ctx, dir_ino);
        }
    }
    ProcOutcome::Success(w)
}

fn access<F: ReadOnlyFs>(r: &mut Reader<'_>, ctx: &Ctx<'_, F>) -> ProcOutcome {
    const READ: u32 = 1;
    const LOOKUP: u32 = 2;
    const EXECUTE: u32 = 32;

    let Some(fh) = r.read_opaque_var(64) else {
        return ProcOutcome::GarbageArgs;
    };
    let Some(requested) = r.read_u32() else {
        return ProcOutcome::GarbageArgs;
    };
    let mut w = Writer::new();
    let Some(ino) = ctx.handle.resolve(&fh) else {
        w.write_u32(NFS3ERR_NOENT);
        write_post_op_attr_absent(&mut w);
        return ProcOutcome::Success(w);
    };
    match ctx.fs.getattr(ino) {
        Ok(attr) => {
            w.write_u32(NFS3_OK);
            write_post_op_attr_ok(&mut w, &attr, ctx.fsid);
            w.write_u32(requested & (READ | LOOKUP | EXECUTE));
        }
        Err(e) => {
            w.write_u32(e.to_nfsstat3());
            write_post_op_attr_absent(&mut w);
        }
    }
    ProcOutcome::Success(w)
}

fn read<F: ReadOnlyFs>(r: &mut Reader<'_>, ctx: &Ctx<'_, F>) -> ProcOutcome {
    let Some(fh) = r.read_opaque_var(64) else {
        return ProcOutcome::GarbageArgs;
    };
    let Some(offset) = r.read_u64() else {
        return ProcOutcome::GarbageArgs;
    };
    let Some(count) = r.read_u32() else {
        return ProcOutcome::GarbageArgs;
    };
    let count = count.min(RTMAX);

    let mut w = Writer::new();
    let Some(ino) = ctx.handle.resolve(&fh) else {
        w.write_u32(NFS3ERR_NOENT);
        write_post_op_attr_absent(&mut w);
        return ProcOutcome::Success(w);
    };

    match ctx.fs.open(ino) {
        Ok(fh) => {
            let mut buf = vec![0u8; count as usize];
            match ctx.fs.read_at(fh, offset, &mut buf) {
                Ok(n) => {
                    buf.truncate(n);
                    // A failed release cannot be reported usefully here;
                    // logged elsewhere is out of scope for v1.
                    let _ = ctx.fs.release(fh);
                    let attr = ctx.fs.getattr(ino).ok();
                    w.write_u32(NFS3_OK);
                    match &attr {
                        Some(a) => write_post_op_attr_ok(&mut w, a, ctx.fsid),
                        None => write_post_op_attr_absent(&mut w),
                    }
                    w.write_u32(n as u32);
                    let eof = attr.as_ref().is_some_and(|a| offset + n as u64 >= a.size);
                    w.write_bool(eof);
                    w.write_opaque_var(&buf);
                }
                Err(e) => {
                    let _ = ctx.fs.release(fh);
                    w.write_u32(e.to_nfsstat3());
                    write_post_op_attr_best_effort(&mut w, ctx, ino);
                }
            }
        }
        Err(e) => {
            w.write_u32(e.to_nfsstat3());
            write_post_op_attr_best_effort(&mut w, ctx, ino);
        }
    }
    ProcOutcome::Success(w)
}

/// Result of packing as many directory entries as fit into a budget.
struct DirList {
    /// Encoded `entry3`/`entryplus3` items, without the final list
    /// terminator or the outer `dirlist3.eof` flag.
    entries: Writer,
    /// `true` only once the trait's `readdir` truly ran out of entries to
    /// serve, not merely because the budget was exhausted this call.
    eof: bool,
}

fn fits(remaining: i64, candidate: &Writer) -> bool {
    candidate.len() as i64 <= remaining
}

fn encode_dirent<F: ReadOnlyFs>(
    ctx: &Ctx<'_, F>,
    ino: Ino,
    name: &OsStr,
    cookie: u64,
    plus: bool,
) -> Writer {
    let mut w = Writer::new();
    w.write_bool(true); // value_follows
    w.write_u64(ino.0); // fileid3
    w.write_string(name);
    w.write_u64(cookie);
    if plus {
        match ctx.fs.getattr(ino) {
            Ok(attr) => {
                w.write_bool(true);
                write_fattr3(&mut w, &attr, ctx.fsid);
                w.write_bool(true);
                w.write_opaque_var(&ctx.handle.encode(ino));
            }
            Err(_) => {
                w.write_bool(false); // name_attributes
                w.write_bool(false); // name_handle
            }
        }
    }
    w
}

/// Pack `.`, `..` and the trait's own entries (resuming from `start_cookie`)
/// into `budget` bytes, stopping — with `eof: false` — at the first entry
/// that would not fit, rather than dropping/truncating one already appended.
fn build_dirlist<F: ReadOnlyFs>(
    ctx: &Ctx<'_, F>,
    dir: Ino,
    start_cookie: u64,
    budget: u32,
    plus: bool,
) -> Result<DirList, FsError> {
    let mut entries = Writer::new();
    let mut remaining = i64::from(budget);
    let mut cookie = start_cookie;

    if cookie == 0 {
        let candidate = encode_dirent(ctx, dir, OsStr::new("."), readdir_cookie::DOT, plus);
        if !fits(remaining, &candidate) {
            return Ok(DirList {
                entries,
                eof: false,
            });
        }
        remaining -= candidate.len() as i64;
        entries.extend_from(&candidate);
        cookie = readdir_cookie::DOT;
    }

    if cookie == readdir_cookie::DOT {
        let parent = ctx.fs.lookup(dir, OsStr::new(".."))?;
        let candidate = encode_dirent(
            ctx,
            parent.ino,
            OsStr::new(".."),
            readdir_cookie::DOTDOT,
            plus,
        );
        if !fits(remaining, &candidate) {
            return Ok(DirList {
                entries,
                eof: false,
            });
        }
        remaining -= candidate.len() as i64;
        entries.extend_from(&candidate);
        cookie = readdir_cookie::DOTDOT;
    }

    let offset = readdir_cookie::trait_offset(cookie);
    let dir_entries = ctx.fs.readdir(dir, offset)?;
    for (i, entry) in dir_entries.iter().enumerate() {
        let entry_cookie = readdir_cookie::for_entry(offset + i as u64);
        let candidate = encode_dirent(ctx, entry.ino, &entry.name, entry_cookie, plus);
        if !fits(remaining, &candidate) {
            return Ok(DirList {
                entries,
                eof: false,
            });
        }
        remaining -= candidate.len() as i64;
        entries.extend_from(&candidate);
    }
    Ok(DirList { entries, eof: true })
}

fn readdir_common<F: ReadOnlyFs>(r: &mut Reader<'_>, ctx: &Ctx<'_, F>, plus: bool) -> ProcOutcome {
    let Some(dirfh) = r.read_opaque_var(64) else {
        return ProcOutcome::GarbageArgs;
    };
    let Some(start_cookie) = r.read_u64() else {
        return ProcOutcome::GarbageArgs;
    };
    let Some(_cookieverf) = r.read_opaque_fixed::<8>() else {
        return ProcOutcome::GarbageArgs;
    };
    let budget = if plus {
        let Some(_dircount) = r.read_u32() else {
            return ProcOutcome::GarbageArgs;
        };
        let Some(maxcount) = r.read_u32() else {
            return ProcOutcome::GarbageArgs;
        };
        maxcount
    } else {
        let Some(count) = r.read_u32() else {
            return ProcOutcome::GarbageArgs;
        };
        count
    };

    let mut w = Writer::new();
    let Some(dir) = ctx.handle.resolve(&dirfh) else {
        w.write_u32(NFS3ERR_NOENT);
        write_post_op_attr_absent(&mut w);
        return ProcOutcome::Success(w);
    };

    let budget = budget.saturating_sub(DIRLIST_OVERHEAD);
    match build_dirlist(ctx, dir, start_cookie, budget, plus) {
        Ok(list) => {
            w.write_u32(NFS3_OK);
            write_post_op_attr_best_effort(&mut w, ctx, dir);
            w.write_opaque_fixed(&ctx.handle.cookieverf());
            w.extend_from(&list.entries);
            w.write_bool(false); // list terminator
            w.write_bool(list.eof);
        }
        Err(e) => {
            w.write_u32(e.to_nfsstat3());
            write_post_op_attr_best_effort(&mut w, ctx, dir);
        }
    }
    ProcOutcome::Success(w)
}

fn fsstat<F: ReadOnlyFs>(r: &mut Reader<'_>, ctx: &Ctx<'_, F>) -> ProcOutcome {
    let Some(fh) = r.read_opaque_var(64) else {
        return ProcOutcome::GarbageArgs;
    };
    let mut w = Writer::new();
    let Some(ino) = ctx.handle.resolve(&fh) else {
        w.write_u32(NFS3ERR_NOENT);
        write_post_op_attr_absent(&mut w);
        return ProcOutcome::Success(w);
    };
    match ctx.fs.statfs() {
        Ok(s) => {
            let frsize = u64::from(s.frsize);
            w.write_u32(NFS3_OK);
            write_post_op_attr_best_effort(&mut w, ctx, ino);
            w.write_u64(s.blocks * frsize); // tbytes
            w.write_u64(s.bfree * frsize); // fbytes
            w.write_u64(s.bavail * frsize); // abytes
            w.write_u64(s.files); // tfiles
            w.write_u64(s.ffree); // ffiles
            w.write_u64(s.ffree); // afiles
            w.write_u32(u32::MAX); // invarsec: content immutable for the mount's life
        }
        Err(e) => {
            w.write_u32(e.to_nfsstat3());
            write_post_op_attr_best_effort(&mut w, ctx, ino);
        }
    }
    ProcOutcome::Success(w)
}

fn fsinfo<F: ReadOnlyFs>(r: &mut Reader<'_>, ctx: &Ctx<'_, F>) -> ProcOutcome {
    let Some(fh) = r.read_opaque_var(64) else {
        return ProcOutcome::GarbageArgs;
    };
    let mut w = Writer::new();
    let Some(ino) = ctx.handle.resolve(&fh) else {
        w.write_u32(NFS3ERR_NOENT);
        write_post_op_attr_absent(&mut w);
        return ProcOutcome::Success(w);
    };
    w.write_u32(NFS3_OK);
    write_post_op_attr_best_effort(&mut w, ctx, ino);
    w.write_u32(RTMAX);
    w.write_u32(RTMAX);
    w.write_u32(4096); // rtmult
    w.write_u32(RTMAX); // wtmax: writes always rejected, but must be present/nonzero
    w.write_u32(RTMAX); // wtpref
    w.write_u32(4096); // wtmult
    w.write_u32(32_768); // dtpref
    w.write_u64(u64::MAX); // maxfilesize
    w.write_u32(1); // time_delta.seconds
    w.write_u32(0); // time_delta.nseconds
    w.write_u32(FSF3_HOMOGENEOUS);
    ProcOutcome::Success(w)
}

fn pathconf<F: ReadOnlyFs>(r: &mut Reader<'_>, ctx: &Ctx<'_, F>) -> ProcOutcome {
    let Some(fh) = r.read_opaque_var(64) else {
        return ProcOutcome::GarbageArgs;
    };
    let mut w = Writer::new();
    let Some(ino) = ctx.handle.resolve(&fh) else {
        w.write_u32(NFS3ERR_NOENT);
        write_post_op_attr_absent(&mut w);
        return ProcOutcome::Success(w);
    };
    w.write_u32(NFS3_OK);
    write_post_op_attr_best_effort(&mut w, ctx, ino);
    w.write_u32(1); // linkmax
    w.write_u32(255); // name_max, matching StatFs::default().namelen
    w.write_bool(true); // no_trunc
    w.write_bool(true); // chown_restricted
    w.write_bool(false); // case_insensitive
    w.write_bool(true); // case_preserving
    ProcOutcome::Success(w)
}

#[cfg(test)]
#[path = "nfs_proto_tests.rs"]
mod nfs_proto_tests;
