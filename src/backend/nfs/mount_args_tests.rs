#![allow(non_snake_case)]
#![allow(clippy::unwrap_used)]
#![allow(clippy::expect_used)]

//! Two kinds of check, because the encoder has no compiler to answer to.
//!
//! `decode` below mirrors the kernel's read order rather than the encoder's
//! write order, so a field written in the wrong place is caught by logic that
//! was not derived from the code under test. The hex snapshot then pins the
//! exact bytes, so a change in the encoding shows up here rather than as a
//! mount failure on a machine nobody is watching.

use super::*;

const SOCKET: &str = "/tmp/.anymount-1/s";
const HANDLE: &[u8] = &[
    0xde, 0xad, 0xbe, 0xef, 0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01,
];

fn sample() -> LocalMountArgs<'static> {
    LocalMountArgs {
        socket_path: SOCKET,
        root_handle: HANDLE,
        label: "demo",
        request_timeout: Duration::from_secs(20),
        soft_retry_count: 2,
    }
}

/// A reader that mirrors the kernel's `xb_get_*` helpers.
struct Xb<'a> {
    buf: &'a [u8],
    at: usize,
}

impl<'a> Xb<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, at: 0 }
    }

    fn u32(&mut self) -> u32 {
        let v = u32::from_be_bytes(self.buf[self.at..self.at + 4].try_into().unwrap());
        self.at += 4;
        v
    }

    /// Length word, then that many bytes rounded up to a word — `xb_get_bytes`
    /// followed by the padding skip.
    fn opaque(&mut self) -> Vec<u8> {
        let len = self.u32() as usize;
        let out = self.buf[self.at..self.at + len].to_vec();
        self.at += len.div_ceil(4) * 4;
        out
    }

    fn string(&mut self) -> String {
        String::from_utf8(self.opaque()).expect("utf-8")
    }

    fn bitmap(&mut self) -> Vec<u32> {
        let len = self.u32() as usize;
        (0..len).map(|_| self.u32()).collect()
    }
}

/// Every attribute, read back in the order the NFS client reads them.
///
/// One walk, several assertions: a field written in the wrong place shifts
/// everything after it, so the tests below all depend on this parse being
/// exact.
struct Decoded {
    total_len: usize,
    attrs_len: usize,
    consumed: usize,
    bitmap: [u32; 2],
    flag_mask: Vec<u32>,
    flag_value: Vec<u32>,
    nfs_version: u32,
    socket_type: String,
    timeout_sec: u32,
    timeout_nsec: u32,
    soft_retry_count: u32,
    root_handle: Vec<u8>,
    location_count: u32,
    server_count: u32,
    server_name: String,
    address_count: u32,
    address: String,
    server_info: u32,
    component_count: u32,
    component: String,
    location_info: u32,
    mntflags: u32,
    mntfrom: String,
    local_nfs_port: String,
}

impl Decoded {
    fn is_set(&self, attr: u32) -> bool {
        self.bitmap[(attr / 32) as usize] & (1 << (attr % 32)) != 0
    }
}

fn decode(buf: &[u8]) -> Decoded {
    let mut xb = Xb::new(buf);
    let version = xb.u32();
    assert_eq!(version, 88, "NFS_ARGSVERSION_XDR");
    let total_len = xb.u32() as usize;
    assert_eq!(xb.u32(), 0, "NFS_XDRARGS_VERSION_0");

    let bitmap = xb.bitmap();
    assert_eq!(bitmap.len(), 2, "NFS_MATTR_BITMAP_LEN");
    let bitmap = [bitmap[0], bitmap[1]];
    let attrs_len = xb.u32() as usize;

    let flag_mask = xb.bitmap();
    let flag_value = xb.bitmap();
    let nfs_version = xb.u32();
    let socket_type = xb.string();
    let timeout_sec = xb.u32();
    let timeout_nsec = xb.u32();
    let soft_retry_count = xb.u32();
    let root_handle = xb.opaque();
    let location_count = xb.u32();
    let server_count = xb.u32();
    let server_name = xb.string();
    let address_count = xb.u32();
    let address = xb.string();
    let server_info = xb.u32();
    let component_count = xb.u32();
    let component = xb.string();
    let location_info = xb.u32();
    let mntflags = xb.u32();
    let mntfrom = xb.string();
    let local_nfs_port = xb.string();

    Decoded {
        total_len,
        attrs_len,
        consumed: xb.at,
        bitmap,
        flag_mask,
        flag_value,
        nfs_version,
        socket_type,
        timeout_sec,
        timeout_nsec,
        soft_retry_count,
        root_handle,
        location_count,
        server_count,
        server_name,
        address_count,
        address,
        server_info,
        component_count,
        component,
        location_info,
        mntflags,
        mntfrom,
        local_nfs_port,
    }
}

#[test]
fn encode____local_socket_mount____declares_its_own_lengths_correctly() {
    let buf = encode(&sample());
    let d = decode(&buf);

    assert_eq!(
        d.total_len,
        buf.len(),
        "args length counts the whole buffer"
    );
    assert_eq!(
        d.attrs_len,
        buf.len() - HEADER_LEN,
        "attrs length counts everything after its own field"
    );
    assert_eq!(d.consumed, buf.len(), "no trailing bytes");
}

/// `NFS_MATTR_FLAGS` is two bitmaps, a mask then a value — 64 bits of flags
/// where the attributes around it are 32. Nothing in `<nfs/nfs.h>` says so.
#[test]
fn encode____local_socket_mount____carries_a_flag_mask_and_a_flag_value() {
    let d = decode(&encode(&sample()));

    assert!(d.is_set(mattr::FLAGS));
    assert_eq!(d.flag_mask.len(), 1);
    assert_eq!(d.flag_value.len(), 1);
    assert_eq!(
        d.flag_mask[0],
        bit(mflag::SOFT) | bit(mflag::RESVPORT) | bit(mflag::CALLUMNT),
        "the mask names every flag this mount decides"
    );
    assert_eq!(
        d.flag_value[0] & bit(mflag::SOFT),
        bit(mflag::SOFT),
        "soft, so a stopped server times out rather than hanging"
    );
    assert_eq!(
        d.flag_value[0] & bit(mflag::RESVPORT),
        0,
        "no reserved port"
    );
    assert_eq!(
        d.flag_value[0] & bit(mflag::CALLUMNT),
        0,
        "no MOUNTPROC_UMNT: this path never spoke the MOUNT protocol"
    );
}

#[test]
fn encode____local_socket_mount____sets_the_transport_and_timing_attributes() {
    let d = decode(&encode(&sample()));

    assert!(d.is_set(mattr::NFS_VERSION));
    assert_eq!(d.nfs_version, 3);
    assert!(d.is_set(mattr::SOCKET_TYPE));
    assert_eq!(d.socket_type, "ticotsord");
    assert!(d.is_set(mattr::REQUEST_TIMEOUT));
    assert_eq!(d.timeout_sec, 20);
    assert_eq!(d.timeout_nsec, 0);
    assert!(d.is_set(mattr::SOFT_RETRY_COUNT));
    assert_eq!(d.soft_retry_count, 2);
    assert!(d.is_set(mattr::LOCAL_NFS_PORT));
    assert_eq!(d.local_nfs_port, SOCKET);
}

#[test]
fn encode____local_socket_mount____carries_the_root_handle_so_mnt_is_never_called() {
    let d = decode(&encode(&sample()));

    assert!(d.is_set(mattr::FH));
    assert_eq!(d.root_handle, HANDLE);
}

/// The socket path has to appear as the file system locations address as well
/// as in `NFS_MATTR_LOCAL_NFS_PORT`; setting the attribute alone times out
/// with no connection attempt.
#[test]
fn encode____local_socket_mount____repeats_the_socket_path_as_the_location_address() {
    let d = decode(&encode(&sample()));

    assert!(d.is_set(mattr::FS_LOCATIONS));
    assert_eq!(d.location_count, 1);
    assert_eq!(d.server_count, 1);
    assert_eq!(d.server_name, SOCKET);
    assert_eq!(d.address_count, 1);
    assert_eq!(d.address, SOCKET);
    assert_eq!(d.server_info, 0, "empty server info");
    assert_eq!(d.component_count, 1);
    assert_eq!(d.component, "demo", "the volume name macOS displays");
    assert_eq!(d.location_info, 0, "empty location info");
}

#[test]
fn encode____local_socket_mount____mounts_read_only_with_one_word_of_mnt_flags() {
    let d = decode(&encode(&sample()));

    assert!(d.is_set(mattr::MNTFLAGS));
    assert_eq!(d.mntflags, MNT_RDONLY, "one word of MNT_* flags, not two");
    assert!(d.is_set(mattr::MNTFROM));
    assert_eq!(d.mntfrom, format!("{SOCKET}:/demo"));
}

/// `NFS_MATTR_NFS_PORT` alongside `NFS_MATTR_LOCAL_NFS_PORT` is `EINVAL`: the
/// kernel refuses the pair rather than choosing one.
#[test]
fn encode____local_socket_mount____does_not_set_the_tcp_port_attributes() {
    let d = decode(&encode(&sample()));

    const NFS_PORT: u32 = 15;
    const MOUNT_PORT: u32 = 16;
    const LOCAL_MOUNT_PORT: u32 = 30;
    assert!(!d.is_set(NFS_PORT));
    assert!(!d.is_set(MOUNT_PORT));
    assert!(
        !d.is_set(LOCAL_MOUNT_PORT),
        "the MOUNT protocol is not served on this path"
    );
}

/// The bytes themselves, so a change in the encoding is visible here rather
/// than only as a mount failure. Regenerate deliberately, never to make a
/// red test green.
#[test]
fn encode____the_sample_mount____produces_exactly_these_bytes() {
    let buf = encode(&sample());
    let hex: String = buf.iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(hex, SAMPLE_HEX);
}

const SAMPLE_HEX: &str = concat!(
    "00000058000000f0000000000000000220f6400300000000000000d40000000100000025",
    "000000010000000100000003000000097469636f74736f72640000000000001400000000",
    "0000000200000018deadbeef00112233445566778899aabb000000000000000100000001",
    "00000001000000122f746d702f2e616e796d6f756e742d312f7300000000000100000012",
    "2f746d702f2e616e796d6f756e742d312f73000000000000000000010000000464656d6f",
    "0000000000000001000000182f746d702f2e616e796d6f756e742d312f733a2f64656d6f",
    "000000122f746d702f2e616e796d6f756e742d312f730000",
);

#[test]
fn encode____any_socket_path_and_label____stays_word_aligned_and_self_describing() {
    use proptest::prelude::*;

    proptest!(|(path in "[a-z/.]{1,40}", label in "[a-zA-Z0-9 ._-]{0,30}", secs in 1u64..120, retries in 0u32..10)| {
        let args = LocalMountArgs {
            socket_path: &path,
            root_handle: HANDLE,
            label: &label,
            request_timeout: Duration::from_secs(secs),
            soft_retry_count: retries,
        };
        let buf = encode(&args);

        prop_assert_eq!(buf.len() % 4, 0, "the buffer is a whole number of XDR words");
        let declared = u32::from_be_bytes(buf[4..8].try_into().unwrap()) as usize;
        prop_assert_eq!(declared, buf.len(), "args length matches reality");
        let attrs = u32::from_be_bytes(buf[24..28].try_into().unwrap()) as usize;
        prop_assert_eq!(attrs, buf.len() - HEADER_LEN, "attrs length matches reality");
    });
}
