//! Town Portal's push: the PC tells paired phones a session waits, by Web
//! Push straight to each phone's push service. No server of ours holds the
//! subscriptions. The push has no body, so it carries nothing, and needs
//! only a VAPID signature: ECDSA P-256 through CNG, as the updater signs.
//! The protocol is in `docs/PORTAL.md`.

use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use horadric_core::portal::{
    b64url, push_audience, unb64url, vapid_authorization, vapid_signing_input, Config,
};
pub use horadric_core::portal::{PushSub, Subs};
use horadric_core::release::split_url;
use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Networking::WinHttp::{
    WinHttpCloseHandle, WinHttpConnect, WinHttpOpen, WinHttpOpenRequest, WinHttpQueryHeaders,
    WinHttpReceiveResponse, WinHttpSendRequest, WinHttpSetTimeouts,
    WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_FLAG_SECURE, WINHTTP_QUERY_FLAG_NUMBER,
    WINHTTP_QUERY_STATUS_CODE,
};

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
/// thread of its own so the UI never waits on a push service.
/// Subscriptions the service says are gone (404, 410) are dropped.
pub fn send_all(dir: &Path, cfg: &Config) {
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
                match post(&sub.endpoint, &auth) {
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

/// POSTs an empty push to `endpoint`, giving the status code.
pub fn post(endpoint: &str, authorization: &str) -> Result<u32, String> {
    let parts = split_url(endpoint).ok_or_else(|| format!("`{endpoint}` is not a URL"))?;
    if !parts.secure {
        return Err("a push endpoint is HTTPS".into());
    }
    let agent = format!("Horadric/{}", env!("CARGO_PKG_VERSION"));
    let headers = format!(
        "TTL: 3600\r\nUrgency: high\r\nContent-Length: 0\r\nAuthorization: {authorization}\r\n"
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
        WinHttpSendRequest(request.0, Some(&headers), None, 0, 0, 0)
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
        println!("STATUS {:?}", post(&endpoint, &auth));
    }
}
