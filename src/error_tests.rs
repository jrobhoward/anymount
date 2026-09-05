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

#[test]
fn io_error____from_an_io_variant____is_returned_whole() {
    // The original kind and raw OS error must survive, not be flattened into
    // a message. The number itself is arbitrary and is not interpreted here,
    // so no platform's `errno` constants are needed.
    const RAW: i32 = 28;
    let converted: std::io::Error = FsError::Io(std::io::Error::from_raw_os_error(RAW)).into();
    assert_eq!(converted.raw_os_error(), Some(RAW));
}

#[test]
fn io_error____from_a_context_wrapped_io_variant____still_unwraps_to_the_original() {
    const RAW: i32 = 28;
    let converted: std::io::Error = FsError::Io(std::io::Error::from_raw_os_error(RAW))
        .context("while mounting")
        .into();
    assert_eq!(converted.raw_os_error(), Some(RAW));
}

#[test]
fn io_error____from_each_plain_variant____maps_to_a_matching_kind() {
    use std::io::ErrorKind;

    let cases = [
        (FsError::NotFound, ErrorKind::NotFound),
        (FsError::PermissionDenied, ErrorKind::PermissionDenied),
        (FsError::ReadOnly, ErrorKind::PermissionDenied),
        (FsError::NotADirectory, ErrorKind::InvalidInput),
        (FsError::IsADirectory, ErrorKind::InvalidInput),
        (FsError::InvalidArgument, ErrorKind::InvalidInput),
        (FsError::NoXattr, ErrorKind::Unsupported),
        (FsError::Unsupported("no backend"), ErrorKind::Unsupported),
        (FsError::Other("boom".into()), ErrorKind::Other),
    ];

    for (err, want) in cases {
        let text = err.to_string();
        let converted: std::io::Error = err.into();
        assert_eq!(converted.kind(), want, "{text}");
        // The message has to survive, or converting loses the explanation.
        assert!(converted.to_string().contains(&text), "{text}");
    }
}

#[test]
fn io_error____from_a_context_wrapped_error____keeps_the_outer_message_and_inner_kind() {
    let err = FsError::NotFound.context("no such backup in the archive");
    let converted: std::io::Error = err.into();
    assert_eq!(converted.kind(), std::io::ErrorKind::NotFound);
    assert!(
        converted.to_string().contains("no such backup"),
        "{converted}"
    );
}
