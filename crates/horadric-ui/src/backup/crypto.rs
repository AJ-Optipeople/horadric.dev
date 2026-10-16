//! The backup's key and hashes, through CNG like the sealing itself
//! ([`crate::portal_crypto`]): no crypto crate to ship.

use windows::Win32::Security::Cryptography::{
    BCryptDeriveKeyPBKDF2, BCryptHash, BCRYPT_HMAC_SHA256_ALG_HANDLE, BCRYPT_SHA256_ALG_HANDLE,
};

/// What the plan asks for, and OWASP's figure for PBKDF2-SHA256.
pub const ROUNDS: u32 = 600_000;

/// SHA-256 of `bytes`.
pub fn sha256(bytes: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    let status = unsafe { BCryptHash(BCRYPT_SHA256_ALG_HANDLE, None, bytes, &mut out) };
    assert!(status.is_ok(), "BCryptHash failed: {status:?}");
    out
}

/// The AES key a passphrase and salt make, PBKDF2 with HMAC-SHA256.
pub fn derive(passphrase: &str, salt: &[u8], rounds: u32) -> [u8; 32] {
    let mut key = [0u8; 32];
    let status = unsafe {
        BCryptDeriveKeyPBKDF2(
            BCRYPT_HMAC_SHA256_ALG_HANDLE,
            Some(passphrase.as_bytes()),
            Some(salt),
            rounds as u64,
            &mut key,
            0,
        )
    };
    assert!(status.is_ok(), "BCryptDeriveKeyPBKDF2 failed: {status:?}");
    key
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn unhex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) || !s.is_ascii() {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_of_abc() {
        assert_eq!(
            hex(&sha256(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    /// The PBKDF2-HMAC-SHA256 vectors everyone checks against (RFC 7914's
    /// sibling, from the same test suite as RFC 6070).
    #[test]
    fn pbkdf2_matches_the_known_vectors() {
        assert_eq!(
            hex(&derive("password", b"salt", 1)),
            "120fb6cffcf8b32c43e7225256c4f837a86548c92ccc35480805987cb70be17b"
        );
        assert_eq!(
            hex(&derive("password", b"salt", 4096)),
            "c5e478d59288c841aa530db6845c4c8d962893a001ce4e11a4963873aa98134a"
        );
    }

    #[test]
    fn hex_round_trips_and_refuses_junk() {
        assert_eq!(unhex(&hex(&[0, 1, 254, 255])).unwrap(), [0, 1, 254, 255]);
        assert_eq!(unhex("abc"), None);
        assert_eq!(unhex("zz"), None);
    }
}
