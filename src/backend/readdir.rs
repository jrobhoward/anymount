//! Directory-listing machinery shared by every backend: the `.`/`..` cookie
//! arithmetic, and the driver that walks one paginated, resumable listing.
//!
//! `.` occupies cookie 1 and `..` cookie 2; the trait's own entry at (0-based)
//! offset `o` — from `fs.readdir(ino, o)` — occupies cookie `o + 3`. A resume
//! request's cookie is the cookie of the last entry the caller accepted, so
//! [`trait_offset`] of a cookie produced by [`for_entry`] lands one entry
//! *past* it — resuming after, not re-serving, that entry.
//!
//! [`emit`] is the part backends share beyond the arithmetic. FUSE and NFS both
//! synthesize `.` and `..`, then page through `fs.readdir` until either the
//! listing runs out or the reply buffer fills; only the "does this entry fit?"
//! test differs, and that is the [`Sink`] closure. cfapi has no `.`/`..` in its
//! enumeration callback and no size budget, so it passes [`Dots::Omit`] and a
//! sink that always accepts, reusing the pagination alone.
//!
//! Compiled and tested unconditionally rather than gated to a backend. Nothing
//! here calls into a platform API, and the tests are the same on every target;
//! the `allow` below is for the Windows build alone, where the cfapi backend is
//! still the Phase 2 stub and so calls none of it yet.

#![allow(dead_code)]

use std::ffi::OsStr;

use crate::error::Result;
use crate::fs::ReadOnlyFs;
use crate::types::{FileKind, Ino};

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

/// Whether [`emit`] synthesizes the `.` and `..` entries a POSIX-shaped
/// listing needs.
///
/// FUSE and NFS both want [`Synthesize`](Dots::Synthesize): the kernel client
/// and the NFS client respectively expect them in the reply, and
/// [`ReadOnlyFs::readdir`] is documented never to return them. cfapi's
/// placeholder enumeration has no equivalent, hence [`Omit`](Dots::Omit).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Dots {
    Synthesize,
    /// Constructed only by the tests until `backend/cfapi.rs`'s enumeration
    /// callback lands (`docs/PLAN.md`, Phase 2), which is what it exists for.
    Omit,
}

/// What a backend's sink did with an offered entry.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Sink {
    /// The entry was written to the reply; keep going.
    Accepted,
    /// The reply buffer is full. The entry was *not* written, and [`emit`]
    /// stops without consuming it.
    Full,
}

/// One entry offered to a [`Sink`], carrying the cookie a client resumes after
/// once it has been accepted.
#[derive(Debug)]
pub(crate) struct Entry<'a> {
    pub(crate) ino: Ino,
    pub(crate) name: &'a OsStr,
    pub(crate) kind: FileKind,
    pub(crate) cookie: u64,
}

/// Offer `.`, `..` and `dir`'s own entries to `sink`, resuming after the
/// cookie `resume_after` (`0` on a fresh listing).
///
/// Returns `true` when the listing was exhausted, and `false` when the sink
/// reported [`Sink::Full`] first — the distinction NFS needs for `dirlist3`'s
/// `eof` flag, and the signal to a caller that another call is coming.
///
/// `..`'s inode is resolved best-effort with `lookup(dir, "..")`, falling back
/// to `dir` itself when the implementation does not answer that name.
/// [`ReadOnlyFs`]'s docs ask implementors to answer it, but a listing is more
/// useful with a cosmetically wrong `..` fileid than failed outright — and the
/// kernel resolves `..` from its own dentry cache under FUSE regardless.
pub(crate) fn emit<F, S>(
    fs: &F,
    dir: Ino,
    resume_after: u64,
    dots: Dots,
    mut sink: S,
) -> Result<bool>
where
    F: ReadOnlyFs + ?Sized,
    S: for<'e> FnMut(Entry<'e>) -> Sink,
{
    let mut cookie = resume_after;

    if dots == Dots::Synthesize {
        if cookie == 0 {
            let entry = Entry {
                ino: dir,
                name: OsStr::new("."),
                kind: FileKind::Directory,
                cookie: DOT,
            };
            if sink(entry) == Sink::Full {
                return Ok(false);
            }
            cookie = DOT;
        }

        if cookie == DOT {
            let parent = fs.lookup(dir, OsStr::new("..")).map_or(dir, |a| a.ino);
            let entry = Entry {
                ino: parent,
                name: OsStr::new(".."),
                kind: FileKind::Directory,
                cookie: DOTDOT,
            };
            if sink(entry) == Sink::Full {
                return Ok(false);
            }
            cookie = DOTDOT;
        }
    }

    let offset = trait_offset(cookie);
    for (i, entry) in fs.readdir(dir, offset)?.iter().enumerate() {
        let offered = Entry {
            ino: entry.ino,
            name: &entry.name,
            kind: entry.kind,
            cookie: for_entry(offset + i as u64),
        };
        if sink(offered) == Sink::Full {
            return Ok(false);
        }
    }
    Ok(true)
}

#[cfg(test)]
#[path = "readdir_tests.rs"]
mod readdir_tests;
