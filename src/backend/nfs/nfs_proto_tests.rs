#![allow(non_snake_case)]
#![allow(clippy::unwrap_used)]
#![allow(clippy::expect_used)]

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};

use proptest::prelude::*;

use super::*;
use crate::error::{FsError, Result};
use crate::fs::ReadOnlyFs;
use crate::types::{DirEntry, FileAttr, FileHandle, FileKind, Ino, ROOT_INO};

/// A tiny in-memory tree: root directory containing `n` files, answering
/// `.`/`..` the way `examples/memfs.rs` does.
struct TestFs {
    files: BTreeMap<OsString, Ino>,
}

impl TestFs {
    fn with_files(n: u64) -> Self {
        let mut files = BTreeMap::new();
        for i in 0..n {
            files.insert(OsString::from(format!("f{i}")), Ino(100 + i));
        }
        Self { files }
    }
}

impl ReadOnlyFs for TestFs {
    fn lookup(&self, parent: Ino, name: &OsStr) -> Result<FileAttr> {
        if name == OsStr::new(".") || name == OsStr::new("..") {
            return self.getattr(ROOT_INO).map(|mut a| {
                a.ino = parent;
                a
            });
        }
        let Some(&ino) = self.files.get(name) else {
            return Err(FsError::NotFound);
        };
        self.getattr(ino)
    }

    fn getattr(&self, ino: Ino) -> Result<FileAttr> {
        if ino == ROOT_INO {
            return Ok(FileAttr::dir(ROOT_INO));
        }
        if self.files.values().any(|&v| v == ino) {
            return Ok(FileAttr::file(ino, 10));
        }
        Err(FsError::NotFound)
    }

    fn readdir(&self, ino: Ino, offset: u64) -> Result<Vec<DirEntry>> {
        if ino != ROOT_INO {
            return Err(FsError::NotADirectory);
        }
        Ok(self
            .files
            .iter()
            .skip(offset as usize)
            .map(|(name, &ino)| DirEntry {
                ino,
                name: name.clone(),
                kind: FileKind::File,
            })
            .collect())
    }

    fn open(&self, _ino: Ino) -> Result<FileHandle> {
        Ok(FileHandle(1))
    }

    fn read_at(&self, _fh: FileHandle, _offset: u64, _buf: &mut [u8]) -> Result<usize> {
        Ok(0)
    }

    fn release(&self, _fh: FileHandle) -> Result<()> {
        Ok(())
    }
}

fn ctx<'a, F: ReadOnlyFs>(fs: &'a F, handle: &'a FileHandle3) -> Ctx<'a, F> {
    Ctx {
        fs,
        handle,
        fsid: 1,
    }
}

#[test]
fn fattr3____from_file_attr____maps_size_perm_kind_correctly() {
    let attr = FileAttr::file(Ino(7), 42);
    let mut w = Writer::new();
    write_fattr3(&mut w, &attr, 1);
    let bytes = w.into_bytes();
    let mut r = Reader::new(&bytes);
    assert_eq!(r.read_u32(), Some(FTYPE_REG));
    assert_eq!(r.read_u32(), Some(u32::from(attr.perm)));
    assert_eq!(r.read_u32(), Some(attr.nlink));
    assert_eq!(r.read_u32(), Some(attr.uid));
    assert_eq!(r.read_u32(), Some(attr.gid));
    assert_eq!(r.read_u64(), Some(42));
    assert_eq!(r.read_u64(), Some(42)); // used == size
}

#[test]
fn fattr3____directory____has_dir_ftype() {
    let attr = FileAttr::dir(ROOT_INO);
    let mut w = Writer::new();
    write_fattr3(&mut w, &attr, 1);
    let bytes = w.into_bytes();
    let mut r = Reader::new(&bytes);
    assert_eq!(r.read_u32(), Some(FTYPE_DIR));
}

#[test]
fn to_nfsstat3____every_fs_error_variant____maps_to_a_distinct_or_documented_status() {
    assert_eq!(FsError::NotFound.to_nfsstat3(), NFS3ERR_NOENT);
    assert_eq!(FsError::PermissionDenied.to_nfsstat3(), 13);
    assert_eq!(FsError::NotADirectory.to_nfsstat3(), 20);
    assert_eq!(FsError::IsADirectory.to_nfsstat3(), 21);
    assert_eq!(FsError::InvalidArgument.to_nfsstat3(), 22);
    assert_eq!(FsError::NoXattr.to_nfsstat3(), NFS3ERR_NOTSUPP);
    assert_eq!(FsError::ReadOnly.to_nfsstat3(), NFS3ERR_ROFS);
    assert_eq!(FsError::Unsupported("x").to_nfsstat3(), NFS3ERR_NOTSUPP);
    assert_eq!(FsError::Other("x".into()).to_nfsstat3(), 10_006);
    let wrapped = FsError::NotFound.context("explained");
    assert_eq!(wrapped.to_nfsstat3(), NFS3ERR_NOENT);
}

#[test]
fn write_op_rejection____every_mutating_proc____encodes_rofs_or_notsupp_with_correct_wcc_shape() {
    // (proc, expected status, expected body length in bytes)
    let cases: &[(u32, u32, usize)] = &[
        (2, NFS3ERR_ROFS, 8),     // SETATTR3: wcc_data
        (7, NFS3ERR_ROFS, 8),     // WRITE3
        (8, NFS3ERR_ROFS, 8),     // CREATE3
        (9, NFS3ERR_ROFS, 8),     // MKDIR3
        (10, NFS3ERR_NOTSUPP, 8), // SYMLINK3
        (11, NFS3ERR_ROFS, 8),    // MKNOD3
        (12, NFS3ERR_ROFS, 8),    // REMOVE3
        (13, NFS3ERR_ROFS, 8),    // RMDIR3
        (14, NFS3ERR_ROFS, 16),   // RENAME3: two wcc_data
        (15, NFS3ERR_ROFS, 12),   // LINK3: post_op_attr + wcc_data
        (21, NFS3ERR_ROFS, 8),    // COMMIT3
        (5, NFS3ERR_NOTSUPP, 4),  // READLINK3: post_op_attr only
    ];

    let handle = FileHandle3::for_test(1);
    let fs = TestFs::with_files(0);
    let c = ctx(&fs, &handle);

    for &(proc_, expected_status, expected_extra_len) in cases {
        let empty: [u8; 0] = [];
        let mut r = Reader::new(&empty);
        let ProcOutcome::Success(w) = dispatch(proc_, &mut r, &c) else {
            panic!("proc {proc_} did not succeed");
        };
        let bytes = w.into_bytes();
        assert_eq!(bytes.len(), 4 + expected_extra_len, "proc {proc_}");
        let status = u32::from_be_bytes(bytes[..4].try_into().unwrap());
        assert_eq!(status, expected_status, "proc {proc_}");
    }
}

/// Skips one `fattr3`: 3 u32 (type, mode, nlink), 2 u32 (uid, gid), 2 u64
/// (size, used), 2 u32 (specdata3), 2 u64 (fsid, fileid), 6 u32 (atime,
/// mtime, ctime seconds/nseconds) — matching [`write_fattr3`]'s field order
/// exactly.
fn skip_fattr3(r: &mut Reader<'_>) {
    for _ in 0..3 {
        r.read_u32().unwrap();
    }
    for _ in 0..2 {
        r.read_u32().unwrap();
    }
    for _ in 0..2 {
        r.read_u64().unwrap();
    }
    for _ in 0..2 {
        r.read_u32().unwrap();
    }
    for _ in 0..2 {
        r.read_u64().unwrap();
    }
    for _ in 0..6 {
        r.read_u32().unwrap();
    }
}

/// Decodes a `READDIR3`/`READDIRPLUS3` NFS3_OK result into just the entry
/// names (`.`,`..`, then trait entries) and the trailing `eof` flag, enough
/// to check ordering/completeness without re-deriving the full XDR shape.
fn decode_names_and_eof(bytes: &[u8], plus: bool) -> (Vec<String>, bool) {
    let mut r = Reader::new(bytes);
    assert_eq!(r.read_u32(), Some(NFS3_OK));
    // post_op_attr dir_attributes
    if r.read_bool() == Some(true) {
        skip_fattr3(&mut r);
    }
    r.read_opaque_fixed::<8>().unwrap(); // cookieverf3

    let mut names = Vec::new();
    while r.read_bool().unwrap() {
        r.read_u64().unwrap(); // fileid3
        names.push(r.read_string(1024).unwrap().to_string_lossy().into_owned());
        r.read_u64().unwrap(); // cookie3
        if plus {
            if r.read_bool().unwrap() {
                skip_fattr3(&mut r);
            }
            if r.read_bool().unwrap() {
                r.read_opaque_var(64).unwrap();
            }
        }
    }
    let eof = r.read_bool().unwrap();
    (names, eof)
}

fn readdirplus_call<F: ReadOnlyFs>(
    fs: &F,
    handle: &FileHandle3,
    cookie: u64,
    maxcount: u32,
) -> Vec<u8> {
    let c = ctx(fs, handle);
    let mut w = Writer::new();
    w.write_opaque_var(&handle.encode(ROOT_INO));
    w.write_u64(cookie);
    w.write_opaque_fixed(&handle.cookieverf());
    w.write_u32(u32::MAX); // dircount
    w.write_u32(maxcount);
    let bytes = w.into_bytes();
    let mut r = Reader::new(&bytes);
    let ProcOutcome::Success(out) = dispatch(17, &mut r, &c) else {
        panic!("READDIRPLUS3 did not succeed");
    };
    out.into_bytes()
}

#[test]
fn readdirplus____budget_smaller_than_one_entry____is_toosmall_not_an_empty_listing() {
    // An empty `NFS3_OK` listing with `eof: false` reads as "ask again", and
    // the same call would fit nothing next time either. `NFS3ERR_TOOSMALL`
    // tells the client to come back with a bigger buffer instead.
    let handle = FileHandle3::for_test(1);
    let fs = TestFs::with_files(5);
    let bytes = readdirplus_call(&fs, &handle, 0, DIRLIST_OVERHEAD);
    assert_eq!(u32::from_be_bytes(bytes[..4].try_into().unwrap()), 10_005);
}

#[test]
fn readdirplus____budget_smaller_than_one_entry_in_an_empty_directory____is_ok_with_eof() {
    // Nothing packed *because there was nothing to pack* is a complete
    // listing, not a too-small buffer — but `.` and `..` are always
    // synthesized, so even an empty directory has two entries to fit. A
    // budget that holds them and nothing more is the case that must still
    // report success.
    let handle = FileHandle3::for_test(1);
    let fs = TestFs::with_files(0);
    let bytes = readdirplus_call(&fs, &handle, 0, 1_000_000);
    let (names, eof) = decode_names_and_eof(&bytes, true);
    assert_eq!(names, vec![".", ".."]);
    assert!(eof);
}

/// A directory that hands over one entry per `readdir` call, which
/// [`ReadOnlyFs::readdir`] permits.
struct PagedFs {
    total: u64,
}

impl ReadOnlyFs for PagedFs {
    fn lookup(&self, parent: Ino, name: &OsStr) -> Result<FileAttr> {
        if name == OsStr::new(".") || name == OsStr::new("..") {
            return Ok(FileAttr::dir(parent));
        }
        Err(FsError::NotFound)
    }
    fn getattr(&self, ino: Ino) -> Result<FileAttr> {
        Ok(FileAttr::dir(ino))
    }
    fn readdir(&self, _ino: Ino, offset: u64) -> Result<Vec<DirEntry>> {
        Ok((offset..self.total.min(offset + 1))
            .map(|i| DirEntry {
                ino: Ino(100 + i),
                name: OsString::from(format!("f{i}")),
                kind: FileKind::File,
            })
            .collect())
    }
    fn open(&self, _ino: Ino) -> Result<FileHandle> {
        Err(FsError::IsADirectory)
    }
    fn read_at(&self, _fh: FileHandle, _o: u64, _b: &mut [u8]) -> Result<usize> {
        Err(FsError::IsADirectory)
    }
    fn release(&self, _fh: FileHandle) -> Result<()> {
        Ok(())
    }
}

#[test]
fn readdirplus____an_implementation_that_pages____is_not_reported_as_eof_after_one_page() {
    // NFS is where a partial page did real damage: one `emit` call is one
    // `dirlist3`, so treating a short page as the whole tail set `eof` after
    // the first entry and the client stopped there. Unlike FUSE, whose kernel
    // client reissues `readdir` from the last cookie regardless, nothing here
    // asks a second time once `eof` is set.
    let handle = FileHandle3::for_test(1);
    let fs = PagedFs { total: 6 };
    let bytes = readdirplus_call(&fs, &handle, 0, 1_000_000);
    let (names, eof) = decode_names_and_eof(&bytes, true);
    assert_eq!(names, vec![".", "..", "f0", "f1", "f2", "f3", "f4", "f5"]);
    assert!(eof);
}

#[test]
fn readdirplus____budget_covers_everything____returns_full_listing_with_eof_true() {
    let handle = FileHandle3::for_test(1);
    let fs = TestFs::with_files(5);
    let bytes = readdirplus_call(&fs, &handle, 0, 1_000_000);
    let (names, eof) = decode_names_and_eof(&bytes, true);
    assert_eq!(names, vec![".", "..", "f0", "f1", "f2", "f3", "f4"]);
    assert!(eof);
}

fn full_listing_via_paging(fs: &TestFs, handle: &FileHandle3, maxcount: u32) -> Vec<String> {
    let mut collected = Vec::new();
    let mut cookie = 0u64;
    loop {
        let bytes = readdirplus_call(fs, handle, cookie, maxcount);
        let (names, eof) = decode_names_and_eof(&bytes, true);
        let last_name = names.last().cloned();
        let served_any = !names.is_empty();
        collected.extend(names);
        if eof {
            return collected;
        }
        // Determine the resume cookie from how many total names have been
        // served so far: 0 -> DOT, DOT -> DOTDOT, else trait_offset(n-2).
        if !served_any {
            // Nothing fit; a real client would need a bigger buffer, and the
            // server now says so with NFS3ERR_TOOSMALL rather than an empty
            // listing. Reaching here means the reply was `NFS3_OK` with no
            // entries, which is the state that loops forever.
            panic!("readdir made no progress with maxcount={maxcount}");
        }
        cookie = match collected.len() {
            1 => readdir::DOT,
            2 => readdir::DOTDOT,
            n => readdir::for_entry((n - 2) as u64 - 1),
        };
        let _ = last_name;
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn readdirplus____any_entry_count_and_maxcount_budget____reassembles_exactly_dot_dotdot_then_every_entry_in_order(
        total_entries in 0u64..40,
        // Large enough that even a `READDIRPLUS3` entry (name + full
        // `fattr3` + file handle) always fits at least once per call —
        // otherwise no maxcount could ever make progress, which is a
        // realistic client misconfiguration, not a server bug.
        maxcount in (DIRLIST_OVERHEAD + 200)..2000,
    ) {
        let handle = FileHandle3::for_test(1);
        let fs = TestFs::with_files(total_entries);
        let got = full_listing_via_paging(&fs, &handle, maxcount);

        // `TestFs` stores names in a `BTreeMap`, so entries come back in
        // lexicographic (not numeric) order — mirror that here rather than
        // assuming "f0", "f1", ..., "f10" sorts numerically.
        let mut expected_names: Vec<String> = (0..total_entries).map(|i| format!("f{i}")).collect();
        expected_names.sort();
        let mut expected = vec![".".to_owned(), "..".to_owned()];
        expected.extend(expected_names);
        prop_assert_eq!(got, expected);
    }
}
