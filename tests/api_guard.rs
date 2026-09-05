//! Compile-time guards on the shape of the public API.
//!
//! Everything here holds today. It is asserted so that a change which quietly
//! takes it away — a backend handle gaining an `Rc` field, say, which would
//! cost `Mount` its `Send` — fails here rather than in a downstream crate.

#![allow(clippy::unwrap_used)]
#![allow(non_snake_case)]

use anymount::{
    Backend, DirEntry, FileAttr, FileHandle, FileKind, FsError, Ino, Mount, MountBuilder, ROOT_INO,
    ReadOnlyFs, StatFs,
};

fn assert_send<T: Send>() {}
fn assert_sync<T: Sync>() {}
fn assert_static<T: 'static>() {}
fn assert_clone<T: Clone>() {}
fn assert_debug<T: std::fmt::Debug>() {}
fn assert_error<T: std::error::Error>() {}

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
fn read_only_fs____as_a_bound____is_object_safe_and_thread_safe() {
    fn takes_dyn(_: &(dyn ReadOnlyFs + Send + Sync)) {}
    let _ = takes_dyn;
    assert_send::<Box<dyn ReadOnlyFs>>();
    assert_sync::<Box<dyn ReadOnlyFs>>();
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
