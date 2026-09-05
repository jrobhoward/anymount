//! Bounds-checked XDR (RFC 4506) primitives used by the RPC, MOUNT and NFS
//! layers. Every `Reader` method returns `None` on truncated or oversized
//! input rather than panicking or slicing out of bounds — a malformed or
//! truncated message from the network must never crash the server.

use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;

/// Cursor over a byte slice, decoding big-endian XDR primitives.
pub(super) struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub(super) fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(n)?;
        let slice = self.buf.get(self.pos..end)?;
        self.pos = end;
        Some(slice)
    }

    pub(super) fn read_u32(&mut self) -> Option<u32> {
        let b = self.take(4)?;
        Some(u32::from_be_bytes(b.try_into().ok()?))
    }

    pub(super) fn read_u64(&mut self) -> Option<u64> {
        let b = self.take(8)?;
        Some(u64::from_be_bytes(b.try_into().ok()?))
    }

    #[allow(dead_code)] // exercised by xdr_tests.rs; no production caller needs it yet
    pub(super) fn read_bool(&mut self) -> Option<bool> {
        Some(self.read_u32()? != 0)
    }

    pub(super) fn read_opaque_fixed<const N: usize>(&mut self) -> Option<[u8; N]> {
        let b = self.take(N)?;
        b.try_into().ok()
    }

    fn pad_len(len: u32) -> usize {
        ((4 - len % 4) % 4) as usize
    }

    pub(super) fn read_opaque_var(&mut self, max: u32) -> Option<Vec<u8>> {
        let len = self.read_u32()?;
        if len > max {
            return None;
        }
        let data = self.take(len as usize)?.to_vec();
        self.take(Self::pad_len(len))?;
        Some(data)
    }

    pub(super) fn read_string(&mut self, max: u32) -> Option<OsString> {
        Some(OsString::from_vec(self.read_opaque_var(max)?))
    }

    fn skip(&mut self, n: usize) -> Option<()> {
        self.take(n).map(|_| ())
    }

    /// Skip an `opaque_auth` (RFC 5531 §9): a 4-byte flavor followed by an
    /// opaque body capped at 400 bytes. The flavor and body contents are
    /// never inspected — `AUTH_SYS`'s claimed uid/gid is decorative — but the
    /// bytes must still be consumed correctly to reach the procedure args.
    /// Uses [`Self::skip`] rather than [`Self::read_opaque_var`] since the
    /// body is discarded, not needed as an owned `Vec`.
    pub(super) fn skip_opaque_auth(&mut self) -> Option<()> {
        self.read_u32()?; // flavor
        let len = self.read_u32()?;
        if len > 400 {
            return None;
        }
        self.skip(len as usize)?;
        self.skip(Self::pad_len(len))
    }
}

/// Accumulates big-endian XDR primitives into an owned buffer.
#[derive(Default)]
pub(super) struct Writer {
    buf: Vec<u8>,
}

impl Writer {
    pub(super) fn new() -> Self {
        Self { buf: Vec::new() }
    }

    pub(super) fn len(&self) -> usize {
        self.buf.len()
    }

    pub(super) fn into_bytes(self) -> Vec<u8> {
        self.buf
    }

    pub(super) fn write_u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }

    pub(super) fn write_u64(&mut self, v: u64) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }

    pub(super) fn write_bool(&mut self, v: bool) {
        self.write_u32(u32::from(v));
    }

    pub(super) fn write_opaque_fixed(&mut self, data: &[u8]) {
        self.buf.extend_from_slice(data);
        let pad = (4 - data.len() % 4) % 4;
        self.buf.extend(std::iter::repeat_n(0u8, pad));
    }

    pub(super) fn write_opaque_var(&mut self, data: &[u8]) {
        self.write_u32(data.len() as u32);
        self.write_opaque_fixed(data);
    }

    pub(super) fn write_string(&mut self, s: &std::ffi::OsStr) {
        use std::os::unix::ffi::OsStrExt;
        self.write_opaque_var(s.as_bytes());
    }

    pub(super) fn extend_from(&mut self, other: &Writer) {
        self.buf.extend_from_slice(&other.buf);
    }
}

#[cfg(test)]
#[path = "xdr_tests.rs"]
mod xdr_tests;
