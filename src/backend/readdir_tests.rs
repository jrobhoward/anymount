#![allow(non_snake_case)]
#![allow(clippy::unwrap_used)]
#![allow(clippy::expect_used)]

//! Two layers are tested here: the cookie arithmetic on its own, and [`emit`]
//! driven the way a backend drives it.
//!
//! The `emit` properties matter most. A listing spread across many
//! buffer-limited calls must reassemble into exactly one `.`, one `..`, then
//! every trait entry in order, with nothing skipped or repeated — and unlike
//! the simulation this replaces, the code under test is the one the backends
//! actually call.

use std::ffi::{OsStr, OsString};

use proptest::prelude::*;

use super::*;
use crate::error::FsError;
use crate::types::{DirEntry, FileAttr};

#[test]
fn trait_offset____resume_after_zero_or_dot____is_the_first_entry() {
    assert_eq!(trait_offset(0), 0);
    assert_eq!(trait_offset(DOT), 0);
    assert_eq!(trait_offset(DOTDOT), 0);
}

#[test]
fn for_entry____first_trait_offset____is_never_a_dot_or_dotdot_cookie() {
    assert!(for_entry(0) > DOTDOT);
}

proptest! {
    /// A resume request's cookie is the *last-served* entry's own cookie —
    /// [`trait_offset`] must therefore land one past it (the entry itself
    /// must not be re-served), not back at the same offset.
    #[test]
    fn for_entry____any_offset____trait_offset_of_its_cookie_resumes_one_past_it(offset in 0u64..1_000_000) {
        prop_assert_eq!(trait_offset(for_entry(offset)), offset + 1);
    }

    #[test]
    fn for_entry____distinct_offsets____never_collide(a in 0u64..10_000, b in 0u64..10_000) {
        prop_assume!(a != b);
        prop_assert_ne!(for_entry(a), for_entry(b));
    }

    #[test]
    fn for_entry____any_offset____cookie_exceeds_dot_and_dotdot(offset in 0u64..1_000_000) {
        prop_assert!(for_entry(offset) > DOTDOT);
    }
}

const DIR: Ino = Ino(1);
const PARENT: Ino = Ino(9);

/// A directory of `total` entries named `e0..e<total-1>`, whose parent is a
/// different inode from the directory itself so a `..` reported as `dir` is
/// distinguishable from one resolved properly.
struct FlatDir {
    total: u64,
    /// When false, `lookup(dir, "..")` fails, exercising [`emit`]'s
    /// best-effort fallback.
    answers_dotdot: bool,
}

impl ReadOnlyFs for FlatDir {
    fn lookup(&self, _parent: Ino, name: &OsStr) -> Result<FileAttr> {
        if name == OsStr::new("..") && self.answers_dotdot {
            return Ok(FileAttr::dir(PARENT));
        }
        Err(FsError::NotFound)
    }

    fn getattr(&self, ino: Ino) -> Result<FileAttr> {
        Ok(FileAttr::dir(ino))
    }

    fn readdir(&self, _ino: Ino, offset: u64) -> Result<Vec<DirEntry>> {
        Ok((offset..self.total)
            .map(|i| DirEntry {
                ino: Ino(100 + i),
                name: OsString::from(format!("e{i}")),
                kind: FileKind::File,
            })
            .collect())
    }

    fn open(&self, _ino: Ino) -> Result<crate::types::FileHandle> {
        Err(FsError::IsADirectory)
    }

    fn read_at(&self, _fh: crate::types::FileHandle, _o: u64, _b: &mut [u8]) -> Result<usize> {
        Err(FsError::IsADirectory)
    }

    fn release(&self, _fh: crate::types::FileHandle) -> Result<()> {
        Ok(())
    }
}

/// A directory whose `readdir` always fails, for the error-propagation test.
struct BrokenDir;

impl ReadOnlyFs for BrokenDir {
    fn lookup(&self, _parent: Ino, _name: &OsStr) -> Result<FileAttr> {
        Err(FsError::NotFound)
    }
    fn getattr(&self, ino: Ino) -> Result<FileAttr> {
        Ok(FileAttr::dir(ino))
    }
    fn readdir(&self, _ino: Ino, _offset: u64) -> Result<Vec<DirEntry>> {
        Err(FsError::PermissionDenied)
    }
    fn open(&self, _ino: Ino) -> Result<crate::types::FileHandle> {
        Err(FsError::IsADirectory)
    }
    fn read_at(&self, _fh: crate::types::FileHandle, _o: u64, _b: &mut [u8]) -> Result<usize> {
        Err(FsError::IsADirectory)
    }
    fn release(&self, _fh: crate::types::FileHandle) -> Result<()> {
        Ok(())
    }
}

/// What one call accepted, plus the cookie to resume from if it filled up.
struct Call {
    names: Vec<String>,
    inos: Vec<Ino>,
    resume_at: Option<u64>,
}

/// Drives [`emit`] once with a sink that accepts exactly `capacity` entries
/// before reporting [`Sink::Full`] — the same contract FUSE's
/// `ReplyDirectory::add` and NFS's budget check both present.
fn one_call(fs: &FlatDir, resume_after: u64, capacity: usize, dots: Dots) -> Call {
    let mut names = Vec::new();
    let mut inos = Vec::new();
    let mut last_cookie = None;
    let mut remaining = capacity;

    let exhausted = emit(fs, DIR, resume_after, dots, |e| {
        if remaining == 0 {
            return Sink::Full;
        }
        remaining -= 1;
        names.push(e.name.to_string_lossy().into_owned());
        inos.push(e.ino);
        last_cookie = Some(e.cookie);
        Sink::Accepted
    })
    .expect("FlatDir never fails");

    Call {
        names,
        inos,
        resume_at: if exhausted {
            None
        } else {
            Some(last_cookie.unwrap_or(resume_after))
        },
    }
}

/// Resubmits whatever cookie the previous call reported until the listing is
/// exhausted, concatenating everything served along the way — what the kernel
/// or an NFS client does across successive `readdir` requests.
fn full_listing(fs: &FlatDir, capacity: usize, dots: Dots) -> (Vec<String>, Vec<Ino>) {
    let mut names = Vec::new();
    let mut inos = Vec::new();
    let mut resume_after = 0u64;
    loop {
        let call = one_call(fs, resume_after, capacity, dots);
        names.extend(call.names);
        inos.extend(call.inos);
        match call.resume_at {
            Some(next) => resume_after = next,
            None => return (names, inos),
        }
    }
}

fn expected_names(total: u64, dots: Dots) -> Vec<String> {
    let lead: Vec<String> = match dots {
        Dots::Synthesize => vec![".".to_owned(), "..".to_owned()],
        Dots::Omit => Vec::new(),
    };
    lead.into_iter()
        .chain((0..total).map(|i| format!("e{i}")))
        .collect()
}

#[test]
fn emit____dotdot_answered_by_lookup____reports_the_parent_inode() {
    let fs = FlatDir {
        total: 0,
        answers_dotdot: true,
    };
    let (names, inos) = full_listing(&fs, 8, Dots::Synthesize);
    assert_eq!(names, vec![".", ".."]);
    assert_eq!(inos, vec![DIR, PARENT]);
}

#[test]
fn emit____dotdot_not_answered_by_lookup____falls_back_to_the_directory_itself() {
    let fs = FlatDir {
        total: 0,
        answers_dotdot: false,
    };
    let (names, inos) = full_listing(&fs, 8, Dots::Synthesize);
    assert_eq!(names, vec![".", ".."]);
    assert_eq!(inos, vec![DIR, DIR]);
}

#[test]
fn emit____dots_omitted____serves_only_the_traits_own_entries() {
    let fs = FlatDir {
        total: 3,
        answers_dotdot: true,
    };
    let (names, _) = full_listing(&fs, 8, Dots::Omit);
    assert_eq!(names, vec!["e0", "e1", "e2"]);
}

#[test]
fn emit____sink_full_on_the_very_first_entry____reports_not_exhausted() {
    let fs = FlatDir {
        total: 5,
        answers_dotdot: true,
    };
    let call = one_call(&fs, 0, 0, Dots::Synthesize);
    assert!(call.names.is_empty());
    assert_eq!(call.resume_at, Some(0));
}

#[test]
fn emit____readdir_fails____propagates_the_error() {
    let err = emit(&BrokenDir, DIR, 0, Dots::Omit, |_| Sink::Accepted)
        .expect_err("BrokenDir::readdir always fails");
    assert!(matches!(err, FsError::PermissionDenied));
}

#[test]
fn emit____lookup_fails____still_serves_the_listing() {
    // `..` failing must not fail the whole call; only `readdir` failing does.
    let fs = FlatDir {
        total: 2,
        answers_dotdot: false,
    };
    let (names, _) = full_listing(&fs, 8, Dots::Synthesize);
    assert_eq!(names, expected_names(2, Dots::Synthesize));
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// However small the per-call buffer is, a full multi-call listing
    /// reassembles into exactly one `.`, one `..`, and every trait entry in
    /// order — nothing skipped, nothing repeated, regardless of where the
    /// buffer happens to fill.
    #[test]
    fn emit____any_buffer_capacity____matches_a_single_unbounded_call(
        total in 0u64..500,
        capacity in 1usize..20,
    ) {
        let fs = FlatDir { total, answers_dotdot: true };
        let (names, _) = full_listing(&fs, capacity, Dots::Synthesize);
        prop_assert_eq!(names, expected_names(total, Dots::Synthesize));
    }

    /// The same invariant without the synthesized entries, which is the shape
    /// the cfapi backend will use.
    #[test]
    fn emit____dots_omitted_any_buffer_capacity____matches_a_single_unbounded_call(
        total in 0u64..500,
        capacity in 1usize..20,
    ) {
        let fs = FlatDir { total, answers_dotdot: true };
        let (names, _) = full_listing(&fs, capacity, Dots::Omit);
        prop_assert_eq!(names, expected_names(total, Dots::Omit));
    }

    /// A buffer that never fills behaves like one unbounded call.
    #[test]
    fn emit____capacity_covers_everything____is_a_single_call(total in 0u64..500) {
        let fs = FlatDir { total, answers_dotdot: true };
        let call = one_call(&fs, 0, total as usize + 2, Dots::Synthesize);
        prop_assert_eq!(call.resume_at, None);
        prop_assert_eq!(call.names, expected_names(total, Dots::Synthesize));
    }
}
