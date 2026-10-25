//! ECDSA P-256 over SHA-256 with the same key pair ECDH uses, in the
//! formats Windows keeps: a private key `x || y || d`, a public key the
//! 65 byte uncompressed point, a signature `r || s`, 32 bytes each, big
//! endian, which is what CNG's `BCryptSignHash` gives. What a brother signs
//! is its word about the brotherhood (see `horadric_core::said`), so a
//! third brother can believe it however it came.

use p256::ecdsa::signature::{Signer, Verifier};
use p256::ecdsa::{Signature, SigningKey, VerifyingKey};

use crate::ecdh::Private;

/// `msg` signed with `private`, `None` when it is not a P-256 key pair.
pub fn sign(private: &Private, msg: &[u8]) -> Option<[u8; 64]> {
    let d: [u8; 32] = private[64..].try_into().ok()?;
    let key = SigningKey::from_slice(&d).ok()?;
    // A stored pair whose point is not its scalar's is refused, as CNG
    // refuses to import such a blob.
    let point = key.verifying_key().to_sec1_point(false);
    if point.as_bytes()[1..] != private[..64] {
        return None;
    }
    let sig: Signature = key.sign(msg);
    sig.to_bytes().as_slice().try_into().ok()
}

/// Whether `sig` is `public`'s signature of `msg`. A key that is not a
/// point on the curve, or a signature of the wrong length, is not.
pub fn verify(public: &[u8], msg: &[u8], sig: &[u8]) -> bool {
    if public.len() != 65 || public[0] != 4 || sig.len() != 64 {
        return false;
    }
    let (Ok(key), Ok(sig)) = (
        VerifyingKey::from_sec1_bytes(public),
        Signature::from_slice(sig),
    ) else {
        return false;
    };
    key.verify(msg, &sig).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ecdh;

    #[test]
    fn a_signature_verifies_under_its_key_alone() {
        let a = ecdh::pair();
        let b = ecdh::pair();
        let sig = sign(&a, b"word").unwrap();
        assert!(verify(&ecdh::public(&a), b"word", &sig));
        assert!(!verify(&ecdh::public(&a), b"other word", &sig));
        assert!(!verify(&ecdh::public(&b), b"word", &sig));
        let mut bent = sig;
        bent[10] ^= 1;
        assert!(!verify(&ecdh::public(&a), b"word", &bent));
        assert!(!verify(&ecdh::public(&a), b"word", &sig[..63]));
        assert!(!verify(&[4u8; 65], b"word", &sig));
    }

    #[test]
    fn a_pair_whose_point_is_not_its_scalars_signs_nothing() {
        let mut a = ecdh::pair();
        a[5] ^= 1;
        assert!(sign(&a, b"word").is_none());
    }
}
