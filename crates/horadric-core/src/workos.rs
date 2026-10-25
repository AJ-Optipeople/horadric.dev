//! The pure half of signing in to the brotherhood with WorkOS AuthKit:
//! the authorize URL with PKCE, the answer the browser brings back to the
//! loopback listener, the device code flow a Linux node signs in with,
//! the authenticate requests and replies, and when an access token runs
//! out. The socket, browser and Credential Manager half
//! is in `horadric-ui/src/workos.rs`.
//!
//! The client is public: PKCE stands in for a secret, so there is no API
//! key in the app, and refresh tokens rotate on every use.

use crate::google::{self, form, query_value, Answer};
use crate::portal::unb64url;

/// The Horadric application in WorkOS's production environment, with our
/// own Google and GitHub apps behind it, and in staging, which a dev
/// instance signs in with so tests never touch real accounts' users.
pub const CLIENT_ID: &str = "client_01M5NBZGRP0NVJ69V5TP9S6PH3";
pub const DEV_CLIENT_ID: &str = "client_01M5NBZGDSBPJMPR5BSJQ9ASEA";

pub fn client_id(dev: bool) -> &'static str {
    if dev {
        DEV_CLIENT_ID
    } else {
        CLIENT_ID
    }
}

pub const AUTHORIZE_URL: &str = "https://api.workos.com/user_management/authorize";
pub const AUTHENTICATE_URL: &str = "https://api.workos.com/user_management/authenticate";

/// The loopback ports registered as redirects in the WorkOS dashboard.
/// Fixed, unlike Google's, because WorkOS matches the redirect exactly. A
/// dev instance has its own so it can sign in beside the installed one.
pub const PORT: u16 = 43127;
pub const DEV_PORT: u16 = 43128;

/// The path the browser comes back to.
pub const CALLBACK: &str = "/callback";

pub fn port(dev: bool) -> u16 {
    if dev {
        DEV_PORT
    } else {
        PORT
    }
}

pub fn redirect_uri(dev: bool) -> String {
    format!("http://127.0.0.1:{}{CALLBACK}", port(dev))
}

/// PKCE's S256 challenge, the same as Google's.
pub fn challenge(verifier: &str) -> String {
    google::challenge(verifier)
}

/// The AuthKit page the default browser opens.
pub fn authorize_url(client_id: &str, redirect: &str, challenge: &str, state: &str) -> String {
    format!(
        "{AUTHORIZE_URL}?{}",
        form(&[
            ("client_id", client_id),
            ("redirect_uri", redirect),
            ("response_type", "code"),
            ("provider", "authkit"),
            ("code_challenge", challenge),
            ("code_challenge_method", "S256"),
            ("state", state),
        ])
    )
}

/// The form that trades the browser's code for tokens. The verifier is
/// what proves this is the app that asked, as there is no secret.
pub fn code_body(client_id: &str, code: &str, verifier: &str) -> String {
    form(&[
        ("client_id", client_id),
        ("grant_type", "authorization_code"),
        ("code", code),
        ("code_verifier", verifier),
    ])
}

pub fn refresh_body(client_id: &str, refresh: &str) -> String {
    form(&[
        ("client_id", client_id),
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh),
    ])
}

/// Reads the request line, `GET /callback?code=..&state=.. HTTP/1.1`.
pub fn read_answer(request_line: &str, state: &str) -> Answer {
    let Some(target) = request_line
        .strip_prefix("GET ")
        .and_then(|r| r.split(' ').next())
    else {
        return Answer::Other;
    };
    let Some((path, query)) = target.split_once('?') else {
        return Answer::Other;
    };
    if path != CALLBACK || query_value(query, "state").as_deref() != Some(state) {
        return Answer::Other;
    }
    if let Some(code) = query_value(query, "code").filter(|c| !c.is_empty()) {
        return Answer::Code(code);
    }
    Answer::Refused(
        query_value(query, "error_description")
            .or_else(|| query_value(query, "error"))
            .unwrap_or_else(|| "no code".into()),
    )
}

/// Who signed in.
#[derive(Debug, Clone, PartialEq)]
pub struct User {
    pub id: String,
    pub email: String,
}

/// A reply from the authenticate endpoint.
#[derive(Debug, PartialEq)]
pub struct Auth {
    pub access: String,
    /// The next refresh token. The one just used may be dead now, so this
    /// one must be kept.
    pub refresh: String,
    pub user: Option<User>,
}

/// Reads an authenticate reply, or says why WorkOS refused in its words.
pub fn parse_auth(body: &[u8]) -> Result<Auth, String> {
    let v: serde_json::Value =
        serde_json::from_slice(body).map_err(|_| "WorkOS's answer is not JSON".to_string())?;
    let text =
        |v: &serde_json::Value, k: &str| v.get(k).and_then(|x| x.as_str()).map(str::to_string);
    let (Some(access), Some(refresh)) = (text(&v, "access_token"), text(&v, "refresh_token"))
    else {
        return Err(refusal(&v).unwrap_or_else(|| "no tokens".into()));
    };
    let user = v.get("user").and_then(|u| {
        Some(User {
            id: text(u, "id")?,
            email: text(u, "email").unwrap_or_default(),
        })
    });
    Ok(Auth {
        access,
        refresh,
        user,
    })
}

/// Where a machine with no browser of its own, a Linux node, starts
/// signing in: WorkOS's device code flow (RFC 8628). The human opens the
/// link it gets on any device and confirms the code.
pub const DEVICE_URL: &str = "https://api.workos.com/user_management/authorize/device";

/// The grant a device code is polled with, at [`AUTHENTICATE_URL`].
pub const DEVICE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";

/// The form that asks for a device code. Like PKCE's, it carries no
/// secret.
pub fn device_body(client_id: &str) -> String {
    form(&[("client_id", client_id)])
}

/// What WorkOS answers a device authorization with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    /// What the poll sends, never shown.
    pub device_code: String,
    /// The short code the human confirms, `BCDF-GHJK`.
    pub user_code: String,
    /// The page to type the code into.
    pub verification_uri: String,
    /// The same page with the code filled in, the link to print.
    pub verification_uri_complete: Option<String>,
    /// Seconds until the codes run out.
    pub expires_in: u64,
    /// Seconds to wait between polls.
    pub interval: u64,
}

impl Device {
    /// The link to give the human: the one with the code in it when
    /// WorkOS sent one.
    pub fn link(&self) -> &str {
        self.verification_uri_complete
            .as_deref()
            .unwrap_or(&self.verification_uri)
    }
}

/// Reads a device authorization, or says why WorkOS refused. A missing
/// `expires_in` or `interval` is RFC 8628's default, 300 and 5 seconds.
pub fn parse_device(body: &[u8]) -> Result<Device, String> {
    let v: serde_json::Value =
        serde_json::from_slice(body).map_err(|_| "WorkOS's answer is not JSON".to_string())?;
    let text = |k: &str| v.get(k).and_then(|x| x.as_str()).map(str::to_string);
    let (Some(device_code), Some(user_code), Some(verification_uri)) = (
        text("device_code"),
        text("user_code"),
        text("verification_uri"),
    ) else {
        return Err(refusal(&v).unwrap_or_else(|| "no device code".into()));
    };
    let secs = |k: &str, default| v.get(k).and_then(|x| x.as_u64()).unwrap_or(default);
    Ok(Device {
        device_code,
        user_code,
        verification_uri,
        verification_uri_complete: text("verification_uri_complete"),
        expires_in: secs("expires_in", 300),
        // At least a second, so a zero never makes a busy loop.
        interval: secs("interval", 5).max(1),
    })
}

/// The form a device code is polled with.
pub fn poll_body(client_id: &str, device_code: &str) -> String {
    form(&[
        ("grant_type", DEVICE_GRANT),
        ("device_code", device_code),
        ("client_id", client_id),
    ])
}

/// What one poll of a device code says.
#[derive(Debug, PartialEq)]
pub enum Poll {
    /// The human confirmed: signed in.
    Signed(Auth),
    /// Not yet: poll again after the interval.
    Pending,
    /// Polling too often: poll again after a longer interval, see
    /// [`slower`].
    SlowDown,
    /// The human said no. Stop.
    Denied,
    /// The codes ran out before the human confirmed. Stop, or start over.
    Expired,
    /// Anything else, in WorkOS's words. Stop.
    Failed(String),
}

/// Reads a poll's answer. Success reads as [`parse_auth`] does; the
/// OAuth errors RFC 8628 names are told apart, any other is a failure.
pub fn parse_poll(body: &[u8]) -> Poll {
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(body) else {
        return Poll::Failed("WorkOS's answer is not JSON".into());
    };
    match v.get("error").and_then(|e| e.as_str()) {
        Some("authorization_pending") => Poll::Pending,
        Some("slow_down") => Poll::SlowDown,
        Some("access_denied") => Poll::Denied,
        Some("expired_token") => Poll::Expired,
        _ => match parse_auth(body) {
            Ok(auth) => Poll::Signed(auth),
            Err(why) => Poll::Failed(why),
        },
    }
}

/// The interval after a [`Poll::SlowDown`]: five seconds more, as RFC
/// 8628 says and WorkOS's reference repeats.
pub fn slower(interval: u64) -> u64 {
    interval + 5
}

/// Why WorkOS refused, in its words.
fn refusal(v: &serde_json::Value) -> Option<String> {
    let text = |k: &str| v.get(k).and_then(|x| x.as_str()).map(str::to_string);
    text("error_description")
        .or_else(|| text("message"))
        .or_else(|| text("error"))
}

/// When an access token runs out, in seconds since 1970, from its `exp`
/// claim. Not verified: it came straight from WorkOS over TLS, and the
/// relay, which is the one that trusts it, checks the signature.
pub fn expiry(jwt: &str) -> Option<u64> {
    let payload = unb64url(jwt.split('.').nth(1)?)?;
    let v: serde_json::Value = serde_json::from_slice(&payload).ok()?;
    v.get("exp")?.as_u64()
}

/// How long before it runs out a token counts as spent, in seconds, so a
/// connection made with it does not arrive dead.
pub const MARGIN: u64 = 60;

/// Whether a token running out at `exp` is still good at `now`.
pub fn fresh(exp: u64, now: u64) -> bool {
    now + MARGIN < exp
}

/// Whether a token running out at `exp` may be handed out at `now` while
/// another caller is already refreshing it: any that has not run out yet,
/// rather than wait on WorkOS for one a minute better. With no refresh
/// under way, only a [`fresh`] one.
pub fn usable(exp: u64, now: u64, refreshing: bool) -> bool {
    if refreshing {
        now < exp
    } else {
        fresh(exp, now)
    }
}

/// What Credential Manager keeps beside the email: the refresh token and
/// the user id, so the UI knows who is signed in without asking WorkOS.
pub fn stored(refresh: &str, user: &User) -> Vec<u8> {
    serde_json::json!({ "refresh": refresh, "id": user.id })
        .to_string()
        .into_bytes()
}

/// Reads [`stored`] back as (refresh token, user id).
pub fn read_stored(blob: &[u8]) -> Option<(String, String)> {
    let v: serde_json::Value = serde_json::from_slice(blob).ok()?;
    let text = |k: &str| v.get(k)?.as_str().map(str::to_string);
    Some((text("refresh")?, text("id")?))
}

/// What one poll of a device code means for the next.
#[derive(Debug, PartialEq)]
pub enum Polled {
    /// Poll again after this many seconds.
    Wait(u64),
    /// Signed in.
    Signed(Auth),
    /// Stop, saying why in the human's words.
    Stop(String),
}

/// The step after a poll answered `poll`, polling every `interval`
/// seconds, `left` seconds before the codes run out. A slow down lasts
/// for every poll after it, so the interval it gives is the new one.
pub fn after_poll(poll: Poll, interval: u64, left: u64) -> Polled {
    let wait = match poll {
        Poll::Signed(auth) => return Polled::Signed(auth),
        Poll::Pending => interval,
        Poll::SlowDown => slower(interval),
        Poll::Denied => return Polled::Stop("the sign in was refused in the browser".into()),
        Poll::Expired => return Polled::Stop(EXPIRED.into()),
        Poll::Failed(why) => return Polled::Stop(format!("WorkOS said: {why}")),
    };
    if wait >= left {
        return Polled::Stop(EXPIRED.into());
    }
    Polled::Wait(wait)
}

const EXPIRED: &str = "the code ran out before it was confirmed; sign in again for a new one";

/// What a machine with no Credential Manager, a Linux node, keeps in
/// `workos.json`: the refresh token, who it is, and which WorkOS client
/// signed it in, so a token from staging is never refreshed against
/// production.
pub fn kept_file(refresh: &str, user: &User, client: &str) -> String {
    serde_json::json!({
        "refresh": refresh,
        "id": user.id,
        "email": user.email,
        "client": client,
    })
    .to_string()
}

/// Reads [`kept_file`] back as (refresh token, user), `None` when it is
/// not one, or was signed in with another client than `client`.
pub fn read_kept_file(text: &str, client: &str) -> Option<(String, User)> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    let field = |k: &str| v.get(k).and_then(|x| x.as_str()).map(str::to_string);
    if field("client").as_deref() != Some(client) {
        return None;
    }
    Some((
        field("refresh")?,
        User {
            id: field("id")?,
            email: field("email").unwrap_or_default(),
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::portal::b64url;

    #[test]
    fn dev_signs_in_with_staging_and_the_installed_with_production() {
        assert_eq!(client_id(true), DEV_CLIENT_ID);
        assert_eq!(client_id(false), CLIENT_ID);
        assert_ne!(CLIENT_ID, DEV_CLIENT_ID);
    }

    #[test]
    fn dev_and_installed_come_back_to_their_own_port() {
        assert_eq!(redirect_uri(false), "http://127.0.0.1:43127/callback");
        assert_eq!(redirect_uri(true), "http://127.0.0.1:43128/callback");
        assert_eq!(port(true), DEV_PORT);
    }

    #[test]
    fn the_authorize_url_asks_authkit_with_pkce() {
        let url = authorize_url(CLIENT_ID, &redirect_uri(false), "ch", "st");
        assert_eq!(
            url,
            "https://api.workos.com/user_management/authorize?\
             client_id=client_01M5NBZGRP0NVJ69V5TP9S6PH3&\
             redirect_uri=http%3A%2F%2F127.0.0.1%3A43127%2Fcallback&response_type=code&\
             provider=authkit&code_challenge=ch&code_challenge_method=S256&state=st"
        );
        assert_eq!(
            challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn the_bodies_carry_no_secret() {
        assert_eq!(
            code_body("c", "a/b", "v"),
            "client_id=c&grant_type=authorization_code&code=a%2Fb&code_verifier=v"
        );
        assert_eq!(
            refresh_body("c", "r"),
            "client_id=c&grant_type=refresh_token&refresh_token=r"
        );
    }

    #[test]
    fn the_answer_comes_to_the_callback() {
        assert_eq!(
            read_answer("GET /callback?code=01AB&state=st HTTP/1.1", "st"),
            Answer::Code("01AB".into())
        );
        assert_eq!(
            read_answer(
                "GET /callback?error=access_denied&error_description=No+thanks&state=st HTTP/1.1",
                "st"
            ),
            Answer::Refused("No thanks".into())
        );
        assert_eq!(
            read_answer("GET /callback?error=access_denied&state=st HTTP/1.1", "st"),
            Answer::Refused("access_denied".into())
        );
    }

    #[test]
    fn strays_are_not_the_answer() {
        for line in [
            "GET /?code=x&state=st HTTP/1.1",
            "GET /callback?code=x&state=old HTTP/1.1",
            "GET /favicon.ico HTTP/1.1",
            "POST /callback?code=x&state=st HTTP/1.1",
            "",
        ] {
            assert_eq!(read_answer(line, "st"), Answer::Other, "{line}");
        }
    }

    #[test]
    fn a_reply_names_the_user() {
        let body = br#"{"user":{"object":"user","id":"user_01","email":"a@b.dk","first_name":"A"},
            "organization_id":null,"access_token":"h.p.s","refresh_token":"r2",
            "authentication_method":"GoogleOAuth"}"#;
        assert_eq!(
            parse_auth(body),
            Ok(Auth {
                access: "h.p.s".into(),
                refresh: "r2".into(),
                user: Some(User {
                    id: "user_01".into(),
                    email: "a@b.dk".into()
                }),
            })
        );
        let bare = parse_auth(br#"{"access_token":"a","refresh_token":"r"}"#).unwrap();
        assert_eq!(bare.user, None);
    }

    #[test]
    fn a_refusal_says_why() {
        let body = br#"{"error":"invalid_grant","error_description":"Invalid refresh token."}"#;
        assert_eq!(parse_auth(body), Err("Invalid refresh token.".into()));
        assert_eq!(
            parse_auth(br#"{"message":"Unauthorized"}"#),
            Err("Unauthorized".into())
        );
        assert_eq!(
            parse_auth(br#"{"access_token":"a"}"#),
            Err("no tokens".into())
        );
        assert!(parse_auth(b"<html>").is_err());
    }

    /// What WorkOS's staging environment answered on 2026-10-24.
    const DEVICE: &[u8] = br#"{"device_code":"zEPhued6w9U8oh5CKa8jU0cCrA52KqmRgqSjihlwzjrqJhcQwgJgRulYnywNsvQP","user_code":"DBZB-DVPJ","verification_uri":"https://knowledgeable-sun-39-staging.authkit.app/device","verification_uri_complete":"https://knowledgeable-sun-39-staging.authkit.app/device?user_code=DBZB-DVPJ","expires_in":300,"interval":5}"#;

    #[test]
    fn a_device_code_is_asked_for_with_the_client_alone() {
        assert_eq!(device_body("client_01"), "client_id=client_01");
        assert_eq!(
            DEVICE_URL,
            "https://api.workos.com/user_management/authorize/device"
        );
    }

    #[test]
    fn a_device_authorization_reads() {
        let d = parse_device(DEVICE).unwrap();
        assert_eq!(d.user_code, "DBZB-DVPJ");
        assert_eq!(d.device_code.len(), 64);
        assert_eq!(
            d.verification_uri,
            "https://knowledgeable-sun-39-staging.authkit.app/device"
        );
        assert_eq!((d.expires_in, d.interval), (300, 5));
        assert_eq!(
            d.link(),
            "https://knowledgeable-sun-39-staging.authkit.app/device?user_code=DBZB-DVPJ"
        );
        let bare = parse_device(
            br#"{"device_code":"d","user_code":"u","verification_uri":"https://v","interval":0}"#,
        )
        .unwrap();
        assert_eq!(bare.verification_uri_complete, None);
        assert_eq!(bare.link(), "https://v");
        assert_eq!((bare.expires_in, bare.interval), (300, 1));
    }

    #[test]
    fn a_refused_device_authorization_says_why() {
        assert_eq!(
            parse_device(br#"{"error":"invalid_client","error_description":"Invalid client ID."}"#),
            Err("Invalid client ID.".into())
        );
        assert_eq!(
            parse_device(br#"{"device_code":"d"}"#),
            Err("no device code".into())
        );
        assert!(parse_device(b"<html>").is_err());
    }

    #[test]
    fn a_poll_carries_the_grant_the_code_and_the_client() {
        assert_eq!(
            poll_body("client_01", "a/b"),
            "grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code&\
             device_code=a%2Fb&client_id=client_01"
        );
    }

    #[test]
    fn a_poll_tells_waiting_from_stopping() {
        let err = |e: &str| format!(r#"{{"error":"{e}","error_description":"words"}}"#);
        assert_eq!(
            parse_poll(err("authorization_pending").as_bytes()),
            Poll::Pending
        );
        assert_eq!(parse_poll(err("slow_down").as_bytes()), Poll::SlowDown);
        assert_eq!(parse_poll(err("access_denied").as_bytes()), Poll::Denied);
        assert_eq!(parse_poll(err("expired_token").as_bytes()), Poll::Expired);
        assert_eq!(
            parse_poll(
                br#"{"error":"invalid_grant","error_description":"The device code provided is invalid, expired, or has already been used."}"#
            ),
            Poll::Failed("The device code provided is invalid, expired, or has already been used.".into())
        );
        assert_eq!(
            parse_poll(br#"{"error":"invalid_client"}"#),
            Poll::Failed("invalid_client".into())
        );
        assert!(matches!(parse_poll(b"<html>"), Poll::Failed(_)));
    }

    #[test]
    fn a_confirmed_poll_signs_in() {
        let body = br#"{"user":{"object":"user","id":"user_01","email":"a@b.dk"},
            "organization_id":null,"authkit_authorization_code":"authkit_authz_code_abc123",
            "access_token":"h.p.s","refresh_token":"r2","authentication_method":"GoogleOAuth"}"#;
        assert_eq!(
            parse_poll(body),
            Poll::Signed(Auth {
                access: "h.p.s".into(),
                refresh: "r2".into(),
                user: Some(User {
                    id: "user_01".into(),
                    email: "a@b.dk".into()
                }),
            })
        );
    }

    #[test]
    fn slowing_down_waits_five_seconds_more() {
        assert_eq!(slower(5), 10);
        assert_eq!(slower(10), 15);
    }

    #[test]
    fn expiry_is_read_from_the_payload() {
        let payload = b64url(br#"{"sub":"user_01","sid":"session_1","exp":1761300000}"#);
        assert_eq!(expiry(&format!("h.{payload}.s")), Some(1_761_300_000));
        assert_eq!(expiry("not a jwt"), None);
        assert_eq!(expiry(&format!("h.{}.s", b64url(b"{}"))), None);
    }

    #[test]
    fn a_token_is_spent_a_minute_early() {
        assert!(fresh(1000, 900));
        assert!(!fresh(1000, 940));
        assert!(!fresh(1000, 2000));
    }

    #[test]
    fn a_token_still_good_is_handed_out_while_another_refreshes() {
        assert!(usable(1000, 900, false));
        assert!(!usable(1000, 960, false));
        assert!(usable(1000, 960, true));
        assert!(!usable(1000, 1000, true));
        assert!(!usable(1000, 2000, true));
    }

    #[test]
    fn the_stored_secret_reads_back() {
        let user = User {
            id: "user_01".into(),
            email: "a@b.dk".into(),
        };
        assert_eq!(
            read_stored(&stored("r\"1", &user)),
            Some(("r\"1".into(), "user_01".into()))
        );
        assert_eq!(read_stored(b"old"), None);
        assert_eq!(read_stored(br#"{"refresh":"r"}"#), None);
    }

    #[test]
    fn a_poll_says_when_to_poll_again_and_when_to_stop() {
        assert_eq!(after_poll(Poll::Pending, 5, 300), Polled::Wait(5));
        assert_eq!(after_poll(Poll::SlowDown, 5, 300), Polled::Wait(10));
        assert!(
            matches!(after_poll(Poll::Pending, 5, 4), Polled::Stop(w) if w.contains("ran out"))
        );
        assert!(
            matches!(after_poll(Poll::Expired, 5, 300), Polled::Stop(w) if w.contains("ran out"))
        );
        assert!(
            matches!(after_poll(Poll::Denied, 5, 300), Polled::Stop(w) if w.contains("refused"))
        );
        assert_eq!(
            after_poll(Poll::Failed("no".into()), 5, 300),
            Polled::Stop("WorkOS said: no".into())
        );
        let auth = Auth {
            access: "a".into(),
            refresh: "r".into(),
            user: None,
        };
        assert!(matches!(
            after_poll(Poll::Signed(auth), 5, 1),
            Polled::Signed(_)
        ));
    }

    #[test]
    fn the_kept_file_reads_back_for_its_own_client_only() {
        let user = User {
            id: "user_01".into(),
            email: "a@b.dk".into(),
        };
        let text = kept_file("r\"1", &user, DEV_CLIENT_ID);
        assert_eq!(
            read_kept_file(&text, DEV_CLIENT_ID),
            Some(("r\"1".into(), user))
        );
        assert_eq!(read_kept_file(&text, CLIENT_ID), None);
        assert_eq!(read_kept_file("{}", CLIENT_ID), None);
        assert_eq!(read_kept_file("old", CLIENT_ID), None);
    }
}
