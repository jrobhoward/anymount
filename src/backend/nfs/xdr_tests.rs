#![allow(non_snake_case)]
#![allow(clippy::unwrap_used)]
#![allow(clippy::expect_used)]

use proptest::prelude::*;

use super::*;

#[test]
fn reader____u32_round_trips____through_writer() {
    let mut w = Writer::new();
    w.write_u32(0xdead_beef);
    let bytes = w.into_bytes();
    let mut r = Reader::new(&bytes);
    assert_eq!(r.read_u32(), Some(0xdead_beef));
}

#[test]
fn reader____u64_round_trips____through_writer() {
    let mut w = Writer::new();
    w.write_u64(0x0123_4567_89ab_cdef);
    let bytes = w.into_bytes();
    let mut r = Reader::new(&bytes);
    assert_eq!(r.read_u64(), Some(0x0123_4567_89ab_cdef));
}

#[test]
fn reader____bool_round_trips____through_writer() {
    let mut w = Writer::new();
    w.write_bool(true);
    w.write_bool(false);
    let bytes = w.into_bytes();
    let mut r = Reader::new(&bytes);
    assert_eq!(r.read_bool(), Some(true));
    assert_eq!(r.read_bool(), Some(false));
}

#[test]
fn reader____opaque_var_round_trips____through_writer() {
    let mut w = Writer::new();
    w.write_opaque_var(b"hello");
    let bytes = w.into_bytes();
    // length prefix (4) + 5 bytes + 3 pad bytes = 12
    assert_eq!(bytes.len(), 12);
    let mut r = Reader::new(&bytes);
    assert_eq!(r.read_opaque_var(100), Some(b"hello".to_vec()));
}

#[test]
fn reader____string_round_trips____through_writer() {
    let mut w = Writer::new();
    w.write_string(std::ffi::OsStr::new("nested.txt"));
    let bytes = w.into_bytes();
    let mut r = Reader::new(&bytes);
    assert_eq!(
        r.read_string(1024),
        Some(std::ffi::OsString::from("nested.txt"))
    );
}

#[test]
fn reader____opaque_var_over_max____is_none() {
    let mut w = Writer::new();
    w.write_opaque_var(b"hello world");
    let bytes = w.into_bytes();
    let mut r = Reader::new(&bytes);
    assert_eq!(r.read_opaque_var(4), None);
}

#[test]
fn reader____truncated_u32____returns_none_not_panic() {
    let bytes = [0u8; 2];
    let mut r = Reader::new(&bytes);
    assert_eq!(r.read_u32(), None);
}

#[test]
fn reader____truncated_u64____returns_none_not_panic() {
    let bytes = [0u8; 4];
    let mut r = Reader::new(&bytes);
    assert_eq!(r.read_u64(), None);
}

#[test]
fn reader____truncated_opaque_fixed____returns_none_not_panic() {
    let bytes = [0u8; 4];
    let mut r = Reader::new(&bytes);
    assert_eq!(r.read_opaque_fixed::<8>(), None);
}

#[test]
fn reader____truncated_opaque_var_length____returns_none_not_panic() {
    let bytes = [0u8; 2];
    let mut r = Reader::new(&bytes);
    assert_eq!(r.read_opaque_var(100), None);
}

#[test]
fn reader____opaque_var_length_exceeds_remaining_bytes____returns_none_not_panic() {
    let mut w = Writer::new();
    w.write_u32(1000);
    let bytes = w.into_bytes();
    let mut r = Reader::new(&bytes);
    assert_eq!(r.read_opaque_var(u32::MAX), None);
}

#[test]
fn reader____truncated_string____returns_none_not_panic() {
    let bytes = [0u8; 1];
    let mut r = Reader::new(&bytes);
    assert_eq!(r.read_string(100), None);
}

#[test]
fn reader____skip_opaque_auth_of_auth_sys____advances_past_flavor_and_body() {
    let mut w = Writer::new();
    w.write_u32(1); // AUTH_SYS
    w.write_opaque_var(b"fake-cred-body");
    w.write_u32(0xabcd_ef01); // sentinel that should remain after skip
    let bytes = w.into_bytes();
    let mut r = Reader::new(&bytes);
    assert_eq!(r.skip_opaque_auth(), Some(()));
    assert_eq!(r.read_u32(), Some(0xabcd_ef01));
}

fn any_bytes() -> impl Strategy<Value = Vec<u8>> {
    proptest::collection::vec(any::<u8>(), 0..64)
}

proptest! {
    #[test]
    fn opaque_var____arbitrary_bytes____roundtrips_through_write_then_read(data in any_bytes()) {
        let mut w = Writer::new();
        w.write_opaque_var(&data);
        let bytes = w.into_bytes();
        let mut r = Reader::new(&bytes);
        prop_assert_eq!(r.read_opaque_var(u32::MAX), Some(data));
    }

    #[test]
    fn reader____arbitrary_truncated_prefix_of_any_valid_encoding____never_panics(data in any_bytes()) {
        let mut w = Writer::new();
        w.write_opaque_var(&data);
        let full = w.into_bytes();
        for len in 0..=full.len() {
            let mut r = Reader::new(&full[..len]);
            let _ = r.read_opaque_var(u32::MAX);
        }
    }
}
