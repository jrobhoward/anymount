//! The `mount(2)` argument buffer for an NFS mount over an `AF_UNIX` socket.
//!
//! `mount_nfs` has no command-line option for any of the three attributes this
//! path needs: there is no file-handle option, no mount-from option, and its
//! local-socket parsing is unreachable from the command line — a hostname is
//! rejected with "no usable addresses for host", and a socket path as the host
//! hangs in multicast DNS resolution. So the buffer is built here and handed to
//! `mount(2)` directly.
//!
//! # Where the layout comes from
//!
//! `<nfs/nfs.h>` declares the attribute numbers behind `__APPLE_API_PRIVATE`
//! but not the shape of the buffer. The shape below follows the encoder in
//! Apple's own `mount_nfs` and the decoder in the NFS client, which agree on:
//!
//! ```text
//! u32  88                  NFS_ARGSVERSION_XDR
//! u32  <total byte length>
//! u32  0                   NFS_XDRARGS_VERSION_0
//! u32  2                   attribute bitmap word count
//! u32  <bitmap word 0>
//! u32  <bitmap word 1>
//! u32  <byte length of everything after this field>
//! ...  attribute values, ascending by attribute number
//! ```
//!
//! Every word is big-endian, and every variable-length field is an XDR
//! `opaque<>`: a length word followed by the bytes, zero-padded to a multiple
//! of four. [`super::xdr::Writer`] already encodes exactly that, so the wire
//! layer's encoder is reused rather than a second one written.
//!
//! Two details are not deducible from the header. `NFS_MATTR_FLAGS` carries
//! *two* bitmaps, a mask and a value, so 64 bits of flags where the attributes
//! around it are 32. And `NFS_MATTR_FS_LOCATIONS` has to carry the socket path
//! as its address even though `NFS_MATTR_LOCAL_NFS_PORT` carries it too;
//! setting the attribute alone times out with no connection attempt.
//!
//! # Failure is loud
//!
//! A wrong buffer is rejected rather than partly honored. A length read in the
//! wrong byte order returns `ENOMEM`, an over-long buffer returns `E2BIG`, and
//! a misaligned attribute stream returns `ENOMEM` when the kernel reads a
//! string length out of the wrong place. That is what makes falling back to
//! the TCP path safe: there is no silent partial mount to fall back *from*.
//! It is not a guarantee, which is why `mount` verifies the root through the
//! mountpoint afterwards rather than trusting the return code.

use std::time::Duration;

use super::xdr::Writer;

/// `NFS_ARGSVERSION_XDR`: the argument buffer is in XDR form.
const ARGSVERSION_XDR: u32 = 88;
/// `NFS_XDRARGS_VERSION_0`.
const XDRARGS_VERSION_0: u32 = 0;
/// `NFS_MATTR_BITMAP_LEN`.
const MATTR_BITMAP_LEN: u32 = 2;
/// `NFS_MFLAG_BITMAP_LEN`.
const MFLAG_BITMAP_LEN: u32 = 1;

/// Bytes before the first attribute value: seven `u32` fields.
const HEADER_LEN: usize = 7 * 4;

/// Attribute numbers from `<nfs/nfs.h>`, for the attributes this buffer sets.
mod mattr {
    pub(super) const FLAGS: u32 = 0;
    pub(super) const NFS_VERSION: u32 = 1;
    pub(super) const SOCKET_TYPE: u32 = 14;
    pub(super) const REQUEST_TIMEOUT: u32 = 17;
    pub(super) const SOFT_RETRY_COUNT: u32 = 18;
    pub(super) const FH: u32 = 20;
    pub(super) const FS_LOCATIONS: u32 = 21;
    pub(super) const MNTFLAGS: u32 = 22;
    pub(super) const MNTFROM: u32 = 23;
    pub(super) const LOCAL_NFS_PORT: u32 = 29;
}

/// `NFS_MFLAG_*` bit positions, for the flags this buffer decides.
mod mflag {
    pub(super) const SOFT: u32 = 0;
    pub(super) const RESVPORT: u32 = 2;
    pub(super) const CALLUMNT: u32 = 5;
}

/// `MNT_RDONLY` from `<sys/mount.h>`.
pub(super) const MNT_RDONLY: u32 = 0x0000_0001;

/// Netid for a connection-oriented, ordered `AF_UNIX` transport.
const SOCKET_TYPE_LOCAL: &str = "ticotsord";

/// `sun_path` on macOS, which bounds the socket path this can carry.
///
/// The kernel rejects a `NFS_MATTR_LOCAL_NFS_PORT` longer than this outright,
/// so the caller has to place its socket somewhere short rather than discover
/// the limit as a mount failure.
pub(super) const SUN_PATH_MAX: usize = 104;

/// What one local-socket mount needs, beyond the constants above.
pub(super) struct LocalMountArgs<'a> {
    /// Absolute path of the listening `AF_UNIX` socket, at most
    /// [`SUN_PATH_MAX`] bytes.
    pub(super) socket_path: &'a str,
    /// Root file handle, handed over directly so the MOUNT protocol never
    /// runs.
    pub(super) root_handle: &'a [u8],
    /// Last path component of the export, which macOS shows as the volume
    /// name. Decorative, and authorizes nothing.
    pub(super) label: &'a str,
    /// How long one RPC waits before being retried.
    pub(super) request_timeout: Duration,
    /// Retransmissions before a soft mount gives up.
    pub(super) soft_retry_count: u32,
}

/// Build the argument buffer for [`LocalMountArgs`].
pub(super) fn encode(args: &LocalMountArgs<'_>) -> Vec<u8> {
    let mut values = Writer::new();

    // NFS_MATTR_FLAGS: mask first, then value, each a one-word bitmap.
    //
    // Soft, so a server that stops answering becomes a bounded
    // `Operation timed out` rather than an unkillable wait. No reserved port,
    // which an unprivileged process could not bind anyway and which means
    // nothing on a Unix socket. No `MOUNTPROC_UMNT` on unmount, because this
    // path never spoke the MOUNT protocol and there is no mount to release.
    let mask = bit(mflag::SOFT) | bit(mflag::RESVPORT) | bit(mflag::CALLUMNT);
    values.write_u32(MFLAG_BITMAP_LEN);
    values.write_u32(mask);
    values.write_u32(MFLAG_BITMAP_LEN);
    values.write_u32(bit(mflag::SOFT));

    // NFS_MATTR_NFS_VERSION
    values.write_u32(3);

    // NFS_MATTR_SOCKET_TYPE
    values.write_opaque_var(SOCKET_TYPE_LOCAL.as_bytes());

    // NFS_MATTR_REQUEST_TIMEOUT, as seconds and nanoseconds.
    values.write_u32(args.request_timeout.as_secs() as u32);
    values.write_u32(args.request_timeout.subsec_nanos());

    // NFS_MATTR_SOFT_RETRY_COUNT
    values.write_u32(args.soft_retry_count);

    // NFS_MATTR_FH
    values.write_opaque_var(args.root_handle);

    // NFS_MATTR_FS_LOCATIONS: one location, one server, one address, one
    // path component. The address repeats the socket path; the trailing zero
    // words are the empty per-server and per-location info blobs, which the
    // kernel reads a length for and skips.
    values.write_u32(1); // location count
    values.write_u32(1); // server count
    values.write_opaque_var(args.socket_path.as_bytes()); // server name
    values.write_u32(1); // address count
    values.write_opaque_var(args.socket_path.as_bytes()); // address
    values.write_u32(0); // empty server info
    values.write_u32(1); // path component count
    values.write_opaque_var(args.label.as_bytes());
    values.write_u32(0); // empty location info

    // NFS_MATTR_MNTFLAGS: one word of VFS `MNT_*` flags, not two. Read-only
    // because the whole crate is.
    values.write_u32(MNT_RDONLY);

    // NFS_MATTR_MNTFROM: what `f_mntfromname` should say. macOS builds the
    // name from the socket path on this transport and ignores the attribute,
    // so nothing may depend on this being honored.
    values.write_opaque_var(mntfrom(args).as_bytes());

    // NFS_MATTR_LOCAL_NFS_PORT. NFS_MATTR_NFS_PORT is deliberately not set
    // alongside it: the kernel rejects the pair with EINVAL rather than
    // choosing one.
    values.write_opaque_var(args.socket_path.as_bytes());

    let values = values.into_bytes();

    let mut out = Writer::new();
    out.write_u32(ARGSVERSION_XDR);
    out.write_u32((HEADER_LEN + values.len()) as u32);
    out.write_u32(XDRARGS_VERSION_0);
    out.write_u32(MATTR_BITMAP_LEN);
    let (word0, word1) = attr_bitmap();
    out.write_u32(word0);
    out.write_u32(word1);
    // Everything after this field, which is the header minus its own last
    // word.
    out.write_u32(values.len() as u32);
    out.write_opaque_fixed(&values);
    out.into_bytes()
}

/// The `f_mntfromname` string, in the `<host>:<path>` shape the mount table
/// uses everywhere else.
pub(super) fn mntfrom(args: &LocalMountArgs<'_>) -> String {
    format!("{}:/{}", args.socket_path, args.label)
}

/// Which attributes the buffer sets, as the two-word `NFS_MATTR_BITMAP_LEN`
/// bitmap. Word 0 holds attributes 0-31, word 1 holds 32-63.
fn attr_bitmap() -> (u32, u32) {
    let set = [
        mattr::FLAGS,
        mattr::NFS_VERSION,
        mattr::SOCKET_TYPE,
        mattr::REQUEST_TIMEOUT,
        mattr::SOFT_RETRY_COUNT,
        mattr::FH,
        mattr::FS_LOCATIONS,
        mattr::MNTFLAGS,
        mattr::MNTFROM,
        mattr::LOCAL_NFS_PORT,
    ];
    let mut words = [0u32; 2];
    for attr in set {
        words[(attr / 32) as usize] |= bit(attr % 32);
    }
    (words[0], words[1])
}

fn bit(n: u32) -> u32 {
    1 << n
}

#[cfg(test)]
#[path = "mount_args_tests.rs"]
mod mount_args_tests;
