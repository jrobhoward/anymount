//! Compile-time guards on the shape of the public API.
//!
//! Everything here holds today. It is asserted so that a change which quietly
//! takes it away — a backend handle gaining an `Rc` field, say, which would
//! cost `Mount` its `Send` — fails here rather than in a downstream crate.

#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]
#![allow(non_snake_case)]

use anymount::{
    Backend, DirEntry, FileAttr, FileHandle, FileKind, FsError, Ino, Mount, MountBuilder, ROOT_INO,
    ReadOnlyFs, StatFs,
};
use std::collections::HashSet;
use std::error::Error;
use std::ffi::OsString;

fn assert_send<T: Send>() {}
fn assert_sync<T: Sync>() {}
fn assert_static<T: 'static>() {}
fn assert_clone<T: Clone>() {}
fn assert_debug<T: std::fmt::Debug>() {}
fn assert_error<T: std::error::Error>() {}
fn assert_eq_trait<T: PartialEq>() {}
fn assert_hash<T: std::hash::Hash + Eq>() {}

#[test]
fn mount____as_a_handle____can_cross_thread_boundaries() {
    // A caller holding a mount on a worker thread, or in a struct shared
    // across threads, is the obvious usage; losing it would be a silent
    // breaking change.
    assert_send::<Mount>();
    assert_sync::<Mount>();
    assert_static::<Mount>();
    assert_debug::<Mount>();
}

#[test]
fn mount_builder____as_configuration____is_send_sync_and_cloneable() {
    assert_send::<MountBuilder>();
    assert_sync::<MountBuilder>();
    assert_clone::<MountBuilder>();
    assert_debug::<MountBuilder>();
}

#[test]
fn fs_error____as_a_returned_error____is_a_send_sync_std_error() {
    // `Send + Sync` is what lets it be boxed into `anyhow::Error` or returned
    // from a thread, which downstream code will expect of any error type.
    assert_send::<FsError>();
    assert_sync::<FsError>();
    assert_error::<FsError>();
}

#[test]
fn value_types____as_data____are_clone_and_debug() {
    assert_clone::<FileAttr>();
    assert_clone::<DirEntry>();
    assert_clone::<StatFs>();
    assert_clone::<Ino>();
    assert_clone::<FileHandle>();
    assert_clone::<FileKind>();
    assert_debug::<FileAttr>();
    assert_debug::<DirEntry>();
    assert_debug::<StatFs>();
}

#[test]
fn value_types____as_test_expectations____compare_by_equality() {
    // An implementor's own tests assert on these, so equality is part of what
    // the crate offers rather than an incidental derive.
    assert_eq_trait::<FileAttr>();
    assert_eq_trait::<DirEntry>();
    assert_eq_trait::<StatFs>();
    assert_eq_trait::<FileKind>();
    assert_eq!(FileAttr::file(Ino(2), 8), FileAttr::file(Ino(2), 8));
    assert_ne!(FileAttr::file(Ino(2), 8), FileAttr::file(Ino(2), 9));
    assert_eq!(StatFs::default(), StatFs::default());
}

#[test]
fn value_types____as_map_keys____are_hashable() {
    assert_hash::<Ino>();
    assert_hash::<FileHandle>();
    assert_hash::<FileKind>();
    assert_hash::<DirEntry>();
    assert_hash::<Backend>();
    let kinds: HashSet<FileKind> = [FileKind::File, FileKind::Directory, FileKind::File]
        .into_iter()
        .collect();
    assert_eq!(kinds.len(), 2);
}

#[test]
fn file_kind____as_a_sort_key____orders_files_before_directories() {
    let mut kinds = vec![FileKind::Directory, FileKind::File];
    kinds.sort();
    assert_eq!(kinds, vec![FileKind::File, FileKind::Directory]);
}

#[test]
fn ino_and_file_handle____as_newtypes____convert_both_ways() {
    // `From` is what generic code reaches for; the public field alone leaves
    // it writing `.0` in a position where a conversion trait is expected.
    assert_eq!(Ino::from(7u64), Ino(7));
    assert_eq!(u64::from(Ino(7)), 7);
    assert_eq!(FileHandle::from(9u64), FileHandle(9));
    assert_eq!(u64::from(FileHandle(9)), 9);
}

#[test]
fn dir_entry____as_data____compares_by_every_field() {
    let entry = |ino, name: &str| DirEntry {
        ino: Ino(ino),
        name: OsString::from(name),
        kind: FileKind::File,
    };
    assert_eq!(entry(2, "a"), entry(2, "a"));
    assert_ne!(entry(2, "a"), entry(2, "b"));
    assert_ne!(entry(2, "a"), entry(3, "a"));
}

#[test]
fn read_only_fs____as_a_bound____is_object_safe_and_thread_safe() {
    fn takes_dyn(_: &(dyn ReadOnlyFs + Send + Sync)) {}
    let _ = takes_dyn;
    assert_send::<Box<dyn ReadOnlyFs>>();
    assert_sync::<Box<dyn ReadOnlyFs>>();
}

#[test]
fn fs_error____wrapped_in_context____keeps_the_chain_traversable() {
    // `context` exists to explain a failure without losing it. If the wrapped
    // error is not reported as `source`, a caller using `anyhow` sees the
    // explanation and nothing underneath it, which is the opposite of the
    // point.
    let e = FsError::NotFound.context("opening the archive index");
    assert_eq!(e.to_string(), "opening the archive index");
    let source = e.source().expect("context must report its inner error");
    assert_eq!(source.to_string(), FsError::NotFound.to_string());

    // Nested context unwinds one layer at a time, down to the real failure.
    let nested = e.context("listing /backups");
    let mut depth = 0;
    let mut cur: Option<&(dyn Error + 'static)> = Some(&nested);
    while let Some(next) = cur.and_then(Error::source) {
        depth += 1;
        cur = Some(next);
    }
    assert_eq!(depth, 2);
}

#[test]
fn fs_error____as_an_io_source____keeps_the_underlying_error() {
    let io = std::io::Error::new(std::io::ErrorKind::BrokenPipe, "pipe closed");
    let e = FsError::from(io).context("serving READ3");
    let mut innermost: &(dyn Error + 'static) = &e;
    while let Some(next) = innermost.source() {
        innermost = next;
    }
    assert_eq!(innermost.to_string(), "pipe closed");
}

#[test]
fn fs_error____as_a_non_exhaustive_enum____is_matched_with_a_rest_pattern() {
    // Downstream code matches `Context` with `..` because the variant is
    // `#[non_exhaustive]`; this asserts the pattern the docs tell callers to
    // write still compiles.
    let e = FsError::PermissionDenied.context("mounting");
    assert!(matches!(e, FsError::Context { .. }));
}

#[test]
fn backend____default____is_auto() {
    // `MountBuilder::new` relies on this, and so does anyone constructing a
    // `Backend` with `Default`.
    assert_eq!(Backend::default(), Backend::Auto);
}

#[test]
fn root_ino____as_the_documented_entry_point____is_one_and_usable_as_a_pattern() {
    assert_eq!(ROOT_INO, Ino(1));
    // `const` patterns are how implementations dispatch on the root; this
    // stops `ROOT_INO` from becoming a non-`const` item.
    assert!(matches!(Ino(1), ROOT_INO));
}
