//! The pure half of signing in to the brotherhood with WorkOS AuthKit:
//! the authorize URL with PKCE, the answer the browser brings back to the
//! loopback listener, the authenticate requests and replies, and when an
//! access token runs out. The socket, browser and Credential Manager half
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
        let why = text(&v, "error_description")
            .or_else(|| text(&v, "message"))
            .or_else(|| text(&v, "error"))
            .unwrap_or_else(|| "no tokens".into());
        return Err(why);
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
}
