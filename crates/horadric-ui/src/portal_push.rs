//! Town Portal's push: the PC tells paired phones a session waits, by Web
//! Push straight to each phone's push service. No server of ours holds the
//! subscriptions. The push is signed with VAPID (ECDSA P-256 through CNG,
//! as the updater signs) and its body is encrypted to the phone's own key
//! (RFC 8291, ECDH P-256 and AES-128-GCM through CNG), so the push service
//! carries what waits without reading it. The protocol is in
//! `docs/PORTAL.md`.

use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use horadric_core::portal::{
    b64url, push_audience, unb64url, vapid_authorization, vapid_signing_input, Config,
};
pub use horadric_core::portal::{PushSub, Subs};
use horadric_core::release::split_url;
use horadric_core::webpush;
use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::NTSTATUS;
use windows::Win32::Networking::WinHttp::{
    WinHttpCloseHandle, WinHttpConnect, WinHttpOpen, WinHttpOpenRequest, WinHttpQueryHeaders,
    WinHttpReceiveResponse, WinHttpSendRequest, WinHttpSetTimeouts,
    WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_FLAG_SECURE, WINHTTP_QUERY_FLAG_NUMBER,
    WINHTTP_QUERY_STATUS_CODE,
};
use windows::Win32::Security::Cryptography::{
    BCryptDeriveKey, BCryptDestroyKey, BCryptDestroySecret, BCryptExportKey, BCryptFinalizeKeyPair,
    BCryptGenerateKeyPair, BCryptImportKeyPair, BCryptSecretAgreement, BCRYPT_ECCPRIVATE_BLOB,
    BCRYPT_ECCPUBLIC_BLOB, BCRYPT_ECDH_P256_ALG_HANDLE, BCRYPT_ECDH_PRIVATE_P256_MAGIC,
    BCRYPT_ECDH_PUBLIC_P256_MAGIC, BCRYPT_KDF_RAW_SECRET, BCRYPT_KEY_HANDLE, BCRYPT_SECRET_HANDLE,
};

use crate::portal_crypto;
use crate::update::{self, PrivateKey};

/// How long a token holds. Push services refuse more than 24 hours.
const TOKEN_SECS: u64 = 12 * 3600;

/// Gives `cfg` a VAPID key pair when it has none, and saves it to `dir`.
/// True when the config changed. A key that cannot be made leaves the
/// config as it was, and push stays off.
pub fn ensure_vapid(cfg: &mut Config, dir: &Path) -> bool {
    if vapid_key(cfg).is_some() {
        return false;
    }
    let Ok(key) = update::generate() else {
        return false;
    };
    let mut point = vec![4u8];
    point.extend_from_slice(&key.public());
    cfg.vapid_pub = b64url(&point);
    cfg.vapid_d = b64url(&key.0[64..]);
    let _ = cfg.save(dir);
    true
}

/// The private key as CNG wants it, `x || y || d`, from the config's two
/// halves. `None` when either is missing or malformed.
fn vapid_key(cfg: &Config) -> Option<PrivateKey> {
    let point = unb64url(&cfg.vapid_pub)?;
    let d = unb64url(&cfg.vapid_d)?;
    if point.len() != 65 || point[0] != 4 || d.len() != 32 {
        return None;
    }
    let mut raw = point[1..].to_vec();
    raw.extend_from_slice(&d);
    PrivateKey::from_bytes(&raw).ok()
}

/// The `Authorization` header for one endpoint.
fn authorization(cfg: &Config, endpoint: &str, now: u64) -> Option<String> {
    let key = vapid_key(cfg)?;
    let input = vapid_signing_input(&push_audience(endpoint)?, now + TOKEN_SECS);
    let signature = update::sign(&key, input.as_bytes()).ok()?;
    Some(vapid_authorization(&input, &signature, &cfg.vapid_pub))
}

/// Pushes "a session waits" to every paired phone's subscription, on a
/// thread of its own so the UI never waits on a push service. `payload` is
/// the JSON the phone's service worker shows, encrypted for each phone
/// apart. A subscription it cannot be encrypted for gets the empty push
/// older PCs sent. Subscriptions the service says are gone (404, 410) are
/// dropped.
pub fn send_all(dir: &Path, cfg: &Config, payload: Vec<u8>) {
    let subs = Subs(cfg.paired_subs(&Subs::load(dir)));
    if subs.0.is_empty() {
        return;
    }
    let (dir, cfg): (PathBuf, Config) = (dir.to_path_buf(), cfg.clone());
    std::thread::Builder::new()
        .name("portal-push".into())
        .spawn(move || {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_secs());
            let mut gone = Vec::new();
            for sub in &subs.0 {
                let Some(auth) = authorization(&cfg, &sub.endpoint, now) else {
                    continue;
                };
                let body = (!payload.is_empty())
                    .then(|| encrypt(&sub.p256dh, &sub.auth, &payload))
                    .flatten();
                match post(&sub.endpoint, &auth, body.as_deref()) {
                    Ok(404 | 410) => gone.push(sub.endpoint.clone()),
                    Ok(_) => {}
                    Err(e) => {
                        if std::env::var_os("HORADRIC_DEBUG").is_some() {
                            eprintln!("portal push: {e}");
                        }
                    }
                }
            }
            if !gone.is_empty() {
                // Read again: a phone may have subscribed while this ran.
                let mut now = Subs::load(&dir);
                for e in &gone {
                    now.remove(e);
                }
                let _ = now.save(&dir);
            }
        })
        .ok();
}

/// An ECDH P-256 private key as CNG keeps it, `x || y || d`.
pub(crate) type EcdhKey = [u8; 96];

fn check(status: NTSTATUS, what: &str) -> Result<(), String> {
    if status.is_ok() {
        Ok(())
    } else {
        Err(format!("{what} failed: NTSTATUS {:#010x}", status.0))
    }
}

/// A CNG key, destroyed when it goes.
struct EcKey(BCRYPT_KEY_HANDLE);

impl Drop for EcKey {
    fn drop(&mut self) {
        unsafe {
            let _ = BCryptDestroyKey(self.0);
        }
    }
}

fn ecdh_import(magic: u32, kind: PCWSTR, key: &[u8]) -> Result<EcKey, String> {
    let mut blob = magic.to_le_bytes().to_vec();
    blob.extend_from_slice(&32u32.to_le_bytes());
    blob.extend_from_slice(key);
    let mut handle = BCRYPT_KEY_HANDLE::default();
    check(
        unsafe {
            BCryptImportKeyPair(
                BCRYPT_ECDH_P256_ALG_HANDLE,
                None,
                kind,
                &mut handle,
                &blob,
                0,
            )
        },
        "BCryptImportKeyPair",
    )?;
    Ok(EcKey(handle))
}

/// A fresh key pair for one message, so no two pushes share a secret.
/// Brothers keep one as who they are.
pub(crate) fn ecdh_pair() -> Result<EcdhKey, String> {
    let mut handle = BCRYPT_KEY_HANDLE::default();
    check(
        unsafe { BCryptGenerateKeyPair(BCRYPT_ECDH_P256_ALG_HANDLE, &mut handle, 256, 0) },
        "BCryptGenerateKeyPair",
    )?;
    let key = EcKey(handle);
    check(
        unsafe { BCryptFinalizeKeyPair(key.0, 0) },
        "BCryptFinalizeKeyPair",
    )?;
    let mut out = [0u8; 8 + 96];
    let mut len = 0u32;
    check(
        unsafe {
            BCryptExportKey(
                key.0,
                None,
                BCRYPT_ECCPRIVATE_BLOB,
                Some(&mut out),
                &mut len,
                0,
            )
        },
        "BCryptExportKey",
    )?;
    let mut raw = [0u8; 96];
    raw.copy_from_slice(&out[8..]);
    Ok(raw)
}

/// The ECDH shared secret, the x coordinate of the shared point, big
/// endian. `peer` is a 65 byte uncompressed point.
pub(crate) fn ecdh(private: &EcdhKey, peer: &[u8]) -> Result<[u8; 32], String> {
    if peer.len() != 65 || peer[0] != 4 {
        return Err("a peer key is a 65 byte uncompressed point".into());
    }
    let mine = ecdh_import(
        BCRYPT_ECDH_PRIVATE_P256_MAGIC,
        BCRYPT_ECCPRIVATE_BLOB,
        private,
    )?;
    let theirs = ecdh_import(
        BCRYPT_ECDH_PUBLIC_P256_MAGIC,
        BCRYPT_ECCPUBLIC_BLOB,
        &peer[1..],
    )?;
    let mut secret = BCRYPT_SECRET_HANDLE::default();
    check(
        unsafe { BCryptSecretAgreement(mine.0, theirs.0, &mut secret, 0) },
        "BCryptSecretAgreement",
    )?;
    let mut out = [0u8; 32];
    let mut len = 0u32;
    let status = unsafe {
        BCryptDeriveKey(
            secret,
            BCRYPT_KDF_RAW_SECRET,
            None,
            Some(&mut out),
            &mut len,
            0,
        )
    };
    unsafe {
        let _ = BCryptDestroySecret(secret);
    }
    check(status, "BCryptDeriveKey")?;
    // CNG gives the raw secret little endian.
    out.reverse();
    Ok(out)
}

/// The `aes128gcm` body of `plain` for the subscription keys `p256dh` and
/// `auth` (b64url, as the browser gives them). `None` when the keys are
/// malformed or the payload does not fit one record.
pub fn encrypt(p256dh: &str, auth: &str, plain: &[u8]) -> Option<Vec<u8>> {
    let mut salt = [0u8; 16];
    portal_crypto::random(&mut salt);
    let pair = ecdh_pair().ok()?;
    encrypt_with(&unb64url(p256dh)?, &unb64url(auth)?, plain, &pair, &salt)
}

fn encrypt_with(
    ua_public: &[u8],
    auth: &[u8],
    plain: &[u8],
    as_private: &EcdhKey,
    salt: &[u8; 16],
) -> Option<Vec<u8>> {
    if plain.len() > webpush::MAX_PLAIN || auth.len() != 16 {
        return None;
    }
    let secret = ecdh(as_private, ua_public).ok()?;
    let mut as_public = vec![4u8];
    as_public.extend_from_slice(&as_private[..64]);
    let keys = webpush::derive(&secret, auth, ua_public, &as_public, salt);
    let mut body = webpush::header(salt, &as_public);
    body.extend_from_slice(&portal_crypto::gcm(
        &keys.cek,
        &keys.nonce,
        &webpush::pad(plain),
    ));
    Some(body)
}

/// Closes a WinHTTP handle when it goes.
struct Handle(*mut c_void);

impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            let _ = WinHttpCloseHandle(self.0);
        }
    }
}

fn handle(raw: *mut c_void, what: &str) -> Result<Handle, String> {
    if raw.is_null() {
        Err(format!("{what}: {}", windows::core::Error::from_thread()))
    } else {
        Ok(Handle(raw))
    }
}

/// POSTs a push to `endpoint`, encrypted or empty, giving the status code.
pub fn post(endpoint: &str, authorization: &str, body: Option<&[u8]>) -> Result<u32, String> {
    let parts = split_url(endpoint).ok_or_else(|| format!("`{endpoint}` is not a URL"))?;
    if !parts.secure {
        return Err("a push endpoint is HTTPS".into());
    }
    let agent = format!("Horadric/{}", env!("CARGO_PKG_VERSION"));
    let body = body.unwrap_or_default();
    let encoding = if body.is_empty() {
        ""
    } else {
        "Content-Encoding: aes128gcm\r\nContent-Type: application/octet-stream\r\n"
    };
    let headers = format!(
        "TTL: 3600\r\nUrgency: high\r\n{encoding}Content-Length: {}\r\nAuthorization: {authorization}\r\n",
        body.len()
    );
    let headers: Vec<u16> = headers.encode_utf16().collect();
    unsafe {
        let session = handle(
            WinHttpOpen(
                &HSTRING::from(agent),
                WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
                PCWSTR::null(),
                PCWSTR::null(),
                0,
            ),
            "WinHttpOpen",
        )?;
        WinHttpSetTimeouts(session.0, 10_000, 10_000, 15_000, 15_000)
            .map_err(|e| format!("WinHttpSetTimeouts: {e}"))?;
        let connection = handle(
            WinHttpConnect(
                session.0,
                &HSTRING::from(parts.host.as_str()),
                parts.port,
                0,
            ),
            "cannot connect",
        )?;
        let request = handle(
            WinHttpOpenRequest(
                connection.0,
                w!("POST"),
                &HSTRING::from(parts.path.as_str()),
                PCWSTR::null(),
                PCWSTR::null(),
                std::ptr::null(),
                WINHTTP_FLAG_SECURE,
            ),
            "WinHttpOpenRequest",
        )?;
        let total = body.len() as u32;
        let data = (!body.is_empty()).then_some(body.as_ptr() as *const c_void);
        WinHttpSendRequest(request.0, Some(&headers), data, total, total, 0)
            .map_err(|e| format!("cannot reach {}: {e}", parts.host))?;
        WinHttpReceiveResponse(request.0, std::ptr::null_mut())
            .map_err(|e| format!("no answer from {}: {e}", parts.host))?;
        let mut status = 0u32;
        let mut size = std::mem::size_of::<u32>() as u32;
        WinHttpQueryHeaders(
            request.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some(&mut status as *mut u32 as *mut c_void),
            &mut size,
            std::ptr::null_mut(),
        )
        .map_err(|e| format!("no status: {e}"))?;
        Ok(status)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vapid_keys_are_made_once_and_sign() {
        let dir = std::env::temp_dir().join(format!("horadric-vapid-{}", std::process::id()));
        let mut cfg = Config::fresh(&[1; 32]);
        assert!(ensure_vapid(&mut cfg, &dir));
        assert!(!ensure_vapid(&mut cfg, &dir));
        assert_eq!(Config::load(&dir).unwrap().vapid_pub, cfg.vapid_pub);
        let point = unb64url(&cfg.vapid_pub).unwrap();
        assert_eq!((point.len(), point[0]), (65, 4));

        let auth = authorization(&cfg, "https://push.example/abc", 1_000).unwrap();
        let rest = auth.strip_prefix("vapid t=").unwrap();
        let (jwt, k) = rest.split_once(", k=").unwrap();
        assert_eq!(k, cfg.vapid_pub);
        let (input, sig) = jwt.rsplit_once('.').unwrap();
        let sig = unb64url(sig).unwrap();
        assert!(update::verify(&point[1..], input.as_bytes(), &sig));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// RFC 8291, Appendix A: the application server's key pair, the user
    /// agent's public key and auth, the salt, and the body they make.
    const AS_PUBLIC: &str =
        "BP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A8";
    const AS_PRIVATE: &str = "yfWPiYE-n46HLnH0KqZOF1fJJU3MYrct3AELtAQ-oRw";
    const UA_PUBLIC: &str =
        "BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4";
    const AUTH: &str = "BTBZMqHH6r4Tts7J_aSIgg";
    const SALT: &str = "DGv6ra1nlYgDCS1FRnbzlw";
    const BODY: &str = "DGv6ra1nlYgDCS1FRnbzlwAAEABBBP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A_yl95bQpu6cVPTpK4Mqgkf1CXztLVBSt2Ks3oZwbuwXPXLWyouBWLVWGNWQexSgSxsj_Qulcy4a-fN";

    const TEST_PAYLOAD: &str = r#"{"id":"x","project":"test","title":"Allow Bash?","body":"test · a push by hand","waiting":1}"#;

    fn as_private() -> EcdhKey {
        let mut key = [0u8; 96];
        key[..64].copy_from_slice(&unb64url(AS_PUBLIC).unwrap()[1..]);
        key[64..].copy_from_slice(&unb64url(AS_PRIVATE).unwrap());
        key
    }

    #[test]
    fn ecdh_matches_the_rfc() {
        let secret = ecdh(&as_private(), &unb64url(UA_PUBLIC).unwrap()).unwrap();
        assert_eq!(
            b64url(&secret),
            "kyrL1jIIOHEzg3sM2ZWRHDRB62YACZhhSlknJ672kSs"
        );
    }

    #[test]
    fn encrypts_the_rfc_8291_message() {
        let salt: [u8; 16] = unb64url(SALT).unwrap().try_into().unwrap();
        let body = encrypt_with(
            &unb64url(UA_PUBLIC).unwrap(),
            &unb64url(AUTH).unwrap(),
            b"When I grow up, I want to be a watermelon",
            &as_private(),
            &salt,
        )
        .unwrap();
        assert_eq!(b64url(&body), BODY);
    }

    /// Opens a body as the phone's browser would, with the RFC's user
    /// agent key, so a push under a fresh key pair and salt is checked too.
    fn open_as_phone(body: &[u8]) -> Option<Vec<u8>> {
        let ua_public = unb64url(UA_PUBLIC).unwrap();
        let mut ua_private = [0u8; 96];
        ua_private[..64].copy_from_slice(&ua_public[1..]);
        ua_private[64..]
            .copy_from_slice(&unb64url("q1dXpw3UpT5VOmu_cf_v6ih07Aems3njxI-JWgLcM94").unwrap());
        let (salt, rest) = body.split_at(16);
        assert_eq!(rest[..4], webpush::RECORD.to_be_bytes());
        let id_len = rest[4] as usize;
        let (as_public, cipher) = rest[5..].split_at(id_len);
        let secret = ecdh(&ua_private, as_public).unwrap();
        let keys = webpush::derive(
            &secret,
            &unb64url(AUTH).unwrap(),
            &ua_public,
            as_public,
            salt,
        );
        let mut sealed = keys.nonce.to_vec();
        sealed.extend_from_slice(cipher);
        let mut plain = portal_crypto::open(&keys.cek, &sealed)?;
        assert_eq!(plain.pop(), Some(2));
        Some(plain)
    }

    #[test]
    fn a_fresh_push_opens_on_the_phone() {
        let body = encrypt(UA_PUBLIC, AUTH, TEST_PAYLOAD.as_bytes()).unwrap();
        assert_eq!(open_as_phone(&body).unwrap(), TEST_PAYLOAD.as_bytes());
        let mut bent = body.clone();
        *bent.last_mut().unwrap() ^= 1;
        assert_eq!(open_as_phone(&bent), None);
    }

    #[test]
    fn fresh_pushes_differ_and_bad_keys_give_none() {
        let a = encrypt(UA_PUBLIC, AUTH, b"x").unwrap();
        let b = encrypt(UA_PUBLIC, AUTH, b"x").unwrap();
        assert_ne!(a, b);
        assert_eq!(a.len(), 86 + 2 + 16);
        assert!(encrypt("nope", AUTH, b"x").is_none());
        assert!(encrypt(UA_PUBLIC, "AAAA", b"x").is_none());
        assert!(encrypt(UA_PUBLIC, AUTH, &vec![b'a'; webpush::MAX_PLAIN + 1]).is_none());
        assert!(encrypt(UA_PUBLIC, AUTH, TEST_PAYLOAD.as_bytes()).is_some());
    }

    /// Prints a token for the Node check in the commit, by hand:
    /// `cargo test -p horadric-ui vapid_token_for_node -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn vapid_token_for_node() {
        let dir = std::env::temp_dir().join(format!("horadric-vapid-n-{}", std::process::id()));
        let mut cfg = Config::fresh(&[2; 32]);
        ensure_vapid(&mut cfg, &dir);
        println!(
            "AUTH {}",
            authorization(&cfg, "https://push.example/x", 1_000).unwrap()
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Posts to a real push service, by hand: `PUSH_ENDPOINT=https://...
    /// cargo test -p horadric-ui posts_to_a_push_service -- --ignored --nocapture`.
    /// With `PUSH_BAD=1` the signature is spoiled, to see the service check it.
    #[test]
    #[ignore]
    fn posts_to_a_push_service() {
        // The key must stay the one the subscription was made with, so it
        // lives in a folder that outlasts the run.
        let dir = std::env::temp_dir().join("horadric-vapid-push-test");
        let mut cfg = Config::load(&dir).unwrap_or_else(|| Config::fresh(&[2; 32]));
        ensure_vapid(&mut cfg, &dir);
        println!("VAPID {}", cfg.vapid_pub);
        let Ok(endpoint) = std::env::var("PUSH_ENDPOINT") else {
            return;
        };
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let mut auth = authorization(&cfg, &endpoint, now).unwrap();
        if std::env::var_os("PUSH_BAD").is_some() {
            auth = auth.replacen(".", ".x", 2);
        }
        // With the subscription's keys the push carries a payload, as the
        // app sends it.
        let body = match (std::env::var("PUSH_P256DH"), std::env::var("PUSH_AUTH")) {
            (Ok(p), Ok(a)) => encrypt(&p, &a, TEST_PAYLOAD.as_bytes()),
            _ => None,
        };
        println!("STATUS {:?}", post(&endpoint, &auth, body.as_deref()));
    }
}
