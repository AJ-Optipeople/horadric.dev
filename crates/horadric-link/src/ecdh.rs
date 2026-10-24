//! ECDH P-256 in the formats Windows already keeps: a public key is the
//! 65 byte uncompressed point `04 || x || y`, a private key CNG's raw
//! `x || y || d` (an `ECCPRIVATE_BLOB` without its header), so a node's
//! `brothers.json` reads like a PC's. The shared secret is the x
//! coordinate of the shared point, big endian, which is what
//! `portal_push::ecdh` gives once it reverses CNG's little endian.
//!
//! `p256` rather than `ring`: ring makes only ephemeral keys and cannot
//! load one from a file.

use p256::elliptic_curve::sec1::ToSec1Point;
use p256::{PublicKey, SecretKey};

/// A private key as CNG keeps it, `x || y || d`.
pub type Private = [u8; 96];

/// A fresh key pair, as `x || y || d`.
pub fn pair() -> Private {
    loop {
        let mut d = [0u8; 32];
        crate::random(&mut d);
        // A scalar of zero or past the group's order is refused; that is
        // about one draw in 2^32, so drawing again is fine.
        if let Some(private) = from_scalar(&d) {
            return private;
        }
    }
}

/// The key pair of the scalar `d`, `None` when `d` is not a valid one.
pub fn from_scalar(d: &[u8; 32]) -> Option<Private> {
    let secret = SecretKey::from_slice(d).ok()?;
    let point = public_of(&secret);
    let mut private = [0u8; 96];
    private[..64].copy_from_slice(&point[1..]);
    private[64..].copy_from_slice(d);
    Some(private)
}

fn public_of(secret: &SecretKey) -> [u8; 65] {
    let point = secret.public_key().to_uncompressed_point();
    let mut out = [0u8; 65];
    out.copy_from_slice(&point);
    out
}

/// The secret in a stored key, `None` when its scalar is invalid or its
/// `x || y` is not that scalar's point, as CNG refuses to import such a
/// blob.
fn secret(private: &Private) -> Option<SecretKey> {
    let secret = SecretKey::from_slice(&private[64..]).ok()?;
    (public_of(&secret)[1..] == private[..64]).then_some(secret)
}

/// The public key of a stored private key, `04 || x || y`.
pub fn public(private: &Private) -> [u8; 65] {
    let mut point = [0u8; 65];
    point[0] = 4;
    point[1..].copy_from_slice(&private[..64]);
    point
}

/// The ECDH shared secret: the x coordinate of the shared point, big
/// endian. `peer` is a 65 byte uncompressed point.
pub fn agree(private: &Private, peer: &[u8]) -> Result<[u8; 32], String> {
    if peer.len() != 65 || peer[0] != 4 {
        return Err("a peer key is a 65 byte uncompressed point".into());
    }
    let theirs =
        PublicKey::from_sec1_bytes(peer).map_err(|_| "the peer key is not on P-256".to_string())?;
    let mine = secret(private).ok_or("the private key is not a P-256 key pair")?;
    let shared = p256::ecdh::diffie_hellman(mine.to_nonzero_scalar(), theirs.as_affine());
    let mut out = [0u8; 32];
    out.copy_from_slice(shared.raw_secret_bytes());
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use horadric_core::portal::{b64url, unb64url};

    /// RFC 8291, Appendix A, as `portal_push`'s test uses it: the
    /// application server's key pair and the user agent's public key.
    const AS_PUBLIC: &str =
        "BP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A8";
    const AS_PRIVATE: &str = "yfWPiYE-n46HLnH0KqZOF1fJJU3MYrct3AELtAQ-oRw";
    const UA_PUBLIC: &str =
        "BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4";

    fn as_private() -> Private {
        let d: [u8; 32] = unb64url(AS_PRIVATE).unwrap().try_into().unwrap();
        from_scalar(&d).unwrap()
    }

    #[test]
    fn the_rfc_scalar_makes_the_rfc_public_key() {
        assert_eq!(b64url(&public(&as_private())), AS_PUBLIC);
    }

    #[test]
    fn agreement_matches_the_rfc() {
        let secret = agree(&as_private(), &unb64url(UA_PUBLIC).unwrap()).unwrap();
        assert_eq!(
            b64url(&secret),
            "kyrL1jIIOHEzg3sM2ZWRHDRB62YACZhhSlknJ672kSs"
        );
    }

    #[test]
    fn two_fresh_pairs_agree_both_ways() {
        let (a, b) = (pair(), pair());
        assert_ne!(a, b);
        let ab = agree(&a, &public(&b)).unwrap();
        assert_eq!(ab, agree(&b, &public(&a)).unwrap());
        assert_ne!(ab, agree(&a, &public(&pair())).unwrap());
    }

    #[test]
    fn bad_keys_are_refused() {
        let me = pair();
        let peer = public(&pair());
        assert!(agree(&me, &peer[1..]).is_err());
        let mut compressed = peer;
        compressed[0] = 2;
        assert!(agree(&me, &compressed).is_err());
        let mut off_curve = peer;
        off_curve[64] ^= 1;
        assert!(agree(&me, &off_curve).is_err());
        let mut mismatched = me;
        mismatched[0] ^= 1;
        assert!(agree(&mismatched, &peer).is_err());
        assert_eq!(from_scalar(&[0; 32]), None);
        assert_eq!(from_scalar(&[0xff; 32]), None);
    }
}
