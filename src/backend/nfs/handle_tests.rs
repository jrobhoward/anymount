#![allow(non_snake_case)]
#![allow(clippy::unwrap_used)]
#![allow(clippy::expect_used)]

use proptest::prelude::*;

use super::*;
use crate::types::Ino;

#[test]
fn resolve____encoded_by_same_handle____round_trips_to_original_ino() {
    let h = FileHandle3::new_random();
    let encoded = h.encode(Ino(42));
    assert_eq!(h.resolve(&encoded), Some(Ino(42)));
}

#[test]
fn resolve____wrong_secret____is_none() {
    let a = FileHandle3::new_random();
    let b = FileHandle3::new_random();
    let encoded = a.encode(Ino(7));
    assert_eq!(b.resolve(&encoded), None);
}

#[test]
fn resolve____wrong_length____is_none() {
    let h = FileHandle3::new_random();
    let mut encoded = h.encode(Ino(1)).to_vec();
    encoded.pop();
    assert_eq!(h.resolve(&encoded), None);

    let mut too_long = h.encode(Ino(1)).to_vec();
    too_long.push(0);
    assert_eq!(h.resolve(&too_long), None);
}

#[test]
fn resolve____all_zero_guess____is_none() {
    let h = FileHandle3::new_random();
    let guess = [0u8; ENCODED_LEN];
    assert_eq!(h.resolve(&guess), None);
}

proptest! {
    #[test]
    fn resolve____any_ino____round_trips(ino in any::<u64>()) {
        let h = FileHandle3::new_random();
        let encoded = h.encode(Ino(ino));
        prop_assert_eq!(h.resolve(&encoded), Some(Ino(ino)));
    }
}
