//! Web Push message encryption (RFC 8291 over RFC 8188's `aes128gcm`), the
//! pure part: HKDF, the content key and nonce, the record's padding and
//! header, and what a Town Portal push says. The ECDH and the AES-GCM run
//! through CNG in the UI crate. The push service carries the body but
//! cannot read it: only the phone's browser holds the private key.

use std::time::UNIX_EPOCH;

use serde_json::{json, Value};

use crate::ask::{Ask, Choice, Kind};
use crate::portal::sha256;
use crate::session::{waiting_on, Phase, Session, WaitReason};

/// The record size the header announces. One record holds the whole
/// payload, so this only has to be more than the payload's length.
pub const RECORD: u32 = 4096;

/// The most plaintext one record holds: the record less the padding
/// delimiter and the 16 byte tag.
pub const MAX_PLAIN: usize = RECORD as usize - 17;

pub fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    let mut k = [0u8; 64];
    if key.len() > 64 {
        k[..32].copy_from_slice(&sha256(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut inner: Vec<u8> = k.iter().map(|b| b ^ 0x36).collect();
    inner.extend_from_slice(data);
    let mut outer: Vec<u8> = k.iter().map(|b| b ^ 0x5c).collect();
    outer.extend_from_slice(&sha256(&inner));
    sha256(&outer)
}

/// HKDF-Expand for one block, which every length here fits in.
fn expand(prk: &[u8; 32], info: &[u8], len: usize) -> Vec<u8> {
    let mut input = info.to_vec();
    input.push(1);
    hmac_sha256(prk, &input)[..len].to_vec()
}

/// The content key and nonce of one message.
#[derive(Debug, PartialEq, Eq)]
pub struct Keys {
    pub cek: [u8; 16],
    pub nonce: [u8; 12],
}

/// RFC 8291 section 3.4: from the ECDH secret, the subscription's `auth`
/// and both public keys (65 byte uncompressed points), and the salt.
pub fn derive(
    ecdh_secret: &[u8],
    auth: &[u8],
    ua_public: &[u8],
    as_public: &[u8],
    salt: &[u8],
) -> Keys {
    let prk_key = hmac_sha256(auth, ecdh_secret);
    let mut key_info = b"WebPush: info\0".to_vec();
    key_info.extend_from_slice(ua_public);
    key_info.extend_from_slice(as_public);
    let ikm = expand(&prk_key, &key_info, 32);
    let prk = hmac_sha256(salt, &ikm);
    let mut keys = Keys {
        cek: [0; 16],
        nonce: [0; 12],
    };
    keys.cek
        .copy_from_slice(&expand(&prk, b"Content-Encoding: aes128gcm\0", 16));
    keys.nonce
        .copy_from_slice(&expand(&prk, b"Content-Encoding: nonce\0", 12));
    keys
}

/// The plaintext of the one and last record: the payload, then 2.
pub fn pad(plain: &[u8]) -> Vec<u8> {
    let mut out = plain.to_vec();
    out.push(2);
    out
}

/// The `aes128gcm` header: salt, record size, and the sender's public key
/// as the key id.
pub fn header(salt: &[u8; 16], as_public: &[u8]) -> Vec<u8> {
    let mut out = salt.to_vec();
    out.extend_from_slice(&RECORD.to_be_bytes());
    out.push(as_public.len() as u8);
    out.extend_from_slice(as_public);
    out
}

/// What the notification says about a waiting session: a title short
/// enough for a lock screen, such as "Allow Bash?", and a body naming the
/// session, then what it said where the title does not already say it.
pub fn wait_text(s: &Session, project: &str) -> (String, String) {
    let who = if project.is_empty() || project == s.label() {
        s.label().to_string()
    } else {
        format!("{project} · {}", s.label())
    };
    let tool = s
        .waits_on
        .as_deref()
        .map(|t| waiting_on(t).trim_start_matches("waiting on ").to_string());
    let (title, line) = match (&s.phase, tool) {
        (Phase::Waiting(WaitReason::Permission), Some(t)) => (format!("Allow {t}?"), ""),
        (Phase::Waiting(WaitReason::Permission), None) => {
            ("Allow a tool?".to_string(), s.last_line.as_str())
        }
        (Phase::Waiting(WaitReason::Error(_)), _) => {
            ("The turn failed".to_string(), s.last_line.as_str())
        }
        (Phase::Waiting(WaitReason::Dialog), _) => {
            ("A dialog waits".to_string(), s.last_line.as_str())
        }
        _ => ("Waiting for you".to_string(), s.last_line.as_str()),
    };
    let body = if line.is_empty() || line.starts_with("waiting on ") {
        who
    } else {
        format!("{who}\n{line}")
    };
    (title, body)
}

/// The push's JSON: the PC and session to open, what to show, how many
/// sessions wait in all, for the app badge, and the answers a notification
/// can offer as buttons, with when the wait began so a late tap cannot
/// answer a later one.
pub fn payload(pc: &str, s: &Session, project: &str, waiting: usize) -> Value {
    let (title, body) = wait_text(s, project);
    let options: Vec<Value> = s
        .ask
        .as_ref()
        .map(buttons)
        .unwrap_or_default()
        .into_iter()
        .map(|c| json!({"label": cut(&c.label, 40), "keys": c.keys}))
        .collect();
    let since = s
        .ask
        .as_ref()
        .and_then(|a| a.since.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs());
    json!({
        "pc": pc,
        "id": s.id,
        "project": project,
        "title": cut(&title, 120),
        "body": cut(&body, 600),
        "waiting": waiting,
        "options": options,
        "since": since,
    })
}

/// The two answers a notification has room for. A permission keeps Yes and
/// No and leaves Always to the page, since a tap on a lock screen should
/// not change what is allowed from then on. A question keeps its first two.
pub fn buttons(ask: &Ask) -> Vec<&Choice> {
    let all = &ask.options;
    match (&ask.kind, all.as_slice()) {
        (Kind::Permission, [first, .., last]) => vec![first, last],
        _ => all.iter().take(2).collect(),
    }
}

/// At most `max` bytes, cut on a character boundary, so a long question
/// keeps the payload inside one record.
fn cut(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max - '…'.len_utf8();
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::portal::unb64url;

    fn b(s: &str) -> Vec<u8> {
        unb64url(s).unwrap()
    }

    /// RFC 8291, Appendix A.
    const AS_PUBLIC: &str =
        "BP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A8";
    const UA_PUBLIC: &str =
        "BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4";
    const ECDH: &str = "kyrL1jIIOHEzg3sM2ZWRHDRB62YACZhhSlknJ672kSs";
    const AUTH: &str = "BTBZMqHH6r4Tts7J_aSIgg";
    const SALT: &str = "DGv6ra1nlYgDCS1FRnbzlw";

    fn unhex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn hmac_matches_rfc_4231() {
        assert_eq!(
            hmac_sha256(&[0x0b; 20], b"Hi There").to_vec(),
            unhex("b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7")
        );
        // A key longer than a block is hashed first.
        assert_eq!(
            hmac_sha256(
                &[0xaa; 131],
                b"Test Using Larger Than Block-Size Key - Hash Key First"
            )
            .to_vec(),
            unhex("60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54")
        );
    }

    #[test]
    fn derives_the_rfc_8291_key_and_nonce() {
        let keys = derive(&b(ECDH), &b(AUTH), &b(UA_PUBLIC), &b(AS_PUBLIC), &b(SALT));
        assert_eq!(keys.cek.to_vec(), b("oIhVW04MRdy2XN9CiKLxTg"));
        assert_eq!(keys.nonce.to_vec(), b("4h_95klXJ5E_qnoN"));
    }

    #[test]
    fn pads_and_heads_as_the_rfc_does() {
        let plain = b"When I grow up, I want to be a watermelon";
        assert_eq!(
            pad(plain),
            b("V2hlbiBJIGdyb3cgdXAsIEkgd2FudCB0byBiZSBhIHdhdGVybWVsb24C")
        );
        let salt: [u8; 16] = b(SALT).try_into().unwrap();
        assert_eq!(
            header(&salt, &b(AS_PUBLIC)),
            b("DGv6ra1nlYgDCS1FRnbzlwAAEABBBP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A8")
        );
    }

    fn session(phase: Phase, tool: Option<&str>, line: &str) -> Session {
        let mut s = Session::new("abc", "horadric", "C:/p/horadric");
        s.phase = phase;
        s.waits_on = tool.map(str::to_string);
        s.last_line = line.into();
        s.name = "fix the tiles".into();
        s.renamed = true;
        s
    }

    #[test]
    fn a_permission_names_its_tool() {
        let s = session(
            Phase::Waiting(WaitReason::Permission),
            Some("Bash"),
            "waiting on Bash",
        );
        assert_eq!(
            wait_text(&s, "horadric"),
            ("Allow Bash?".into(), "horadric · fix the tiles".into())
        );
        let s = session(
            Phase::Waiting(WaitReason::Permission),
            Some("mcp__horadric__browser_click"),
            "",
        );
        assert_eq!(wait_text(&s, "horadric").0, "Allow browser_click?");
    }

    #[test]
    fn a_question_says_what_it_asks() {
        let s = session(
            Phase::Waiting(WaitReason::Input),
            None,
            "Which colour should the tile be?",
        );
        assert_eq!(
            wait_text(&s, "horadric"),
            (
                "Waiting for you".into(),
                "horadric · fix the tiles\nWhich colour should the tile be?".into()
            )
        );
        let s = session(Phase::Waiting(WaitReason::Error("x".into())), None, "");
        assert_eq!(
            wait_text(&s, "fix the tiles"),
            ("The turn failed".into(), "fix the tiles".into())
        );
    }

    #[test]
    fn the_payload_fits_one_record() {
        let long = "ö".repeat(5000);
        let s = session(Phase::Waiting(WaitReason::Input), None, &long);
        let p = payload("0123", &s, "horadric", 3);
        assert_eq!(p["pc"], "0123");
        assert_eq!(p["id"], "abc");
        assert_eq!(p["waiting"], 3);
        assert_eq!(p["title"], "Waiting for you");
        let body = p["body"].as_str().unwrap();
        assert!(body.len() <= 600 && body.ends_with('…'));
        assert!(p.to_string().len() <= MAX_PLAIN);
    }

    fn asking(kind: Kind, labels: &[&str]) -> Ask {
        Ask {
            kind,
            tool: None,
            detail: String::new(),
            options: labels
                .iter()
                .enumerate()
                .map(|(i, l)| Choice {
                    label: l.to_string(),
                    keys: (i + 1).to_string(),
                })
                .collect(),
            since: UNIX_EPOCH + std::time::Duration::from_secs(1_760_000_000),
        }
    }

    #[test]
    fn a_permission_offers_yes_and_no_but_never_always() {
        let labels =
            |a: &Ask| -> Vec<String> { buttons(a).iter().map(|c| c.label.clone()).collect() };
        let a = asking(Kind::Permission, &["Yes", "Always", "No"]);
        assert_eq!(labels(&a), ["Yes", "No"]);
        let a = asking(Kind::Permission, &["Yes", "No"]);
        assert_eq!(labels(&a), ["Yes", "No"]);
        let a = asking(Kind::Question, &["SQLite", "Postgres", "Both"]);
        assert_eq!(labels(&a), ["SQLite", "Postgres"]);
        assert!(buttons(&asking(Kind::Permission, &[])).is_empty());
        assert!(buttons(&asking(Kind::Input, &[])).is_empty());
    }

    #[test]
    fn the_payload_carries_the_buttons_and_when_the_wait_began() {
        let mut s = session(Phase::Waiting(WaitReason::Permission), Some("Bash"), "");
        let p = payload("0123", &s, "horadric", 1);
        assert_eq!(p["options"], json!([]));
        assert_eq!(p["since"], 0);
        let long = "a".repeat(100);
        s.ask = Some(asking(Kind::Question, &[&long, "Postgres", "Both"]));
        let p = payload("0123", &s, "horadric", 1);
        assert_eq!(p["since"], 1_760_000_000);
        assert_eq!(p["options"][1], json!({"label": "Postgres", "keys": "2"}));
        assert_eq!(p["options"].as_array().unwrap().len(), 2);
        assert!(p["options"][0]["label"].as_str().unwrap().len() <= 40);
    }
}
