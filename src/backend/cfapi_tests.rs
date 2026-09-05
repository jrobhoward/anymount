#![allow(non_snake_case)]
#![allow(clippy::unwrap_used)]
#![allow(clippy::expect_used)]

//! What is testable here without a live sync root session: the pure
//! translation functions between this crate's types and cfapi's — wide-string
//! encoding, the `FileIdentity` round trip, and `FILETIME` conversion.

use std::time::{Duration, UNIX_EPOCH};

use super::*;

#[test]
fn to_wide____an_ascii_string____is_null_terminated_utf16() {
    let wide = to_wide("abc");
    assert_eq!(wide, vec![b'a' as u16, b'b' as u16, b'c' as u16, 0]);
}

#[test]
fn to_wide____empty_string____is_just_the_terminator() {
    assert_eq!(to_wide(""), vec![0u16]);
}

#[test]
fn decode_ino____an_ino_round_tripped_through_le_bytes____matches() {
    let ino = Ino(0x1122_3344_5566_7788);
    let bytes = ino.0.to_le_bytes();
    let decoded = decode_ino(bytes.as_ptr().cast(), bytes.len() as u32);
    assert_eq!(decoded, Some(ino));
}

#[test]
fn decode_ino____wrong_length____is_rejected() {
    let bytes = [0u8; 4];
    assert_eq!(decode_ino(bytes.as_ptr().cast(), bytes.len() as u32), None);
}

#[test]
fn decode_ino____null_pointer____is_rejected() {
    assert_eq!(decode_ino(std::ptr::null(), 8), None);
}

#[test]
fn to_filetime____unix_epoch____is_the_1601_offset() {
    assert_eq!(to_filetime(UNIX_EPOCH), 116_444_736_000_000_000);
}

#[test]
fn to_filetime____one_second_after_epoch____advances_by_ten_million_units() {
    assert_eq!(
        to_filetime(UNIX_EPOCH + Duration::from_secs(1)),
        116_444_736_000_000_000 + 10_000_000
    );
}

#[test]
fn to_filetime____before_the_unix_epoch____is_the_unspecified_sentinel() {
    // `SystemTime` can represent times before `UNIX_EPOCH` on platforms that
    // support it; this crate's own `FileAttr` constructors never produce one,
    // but the conversion still needs to not panic if an implementor does.
    let before = UNIX_EPOCH - Duration::from_secs(1);
    assert_eq!(to_filetime(before), 0);
}

#[test]
fn to_fs_metadata____a_directory____carries_the_directory_attribute() {
    let attr = FileAttr::dir(Ino(2));
    let meta = to_fs_metadata(&attr);
    assert_eq!(meta.BasicInfo.FileAttributes, FILE_ATTRIBUTE_DIRECTORY.0);
    assert_eq!(meta.FileSize, 0);
}

#[test]
fn to_fs_metadata____a_file____carries_size_and_the_readonly_attribute() {
    let attr = FileAttr::file(Ino(2), 4096);
    let meta = to_fs_metadata(&attr);
    assert_eq!(meta.BasicInfo.FileAttributes, FILE_ATTRIBUTE_READONLY.0);
    assert_eq!(meta.FileSize, 4096);
}

#[test]
fn to_create_info____carries_the_name_pointer_and_identity_through() {
    let name = to_wide("child.txt");
    let id = 7u64.to_le_bytes();
    let attr = FileAttr::file(Ino(7), 10);

    let info = to_create_info(&name, &id, &attr);

    assert_eq!(info.FileIdentityLength, 8);
    // SAFETY: `name` outlives this assertion.
    let read_back = unsafe { info.RelativeFileName.to_string() }.expect("valid utf-16");
    assert_eq!(read_back, "child.txt");
}
