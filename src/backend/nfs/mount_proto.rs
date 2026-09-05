//! MOUNT program (100005, version 3) — RFC 1813 Appendix I.
//!
//! Single-export server: the only valid `dirpath` is `/export/<32 lowercase
//! hex chars>`, where the hex is this mount's [`FileHandle3`] secret. Any
//! other shape is `MNT3ERR_NOENT`; the right shape with the wrong secret is
//! `MNT3ERR_ACCES`, matching `mount_nfs`'s observed "Permission denied, exit
//! 13" when the spike deliberately supplied a wrong secret.

use crate::types::ROOT_INO;

use super::handle::FileHandle3;
use super::rpc::ProcOutcome;
use super::xdr::{Reader, Writer};

const MNT3_OK: u32 = 0;
const MNT3ERR_NOENT: u32 = 2;
const MNT3ERR_ACCES: u32 = 13;

const MNTPATHLEN: u32 = 1024;

const EXPORT_PREFIX: &str = "/export/";

/// Route one MOUNT-program call to its procedure handler.
pub(super) fn dispatch(proc_: u32, r: &mut Reader<'_>, handle: &FileHandle3) -> ProcOutcome {
    match proc_ {
        0 => ProcOutcome::Success(Writer::new()), // MOUNTPROC3_NULL
        1 => mnt(r, handle),
        3 => umnt(r),
        5 => export(r),
        _ => ProcOutcome::ProcUnavail,
    }
}

fn mnt(r: &mut Reader<'_>, handle: &FileHandle3) -> ProcOutcome {
    let Some(dirpath) = r.read_string(MNTPATHLEN) else {
        return ProcOutcome::GarbageArgs;
    };
    let mut w = Writer::new();

    let Some(dirpath) = dirpath.to_str() else {
        w.write_u32(MNT3ERR_NOENT);
        return ProcOutcome::Success(w);
    };
    let Some(hex) = dirpath.strip_prefix(EXPORT_PREFIX) else {
        w.write_u32(MNT3ERR_NOENT);
        return ProcOutcome::Success(w);
    };
    if hex.len() != 32 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        w.write_u32(MNT3ERR_NOENT);
        return ProcOutcome::Success(w);
    }
    if hex != handle.secret_hex() {
        w.write_u32(MNT3ERR_ACCES);
        return ProcOutcome::Success(w);
    }

    w.write_u32(MNT3_OK);
    w.write_opaque_var(&handle.encode(ROOT_INO));
    w.write_u32(1); // auth_flavors<> length
    w.write_u32(1); // AUTH_SYS
    ProcOutcome::Success(w)
}

/// Single-export server: teardown is done via `Mount::unmount()`, so `UMNT`
/// is accepted unconditionally.
fn umnt(r: &mut Reader<'_>) -> ProcOutcome {
    let Some(_dirpath) = r.read_string(MNTPATHLEN) else {
        return ProcOutcome::GarbageArgs;
    };
    ProcOutcome::Success(Writer::new())
}

fn export(_r: &mut Reader<'_>) -> ProcOutcome {
    let mut w = Writer::new();
    w.write_bool(false); // empty export list
    ProcOutcome::Success(w)
}

#[cfg(test)]
#[path = "mount_proto_tests.rs"]
mod mount_proto_tests;
