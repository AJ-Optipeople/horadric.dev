//! Town Portal's end to end encryption: AES-256-GCM through Windows CNG,
//! so the relay carries frames it cannot read. A payload is 12 bytes of
//! nonce, then the ciphertext with its 16 byte tag, as WebCrypto makes it.
//! Web Push's AES-128-GCM goes through here too.

use windows::Win32::Security::Cryptography::{
    BCryptDecrypt, BCryptDestroyKey, BCryptEncrypt, BCryptGenRandom, BCryptGenerateSymmetricKey,
    BCRYPT_AES_GCM_ALG_HANDLE, BCRYPT_AUTHENTICATED_CIPHER_MODE_INFO, BCRYPT_FLAGS,
    BCRYPT_KEY_HANDLE, BCRYPT_USE_SYSTEM_PREFERRED_RNG,
};

const NONCE: usize = 12;
const TAG: usize = 16;

/// Bytes from the system's random generator.
pub fn random(buf: &mut [u8]) {
    let status = unsafe { BCryptGenRandom(None, buf, BCRYPT_USE_SYSTEM_PREFERRED_RNG) };
    assert!(status.is_ok(), "BCryptGenRandom failed: {status:?}");
}

/// A key handle, destroyed when it goes.
struct Key(BCRYPT_KEY_HANDLE);

impl Key {
    fn new(key: &[u8]) -> Option<Key> {
        let mut handle = BCRYPT_KEY_HANDLE::default();
        // The GCM pseudo handle needs no provider opened and closed.
        let status = unsafe {
            BCryptGenerateSymmetricKey(BCRYPT_AES_GCM_ALG_HANDLE, &mut handle, None, key, 0)
        };
        status.is_ok().then_some(Key(handle))
    }
}

impl Drop for Key {
    fn drop(&mut self) {
        unsafe {
            let _ = BCryptDestroyKey(self.0);
        }
    }
}

fn info(nonce: &[u8], tag: &mut [u8; TAG]) -> BCRYPT_AUTHENTICATED_CIPHER_MODE_INFO {
    BCRYPT_AUTHENTICATED_CIPHER_MODE_INFO {
        cbSize: std::mem::size_of::<BCRYPT_AUTHENTICATED_CIPHER_MODE_INFO>() as u32,
        dwInfoVersion: 1,
        pbNonce: nonce.as_ptr() as *mut u8,
        cbNonce: nonce.len() as u32,
        pbTag: tag.as_mut_ptr(),
        cbTag: TAG as u32,
        ..Default::default()
    }
}

/// Encrypts with a fresh random nonce.
pub fn seal(key: &[u8; 32], plain: &[u8]) -> Vec<u8> {
    let mut nonce = [0u8; NONCE];
    random(&mut nonce);
    seal_with(key, &nonce, plain)
}

fn seal_with(key: &[u8; 32], nonce: &[u8; NONCE], plain: &[u8]) -> Vec<u8> {
    let mut sealed = nonce.to_vec();
    sealed.extend_from_slice(&gcm(key, nonce, plain));
    sealed
}

/// AES-GCM under a 16 or 32 byte key, giving the ciphertext with its tag
/// and no nonce, which the other side works out for itself.
pub fn gcm(key: &[u8], nonce: &[u8; NONCE], plain: &[u8]) -> Vec<u8> {
    let k = Key::new(key).expect("AES key");
    let mut tag = [0u8; TAG];
    let info = info(nonce, &mut tag);
    let mut out = vec![0u8; plain.len()];
    let mut written = 0u32;
    let status = unsafe {
        BCryptEncrypt(
            k.0,
            Some(plain),
            Some(&info as *const _ as *const _),
            None,
            Some(&mut out),
            &mut written,
            BCRYPT_FLAGS(0),
        )
    };
    assert!(status.is_ok(), "BCryptEncrypt failed: {status:?}");
    out.truncate(written as usize);
    out.extend_from_slice(&tag);
    out
}

/// Decrypts, `None` when the frame is short, forged or under another key.
pub fn open(key: &[u8], sealed: &[u8]) -> Option<Vec<u8>> {
    if sealed.len() < NONCE + TAG {
        return None;
    }
    let (nonce, rest) = sealed.split_at(NONCE);
    let (cipher, tag) = rest.split_at(rest.len() - TAG);
    let mut tag: [u8; TAG] = tag.try_into().ok()?;
    let k = Key::new(key)?;
    let info = info(nonce, &mut tag);
    let mut out = vec![0u8; cipher.len()];
    let mut written = 0u32;
    let status = unsafe {
        BCryptDecrypt(
            k.0,
            Some(cipher),
            Some(&info as *const _ as *const _),
            None,
            Some(&mut out),
            &mut written,
            BCRYPT_FLAGS(0),
        )
    };
    status.is_ok().then(|| {
        out.truncate(written as usize);
        out
    })
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

    /// Made by Node's `createCipheriv('aes-256-gcm')`, key all 1s, nonce
    /// all 2s, so the browser's WebCrypto agrees with us.
    const NODE: &str = "0202020202020202020202027cf4bd6b7075a998bfa0d3ea7085b4909884ac59399b20a56716caecab58dc8a0a00cd";

    #[test]
    fn opens_what_node_sealed() {
        let plain = open(&[1; 32], &unhex(NODE)).unwrap();
        assert_eq!(plain, br#"{"t":"hello","v":1}"#);
    }

    #[test]
    fn seals_what_node_would() {
        let sealed = seal_with(&[1; 32], &[2; 12], br#"{"t":"hello","v":1}"#);
        assert_eq!(sealed, unhex(NODE));
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
        assert_eq!(open(&key, &sealed[..20]), None);
        assert_eq!(open(&key, &seal(&key, b"")).unwrap(), b"");
    }
}
