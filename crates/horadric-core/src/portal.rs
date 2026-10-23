//! Town Portal's pairing and framing, the pure part: the config in
//! `portal.json`, the paired phones, the one-time pairing ticket, the PC's
//! id, the pairing link and the relay's channel frames. The protocol is in
//! `docs/PORTAL.md`.

use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// The relay we host. Anyone can set their own.
pub const DEFAULT_RELAY: &str = "wss://portal.horadric.dev";

/// The channel that reaches every phone of a PC at once.
pub const BROADCAST: u32 = 0xFFFF_FFFF;

/// How long a pairing code works, in seconds. Long enough to find the
/// phone, short enough that a code seen over a shoulder or left in a
/// browser's history is dead by the time anyone tries it.
pub const TICKET_SECS: u64 = 300;

/// A phone not seen for this long, in seconds, is forgotten: a lost phone
/// stops being a way in by itself.
pub const STALE_SECS: u64 = 30 * 24 * 3600;

/// The file in the state folder.
const FILE: &str = "portal.json";

/// What `portal.json` holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    pub enabled: bool,
    pub relay: String,
    pub secret: String,
    /// The Web Push signing key's private scalar, b64url, made on first
    /// use. Older files have none, hence the defaults.
    #[serde(default)]
    pub vapid_d: String,
    /// Its public key as an uncompressed point, `04 || x || y`, b64url.
    #[serde(default)]
    pub vapid_pub: String,
    /// Every paired phone, each with a key of its own, so one can be
    /// forgotten without the others.
    #[serde(default)]
    pub phones: Vec<Phone>,
    /// The pairing code the QR shows, while there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ticket: Option<Ticket>,
}

/// One paired phone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Phone {
    pub id: String,
    /// What the phone called itself, such as "iPhone, Safari".
    pub name: String,
    /// Its AES-256 key, b64url. It crossed the relay once, sealed with
    /// the ticket, and lives only here and in that phone's browser.
    pub key: String,
    /// When it paired and when it last said hello, Unix seconds.
    pub paired: u64,
    pub seen: u64,
}

/// A pairing code: works once, until `until` (Unix seconds).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ticket {
    pub key: String,
    pub until: u64,
    /// Set when a phone paired with it. The key is kept until it expires
    /// so a second phone trying it hears "used", which tells the human
    /// that someone else got there first.
    #[serde(default)]
    pub used: bool,
}

/// What a pairing attempt with the ticket comes to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Paired {
    /// A new phone, now in the list.
    New(Phone),
    /// The ticket was used already.
    Used,
    /// The ticket ran out, or there is none.
    Expired,
}

impl Config {
    /// A new config from 32 random bytes, the secret. No phone is paired
    /// yet. Taking the bytes keeps this pure.
    pub fn fresh(random: &[u8; 32]) -> Config {
        Config {
            enabled: true,
            relay: DEFAULT_RELAY.to_string(),
            secret: b64url(random),
            vapid_d: String::new(),
            vapid_pub: String::new(),
            phones: Vec::new(),
            ticket: None,
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

    /// A new pairing code from 32 random bytes, replacing any before it.
    pub fn open_ticket(&mut self, random: &[u8; 32], now: u64) {
        self.ticket = Some(Ticket {
            key: b64url(random),
            until: now + TICKET_SECS,
            used: false,
        });
    }

    /// The ticket's key while it has not run out, used or not.
    pub fn ticket_key(&self, now: u64) -> Option<[u8; 32]> {
        let t = self.ticket.as_ref().filter(|t| now < t.until)?;
        key32(&t.key)
    }

    /// Pairs a phone with the ticket: the first to ask gets a key of its
    /// own, made from `random`, and the ticket is spent.
    pub fn pair(&mut self, name: &str, random: &[u8; 40], now: u64) -> Paired {
        let Some(t) = self.ticket.as_mut().filter(|t| now < t.until) else {
            return Paired::Expired;
        };
        if t.used {
            return Paired::Used;
        }
        t.used = true;
        let name: String = name.trim().chars().take(60).collect();
        let phone = Phone {
            id: b64url(&random[..8]),
            name: if name.is_empty() {
                "Phone".into()
            } else {
                name
            },
            key: b64url(&random[8..]),
            paired: now,
            seen: now,
        };
        self.phones.push(phone.clone());
        Paired::New(phone)
    }

    /// Drops a ticket that ran out, so the file does not keep a dead key.
    /// True when it did.
    pub fn drop_expired_ticket(&mut self, now: u64) -> bool {
        let gone = self.ticket.as_ref().is_some_and(|t| now >= t.until);
        if gone {
            self.ticket = None;
        }
        gone
    }

    /// Forgets the phones not seen for [`STALE_SECS`]. True when any went.
    pub fn forget_stale(&mut self, now: u64) -> bool {
        let before = self.phones.len();
        self.phones
            .retain(|p| now.saturating_sub(p.seen) < STALE_SECS);
        self.phones.len() != before
    }

    /// The link a phone opens to pair, while there is a ticket. Everything
    /// after `#` stays in the browser, so the code never reaches a server.
    pub fn link(&self, pc_name: &str) -> Option<String> {
        Some(format!(
            "{}/#{}",
            page_origin(&self.relay),
            self.fragment(pc_name)?
        ))
    }

    /// The page that shows [`Config::link`] as a QR code.
    pub fn pair_url(&self, pc_name: &str) -> Option<String> {
        Some(format!(
            "{}/pair.html#{}",
            page_origin(&self.relay),
            self.fragment(pc_name)?
        ))
    }

    /// The subscriptions of phones still paired. One from a phone since
    /// forgotten, or from before phones had keys of their own, is skipped.
    pub fn paired_subs(&self, subs: &Subs) -> Vec<PushSub> {
        subs.0
            .iter()
            .filter(|s| self.phones.iter().any(|p| p.id == s.phone))
            .cloned()
            .collect()
    }

    fn fragment(&self, pc_name: &str) -> Option<String> {
        let t = self.ticket.as_ref()?;
        Some(format!(
            "r={}&id={}&p={}&x={}&n={}",
            uri_encode(&self.relay),
            id(&self.secret),
            t.key,
            t.until,
            uri_encode(pc_name)
        ))
    }
}

/// A b64url key as AES-256 wants it, `None` when it is not 32 bytes.
pub fn key32(b64: &str) -> Option<[u8; 32]> {
    unb64url(b64)?.try_into().ok()
}

/// Whether a phone's message belongs to the connection it arrived on:
/// it carries that connection's `s`, and a count past the last one. A
/// relay that replays an old frame, from this connection or another,
/// fails one or the other. `last` moves on when it passes.
pub fn fresh_message(msg: &serde_json::Value, s: &str, last: &mut u64) -> bool {
    let n = msg.get("n").and_then(|n| n.as_u64());
    match n {
        Some(n) if msg.get("s").and_then(|v| v.as_str()) == Some(s) && n > *last => {
            *last = n;
            true
        }
        _ => false,
    }
}

/// The newest protocol this PC speaks. Version 3 adds raw streams and
/// brothers; a client that asks for less is answered in its own version,
/// so a v2 page never sees a number it does not know.
pub const NEWEST: u64 = 3;

/// The version to answer a hello with: the client's own, up to
/// [`NEWEST`], and never below 2, which every page since 0.22 speaks.
pub fn answer_version(hello: &serde_json::Value) -> u64 {
    hello["v"].as_u64().unwrap_or(2).clamp(2, NEWEST)
}

/// What is on the other end of a connection, as its hello says.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Kind {
    /// The page, or a client that did not say: every v2 phone.
    #[default]
    Phone,
    /// Another PC of the same human.
    Brother,
}

impl Kind {
    pub fn of_hello(hello: &serde_json::Value) -> Kind {
        match hello["kind"].as_str() {
            Some("brother") => Kind::Brother,
            _ => Kind::Phone,
        }
    }
}

/// Whether a message needs a link that may control this PC: whatever
/// types into a session, starts or moves one. A view only link sees
/// tiles and screens and sends none of these.
pub fn needs_control(t: &str) -> bool {
    matches!(
        t,
        "input" | "start" | "resume" | "create" | "handoff" | "takeover"
    )
}

/// One viewer of a session on another machine, for [`size_owner`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Viewer {
    pub kind: Kind,
    /// When it last typed or clicked in the session, in any unit that
    /// only goes up, the same as the stage's. None when it never did.
    pub active: Option<u64>,
}

/// Who sets a session's size, which its one pseudo terminal has only one
/// of: the viewer that last typed or clicked in it, like tmux's `latest`.
/// Some(i) is `viewers[i]`, None the stage of the PC that owns the session,
/// which also keeps it when nobody did anything yet or on a tie. Phones
/// never resize, so they never win.
pub fn size_owner(stage: Option<u64>, viewers: &[Viewer]) -> Option<usize> {
    let (i, at) = viewers
        .iter()
        .enumerate()
        .filter(|(_, v)| v.kind != Kind::Phone)
        .filter_map(|(i, v)| v.active.map(|at| (i, at)))
        .fold(None, |best: Option<(usize, u64)>, (i, at)| match best {
            Some((_, b)) if b >= at => best,
            _ => Some((i, at)),
        })?;
    match stage {
        Some(s) if s >= at => None,
        _ => Some(i),
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
    /// The id of the phone that sent it. A push goes only to phones still
    /// paired, so forgetting a phone also stops its notifications.
    #[serde(default)]
    pub phone: String,
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

    /// Forgets every phone's subscription, as forgetting every phone does.
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
            phone: String::new(),
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
    fn fresh_config_has_a_secret_and_no_phones() {
        let c = Config::fresh(&[0; 32]);
        assert!(c.enabled);
        assert_eq!(c.relay, DEFAULT_RELAY);
        assert_eq!(unb64url(&c.secret).unwrap(), vec![0; 32]);
        assert!(c.phones.is_empty() && c.ticket.is_none());
        assert_eq!(c.link("x"), None);
    }

    #[test]
    fn links_carry_the_ticket_in_the_fragment() {
        let mut c = Config::fresh(&[0; 32]);
        c.secret = "abc".into();
        c.open_ticket(&[0xfb; 32], 1000);
        let p = b64url(&[0xfb; 32]);
        assert_eq!(
            c.link("MY PC").unwrap(),
            format!(
                "https://portal.horadric.dev/#r=wss%3A%2F%2Fportal.horadric.dev\
                 &id=ba7816bf8f01cfea414140de5dae2223&p={p}&x=1300&n=MY%20PC"
            )
        );
        assert!(c
            .pair_url("x")
            .unwrap()
            .starts_with("https://portal.horadric.dev/pair.html#r="));
        assert_eq!(
            page_origin("ws://127.0.0.1:43201/"),
            "http://127.0.0.1:43201"
        );
    }

    #[test]
    fn a_ticket_pairs_one_phone_once_and_runs_out() {
        let mut c = Config::fresh(&[0; 32]);
        assert_eq!(c.pair("a", &[1; 40], 0), Paired::Expired);
        c.open_ticket(&[9; 32], 1000);
        assert_eq!(c.ticket_key(1299), Some([9; 32]));
        let Paired::New(phone) = c.pair("  iPhone, Safari ", &[1; 40], 1010) else {
            panic!("did not pair");
        };
        assert_eq!(phone.name, "iPhone, Safari");
        assert_eq!(key32(&phone.key), Some([1; 32]));
        assert_eq!((phone.paired, phone.seen), (1010, 1010));
        assert_eq!(c.phones, vec![phone]);
        // A second phone with the same code hears it was used, while the
        // code lasts, and then that it ran out.
        assert_eq!(c.pair("b", &[2; 40], 1020), Paired::Used);
        assert_eq!(c.ticket_key(1299), Some([9; 32]));
        assert_eq!(c.ticket_key(1300), None);
        assert_eq!(c.pair("b", &[2; 40], 1300), Paired::Expired);
        assert_eq!(c.phones.len(), 1);
        assert!(!c.drop_expired_ticket(1299));
        assert!(c.drop_expired_ticket(1300));
        assert!(c.ticket.is_none());
    }

    #[test]
    fn a_phone_without_a_name_is_a_phone() {
        let mut c = Config::fresh(&[0; 32]);
        c.open_ticket(&[9; 32], 0);
        let Paired::New(phone) = c.pair(" ", &[1; 40], 1) else {
            panic!("did not pair");
        };
        assert_eq!(phone.name, "Phone");
    }

    #[test]
    fn phones_unseen_for_thirty_days_are_forgotten() {
        let mut c = Config::fresh(&[0; 32]);
        for (i, seen) in [0, 100].into_iter().enumerate() {
            c.open_ticket(&[9; 32], seen);
            c.pair(&format!("p{i}"), &[i as u8; 40], seen);
        }
        assert!(!c.forget_stale(STALE_SECS - 1));
        assert!(c.forget_stale(STALE_SECS));
        assert_eq!(c.phones.len(), 1);
        assert_eq!(c.phones[0].name, "p1");
    }

    #[test]
    fn only_paired_phones_are_pushed() {
        let mut c = Config::fresh(&[0; 32]);
        c.open_ticket(&[9; 32], 0);
        let Paired::New(phone) = c.pair("p", &[1; 40], 1) else {
            panic!("did not pair");
        };
        let sub = |e: &str, phone: &str| PushSub {
            endpoint: e.into(),
            p256dh: "k".into(),
            auth: "a".into(),
            phone: phone.into(),
        };
        let subs = Subs(vec![
            sub("https://p/1", &phone.id),
            sub("https://p/2", "gone"),
            sub("https://p/3", ""),
        ]);
        assert_eq!(c.paired_subs(&subs), vec![sub("https://p/1", &phone.id)]);
    }

    #[test]
    fn a_message_belongs_to_its_connection_and_comes_once() {
        let mut last = 0;
        let m = |s: &str, n: u64| serde_json::json!({"t":"input","s":s,"n":n});
        assert!(fresh_message(&m("S", 1), "S", &mut last));
        assert!(!fresh_message(&m("S", 1), "S", &mut last));
        assert!(fresh_message(&m("S", 5), "S", &mut last));
        assert!(!fresh_message(&m("S", 4), "S", &mut last));
        assert!(!fresh_message(&m("old", 9), "S", &mut last));
        assert!(!fresh_message(
            &serde_json::json!({"t":"input"}),
            "S",
            &mut last
        ));
        assert_eq!(last, 5);
    }

    #[test]
    fn a_hello_is_answered_in_the_clients_version() {
        let v = |h: serde_json::Value| answer_version(&h);
        assert_eq!(v(serde_json::json!({"t":"hello","v":2})), 2, "today's page");
        assert_eq!(v(serde_json::json!({"t":"hello","v":1})), 2, "as before v3");
        assert_eq!(v(serde_json::json!({"t":"hello"})), 2);
        assert_eq!(v(serde_json::json!({"t":"hello","v":3})), 3);
        assert_eq!(v(serde_json::json!({"t":"hello","v":9})), NEWEST);
    }

    #[test]
    fn a_hello_without_a_kind_is_a_phone() {
        let k = |h: serde_json::Value| Kind::of_hello(&h);
        assert_eq!(k(serde_json::json!({"t":"hello","v":2})), Kind::Phone);
        assert_eq!(k(serde_json::json!({"kind":"phone"})), Kind::Phone);
        assert_eq!(k(serde_json::json!({"kind":"brother"})), Kind::Brother);
        assert_eq!(k(serde_json::json!({"kind":"toaster"})), Kind::Phone);
    }

    #[test]
    fn only_what_acts_needs_control() {
        for t in ["input", "start", "resume", "create", "handoff", "takeover"] {
            assert!(needs_control(t), "{t}");
        }
        for t in [
            "hello", "watch", "attach", "detach", "resize", "sessions", "reply",
        ] {
            assert!(!needs_control(t), "{t}");
        }
    }

    #[test]
    fn the_viewer_that_typed_last_sets_the_size() {
        let brother = |active| Viewer {
            kind: Kind::Brother,
            active,
        };
        let phone = |active| Viewer {
            kind: Kind::Phone,
            active,
        };
        assert_eq!(size_owner(None, &[]), None, "the stage, alone");
        assert_eq!(size_owner(None, &[brother(None)]), None, "nobody typed");
        assert_eq!(size_owner(None, &[brother(Some(5))]), Some(0));
        assert_eq!(size_owner(Some(4), &[brother(Some(5))]), Some(0));
        assert_eq!(size_owner(Some(6), &[brother(Some(5))]), None);
        assert_eq!(
            size_owner(Some(5), &[brother(Some(5))]),
            None,
            "a tie stays"
        );
        assert_eq!(
            size_owner(
                Some(1),
                &[brother(Some(3)), brother(Some(7)), brother(None)]
            ),
            Some(1)
        );
        assert_eq!(
            size_owner(Some(1), &[brother(Some(7)), brother(Some(7))]),
            Some(0),
            "the first of a tie"
        );
        assert_eq!(
            size_owner(Some(1), &[phone(Some(9)), brother(Some(3))]),
            Some(1),
            "a phone never resizes"
        );
        assert_eq!(size_owner(None, &[phone(Some(9))]), None);
    }

    #[test]
    fn config_saves_and_loads() {
        let dir = std::env::temp_dir().join(format!("horadric-portal-{}", std::process::id()));
        let mut c = Config::fresh(&[3; 32]);
        c.open_ticket(&[4; 32], 7);
        c.pair("p", &[5; 40], 8);
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
            phone: String::new(),
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
                phone: String::new(),
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
    fn old_configs_load_and_their_shared_key_pairs_nothing() {
        let c: Config =
            serde_json::from_str(r#"{"enabled":true,"relay":"r","secret":"s","key":"k"}"#).unwrap();
        assert!(c.vapid_d.is_empty() && c.vapid_pub.is_empty());
        assert!(c.phones.is_empty() && c.ticket.is_none());
        assert!(!serde_json::to_string(&c).unwrap().contains("\"key\""));
    }
}
