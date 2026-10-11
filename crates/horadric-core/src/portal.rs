//! Town Portal's pairing and framing, the pure part: the config in
//! `portal.json`, the PC's id, the pairing link and the relay's channel
//! frames. The protocol is in `docs/PORTAL.md`.

use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// The relay we host. Anyone can set their own.
pub const DEFAULT_RELAY: &str = "wss://portal.horadric.dev";

/// The channel that reaches every phone of a PC at once.
pub const BROADCAST: u32 = 0xFFFF_FFFF;

/// The file in the state folder.
const FILE: &str = "portal.json";

/// What `portal.json` holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    pub enabled: bool,
    pub relay: String,
    pub secret: String,
    pub key: String,
    /// The Web Push signing key's private scalar, b64url, made on first
    /// use. Older files have none, hence the defaults.
    #[serde(default)]
    pub vapid_d: String,
    /// Its public key as an uncompressed point, `04 || x || y`, b64url.
    #[serde(default)]
    pub vapid_pub: String,
}

impl Config {
    /// A new config from 64 random bytes: the first 32 the secret, the
    /// rest the key. Taking the bytes keeps this pure.
    pub fn fresh(random: &[u8; 64]) -> Config {
        Config {
            enabled: true,
            relay: DEFAULT_RELAY.to_string(),
            secret: b64url(&random[..32]),
            key: b64url(&random[32..]),
            vapid_d: String::new(),
            vapid_pub: String::new(),
        }
    }

    /// The config in `dir`, or `None` when it is missing or unreadable.
    pub fn load(dir: &Path) -> Option<Config> {
        let text = std::fs::read_to_string(dir.join(FILE)).ok()?;
        serde_json::from_str(&text).ok()
    }

    pub fn save(&self, dir: &Path) -> io::Result<()> {
        std::fs::create_dir_all(dir)?;
        let text = serde_json::to_string_pretty(self).map_err(io::Error::other)?;
        std::fs::write(dir.join(FILE), text)
    }

    /// The key as AES-256 wants it, `None` when it is not 32 bytes.
    pub fn key_bytes(&self) -> Option<[u8; 32]> {
        unb64url(&self.key)?.try_into().ok()
    }

    /// The link a phone opens to pair. Everything after `#` stays in the
    /// browser, so the key never reaches a server.
    pub fn link(&self, pc_name: &str) -> String {
        format!("{}/#{}", page_origin(&self.relay), self.fragment(pc_name))
    }

    /// The page that shows [`Config::link`] as a QR code.
    pub fn pair_url(&self, pc_name: &str) -> String {
        format!(
            "{}/pair.html#{}",
            page_origin(&self.relay),
            self.fragment(pc_name)
        )
    }

    fn fragment(&self, pc_name: &str) -> String {
        format!(
            "r={}&id={}&k={}&n={}",
            uri_encode(&self.relay),
            id(&self.secret),
            self.key,
            uri_encode(pc_name)
        )
    }
}

/// The file of phones' push subscriptions, beside `portal.json`.
const PUSH_FILE: &str = "portal-push.json";

/// One phone's Web Push subscription, as the page's `PushSubscription`
/// gives it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PushSub {
    pub endpoint: String,
    pub p256dh: String,
    pub auth: String,
}

/// Every stored subscription, one per endpoint.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Subs(pub Vec<PushSub>);

impl Subs {
    pub fn load(dir: &Path) -> Subs {
        std::fs::read_to_string(dir.join(PUSH_FILE))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, dir: &Path) -> io::Result<()> {
        std::fs::create_dir_all(dir)?;
        let text = serde_json::to_string_pretty(self).map_err(io::Error::other)?;
        std::fs::write(dir.join(PUSH_FILE), text)
    }

    /// Forgets every phone's subscription, as a new key unpairs them.
    pub fn clear(dir: &Path) -> io::Result<()> {
        match std::fs::remove_file(dir.join(PUSH_FILE)) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }

    /// Adds `sub`, replacing one with the same endpoint.
    pub fn add(&mut self, sub: PushSub) {
        self.remove(&sub.endpoint);
        self.0.push(sub);
    }

    pub fn remove(&mut self, endpoint: &str) {
        self.0.retain(|s| s.endpoint != endpoint);
    }

    /// A subscription from the phone's `{"endpoint","keys":{"p256dh","auth"}}`.
    pub fn parse(sub: &serde_json::Value) -> Option<PushSub> {
        let text = |v: Option<&serde_json::Value>| v?.as_str().map(str::to_string);
        let endpoint = text(sub.get("endpoint"))?;
        if !endpoint.starts_with("https://") {
            return None;
        }
        let keys = sub.get("keys")?;
        Some(PushSub {
            endpoint,
            p256dh: text(keys.get("p256dh"))?,
            auth: text(keys.get("auth"))?,
        })
    }
}

/// Who a VAPID token is for: the push service's origin, `https://host`
/// with any port, without the path.
pub fn push_audience(endpoint: &str) -> Option<String> {
    let rest = endpoint.strip_prefix("https://")?;
    let host = rest.split('/').next().filter(|h| !h.is_empty())?;
    Some(format!("https://{host}"))
}

/// The part of a VAPID JWT that is signed: header and claims, b64url,
/// joined by a dot. `exp` is in Unix seconds.
pub fn vapid_signing_input(aud: &str, exp: u64) -> String {
    let header = br#"{"typ":"JWT","alg":"ES256"}"#;
    let claims = serde_json::json!({
        "aud": aud,
        "exp": exp,
        "sub": "mailto:portal@horadric.dev",
    });
    format!(
        "{}.{}",
        b64url(header),
        b64url(claims.to_string().as_bytes())
    )
}

/// The `Authorization` header of a push: the JWT, its signature (`r || s`)
/// appended, and the public key.
pub fn vapid_authorization(signing_input: &str, signature: &[u8], vapid_pub: &str) -> String {
    format!(
        "vapid t={signing_input}.{}, k={vapid_pub}",
        b64url(signature)
    )
}

/// The relay's page, which shares its host: `wss://x` is `https://x`.
pub fn page_origin(relay: &str) -> String {
    let relay = relay.trim_end_matches('/');
    if let Some(rest) = relay.strip_prefix("wss://") {
        format!("https://{rest}")
    } else if let Some(rest) = relay.strip_prefix("ws://") {
        format!("http://{rest}")
    } else {
        relay.to_string()
    }
}

/// A PC's id: the first 32 hex characters of the SHA-256 of its secret.
/// The relay works it out the same way, so it stores nothing.
pub fn id(secret: &str) -> String {
    sha256(secret.as_bytes())[..16]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A relay frame between PC and relay: the channel, big endian, then the
/// payload.
pub fn frame(ch: u32, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + payload.len());
    out.extend_from_slice(&ch.to_be_bytes());
    out.extend_from_slice(payload);
    out
}

pub fn unframe(bytes: &[u8]) -> Option<(u32, &[u8])> {
    let (head, rest) = bytes.split_first_chunk::<4>()?;
    Some((u32::from_be_bytes(*head), rest))
}

/// Percent-encodes everything but the characters `encodeURIComponent`
/// leaves alone, so the page's `decodeURIComponent` reads it back.
pub fn uri_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// Base64url without padding.
pub fn b64url(bytes: &[u8]) -> String {
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, b)| n | (*b as u32) << (16 - 8 * i));
        for i in 0..=chunk.len() {
            out.push(B64[(n >> (18 - 6 * i) & 63) as usize] as char);
        }
    }
    out
}

/// Reads base64url, padded or not, `None` on anything else.
pub fn unb64url(s: &str) -> Option<Vec<u8>> {
    let s = s.trim_end_matches('=');
    let mut out = Vec::new();
    let (mut acc, mut bits) = (0u32, 0);
    for c in s.bytes() {
        let v = B64.iter().position(|&b| b == c)? as u32;
        acc = acc << 6 | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    (bits < 6).then_some(out)
}

const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

/// SHA-256, small and slow, for an id worked out once. Not worth a crate.
pub fn sha256(data: &[u8]) -> [u8; 32] {
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&((data.len() as u64) * 8).to_be_bytes());
    for block in msg.chunks(64) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes(block[i * 4..i * 4 + 4].try_into().unwrap());
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let mut v = h;
        for i in 0..64 {
            let s1 = v[4].rotate_right(6) ^ v[4].rotate_right(11) ^ v[4].rotate_right(25);
            let ch = (v[4] & v[5]) ^ (!v[4] & v[6]);
            let t1 = v[7]
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = v[0].rotate_right(2) ^ v[0].rotate_right(13) ^ v[0].rotate_right(22);
            let maj = (v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]);
            let t2 = s0.wrapping_add(maj);
            v = [
                t1.wrapping_add(t2),
                v[0],
                v[1],
                v[2],
                v[3].wrapping_add(t1),
                v[4],
                v[5],
                v[6],
            ];
        }
        for (a, b) in h.iter_mut().zip(v) {
            *a = a.wrapping_add(b);
        }
    }
    let mut out = [0u8; 32];
    for (i, word) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(b: &[u8]) -> String {
        b.iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn sha256_matches_nist() {
        assert_eq!(
            hex(&sha256(b"")),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            hex(&sha256(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            hex(&sha256(
                b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"
            )),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
        assert_eq!(
            hex(&sha256(&[b'a'; 1000])),
            "41edece42d63e8d9bf515a9ba6932e1c20cbc9f5a5d134645adb5db1b9737ea3"
        );
    }

    #[test]
    fn id_is_the_first_half_of_the_hash() {
        assert_eq!(id("abc"), "ba7816bf8f01cfea414140de5dae2223");
    }

    #[test]
    fn b64url_round_trips_every_length() {
        for n in 0..40u8 {
            let bytes: Vec<u8> = (0..n)
                .map(|i| i.wrapping_mul(37).wrapping_add(250))
                .collect();
            assert_eq!(unb64url(&b64url(&bytes)).unwrap(), bytes);
        }
        assert_eq!(b64url(b"\xfb\xff"), "-_8");
        assert_eq!(b64url(b"hello"), "aGVsbG8");
        assert_eq!(unb64url("aGVsbG8=").unwrap(), b"hello");
        assert_eq!(unb64url("a+b"), None);
        assert_eq!(unb64url("a"), None);
    }

    #[test]
    fn frames_round_trip() {
        let f = frame(0x0102_0304, b"hi");
        assert_eq!(f, [1, 2, 3, 4, b'h', b'i']);
        assert_eq!(unframe(&f), Some((0x0102_0304, &b"hi"[..])));
        assert_eq!(unframe(&[1, 2, 3]), None);
        assert_eq!(unframe(&frame(BROADCAST, b"")), Some((BROADCAST, &b""[..])));
    }

    #[test]
    fn fresh_config_splits_the_bytes() {
        let mut r = [0u8; 64];
        r[32..].fill(7);
        let c = Config::fresh(&r);
        assert!(c.enabled);
        assert_eq!(c.relay, DEFAULT_RELAY);
        assert_eq!(c.key_bytes(), Some([7; 32]));
        assert_eq!(unb64url(&c.secret).unwrap(), vec![0; 32]);
    }

    #[test]
    fn links_carry_the_fragment() {
        let c = Config {
            enabled: true,
            relay: "wss://portal.horadric.dev".into(),
            secret: "abc".into(),
            key: "KEY".into(),
            vapid_d: String::new(),
            vapid_pub: String::new(),
        };
        assert_eq!(
            c.link("MY PC"),
            "https://portal.horadric.dev/#r=wss%3A%2F%2Fportal.horadric.dev\
             &id=ba7816bf8f01cfea414140de5dae2223&k=KEY&n=MY%20PC"
        );
        assert!(c
            .pair_url("x")
            .starts_with("https://portal.horadric.dev/pair.html#r="));
        assert_eq!(
            page_origin("ws://127.0.0.1:43201/"),
            "http://127.0.0.1:43201"
        );
    }

    #[test]
    fn config_saves_and_loads() {
        let dir = std::env::temp_dir().join(format!("horadric-portal-{}", std::process::id()));
        let c = Config::fresh(&[3; 64]);
        c.save(&dir).unwrap();
        assert_eq!(Config::load(&dir), Some(c));
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(Config::load(&dir), None);
    }

    #[test]
    fn subs_add_replace_remove_and_persist() {
        let sub = |e: &str, k: &str| PushSub {
            endpoint: e.into(),
            p256dh: k.into(),
            auth: "a".into(),
        };
        let mut s = Subs::default();
        s.add(sub("https://p/1", "x"));
        s.add(sub("https://p/2", "x"));
        s.add(sub("https://p/1", "y"));
        assert_eq!(s.0.len(), 2);
        assert_eq!(s.0[1], sub("https://p/1", "y"));
        s.remove("https://p/2");
        assert_eq!(s.0, vec![sub("https://p/1", "y")]);
        let dir = std::env::temp_dir().join(format!("horadric-push-{}", std::process::id()));
        s.save(&dir).unwrap();
        assert_eq!(Subs::load(&dir), s);
        Subs::clear(&dir).unwrap();
        Subs::clear(&dir).unwrap();
        assert_eq!(Subs::load(&dir), Subs::default());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn subs_parse_the_page_json() {
        let v = serde_json::json!({"endpoint":"https://fcm.googleapis.com/fcm/send/abc",
            "keys":{"p256dh":"P","auth":"A"}});
        assert_eq!(
            Subs::parse(&v),
            Some(PushSub {
                endpoint: "https://fcm.googleapis.com/fcm/send/abc".into(),
                p256dh: "P".into(),
                auth: "A".into(),
            })
        );
        assert_eq!(
            Subs::parse(
                &serde_json::json!({"endpoint":"http://x/1","keys":{"p256dh":"P","auth":"A"}})
            ),
            None
        );
        assert_eq!(
            Subs::parse(&serde_json::json!({"endpoint":"https://x/1"})),
            None
        );
    }

    #[test]
    fn audience_is_the_endpoint_origin() {
        assert_eq!(
            push_audience("https://fcm.googleapis.com/fcm/send/abc").as_deref(),
            Some("https://fcm.googleapis.com")
        );
        assert_eq!(
            push_audience("https://web.push.apple.com:443/QX").as_deref(),
            Some("https://web.push.apple.com:443")
        );
        assert_eq!(push_audience("http://x/1"), None);
        assert_eq!(push_audience("https:///1"), None);
    }

    #[test]
    fn vapid_jwt_is_header_claims_signature() {
        let input = vapid_signing_input("https://push.example", 1_700_000_000);
        let (h, c) = input.split_once('.').unwrap();
        assert_eq!(unb64url(h).unwrap(), br#"{"typ":"JWT","alg":"ES256"}"#);
        let claims: serde_json::Value = serde_json::from_slice(&unb64url(c).unwrap()).unwrap();
        assert_eq!(claims["aud"], "https://push.example");
        assert_eq!(claims["exp"], 1_700_000_000u64);
        assert_eq!(claims["sub"], "mailto:portal@horadric.dev");
        assert_eq!(
            vapid_authorization("H.C", &[0xfb, 0xff], "PUB"),
            "vapid t=H.C.-_8, k=PUB"
        );
    }

    #[test]
    fn old_configs_load_without_vapid() {
        let c: Config =
            serde_json::from_str(r#"{"enabled":true,"relay":"r","secret":"s","key":"k"}"#).unwrap();
        assert!(c.vapid_d.is_empty() && c.vapid_pub.is_empty());
    }
}
