#![allow(non_snake_case)]
#![allow(clippy::unwrap_used)]
#![allow(clippy::expect_used)]

//! The framing layer is the first thing an untrusted client reaches, so the
//! cases that matter here are the malformed ones: a truncated message, a
//! fragment marked non-final, a length field that would have this server
//! allocate whatever a client asks for. Every one of them must be an error or
//! a clean stop, never a panic and never an oversized allocation.

use super::*;

/// Frame `body` the way a client would: a 4-byte record mark with the
/// last-fragment bit set, then the body.
fn framed(body: &[u8]) -> Vec<u8> {
    let mut out = (LAST_FRAGMENT_BIT | body.len() as u32)
        .to_be_bytes()
        .to_vec();
    out.extend_from_slice(body);
    out
}

#[test]
fn read_message____one_final_fragment____returns_the_body() {
    let wire = framed(b"payload!");
    let got = read_message(&mut wire.as_slice()).unwrap();
    assert_eq!(got.as_deref(), Some(&b"payload!"[..]));
}

#[test]
fn read_message____empty_body____is_an_empty_message_not_eof() {
    // A zero-length final fragment is well formed: NULL procedures send one.
    let wire = framed(b"");
    assert_eq!(
        read_message(&mut wire.as_slice()).unwrap(),
        Some(Vec::new())
    );
}

#[test]
fn read_message____stream_already_at_eof____is_a_clean_stop() {
    assert_eq!(read_message(&mut [].as_slice()).unwrap(), None);
}

#[test]
fn read_message____eof_partway_through_the_record_mark____is_an_error() {
    // Distinct from the clean-EOF case above: bytes arrived, then the peer
    // vanished, which is a broken connection rather than an orderly close.
    let err = read_message(&mut [0x80, 0x00].as_slice()).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
}

#[test]
fn read_message____body_shorter_than_the_record_mark_claims____is_an_error() {
    let mut wire = framed(b"twelve bytes");
    wire.truncate(wire.len() - 3);
    let err = read_message(&mut wire.as_slice()).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
}

#[test]
fn read_message____fragment_not_marked_final____is_rejected_rather_than_reassembled() {
    // v1 scope: the connection is closed rather than buffering fragments.
    let mut wire = (4u32).to_be_bytes().to_vec(); // no LAST_FRAGMENT_BIT
    wire.extend_from_slice(b"abcd");
    let err = read_message(&mut wire.as_slice()).unwrap_err();
    assert!(err.to_string().contains("multi-fragment"), "{err}");
}

#[test]
fn read_message____length_over_the_cap____is_rejected_before_allocating() {
    // Only the 4-byte record mark is supplied. If the cap were not checked
    // first, this would try to allocate the claimed size and then block.
    let wire = (LAST_FRAGMENT_BIT | (MAX_FRAGMENT_LEN + 1)).to_be_bytes();
    let err = read_message(&mut wire.as_slice()).unwrap_err();
    assert!(err.to_string().contains("size limit"), "{err}");
}

#[test]
fn read_message____length_exactly_at_the_cap____is_accepted() {
    let body = vec![0u8; MAX_FRAGMENT_LEN as usize];
    let wire = framed(&body);
    assert_eq!(read_message(&mut wire.as_slice()).unwrap(), Some(body));
}

#[test]
fn write_message____any_body____round_trips_through_read_message() {
    let mut wire = Vec::new();
    write_message(&mut wire, b"round trip").unwrap();
    assert_eq!(
        read_message(&mut wire.as_slice()).unwrap().as_deref(),
        Some(&b"round trip"[..])
    );
}

/// Build a CALL header: xid, msg_type, rpcvers, prog, vers, proc, then two
/// `opaque_auth` bodies (AUTH_NONE, empty).
fn call_header(xid: u32, rpcvers: u32, prog: u32, vers: u32, proc_: u32) -> Vec<u8> {
    let mut w = Writer::new();
    w.write_u32(xid);
    w.write_u32(0); // CALL
    w.write_u32(rpcvers);
    w.write_u32(prog);
    w.write_u32(vers);
    w.write_u32(proc_);
    for _ in 0..2 {
        w.write_u32(0); // flavor: AUTH_NONE
        w.write_u32(0); // body length
    }
    w.into_bytes()
}

#[test]
fn read_call_header____a_well_formed_call____is_parsed_with_the_cursor_at_the_args() {
    let mut bytes = call_header(0xDEAD_BEEF, 2, 100_003, 3, 6);
    bytes.extend_from_slice(b"ARGS");

    let mut r = Reader::new(&bytes);
    let HeaderOutcome::Call(call) = read_call_header(&mut r) else {
        panic!("expected a Call");
    };
    assert_eq!(call.xid, 0xDEAD_BEEF);
    assert_eq!(call.prog, 100_003);
    assert_eq!(call.vers, 3);
    assert_eq!(call.proc_, 6);
    // The cursor must be left exactly where the procedure arguments start.
    assert_eq!(r.read_u32(), Some(u32::from_be_bytes(*b"ARGS")));
}

#[test]
fn read_call_header____auth_sys_credentials____are_skipped_not_trusted() {
    // A real `mount_nfs` sends AUTH_SYS with a claimed uid/gid. The parser
    // must step over the body to reach the arguments; the contents are
    // deliberately never read — see `docs/ARCHITECTURE.md`'s platform
    // constraints for why AUTH_SYS is not trusted.
    let mut w = Writer::new();
    w.write_u32(1);
    w.write_u32(0); // CALL
    w.write_u32(2); // rpcvers
    w.write_u32(100_003);
    w.write_u32(3);
    w.write_u32(1);
    w.write_u32(1); // flavor: AUTH_SYS
    w.write_opaque_var(&[0u8; 28]); // claimed uid/gid and friends
    w.write_u32(0); // verf flavor: AUTH_NONE
    w.write_u32(0);
    let mut bytes = w.into_bytes();
    bytes.extend_from_slice(b"ARGS");

    let mut r = Reader::new(&bytes);
    let HeaderOutcome::Call(call) = read_call_header(&mut r) else {
        panic!("expected a Call");
    };
    assert_eq!(call.proc_, 1);
    assert_eq!(r.read_u32(), Some(u32::from_be_bytes(*b"ARGS")));
}

#[test]
fn read_call_header____unsupported_rpcvers____reports_the_xid_for_a_denied_reply() {
    let bytes = call_header(7, 3, 100_003, 3, 0);
    let mut r = Reader::new(&bytes);
    assert!(matches!(
        read_call_header(&mut r),
        HeaderOutcome::BadRpcvers(7)
    ));
}

#[test]
fn read_call_header____a_reply_rather_than_a_call____is_malformed() {
    let mut w = Writer::new();
    w.write_u32(1);
    w.write_u32(1); // REPLY, not CALL
    let bytes = w.into_bytes();
    let mut r = Reader::new(&bytes);
    assert!(matches!(read_call_header(&mut r), HeaderOutcome::Malformed));
}

#[test]
fn read_call_header____truncated_anywhere____is_malformed_not_a_panic() {
    let full = call_header(1, 2, 100_003, 3, 0);
    for cut in 0..full.len() {
        let mut r = Reader::new(&full[..cut]);
        let outcome = read_call_header(&mut r);
        assert!(
            matches!(
                outcome,
                HeaderOutcome::Malformed | HeaderOutcome::BadRpcvers(_)
            ),
            "prefix of {cut} bytes should not parse as a complete call"
        );
    }
}

#[test]
fn read_call_header____oversized_auth_body____is_malformed() {
    // `opaque_auth` bodies are capped at 400 bytes by RFC 5531; a larger
    // claim must be refused rather than skipped over.
    let mut w = Writer::new();
    w.write_u32(1);
    w.write_u32(0); // CALL
    w.write_u32(2);
    w.write_u32(100_003);
    w.write_u32(3);
    w.write_u32(0);
    w.write_u32(0); // flavor
    w.write_u32(401); // body length, over the cap
    let bytes = w.into_bytes();
    let mut r = Reader::new(&bytes);
    assert!(matches!(read_call_header(&mut r), HeaderOutcome::Malformed));
}

/// Decode an accepted reply header, returning `(xid, accept_stat)`.
fn decode_accepted(bytes: &[u8]) -> (u32, u32) {
    let mut r = Reader::new(bytes);
    let xid = r.read_u32().unwrap();
    assert_eq!(r.read_u32(), Some(1), "REPLY");
    assert_eq!(r.read_u32(), Some(0), "MSG_ACCEPTED");
    assert_eq!(r.read_u32(), Some(0), "verf flavor AUTH_NONE");
    assert_eq!(r.read_u32(), Some(0), "verf body length");
    (xid, r.read_u32().unwrap())
}

#[test]
fn build_reply____a_successful_procedure____carries_its_body_after_the_header() {
    let mut body = Writer::new();
    body.write_u32(0xABCD);
    let bytes = build_reply(42, ProcOutcome::Success(body));

    let (xid, stat) = decode_accepted(&bytes);
    assert_eq!(xid, 42);
    assert_eq!(stat, accept_stat::SUCCESS);
    let mut r = Reader::new(&bytes[24..]);
    assert_eq!(r.read_u32(), Some(0xABCD));
}

#[test]
fn build_reply____proc_unavail_and_garbage_args____report_their_own_accept_stat() {
    let (_, stat) = decode_accepted(&build_reply(1, ProcOutcome::ProcUnavail));
    assert_eq!(stat, accept_stat::PROC_UNAVAIL);

    let (_, stat) = decode_accepted(&build_reply(1, ProcOutcome::GarbageArgs));
    assert_eq!(stat, accept_stat::GARBAGE_ARGS);
}

#[test]
fn prog_unavail_reply____an_unknown_program____is_accepted_with_prog_unavail() {
    let (xid, stat) = decode_accepted(&prog_unavail_reply(5));
    assert_eq!(xid, 5);
    assert_eq!(stat, accept_stat::PROG_UNAVAIL);
}

#[test]
fn prog_mismatch_reply____a_version_other_than_three____advertises_three_to_three() {
    let bytes = prog_mismatch_reply(5);
    let (xid, stat) = decode_accepted(&bytes);
    assert_eq!(xid, 5);
    assert_eq!(stat, accept_stat::PROG_MISMATCH);

    let mut r = Reader::new(&bytes[24..]);
    assert_eq!(r.read_u32(), Some(3), "low");
    assert_eq!(r.read_u32(), Some(3), "high");
}

#[test]
fn rpc_mismatch_reply____an_unsupported_rpcvers____is_denied_advertising_version_two() {
    // The one case answered with MSG_DENIED rather than an accept_stat.
    let bytes = rpc_mismatch_reply(9);
    let mut r = Reader::new(&bytes);
    assert_eq!(r.read_u32(), Some(9), "xid");
    assert_eq!(r.read_u32(), Some(1), "REPLY");
    assert_eq!(r.read_u32(), Some(1), "MSG_DENIED");
    assert_eq!(r.read_u32(), Some(0), "RPC_MISMATCH");
    assert_eq!(r.read_u32(), Some(2), "low");
    assert_eq!(r.read_u32(), Some(2), "high");
}
