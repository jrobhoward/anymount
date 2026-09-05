#![allow(non_snake_case)]
#![allow(clippy::unwrap_used)]
#![allow(clippy::expect_used)]

use super::*;
use crate::backend::nfs::xdr::Writer;

fn call_mnt(handle: &FileHandle3, dirpath: &str) -> ProcOutcome {
    let mut w = Writer::new();
    w.write_string(std::ffi::OsStr::new(dirpath));
    let bytes = w.into_bytes();
    let mut r = Reader::new(&bytes);
    dispatch(1, &mut r, handle)
}

fn status_of(outcome: ProcOutcome) -> u32 {
    match outcome {
        ProcOutcome::Success(w) => {
            let bytes = w.into_bytes();
            u32::from_be_bytes(bytes[..4].try_into().unwrap())
        }
        _ => panic!("expected Success"),
    }
}

#[test]
fn mnt____correct_export_path____succeeds_with_root_handle() {
    let handle = FileHandle3::new_random();
    let path = format!("{EXPORT_PREFIX}{}", handle.secret_hex());
    assert_eq!(status_of(call_mnt(&handle, &path)), MNT3_OK);
}

#[test]
fn mnt____malformed_path____is_noent() {
    let handle = FileHandle3::new_random();
    assert_eq!(
        status_of(call_mnt(&handle, "/not/an/export")),
        MNT3ERR_NOENT
    );
}

#[test]
fn mnt____right_shape_wrong_secret____is_acces() {
    let handle = FileHandle3::new_random();
    let wrong = "0".repeat(32);
    let path = format!("{EXPORT_PREFIX}{wrong}");
    assert_eq!(status_of(call_mnt(&handle, &path)), MNT3ERR_ACCES);
}

#[test]
fn umnt____any_path____is_accepted_unconditionally() {
    let mut w = Writer::new();
    w.write_string(std::ffi::OsStr::new("/export/whatever"));
    let bytes = w.into_bytes();
    let mut r = Reader::new(&bytes);
    let handle = FileHandle3::new_random();
    assert!(matches!(
        dispatch(3, &mut r, &handle),
        ProcOutcome::Success(_)
    ));
}

#[test]
fn export____no_args____returns_empty_list() {
    let bytes: [u8; 0] = [];
    let mut r = Reader::new(&bytes);
    let handle = FileHandle3::new_random();
    let outcome = dispatch(5, &mut r, &handle);
    assert!(matches!(outcome, ProcOutcome::Success(_)));
}

#[test]
fn dispatch____unknown_proc____is_proc_unavail() {
    let bytes: [u8; 0] = [];
    let mut r = Reader::new(&bytes);
    let handle = FileHandle3::new_random();
    assert!(matches!(
        dispatch(99, &mut r, &handle),
        ProcOutcome::ProcUnavail
    ));
}
