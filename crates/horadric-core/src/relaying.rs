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
