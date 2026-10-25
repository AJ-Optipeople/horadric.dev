//! The rules a machine's link to the relay keeps, a Windows PC's (WinHTTP,
//! `horadric-ui/src/portal_link.rs`) and a Linux node's (`horadric-link`)
//! alike: where it connects, how often it pings, when a quiet link is
//! dead, and how long it waits before trying again.

use crate::joining;
use crate::portal::uri_encode;

/// The longest wait between attempts to reach the relay, in seconds.
pub const MAX_BACKOFF: u64 = 30;

/// How often a machine pings the relay, so a dead connection shows, in
/// seconds.
pub const PING_SECS: u64 = 25;

/// A connection that lasted this long, in seconds, was a good one: the
/// next try after it waits only a second. One the relay closes sooner
/// keeps backing off, so a refused token is not offered every second.
pub const LIVED_SECS: u64 = 10;

/// Silence from the relay this long, in seconds, means the connection is
/// dead even if TCP has not said so: drop it and connect again. It also
/// ends a send stuck on such a connection.
pub const SILENT_SECS: u64 = 70;

/// How a machine reaches the relay's `/pc`: the address, which carries
/// only its name, and the request headers, which carry what proves it.
/// The secret and the access token never go in the address, where a proxy
/// in front of the relay would write them to its access log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dial {
    pub url: String,
    pub headers: Vec<(&'static str, String)>,
}

/// The header that carries a machine's relay secret.
pub const SECRET_HEADER: &str = "X-Horadric-Secret";

/// The header a signed in machine dials with to say it sends fresh access
/// tokens on the open link: the relay then closes a link whose token ran
/// out and was never renewed, so a stolen token cannot hold one open.
pub const REFRESH_HEADER: &str = "X-Horadric-Refresh";

/// How long before its access token runs out a machine sends a fresh one
/// on its link. Inside `workos::MARGIN`, so asking for the token then gets
/// a new one, and well inside the relay's two minutes of grace after.
pub const REFRESH_AHEAD: u64 = 45;

/// Whether a link dialled with a token running out at `exp` is due a
/// fresh one at `now`. Nought is a link that was not signed in.
pub fn refresh_due(exp: u64, now: u64) -> bool {
    exp != 0 && now + REFRESH_AHEAD >= exp
}

/// The text frame that hands the relay a fresh access token on the link.
pub fn refresh_message(token: &str) -> Vec<u8> {
    serde_json::json!({ "token": token })
        .to_string()
        .into_bytes()
}

/// How long a fresh token sent waits for the relay's answer before it is
/// sent again.
pub const ASK_AGAIN: u64 = 20;

/// How long after the relay answered `later` (it could not ask WorkOS, or
/// was checking another) a fresh token goes again.
pub const LATER: u64 = 10;

/// How long after the relay refused a fresh token one goes again, a new
/// one asked of WorkOS.
pub const REFUSED: u64 = 30;

/// Where a link's in-band token refresh stands: when the token the relay
/// holds for it runs out, when a fresh one went that the relay has not
/// answered, and when one may go again. The expiry moves only on the
/// relay's `ok`, so a token it refused is never taken as renewed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Refresh {
    pub exp: u64,
    pub sent: u64,
    pub after: u64,
}

impl Refresh {
    /// A link dialled with a token that runs out at `exp` (nought for one
    /// not signed in).
    pub fn dialled(exp: u64) -> Refresh {
        Refresh {
            exp,
            ..Refresh::default()
        }
    }

    /// Whether a fresh token goes now.
    pub fn due(&self, now: u64) -> bool {
        refresh_due(self.exp, now)
            && now >= self.after
            && (self.sent == 0 || now >= self.sent.saturating_add(ASK_AGAIN))
    }

    /// A fresh token went at `now`.
    pub fn sent(&mut self, now: u64) {
        self.sent = now;
    }

    /// The relay's text frame `v`, at `now`: whether it is the relay's
    /// answer to a fresh token, and if so whether it refused it (so a new
    /// one is to be asked of WorkOS).
    pub fn answer(&mut self, v: &serde_json::Value, now: u64) -> Option<bool> {
        let word = v.get("token")?.as_str()?;
        self.sent = 0;
        match word {
            "ok" => {
                let exp = v.get("exp").and_then(serde_json::Value::as_u64);
                self.exp = self.exp.max(exp.unwrap_or(0));
                Some(false)
            }
            "later" => {
                self.after = now.saturating_add(LATER);
                Some(false)
            }
            _ => {
                self.after = now.saturating_add(REFUSED);
                Some(true)
            }
        }
    }
}

/// How to dial the relay's `/pc`. A signed in machine (`signed` is its
/// access token and name) is a v3 brother the relay groups with the rest
/// of its human's; without it the machine is served as a v2 PC.
pub fn pc_dial(relay: &str, secret: &str, signed: Option<(&str, &str)>) -> Dial {
    let mut url = format!("{}/pc", relay.trim_end_matches('/'));
    let mut headers = vec![(SECRET_HEADER, secret.to_string())];
    if let Some((token, name)) = signed {
        url.push_str(&format!("?name={}", uri_encode(name)));
        headers.push(("Authorization", format!("Bearer {token}")));
        headers.push((REFRESH_HEADER, "1".to_string()));
    }
    Dial { url, headers }
}

/// Whether the relay has been quiet too long, both in milliseconds.
pub fn silent(now: u64, heard: u64) -> bool {
    now.saturating_sub(heard) > SILENT_SECS * 1000
}

/// What to do after a connection that lasted `lived` seconds closed with
/// `code`, waiting `backoff` before: (seconds to wait now, the backoff
/// after, whether to drop the access token in hand). A good connection
/// starts the backoff over; a refused token (4401) is dropped so the next
/// try refreshes it; a busy relay (4503) waits the longest.
pub fn after_link(lived: u64, code: Option<u16>, backoff: u64) -> (u64, u64, bool) {
    let backoff = if lived >= LIVED_SECS { 1 } else { backoff };
    joining::after_close(code, backoff, MAX_BACKOFF)
}

/// What the human is told when the relay closes this machine's link with
/// `code`, None for a close that needs no word. 4429 is the relay
/// refusing one machine more than it serves a human: said plainly, since
/// a login someone else holds would look just like this.
pub fn closed_word(code: Option<u16>) -> Option<&'static str> {
    (code == Some(4429)).then_some(
        "Town Portal refused this machine: too many machines are signed in as you \
         right now (16 at most). If you did not sign in that many, someone else may \
         have your login: sign out of Horadric on machines you no longer use, and \
         change your password. It tries again by itself.",
    )
}

/// The wait after a try that never connected, and the backoff after it.
pub fn after_failure(backoff: u64) -> (u64, u64) {
    (backoff, (backoff * 2).min(MAX_BACKOFF))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_link_renews_its_token_just_before_it_runs_out() {
        assert!(!refresh_due(0, 5));
        assert!(!refresh_due(1_000, 1_000 - REFRESH_AHEAD - 1));
        assert!(refresh_due(1_000, 1_000 - REFRESH_AHEAD));
        assert!(refresh_due(1_000, 2_000));
        const { assert!(REFRESH_AHEAD < crate::workos::MARGIN) };
        let m: serde_json::Value = serde_json::from_slice(&refresh_message("t.k.n")).unwrap();
        assert_eq!(m, serde_json::json!({"token": "t.k.n"}));
        // Signed in, the dial says it renews; not signed in, it does not.
        let d = pc_dial("wss://r", "s", Some(("tok", "Desk")));
        assert!(d.headers.contains(&(REFRESH_HEADER, "1".to_string())));
        assert!(!pc_dial("wss://r", "s", None)
            .headers
            .iter()
            .any(|h| h.0 == REFRESH_HEADER));
    }

    #[test]
    fn a_fresh_token_counts_only_once_the_relay_took_it() {
        // Review four, finding 6.
        let mut r = Refresh::dialled(1_000);
        assert!(!r.due(1_000 - REFRESH_AHEAD - 1));
        let now = 1_000 - REFRESH_AHEAD;
        assert!(r.due(now));
        r.sent(now);
        // Sent, not answered: not sent again at once, but again in time.
        assert!(!r.due(now + 1));
        assert!(r.due(now + ASK_AGAIN));
        // Later: the expiry stays, a token goes again shortly.
        let later = serde_json::json!({"token": "later"});
        assert_eq!(r.answer(&later, now + 2), Some(false));
        assert_eq!(r.exp, 1_000);
        assert!(!r.due(now + 3));
        assert!(r.due(now + 2 + LATER));
        // Refused: a new one is asked for, and goes after a while.
        r.sent(now + 20);
        let refused = serde_json::json!({"token": "refused", "why": "token expired"});
        assert_eq!(r.answer(&refused, now + 21), Some(true));
        assert_eq!(r.exp, 1_000);
        assert!(!r.due(now + 22));
        assert!(r.due(now + 21 + REFUSED));
        // Taken: the relay's expiry is the link's now.
        let ok = serde_json::json!({"token": "ok", "exp": 1_300});
        assert_eq!(r.answer(&ok, now + 60), Some(false));
        assert_eq!(r.exp, 1_300);
        assert!(!r.due(now + 61));
        // Anything else from the relay is not an answer.
        assert_eq!(r.answer(&serde_json::json!({"open": 3}), now), None);
    }

    #[test]
    fn only_too_many_machines_is_told_to_the_human() {
        assert!(closed_word(Some(4429))
            .unwrap()
            .contains("too many machines"));
        assert_eq!(closed_word(Some(4401)), None);
        assert_eq!(closed_word(None), None);
    }

    #[test]
    fn a_quiet_relay_is_dead_after_seventy_seconds() {
        assert!(!silent(70_000, 0));
        assert!(silent(70_001, 0));
        assert!(!silent(5, 10));
    }

    #[test]
    fn a_signed_in_machine_brings_its_token_and_name() {
        assert_eq!(
            pc_dial("wss://r.dev/", "s+1", None),
            Dial {
                url: "wss://r.dev/pc".into(),
                headers: vec![("X-Horadric-Secret", "s+1".into())],
            }
        );
        let signed = pc_dial("wss://r.dev", "s", Some(("h.p.s", "Big PC")));
        assert_eq!(signed.url, "wss://r.dev/pc?name=Big%20PC");
        assert_eq!(
            signed.headers,
            vec![
                ("X-Horadric-Secret", "s".to_string()),
                ("Authorization", "Bearer h.p.s".to_string()),
                ("X-Horadric-Refresh", "1".to_string()),
            ]
        );
        // Nothing that proves the machine is in the address, which a
        // proxy logs.
        assert!(!signed.url.contains("h.p.s") && !signed.url.contains("secret"));
    }

    #[test]
    fn a_good_link_starts_over_and_a_short_one_keeps_backing_off() {
        assert_eq!(after_link(60, None, 16), (1, 2, false));
        assert_eq!(after_link(3, None, 16), (16, 30, false));
        assert_eq!(after_link(3, Some(4401), 4), (4, 8, true));
        assert_eq!(after_link(60, Some(4503), 1), (30, 30, false));
    }

    #[test]
    fn a_failed_try_doubles_up_to_thirty_seconds() {
        assert_eq!(after_failure(1), (1, 2));
        assert_eq!(after_failure(16), (16, 30));
        assert_eq!(after_failure(30), (30, 30));
    }
}
