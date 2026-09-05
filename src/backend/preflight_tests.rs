#![allow(non_snake_case)]
#![allow(clippy::unwrap_used)]
#![allow(clippy::expect_used)]

//! [`check`] is the one part of mounting that runs on every platform with
//! no live mount involved, so it is tested directly rather than through a
//! backend.

use std::fs;

use super::*;

const FULL: Caps = Caps {
    name: "test-full",
    allow_other: true,
    auto_unmount: true,
    empty_mountpoint: false,
    threads: true,
};

const MINIMAL: Caps = Caps {
    name: "test-minimal",
    allow_other: false,
    auto_unmount: false,
    empty_mountpoint: false,
    threads: false,
};

/// Shaped like cfapi: projects into the mountpoint, so it must be empty.
const PROJECTING: Caps = Caps {
    name: "test-projecting",
    allow_other: false,
    auto_unmount: false,
    empty_mountpoint: true,
    threads: false,
};

fn builder_at(path: &std::path::Path) -> MountBuilder {
    MountBuilder::new(path)
}

#[test]
fn preflight____existing_directory_and_no_options____is_accepted() {
    let dir = tempfile::tempdir().unwrap();
    check(&builder_at(dir.path()), &MINIMAL).unwrap();
}

#[test]
fn preflight____mountpoint_does_not_exist____is_rejected_with_the_path() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("nope");
    let err = check(&builder_at(&missing), &FULL).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("must exist and be a directory"), "{msg}");
    assert!(msg.contains("nope"), "{msg}");
}

#[test]
fn preflight____mountpoint_is_a_regular_file____is_rejected_as_not_a_directory() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("a-file");
    fs::write(&file, b"not a directory").unwrap();

    let err = check(&builder_at(&file), &FULL).unwrap_err();
    assert!(err.to_string().contains("is not a directory"));
    #[cfg(unix)]
    assert_eq!(err.to_errno(), FsError::NotADirectory.to_errno());
}

#[test]
fn preflight____allow_other_on_a_backend_without_it____names_that_backend() {
    let dir = tempfile::tempdir().unwrap();
    let err = check(&builder_at(dir.path()).allow_other(true), &MINIMAL).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("test-minimal"), "{msg}");
    assert!(msg.contains("allow_other"), "{msg}");
}

#[test]
fn preflight____auto_unmount_on_a_backend_without_it____names_that_backend() {
    let dir = tempfile::tempdir().unwrap();
    let err = check(&builder_at(dir.path()).auto_unmount(true), &MINIMAL).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("test-minimal"), "{msg}");
    assert!(msg.contains("auto_unmount"), "{msg}");
}

#[test]
fn preflight____options_the_backend_supports____are_accepted() {
    let dir = tempfile::tempdir().unwrap();
    let builder = builder_at(dir.path()).allow_other(true).auto_unmount(true);
    check(&builder, &FULL).unwrap();
}

#[cfg(unix)]
#[test]
fn preflight____unsupported_option____maps_to_einval() {
    // The explanation is attached with `FsError::context`, so the errno the
    // kernel would see must still be the inner one.
    let dir = tempfile::tempdir().unwrap();
    let err = check(&builder_at(dir.path()).allow_other(true), &MINIMAL).unwrap_err();
    assert_eq!(err.to_errno(), FsError::InvalidArgument.to_errno());
}

#[test]
fn preflight____empty_mountpoint_required_and_directory_is_empty____is_accepted() {
    let dir = tempfile::tempdir().unwrap();
    check(&builder_at(dir.path()), &PROJECTING).unwrap();
}

#[test]
fn preflight____empty_mountpoint_required_and_a_file_is_present____is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("already-here.txt"), b"do not delete me").unwrap();

    let err = check(&builder_at(dir.path()), &PROJECTING).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("not empty"), "{msg}");
    assert!(msg.contains("test-projecting"), "{msg}");
    // The destructive consequence is the reason for the check, so it has to
    // be in the message a caller actually sees.
    assert!(msg.contains("unmount"), "{msg}");
    // EINVAL, spelled the way `to_errno` reports it on every platform.
    assert_eq!(err.to_errno(), FsError::InvalidArgument.to_errno());
}

#[test]
fn preflight____empty_mountpoint_not_required____tolerates_existing_contents() {
    // A Unix mount covers the mountpoint, so its prior contents are hidden
    // rather than destroyed; the check must not fire for those backends.
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("already-here.txt"), b"still fine").unwrap();
    check(&builder_at(dir.path()), &MINIMAL).unwrap();
}

#[test]
fn preflight____threads_on_a_backend_without_a_worker_pool____names_that_backend() {
    let dir = tempfile::tempdir().unwrap();
    let builder = builder_at(dir.path()).threads(8);
    let err = check(&builder, &MINIMAL).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("threads"), "{msg}");
    assert!(msg.contains("test-minimal"), "{msg}");
}

#[test]
fn preflight____threads_on_a_backend_that_has_one____is_accepted() {
    let dir = tempfile::tempdir().unwrap();
    check(&builder_at(dir.path()).threads(8), &FULL).unwrap();
}
