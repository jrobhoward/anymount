//! `readdir` cookie arithmetic shared by every backend that must synthesize
//! `.` and `..` entries and support paginated, resumable listings.
//!
//! `.` occupies cookie 1 and `..` cookie 2; the trait's own entry at (0-based)
//! offset `o` — from `fs.readdir(ino, o)` — occupies cookie `o + 3`. A resume
//! request's cookie is the cookie of the last entry the caller accepted, so
//! [`trait_offset`] of a cookie produced by [`for_entry`] lands one entry
//! *past* it — resuming after, not re-serving, that entry.
//!
//! Unused on Windows, where the cfapi backend has no equivalent concept.

#![allow(dead_code)]

pub(crate) const DOT: u64 = 1;
pub(crate) const DOTDOT: u64 = 2;

/// Trait-level offset to resume `fs.readdir` from, given the cookie the
/// caller is resuming after (`0` on a fresh `readdir`).
pub(crate) fn trait_offset(resume_after: u64) -> u64 {
    resume_after.saturating_sub(DOTDOT)
}

/// Cookie for the trait's entry at `trait_offset`.
pub(crate) fn for_entry(trait_offset: u64) -> u64 {
    trait_offset + DOTDOT + 1
}

#[cfg(test)]
#[path = "readdir_cookie_tests.rs"]
mod readdir_cookie_tests;
