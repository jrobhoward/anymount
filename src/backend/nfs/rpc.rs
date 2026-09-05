//! ONC RPC (RFC 5531) record marking and call/reply envelopes, hand-rolled
//! rather than pulling in `onc-rpc`: that crate covers this layer but not the
//! MOUNT/NFSv3 payload XDR (`fattr3`, `dirlistplus3`, ...), which still needs
//! hand-rolling regardless, so there is little to gain from a partial
//! dependency. See `docs/GAPS.md` if this ever needs replacing.
//!
//! v1 scope: single-fragment TCP messages only. A multi-fragment request
//! closes the connection rather than reassembling — every `mount_nfs`
//! request observed in the spike fits in one fragment.

use std::io::{self, Read, Write};

use super::xdr::{Reader, Writer};

/// Reject any single fragment larger than this rather than allocating a
/// buffer sized by an untrusted client.
const MAX_FRAGMENT_LEN: u32 = 256 * 1024;

const LAST_FRAGMENT_BIT: u32 = 0x8000_0000;

/// RPC accept_stat values (RFC 5531 §9).
pub(super) mod accept_stat {
    pub(crate) const SUCCESS: u32 = 0;
    pub(crate) const PROG_UNAVAIL: u32 = 1;
    pub(crate) const PROG_MISMATCH: u32 = 2;
    pub(crate) const PROC_UNAVAIL: u32 = 3;
    pub(crate) const GARBAGE_ARGS: u32 = 4;
}

/// Read one length-prefixed RPC message body. `Ok(None)` on a clean EOF
/// before any header byte arrives; `Err` on a short read mid-message, a
/// fragment marked non-final (unsupported in v1), or an oversized length.
///
/// Generic over [`Read`] rather than taking a `TcpStream`: this is the code
/// that parses untrusted bytes off a socket, and the framing rules it enforces
/// are worth testing against a byte slice rather than only against a live
/// connection.
pub(super) fn read_message<R: Read>(stream: &mut R) -> io::Result<Option<Vec<u8>>> {
    let mut hdr = [0u8; 4];
    if !read_exact_or_eof(stream, &mut hdr)? {
        return Ok(None);
    }
    let hdr = u32::from_be_bytes(hdr);
    let last = hdr & LAST_FRAGMENT_BIT != 0;
    let len = hdr & !LAST_FRAGMENT_BIT;

    if !last {
        return Err(io::Error::other("multi-fragment RPC message not supported"));
    }
    if len > MAX_FRAGMENT_LEN {
        return Err(io::Error::other("RPC fragment exceeds size limit"));
    }

    let mut body = vec![0u8; len as usize];
    stream.read_exact(&mut body)?;
    Ok(Some(body))
}

/// Reads into `buf`, returning `Ok(true)` on success, `Ok(false)` if the
/// stream was already at EOF, and `Err` on a short read after some bytes
/// were received.
fn read_exact_or_eof<R: Read>(stream: &mut R, buf: &mut [u8]) -> io::Result<bool> {
    let mut filled = 0;
    while filled < buf.len() {
        let n = stream.read(&mut buf[filled..])?;
        if n == 0 {
            return if filled == 0 {
                Ok(false)
            } else {
                Err(io::Error::from(io::ErrorKind::UnexpectedEof))
            };
        }
        filled += n;
    }
    Ok(true)
}

/// Write one length-prefixed RPC message, as a single final fragment.
pub(super) fn write_message<W: Write>(stream: &mut W, body: &[u8]) -> io::Result<()> {
    let hdr = LAST_FRAGMENT_BIT | (body.len() as u32);
    stream.write_all(&hdr.to_be_bytes())?;
    stream.write_all(body)?;
    Ok(())
}

/// Decoded RPC call header (RFC 5531 §9), with the cursor left positioned at
/// the start of the procedure-specific arguments.
pub(super) struct CallHeader {
    pub(super) xid: u32,
    pub(super) prog: u32,
    pub(super) vers: u32,
    pub(super) proc_: u32,
}

/// Outcome of parsing a call header.
pub(super) enum HeaderOutcome {
    Call(CallHeader),
    /// A well-formed `CALL` with an `rpcvers` this server does not speak —
    /// the one case answered with `MSG_DENIED` rather than a per-program
    /// `accept_stat`.
    BadRpcvers(u32),
    /// Too short, or not a `CALL` at all — no `xid` can be trusted, so no
    /// reply is sent; the connection is simply closed.
    Malformed,
}

/// Parse the call header, skipping `cred`/`verf` bodies.
pub(super) fn read_call_header(r: &mut Reader<'_>) -> HeaderOutcome {
    let Some(xid) = r.read_u32() else {
        return HeaderOutcome::Malformed;
    };
    let Some(mtype) = r.read_u32() else {
        return HeaderOutcome::Malformed;
    };
    if mtype != 0 {
        return HeaderOutcome::Malformed; // not a CALL
    }
    let Some(rpcvers) = r.read_u32() else {
        return HeaderOutcome::Malformed;
    };
    if rpcvers != 2 {
        return HeaderOutcome::BadRpcvers(xid);
    }
    (|| {
        let prog = r.read_u32()?;
        let vers = r.read_u32()?;
        let proc_ = r.read_u32()?;
        r.skip_opaque_auth()?;
        r.skip_opaque_auth()?;
        Some(HeaderOutcome::Call(CallHeader {
            xid,
            prog,
            vers,
            proc_,
        }))
    })()
    .unwrap_or(HeaderOutcome::Malformed)
}

/// Build an `MSG_DENIED` / `RPC_MISMATCH` reply for [`HeaderOutcome::BadRpcvers`].
pub(super) fn rpc_mismatch_reply(xid: u32) -> Vec<u8> {
    let mut w = Writer::new();
    w.write_u32(xid);
    w.write_u32(1); // REPLY
    w.write_u32(1); // MSG_DENIED
    w.write_u32(0); // RPC_MISMATCH
    w.write_u32(2); // low
    w.write_u32(2); // high
    w.into_bytes()
}

/// Write an accepted reply header: `AUTH_NONE` verifier plus `accept_stat`.
/// The caller appends any procedure-specific result body afterward.
pub(super) fn write_reply_header(w: &mut Writer, xid: u32, stat: u32) {
    w.write_u32(xid);
    w.write_u32(1); // REPLY
    w.write_u32(0); // MSG_ACCEPTED
    w.write_u32(0); // verf flavor: AUTH_NONE
    w.write_u32(0); // verf body length: 0
    w.write_u32(stat);
}

/// Write a `PROG_MISMATCH` reply body (`low`, `high` version bounds) after
/// [`write_reply_header`].
pub(super) fn write_prog_mismatch_body(w: &mut Writer, low: u32, high: u32) {
    w.write_u32(low);
    w.write_u32(high);
}

/// Result of routing one call to a program's procedure table.
pub(super) enum ProcOutcome {
    /// The procedure ran; `Writer` holds its already-encoded result body
    /// (which itself carries an `nfsstat3`/`mountstat3` — RPC-level success
    /// and protocol-level failure are different layers).
    Success(Writer),
    /// `proc` is not one this program implements.
    ProcUnavail,
    /// The arguments could not be decoded (a `Reader` ran out of bytes).
    GarbageArgs,
}

/// Build the full reply message for one dispatched call, given the outcome
/// of routing it to a program's procedure table.
pub(super) fn build_reply(xid: u32, outcome: ProcOutcome) -> Vec<u8> {
    let mut w = Writer::new();
    match outcome {
        ProcOutcome::Success(body) => {
            write_reply_header(&mut w, xid, accept_stat::SUCCESS);
            w.extend_from(&body);
        }
        ProcOutcome::ProcUnavail => write_reply_header(&mut w, xid, accept_stat::PROC_UNAVAIL),
        ProcOutcome::GarbageArgs => write_reply_header(&mut w, xid, accept_stat::GARBAGE_ARGS),
    }
    w.into_bytes()
}

/// Build a `PROG_UNAVAIL` reply.
pub(super) fn prog_unavail_reply(xid: u32) -> Vec<u8> {
    let mut w = Writer::new();
    write_reply_header(&mut w, xid, accept_stat::PROG_UNAVAIL);
    w.into_bytes()
}

/// Build a `PROG_MISMATCH` reply for a program that only speaks version 3.
pub(super) fn prog_mismatch_reply(xid: u32) -> Vec<u8> {
    let mut w = Writer::new();
    write_reply_header(&mut w, xid, accept_stat::PROG_MISMATCH);
    write_prog_mismatch_body(&mut w, 3, 3);
    w.into_bytes()
}

#[cfg(test)]
#[path = "rpc_tests.rs"]
mod rpc_tests;
