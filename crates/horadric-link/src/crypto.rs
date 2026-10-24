//! Randomness and AES-GCM through `ring`, laid out as
//! `horadric-ui/src/portal_crypto.rs` lays them out through CNG and as
//! WebCrypto makes them: a sealed payload is 12 bytes of nonce, then the
//! ciphertext, then its 16 byte tag.

use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_128_GCM, AES_256_GCM};
use ring::rand::{SecureRandom, SystemRandom};

pub const NONCE: usize = 12;
pub const TAG: usize = 16;

/// Bytes from the system's random generator (`getrandom` on Linux).
pub fn random(buf: &mut [u8]) {
    SystemRandom::new()
        .fill(buf)
        .expect("the system's random generator failed");
}

/// AES-128-GCM for a 16 byte key and AES-256-GCM for a 32 byte one, as
/// CNG picks by the key's length. Web Push uses the first.
fn key(key: &[u8]) -> Option<LessSafeKey> {
    let alg = match key.len() {
        16 => &AES_128_GCM,
        32 => &AES_256_GCM,
        _ => return None,
    };
    Some(LessSafeKey::new(UnboundKey::new(alg, key).ok()?))
}

/// Encrypts with a fresh random nonce.
pub fn seal(key: &[u8; 32], plain: &[u8]) -> Vec<u8> {
    seal_bound(key, plain, &[])
}

/// Encrypts with a fresh random nonce, bound to `aad` so that what is
/// sealed for one purpose cannot be passed off as another.
pub fn seal_bound(key: &[u8; 32], plain: &[u8], aad: &[u8]) -> Vec<u8> {
    let mut nonce = [0u8; NONCE];
    random(&mut nonce);
    seal_with(key, &nonce, plain, aad)
}

fn seal_with(key: &[u8; 32], nonce: &[u8; NONCE], plain: &[u8], aad: &[u8]) -> Vec<u8> {
    let mut sealed = nonce.to_vec();
    sealed.extend_from_slice(&gcm_bound(key, nonce, plain, aad));
    sealed
}

/// AES-GCM under a 16 or 32 byte key, giving the ciphertext with its tag
/// and no nonce, which the other side works out for itself. Panics on any
/// other key length, as the CNG one does.
pub fn gcm(key: &[u8], nonce: &[u8; NONCE], plain: &[u8]) -> Vec<u8> {
    gcm_bound(key, nonce, plain, &[])
}

fn gcm_bound(k: &[u8], nonce: &[u8; NONCE], plain: &[u8], aad: &[u8]) -> Vec<u8> {
    let k = key(k).expect("an AES key is 16 or 32 bytes");
    let mut out = plain.to_vec();
    k.seal_in_place_append_tag(
        Nonce::assume_unique_for_key(*nonce),
        Aad::from(aad),
        &mut out,
    )
    .expect("a message this size seals");
    out
}

/// Decrypts, `None` when the frame is short, forged or under another key.
pub fn open(key: &[u8], sealed: &[u8]) -> Option<Vec<u8>> {
    open_bound(key, sealed, &[])
}

/// Decrypts what [`seal_bound`] sealed, `None` as [`open`] is, and when
/// `aad` is not what it was sealed with.
pub fn open_bound(k: &[u8], sealed: &[u8], aad: &[u8]) -> Option<Vec<u8>> {
    if sealed.len() < NONCE + TAG {
        return None;
    }
    let (nonce, rest) = sealed.split_at(NONCE);
    let nonce = Nonce::try_assume_unique_for_key(nonce).ok()?;
    let mut buf = rest.to_vec();
    let plain = key(k)?
        .open_in_place(nonce, Aad::from(aad), &mut buf)
        .ok()?;
    let len = plain.len();
    buf.truncate(len);
    Some(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unhex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    /// The vector `portal_crypto` is tested with: Node's
    /// `createCipheriv('aes-256-gcm')`, key all 1s, nonce all 2s.
    const NODE: &str = "0202020202020202020202027cf4bd6b7075a998bfa0d3ea7085b4909884ac59399b20a56716caecab58dc8a0a00cd";

    #[test]
    fn opens_what_node_sealed() {
        let plain = open(&[1; 32], &unhex(NODE)).unwrap();
        assert_eq!(plain, br#"{"t":"hello","v":1}"#);
    }

    #[test]
    fn seals_what_node_would() {
        let sealed = seal_with(&[1; 32], &[2; 12], br#"{"t":"hello","v":1}"#, &[]);
        assert_eq!(sealed, unhex(NODE));
        assert_eq!(
            gcm(&[1; 32], &[2; 12], br#"{"t":"hello","v":1}"#),
            unhex(NODE)[NONCE..]
        );
    }

    /// NIST's GCM test case 3, AES-128, the length Web Push uses.
    #[test]
    fn aes_128_follows_the_key_length() {
        let key = unhex("feffe9928665731c6d6a8f9467308308");
        let nonce: [u8; 12] = unhex("cafebabefacedbaddecaf888").try_into().unwrap();
        let plain = unhex(
            "d9313225f88406e5a55909c5aff5269a86a7a9531534f7da2e4c303d8a318a72\
             1c3c0c95956809532fcf0e2449a6b525b16aedf5aa0de657ba637b391aafd255",
        );
        let out = gcm(&key, &nonce, &plain);
        assert_eq!(
            out,
            unhex(
                "42831ec2217774244b7221b784d0d49ce3aa212f2c02a4e035c17e2329aca12e\
                 21d514b25466931c7d8f6a5aac84aa051ba30b396a0aac973d58e091473f5985\
                 4d5c2af327cd64a62cf35abd2ba6fab4"
            )
        );
        let mut sealed = nonce.to_vec();
        sealed.extend_from_slice(&out);
        assert_eq!(open(&key, &sealed).unwrap(), plain);
    }

    #[test]
    fn round_trips_and_refuses_forgeries() {
        let key = [9; 32];
        let sealed = seal(&key, b"some terminal text");
        assert_eq!(open(&key, &sealed).unwrap(), b"some terminal text");
        assert_eq!(open(&[8; 32], &sealed), None);
        let mut bent = sealed.clone();
        bent[14] ^= 1;
        assert_eq!(open(&key, &bent), None);
        let mut tag = sealed.clone();
        *tag.last_mut().unwrap() ^= 1;
        assert_eq!(open(&key, &tag), None);
        assert_eq!(open(&key, &sealed[..20]), None);
        assert_eq!(open(&key, &sealed[..NONCE + TAG - 1]), None);
        assert_eq!(open(&key, &seal(&key, b"")).unwrap(), b"");
        assert_eq!(open(&[1; 7], &sealed), None);
    }

    #[test]
    fn bound_frames_open_only_with_their_data() {
        let key = [3; 32];
        let sealed = seal_bound(&key, b"a month of memory", b"memory/2026-10.jsonl");
        assert_eq!(
            open_bound(&key, &sealed, b"memory/2026-10.jsonl").unwrap(),
            b"a month of memory"
        );
        assert_eq!(open_bound(&key, &sealed, b"memory/2026-09.jsonl"), None);
        assert_eq!(open(&key, &sealed), None);
    }

    #[test]
    fn nonces_are_fresh() {
        let (a, b) = (seal(&[5; 32], b"x"), seal(&[5; 32], b"x"));
        assert_ne!(a[..NONCE], b[..NONCE]);
        let mut buf = [0u8; 64];
        random(&mut buf);
        assert_ne!(buf, [0u8; 64]);
    }
}
