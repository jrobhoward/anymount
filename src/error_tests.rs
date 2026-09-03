//! Tests for the shared filesystem error type and its errno mapping.

#![allow(clippy::unwrap_used)]
#![allow(clippy::expect_used)]
#![allow(non_snake_case)]

use super::*;

#[cfg(unix)]
#[test]
fn to_errno____not_found____is_enoent() {
    assert_eq!(FsError::NotFound.to_errno(), libc::ENOENT);
}

#[cfg(unix)]
#[test]
fn to_errno____read_only____is_erofs() {
    assert_eq!(FsError::ReadOnly.to_errno(), libc::EROFS);
}

#[cfg(unix)]
#[test]
fn to_errno____unsupported____is_enosys() {
    assert_eq!(FsError::Unsupported("nope").to_errno(), libc::ENOSYS);
}

#[cfg(unix)]
#[test]
fn to_errno____io_error____uses_underlying_os_error() {
    let io = io::Error::from_raw_os_error(libc::EACCES);
    assert_eq!(FsError::Io(io).to_errno(), libc::EACCES);
}

#[cfg(unix)]
#[test]
fn context____wrapping_an_error____preserves_the_inner_errno() {
    let wrapped = FsError::NotADirectory.context("extra explanation");
    assert_eq!(wrapped.to_errno(), libc::ENOTDIR);
}

#[test]
fn context____wrapping_an_error____display_shows_the_message() {
    let wrapped = FsError::NotADirectory.context("extra explanation");
    assert_eq!(wrapped.to_string(), "extra explanation");
}
