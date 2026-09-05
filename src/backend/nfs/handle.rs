//! NFSv3 `fhandle3` encode/decode: a per-mount random secret plus the raw
//! [`Ino`], so a client cannot forge a working handle without having first
//! received one from this server.

use crate::types::Ino;

pub(super) const ENCODED_LEN: usize = 24;

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

    /// Draw a fresh per-mount secret from the OS.
    ///
    /// macOS only, because that is the only platform this backend mounts on —
    /// deliberately not given a portable fallback, since a weaker source of
    /// randomness compiled in "just for symmetry" is exactly the kind of thing
    /// that later gets used for real.
    #[cfg(target_os = "macos")]
    pub(super) fn new_random() -> Self {
        let mut secret = [0u8; 16];
        // SAFETY: arc4random_buf writes exactly `secret.len()` bytes into a
        // buffer of that size that this call owns exclusively; it cannot
        // fail and needs no seeding.
        unsafe { libc::arc4random_buf(secret.as_mut_ptr().cast(), secret.len()) };
        Self::from_secret(secret)
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
        if bytes.len() != ENCODED_LEN || bytes[..16] != self.secret {
            return None;
        }
        Some(Ino(u64::from_be_bytes(bytes[16..24].try_into().ok()?)))
    }

    /// Lowercase hex rendering of the secret, used in the `MNT` export path
    /// (`/export/<hex>`) and the `mount_nfs` command line.
    pub(super) fn secret_hex(&self) -> String {
        self.secret.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// A constant per-mount `cookieverf3`: content is immutable for the
    /// mount's life, so there is no verifier-mismatch case to handle.
    pub(super) fn cookieverf(&self) -> [u8; 8] {
        self.secret[..8].try_into().unwrap_or([0; 8])
    }
}

#[cfg(test)]
#[path = "handle_tests.rs"]
mod handle_tests;
