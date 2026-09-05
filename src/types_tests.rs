//! Tests for inode, handle, attribute and directory-entry types.

#![allow(clippy::unwrap_used)]
#![allow(clippy::expect_used)]
#![allow(non_snake_case)]

use super::*;

#[test]
fn file____default_construction____is_read_only_regular_file() {
    let attr = FileAttr::file(Ino(7), 1234);
    assert_eq!(attr.ino, Ino(7));
    assert_eq!(attr.size, 1234);
    assert_eq!(attr.kind, FileKind::File);
    assert_eq!(attr.perm, 0o444);
    assert_eq!(attr.nlink, 1);
}

#[test]
fn dir____default_construction____is_traversable_and_zero_sized() {
    let attr = FileAttr::dir(Ino(1));
    assert_eq!(attr.kind, FileKind::Directory);
    assert_eq!(attr.size, 0);
    assert_eq!(attr.perm, 0o555);
}

#[test]
fn root_ino____by_convention____is_one() {
    // Both FUSE and the Windows backends assume the root is inode 1.
    assert_eq!(ROOT_INO, Ino(1));
}

#[test]
fn statfs____default____reports_a_sane_block_size_and_name_length() {
    let s = StatFs::default();
    assert_eq!(s.bsize, 512);
    assert_eq!(s.namelen, 255);
}

#[test]
fn display____an_ino_and_a_file_handle____render_as_bare_numbers() {
    // Log lines read `ino 42`, not `ino Ino(42)`.
    assert_eq!(Ino(42).to_string(), "42");
    assert_eq!(FileHandle(7).to_string(), "7");
    assert_eq!(format!("ino {}", ROOT_INO), "ino 1");
}
