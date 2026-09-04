//! The trait must stay usable the two ways downstream code will want it:
//! monomorphised, and behind a `dyn` pointer.

#![allow(clippy::unwrap_used)]
#![allow(non_snake_case)]

use std::ffi::{OsStr, OsString};

use anymount::{
    DirEntry, FileAttr, FileHandle, FileKind, FsError, Ino, ROOT_INO, ReadOnlyFs, Result,
};

struct OneFile;

const CONTENT: &[u8] = b"anymount";

impl ReadOnlyFs for OneFile {
    fn lookup(&self, parent: Ino, name: &OsStr) -> Result<FileAttr> {
        if parent == ROOT_INO && name == OsStr::new("f") {
            Ok(FileAttr::file(Ino(2), CONTENT.len() as u64))
        } else {
            Err(FsError::NotFound)
        }
    }

    fn getattr(&self, ino: Ino) -> Result<FileAttr> {
        match ino {
            ROOT_INO => Ok(FileAttr::dir(ROOT_INO)),
            Ino(2) => Ok(FileAttr::file(Ino(2), CONTENT.len() as u64)),
            _ => Err(FsError::NotFound),
        }
    }

    fn readdir(&self, ino: Ino, offset: u64) -> Result<Vec<DirEntry>> {
        if ino != ROOT_INO {
            return Err(FsError::NotADirectory);
        }
        Ok(std::iter::once(DirEntry {
            ino: Ino(2),
            name: OsString::from("f"),
            kind: FileKind::File,
        })
        .skip(offset as usize)
        .collect())
    }

    fn open(&self, _ino: Ino) -> Result<FileHandle> {
        Ok(FileHandle(1))
    }

    fn read_at(&self, _fh: FileHandle, offset: u64, buf: &mut [u8]) -> Result<usize> {
        let start = (offset as usize).min(CONTENT.len());
        let n = buf.len().min(CONTENT.len() - start);
        buf[..n].copy_from_slice(&CONTENT[start..start + n]);
        Ok(n)
    }

    fn release(&self, _fh: FileHandle) -> Result<()> {
        Ok(())
    }
}

#[test]
fn read_only_fs____used_as_a_trait_object____is_object_safe() {
    // ciphercask will likely hold this as `Box<dyn ReadOnlyFs>` so the cask
    // backend can be chosen at runtime; keep that possible.
    let fs: Box<dyn ReadOnlyFs> = Box::new(OneFile);
    assert_eq!(fs.getattr(ROOT_INO).unwrap().kind, FileKind::Directory);
}

#[test]
fn read_at____offset_past_start____returns_the_tail() {
    let fs = OneFile;
    let mut buf = [0u8; 16];
    let n = fs.read_at(FileHandle(1), 3, &mut buf).unwrap();
    assert_eq!(&buf[..n], b"mount");
}

#[test]
fn read_at____offset_at_eof____returns_zero_bytes() {
    let fs = OneFile;
    let mut buf = [0u8; 16];
    assert_eq!(fs.read_at(FileHandle(1), 8, &mut buf).unwrap(), 0);
}

#[test]
fn readdir____offset_past_the_only_entry____returns_empty() {
    let fs = OneFile;
    assert!(fs.readdir(ROOT_INO, 1).unwrap().is_empty());
}

#[test]
fn lookup____unknown_name____is_not_found() {
    let fs = OneFile;
    assert!(matches!(
        fs.lookup(ROOT_INO, OsStr::new("nope")),
        Err(FsError::NotFound)
    ));
}

#[test]
fn getxattr____default_implementation____reports_no_attribute() {
    let fs = OneFile;
    assert!(matches!(
        fs.getxattr(ROOT_INO, OsStr::new("user.x")),
        Err(FsError::NoXattr)
    ));
}

#[test]
fn forget____default_implementation____is_a_harmless_no_op() {
    // No assertion beyond "does not panic": implementations with no
    // inode-keyed cache are meant to ignore this entirely.
    let fs = OneFile;
    fs.forget(Ino(2), 1);
}
