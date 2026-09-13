#![allow(non_snake_case)]
#![allow(clippy::unwrap_used)]
#![allow(clippy::expect_used)]

//! [`still_mounted`] decides whether a failed `unmount(2)` means the mount is
//! already gone. It reads device numbers off `stat`, with no platform API in
//! it, so it is tested on every Unix rather than only where it is called.

use std::path::Path;

use super::*;

#[test]
fn still_mounted____an_ordinary_directory____is_false() {
    let dir = tempfile::tempdir().unwrap();
    assert!(!still_mounted(dir.path()));
}

#[test]
fn still_mounted____a_path_that_does_not_exist____is_false() {
    let dir = tempfile::tempdir().unwrap();
    assert!(!still_mounted(&dir.path().join("no-such-entry")));
}

#[test]
fn still_mounted____the_filesystem_root____is_true() {
    // `/` has no parent to compare against and is a mount point by
    // definition, so the device-number check has to be skipped there.
    assert!(still_mounted(Path::new("/")));
}

#[test]
fn still_mounted____a_relative_path____resolves_against_the_working_directory() {
    // A path with no directory part has an empty parent, which would be an
    // unreadable path rather than the working directory it stands for.
    // `src` is in the same filesystem as the crate root that holds it.
    assert!(!still_mounted(Path::new("src")));
}

#[test]
fn still_mounted____a_trailing_slash____does_not_hide_the_parent() {
    let dir = tempfile::tempdir().unwrap();
    let with_slash = format!("{}/", dir.path().display());
    assert!(!still_mounted(Path::new(&with_slash)));
}
