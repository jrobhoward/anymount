#![allow(non_snake_case)]
#![allow(clippy::unwrap_used)]
#![allow(clippy::expect_used)]

//! What is testable here without a live kernel session: the pure translation
//! functions between this crate's types and `fuser`'s, and FUSE's `getxattr`
//! size-query convention.
//!
//! The `readdir` cookie and pagination properties used to live here as a
//! hand-written simulation of `FuseAdapter::readdir`. They now live in
//! `backend/readdir_tests.rs` and drive `readdir::emit` — the code this
//! adapter actually calls — instead of a copy of it.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::*;

#[test]
fn to_fuser_kind____each_variant____maps_to_the_matching_fuser_type() {
    assert_eq!(to_fuser_kind(FileKind::File), fuser::FileType::RegularFile);
    assert_eq!(
        to_fuser_kind(FileKind::Directory),
        fuser::FileType::Directory
    );
}

#[test]
fn to_fuser_attr____a_regular_file____carries_every_field_across() {
    let mtime = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let attr = FileAttr {
        ino: Ino(42),
        kind: FileKind::File,
        size: 1234,
        perm: 0o644,
        nlink: 3,
        uid: 1000,
        gid: 1001,
        atime: UNIX_EPOCH + Duration::from_secs(10),
        mtime,
        ctime: UNIX_EPOCH + Duration::from_secs(20),
    };

    let out = to_fuser_attr(&attr);

    assert_eq!(out.ino, INodeNo(42));
    assert_eq!(out.size, 1234);
    assert_eq!(out.perm, 0o644);
    assert_eq!(out.nlink, 3);
    assert_eq!(out.uid, 1000);
    assert_eq!(out.gid, 1001);
    assert_eq!(out.mtime, mtime);
    assert_eq!(out.kind, fuser::FileType::RegularFile);
    assert_eq!(out.rdev, 0);
    assert_eq!(out.flags, 0);
    assert_eq!(out.crtime, SystemTime::UNIX_EPOCH);
}

#[test]
fn to_fuser_attr____size_not_a_block_multiple____rounds_blocks_up() {
    // `stat` reports allocated blocks, so a partial block still counts.
    let one_byte = to_fuser_attr(&FileAttr::file(Ino(2), 1));
    assert_eq!(one_byte.blocks, 1);

    let exact = to_fuser_attr(&FileAttr::file(Ino(2), 512));
    assert_eq!(exact.blocks, 1);

    let over = to_fuser_attr(&FileAttr::file(Ino(2), 513));
    assert_eq!(over.blocks, 2);

    let empty = to_fuser_attr(&FileAttr::file(Ino(2), 0));
    assert_eq!(empty.blocks, 0);
}

#[test]
fn errno____a_context_wrapped_error____keeps_the_inner_code() {
    let plain = errno(&FsError::NotFound);
    let wrapped = errno(&FsError::NotFound.context("while walking the archive"));
    assert_eq!(plain, wrapped);
}

#[test]
fn errno____read_only____is_erofs() {
    assert_eq!(errno(&FsError::ReadOnly), Errno::from_i32(libc::EROFS));
}

#[test]
fn xattr_reply____size_zero____asks_for_the_length_only() {
    assert_eq!(xattr_reply(17, 0), XattrReply::Size(17));
    assert_eq!(xattr_reply(0, 0), XattrReply::Size(0));
}

#[test]
fn xattr_reply____buffer_large_enough____returns_the_value() {
    assert_eq!(xattr_reply(17, 17), XattrReply::Data);
    assert_eq!(xattr_reply(17, 64), XattrReply::Data);
}

#[test]
fn xattr_reply____buffer_one_byte_short____is_rejected_not_truncated() {
    assert_eq!(xattr_reply(17, 16), XattrReply::TooLarge);
}

#[test]
fn read_buffer_len____an_ordinary_kernel_sized_request____is_not_capped() {
    // The kernel negotiates its own ceiling, typically 128 KiB; MAX_READ only
    // bounds how much a `size` field can make the adapter allocate.
    assert_eq!(read_buffer_len(4096), 4096);
    assert_eq!(read_buffer_len(128 * 1024), 128 * 1024);
    assert_eq!(read_buffer_len(0), 0);
}

#[test]
fn read_buffer_len____an_absurd_request____is_capped_rather_than_allocated() {
    assert_eq!(read_buffer_len(u32::MAX), MAX_READ as usize);
    assert_eq!(read_buffer_len(MAX_READ + 1), MAX_READ as usize);
    assert_eq!(read_buffer_len(MAX_READ), MAX_READ as usize);
}
