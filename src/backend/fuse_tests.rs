#![allow(non_snake_case)]
#![allow(clippy::unwrap_used)]
#![allow(clippy::expect_used)]

//! The `readdir` cookie arithmetic is the one part of the FUSE adapter with
//! real invariants worth checking beyond a handful of examples: cookies must
//! round-trip back to the trait offset they came from, and a listing spread
//! across many buffer-limited kernel calls must reassemble into exactly one
//! `.`, one `..`, then every trait entry in order with nothing skipped or
//! repeated. `super::cookie` is tested directly, with no `fuser` session
//! involved.

use proptest::prelude::*;

use super::cookie;

/// One simulated FUSE `readdir` call: mirrors `FuseAdapter::readdir`'s
/// three-stage structure (`.`, `..`, then trait entries) against a
/// buffer that accepts exactly `capacity` entries before reporting full,
/// the same contract `ReplyDirectory::add` has (return `true` = not added,
/// buffer full).
struct Call {
    added: Vec<String>,
    /// `Some(cookie)` to resume from if the buffer filled before the listing
    /// was exhausted; `None` once every entry has been served.
    resume_at: Option<u64>,
}

fn one_call(offset: u64, total_entries: u64, capacity: usize) -> Call {
    let mut added = Vec::new();
    let mut remaining = capacity;
    let mut next = offset;

    if next == 0 {
        if remaining == 0 {
            return Call {
                added,
                resume_at: Some(0),
            };
        }
        added.push(".".to_owned());
        remaining -= 1;
        next = cookie::DOT;
    }
    if next == cookie::DOT {
        if remaining == 0 {
            return Call {
                added,
                resume_at: Some(cookie::DOT),
            };
        }
        added.push("..".to_owned());
        remaining -= 1;
        next = cookie::DOTDOT;
    }

    let mut i = cookie::trait_offset(next);
    let mut last_added_cookie = None;
    while i < total_entries && remaining > 0 {
        added.push(format!("e{i}"));
        last_added_cookie = Some(cookie::for_entry(i));
        remaining -= 1;
        i += 1;
    }

    let resume_at = if i >= total_entries {
        None
    } else {
        Some(last_added_cookie.unwrap_or(next))
    };
    Call { added, resume_at }
}

/// Drives [`one_call`] to completion the way the kernel would: resubmitting
/// whatever cookie the previous call reported until the listing is
/// exhausted, and concatenating everything served along the way.
fn full_listing(total_entries: u64, capacity: usize) -> Vec<String> {
    let mut collected = Vec::new();
    let mut offset = 0u64;
    loop {
        let call = one_call(offset, total_entries, capacity);
        collected.extend(call.added);
        match call.resume_at {
            Some(next) => offset = next,
            None => return collected,
        }
    }
}

fn expected_listing(total_entries: u64) -> Vec<String> {
    std::iter::once(".".to_owned())
        .chain(std::iter::once("..".to_owned()))
        .chain((0..total_entries).map(|i| format!("e{i}")))
        .collect()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// However small the per-call buffer is, a full multi-call listing
    /// reassembles into exactly one `.`, one `..`, and every trait entry in
    /// order — nothing skipped, nothing repeated, regardless of where the
    /// buffer happens to fill.
    #[test]
    fn full_listing____any_buffer_capacity____matches_a_single_unbounded_call(
        total_entries in 0u64..500,
        capacity in 1usize..20,
    ) {
        prop_assert_eq!(full_listing(total_entries, capacity), expected_listing(total_entries));
    }

    /// A buffer that never fills behaves like one unbounded call.
    #[test]
    fn full_listing____capacity_covers_everything____is_a_single_call(total_entries in 0u64..500) {
        let capacity = total_entries as usize + 2;
        prop_assert_eq!(full_listing(total_entries, capacity), expected_listing(total_entries));
    }
}
