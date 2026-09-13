//! NFSv3 `fhandle3` encode/decode, and the separate public value the MOUNT
//! protocol checks.
//!
//! Two independent random values, not one. [`FileHandle3`]'s secret prefixes
//! every file handle and is never written anywhere a client could read it.
//! [`ExportSecret`] is the value that goes into the `MNT` export path, which
//! `mount_nfs` records in the system mount table and `nfsstat -m` reprints, so
//! it is public from the moment the mount succeeds. Deriving one from the
//! other would let anyone reading the mount table compute the handle secret,
//! which is the property the split buys.
//!
//! The local-socket path uses neither the export path nor `MNT`, and so needs
//! no [`ExportSecret`] at all.

use crate::types::Ino;

pub(super) const ENCODED_LEN: usize = 24;

/// A per-mount `cookieverf3`.
///
/// Constant rather than derived from either secret. Content is immutable for
/// a mount's life, so there is no verifier-mismatch case to detect, and the
/// verifier needs neither secrecy nor unpredictability. It used to be the
/// first eight bytes of the one secret, which `nfs_proto.rs` writes into every
/// `READDIR3` and `READDIRPLUS3` reply — that would publish half the handle
/// secret to anyone able to read a listing.
pub(super) const COOKIEVERF: [u8; 8] = [0; 8];

/// Compare two byte strings without an early exit.
///
/// Both callers compare a client-supplied value against a secret, where the
/// position of the first differing byte is exactly what a timing attack reads.
/// `black_box` keeps the accumulator opaque so the loop is not rewritten into
/// a short-circuiting `memcmp`.
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    std::hint::black_box(diff) == 0
}

/// Draw `N` bytes from the OS's random source.
///
/// macOS only, because that is the only platform this backend mounts on —
/// deliberately not given a portable fallback, since a weaker source of
/// randomness compiled in "just for symmetry" is exactly the kind of thing
/// that later gets used for real.
#[cfg(target_os = "macos")]
pub(super) fn random_bytes<const N: usize>() -> [u8; N] {
    let mut out = [0u8; N];
    // SAFETY: arc4random_buf writes exactly `out.len()` bytes into a buffer of
    // that size that this call owns exclusively; it cannot fail and needs no
    // seeding.
    unsafe { libc::arc4random_buf(out.as_mut_ptr().cast(), out.len()) };
    out
}

/// The private per-mount value that prefixes every file handle.
pub(super) struct FileHandle3 {
    secret: [u8; 16],
}

impl FileHandle3 {
    /// Wrap an already-chosen secret. The codec is portable, so this is what
    /// the tests use; production always goes through `new_random`.
    pub(super) fn from_secret(secret: [u8; 16]) -> Self {
        Self { secret }
    }

    /// A deterministic secret for tests, so the codec can be exercised on any
    /// Unix rather than only where `new_random` compiles. Distinct `seed`s
    /// give distinct secrets, which is what the wrong-secret cases need.
    #[cfg(test)]
    pub(super) fn for_test(seed: u64) -> Self {
        let mut secret = [0u8; 16];
        secret[..8].copy_from_slice(&seed.to_be_bytes());
        secret[8..].copy_from_slice(&(!seed).to_be_bytes());
        Self::from_secret(secret)
    }

    /// Draw a fresh per-mount handle secret from the OS.
    #[cfg(target_os = "macos")]
    pub(super) fn new_random() -> Self {
        Self::from_secret(random_bytes())
    }

    pub(super) fn encode(&self, ino: Ino) -> [u8; ENCODED_LEN] {
        let mut out = [0u8; ENCODED_LEN];
        out[..16].copy_from_slice(&self.secret);
        out[16..].copy_from_slice(&ino.0.to_be_bytes());
        out
    }

    /// `None` on wrong length or secret mismatch — both map to
    /// `NFS3ERR_NOENT` / `MNT3ERR_ACCES` by the caller, never a panic.
    pub(super) fn resolve(&self, bytes: &[u8]) -> Option<Ino> {
        if bytes.len() != ENCODED_LEN || !ct_eq(&bytes[..16], &self.secret) {
            return None;
        }
        Some(Ino(u64::from_be_bytes(bytes[16..24].try_into().ok()?)))
    }

    /// A constant per-mount `cookieverf3`; see [`COOKIEVERF`].
    pub(super) fn cookieverf(&self) -> [u8; 8] {
        COOKIEVERF
    }
}

/// The public per-mount value carried in the `MNT` export path.
///
/// Reaching the mount table is what this value is for, so it is not a secret
/// once the mount is established. It is still drawn at random and still
/// checked: it has to be unguessable during the window between the server
/// starting and `mount_nfs` exiting, which is the only window in which `MNT`
/// is answered at all.
#[cfg(feature = "nfs-tcp")]
pub(super) struct ExportSecret {
    hex: String,
}

#[cfg(feature = "nfs-tcp")]
impl ExportSecret {
    /// Wrap an already-chosen value, rendered as 32 lowercase hex characters.
    pub(super) fn from_secret(secret: [u8; 16]) -> Self {
        Self {
            hex: secret.iter().map(|b| format!("{b:02x}")).collect(),
        }
    }

    /// A deterministic value for tests, independent of [`FileHandle3`]'s.
    #[cfg(test)]
    pub(super) fn for_test(seed: u64) -> Self {
        let mut secret = [0u8; 16];
        secret[..8].copy_from_slice(&seed.rotate_left(17).to_le_bytes());
        secret[8..].copy_from_slice(&(!seed).rotate_left(29).to_le_bytes());
        Self::from_secret(secret)
    }

    /// Draw a fresh per-mount export value from the OS, independently of the
    /// handle secret.
    #[cfg(target_os = "macos")]
    pub(super) fn new_random() -> Self {
        Self::from_secret(random_bytes())
    }

    /// Lowercase hex rendering, used as the first segment of the `MNT` export
    /// path (`/export/<hex>/<label>`) and on the `mount_nfs` command line.
    pub(super) fn hex(&self) -> &str {
        &self.hex
    }

    /// Whether a client-supplied path segment is this value.
    pub(super) fn matches(&self, candidate: &str) -> bool {
        ct_eq(candidate.as_bytes(), self.hex.as_bytes())
    }
}

#[cfg(test)]
#[path = "handle_tests.rs"]
mod handle_tests;
