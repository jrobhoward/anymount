#![allow(non_snake_case)]
#![allow(clippy::unwrap_used)]
#![allow(clippy::expect_used)]

use proptest::prelude::*;

use super::*;
use crate::types::Ino;

#[test]
fn resolve____encoded_by_same_handle____round_trips_to_original_ino() {
    let h = FileHandle3::for_test(1);
    let encoded = h.encode(Ino(42));
    assert_eq!(h.resolve(&encoded), Some(Ino(42)));
}

#[test]
fn resolve____wrong_secret____is_none() {
    let a = FileHandle3::for_test(1);
    let b = FileHandle3::for_test(2);
    let encoded = a.encode(Ino(7));
    assert_eq!(b.resolve(&encoded), None);
}

#[test]
fn resolve____wrong_length____is_none() {
    let h = FileHandle3::for_test(1);
    let mut encoded = h.encode(Ino(1)).to_vec();
    encoded.pop();
    assert_eq!(h.resolve(&encoded), None);

    let mut too_long = h.encode(Ino(1)).to_vec();
    too_long.push(0);
    assert_eq!(h.resolve(&too_long), None);
}

#[test]
fn from_secret____two_different_secrets____do_not_accept_each_others_handles() {
    // The property the export path depends on: a handle is only resolvable by
    // the mount that issued it.
    let a = FileHandle3::from_secret([0xAA; 16]);
    let b = FileHandle3::from_secret([0xBB; 16]);
    assert_eq!(a.resolve(&a.encode(Ino(9))), Some(Ino(9)));
    assert_eq!(b.resolve(&a.encode(Ino(9))), None);
}

#[test]
fn resolve____all_zero_guess____is_none() {
    let h = FileHandle3::for_test(1);
    let guess = [0u8; ENCODED_LEN];
    assert_eq!(h.resolve(&guess), None);
}

proptest! {
    #[test]
    fn resolve____any_ino____round_trips(ino in any::<u64>()) {
        let h = FileHandle3::for_test(1);
        let encoded = h.encode(Ino(ino));
        prop_assert_eq!(h.resolve(&encoded), Some(Ino(ino)));
    }
}
