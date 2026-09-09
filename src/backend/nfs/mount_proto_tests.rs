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
    let handle = FileHandle3::for_test(1);
    let path = format!("{EXPORT_PREFIX}{}", handle.secret_hex());
    assert_eq!(status_of(call_mnt(&handle, &path)), MNT3_OK);
}

#[test]
fn mnt____malformed_path____is_noent() {
    let handle = FileHandle3::for_test(1);
    assert_eq!(
        status_of(call_mnt(&handle, "/not/an/export")),
        MNT3ERR_NOENT
    );
}

#[test]
fn mnt____right_shape_wrong_secret____is_acces() {
    let handle = FileHandle3::for_test(1);
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
    let handle = FileHandle3::for_test(1);
    assert!(matches!(
        dispatch(3, &mut r, &handle),
        ProcOutcome::Success(_)
    ));
}

#[test]
fn export____no_args____returns_empty_list() {
    let bytes: [u8; 0] = [];
    let mut r = Reader::new(&bytes);
    let handle = FileHandle3::for_test(1);
    let outcome = dispatch(5, &mut r, &handle);
    assert!(matches!(outcome, ProcOutcome::Success(_)));
}

#[test]
fn dispatch____unknown_proc____is_proc_unavail() {
    let bytes: [u8; 0] = [];
    let mut r = Reader::new(&bytes);
    let handle = FileHandle3::for_test(1);
    assert!(matches!(
        dispatch(99, &mut r, &handle),
        ProcOutcome::ProcUnavail
    ));
}

#[test]
fn mnt____export_path_with_a_volume_label____succeeds_with_root_handle() {
    // The shape `mount` actually sends: the secret, then the label macOS uses
    // as the volume name.
    let handle = FileHandle3::for_test(1);
    let path = format!("{EXPORT_PREFIX}{}/anymount-memfs", handle.secret_hex());
    assert_eq!(status_of(call_mnt(&handle, &path)), MNT3_OK);
}

#[test]
fn mnt____a_label_that_is_wrong____does_not_affect_authorization() {
    // The label authorizes nothing, so any label with the right secret works.
    let handle = FileHandle3::for_test(1);
    for label in ["", "a b c", "..", "with/separators/in/it"] {
        let path = format!("{EXPORT_PREFIX}{}/{label}", handle.secret_hex());
        assert_eq!(
            status_of(call_mnt(&handle, &path)),
            MNT3_OK,
            "label {label:?}"
        );
    }
}

#[test]
fn mnt____correct_secret_in_the_label_rather_than_the_first_segment____is_acces() {
    // The check must read the segment before the first `/`, never search the
    // whole path. Otherwise a client could park the real secret in the label
    // and have a wrong first segment accepted.
    let handle = FileHandle3::for_test(1);
    let wrong = "0".repeat(32);
    let path = format!("{EXPORT_PREFIX}{wrong}/{}", handle.secret_hex());
    assert_eq!(status_of(call_mnt(&handle, &path)), MNT3ERR_ACCES);
}

#[test]
fn mnt____wrong_secret_with_a_label____is_still_acces() {
    let handle = FileHandle3::for_test(1);
    let wrong = "0".repeat(32);
    let path = format!("{EXPORT_PREFIX}{wrong}/anymount");
    assert_eq!(status_of(call_mnt(&handle, &path)), MNT3ERR_ACCES);
}

#[test]
fn mnt____a_label_after_a_malformed_secret____is_still_noent() {
    let handle = FileHandle3::for_test(1);
    assert_eq!(
        status_of(call_mnt(&handle, "/export/not-hex/anymount")),
        MNT3ERR_NOENT
    );
}

#[test]
fn volume_label____an_ordinary_name____is_kept_as_is() {
    use crate::backend::nfs::volume_label;
    assert_eq!(volume_label("anymount-memfs"), "anymount-memfs");
    assert_eq!(volume_label("My Archive"), "My Archive");
    assert_eq!(volume_label("backup.2024"), "backup.2024");
}

#[test]
fn volume_label____a_name_with_separators____cannot_add_a_path_segment() {
    // A `/` surviving here would change which text the `MNT` handler checks
    // against the secret.
    use crate::backend::nfs::volume_label;
    for name in ["a/b", "../..", "/etc/passwd", "a//b"] {
        let label = volume_label(name);
        assert!(!label.contains('/'), "{name:?} produced {label:?}");
        assert!(!label.is_empty(), "{name:?} produced an empty label");
    }
}

#[test]
fn volume_label____control_characters____are_replaced() {
    use crate::backend::nfs::volume_label;
    assert_eq!(volume_label("a\nb"), "a-b");
    assert_eq!(
        volume_label("a\n\r\tb"),
        "a-b",
        "a run collapses to one dash"
    );
    assert!(!volume_label("tab\there").contains('\t'));
}

#[test]
fn volume_label____an_accented_or_non_latin_name____is_kept_intact() {
    // The label is decorative and never compared, so there is no reason to
    // mangle a name a user would recognise.
    use crate::backend::nfs::volume_label;
    assert_eq!(volume_label("café"), "café");
    assert_eq!(volume_label("Sauvegarde Été"), "Sauvegarde Été");
    assert_eq!(volume_label("バックアップ"), "バックアップ");
}

#[test]
fn volume_label____nothing_usable____falls_back_to_the_crate_name() {
    use crate::backend::nfs::{DEFAULT_LABEL, volume_label};
    assert_eq!(volume_label(""), DEFAULT_LABEL);
    assert_eq!(volume_label("///"), DEFAULT_LABEL);
    assert_eq!(volume_label("..."), DEFAULT_LABEL);
}

#[test]
fn volume_label____a_very_long_name____is_capped() {
    use crate::backend::nfs::{MAX_LABEL_LEN, volume_label};
    let label = volume_label(&"a".repeat(500));
    assert_eq!(label.len(), MAX_LABEL_LEN);
}
