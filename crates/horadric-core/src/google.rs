//! The pure half of Google sign in and Google Drive: the consent URL with
//! PKCE, the answer the browser brings back to the loopback listener, the
//! token and file list replies, and Drive's query language. The WinHTTP
//! and socket half is in `horadric-ui/src/google.rs` and `drive.rs`. See
//! docs/plan/backup.md, Google Drive.

use crate::portal::{b64url, sha256, unb64url, uri_encode};

pub const AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
pub const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
pub const REVOKE_URL: &str = "https://oauth2.googleapis.com/revoke";
pub const FILES_URL: &str = "https://www.googleapis.com/drive/v3/files";
pub const UPLOAD_URL: &str = "https://www.googleapis.com/upload/drive/v3/files";

/// `drive.file` sees only the files Horadric made, and is not a sensitive
/// scope, so no security review and no "unverified app" warning. `openid
/// email` names the account in Settings.
pub const SCOPE: &str = "https://www.googleapis.com/auth/drive.file openid email";

pub const FOLDER_MIME: &str = "application/vnd.google-apps.folder";

/// The folder every machine's backup sits in, at the top of the Drive.
pub const BACKUP_FOLDER: &str = "Horadric backup";

/// PKCE's S256 challenge for `verifier` (RFC 7636).
pub fn challenge(verifier: &str) -> String {
    b64url(&sha256(verifier.as_bytes()))
}

/// The page the default browser opens. `prompt=consent` with offline
/// access makes Google send a refresh token every time, not only on the
/// first sign in, so signing in again after a sign out works.
pub fn consent_url(client_id: &str, redirect: &str, challenge: &str, state: &str) -> String {
    format!(
        "{AUTH_URL}?{}",
        form(&[
            ("client_id", client_id),
            ("redirect_uri", redirect),
            ("response_type", "code"),
            ("scope", SCOPE),
            ("code_challenge", challenge),
            ("code_challenge_method", "S256"),
            ("state", state),
            ("access_type", "offline"),
            ("prompt", "consent"),
        ])
    )
}

/// `application/x-www-form-urlencoded`, also good for a query string.
pub fn form(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{k}={}", uri_encode(v)))
        .collect::<Vec<_>>()
        .join("&")
}

/// What the browser brought to the loopback listener.
#[derive(Debug, PartialEq)]
pub enum Answer {
    Code(String),
    /// Google said no, usually because the human pressed Cancel.
    Refused(String),
    /// Not the answer: a favicon, a probe, or a stale tab whose state is
    /// not this sign in's. The listener keeps waiting.
    Other,
}

/// Reads the request line, `GET /?state=..&code=.. HTTP/1.1`.
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
    if path != "/" || query_value(query, "state").as_deref() != Some(state) {
        return Answer::Other;
    }
    if let Some(code) = query_value(query, "code").filter(|c| !c.is_empty()) {
        return Answer::Code(code);
    }
    Answer::Refused(query_value(query, "error").unwrap_or_else(|| "no code".into()))
}

/// One value of a query string, percent and plus decoded.
pub fn query_value(query: &str, key: &str) -> Option<String> {
    query.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        (k == key).then(|| percent_decode(v)).flatten()
    })
}

fn percent_decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' => {
                let hex = s.get(i + 1..i + 3)?;
                out.push(u8::from_str_radix(hex, 16).ok()?);
                i += 2;
            }
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8(out).ok()
}

/// A reply from the token endpoint.
#[derive(Debug, PartialEq)]
pub struct Tokens {
    pub access: String,
    pub expires_in: u64,
    /// Only on the first exchange; a refresh keeps the old one.
    pub refresh: Option<String>,
    /// From the id token, only when `openid email` was asked for.
    pub email: Option<String>,
}

/// Reads a token reply, or says why Google refused in its own words.
pub fn parse_tokens(body: &[u8]) -> Result<Tokens, String> {
    let v: serde_json::Value =
        serde_json::from_slice(body).map_err(|_| "Google's answer is not JSON".to_string())?;
    let text = |k: &str| v.get(k).and_then(|x| x.as_str()).map(str::to_string);
    let Some(access) = text("access_token") else {
        let why = text("error_description")
            .or_else(|| text("error"))
            .unwrap_or_else(|| "no access token".into());
        return Err(why);
    };
    Ok(Tokens {
        access,
        expires_in: v.get("expires_in").and_then(|x| x.as_u64()).unwrap_or(3600),
        refresh: text("refresh_token"),
        email: text("id_token").and_then(|t| email_in_id_token(&t)),
    })
}

/// The `email` claim of a JWT. The signature is not checked: the token
/// came straight from Google over TLS, and it only names the account.
pub fn email_in_id_token(jwt: &str) -> Option<String> {
    let payload = unb64url(jwt.split('.').nth(1)?)?;
    let v: serde_json::Value = serde_json::from_slice(&payload).ok()?;
    v.get("email")?.as_str().map(str::to_string)
}

/// A file or folder as Drive lists it.
#[derive(Debug, Clone, PartialEq)]
pub struct DriveFile {
    pub id: String,
    pub name: String,
}

/// A page of `files.list`, and the token for the next page if any.
pub fn parse_files(body: &[u8]) -> Result<(Vec<DriveFile>, Option<String>), String> {
    let v: serde_json::Value =
        serde_json::from_slice(body).map_err(|_| "Drive's answer is not JSON".to_string())?;
    let files = v
        .get("files")
        .and_then(|f| f.as_array())
        .ok_or("Drive's answer has no file list")?
        .iter()
        .filter_map(|f| {
            Some(DriveFile {
                id: f.get("id")?.as_str()?.to_string(),
                name: f.get("name")?.as_str()?.to_string(),
            })
        })
        .collect();
    let next = v
        .get("nextPageToken")
        .and_then(|t| t.as_str())
        .map(str::to_string);
    Ok((files, next))
}

/// The id in a reply that describes one file.
pub fn parse_id(body: &[u8]) -> Option<String> {
    let v: serde_json::Value = serde_json::from_slice(body).ok()?;
    v.get("id")?.as_str().map(str::to_string)
}

/// A string literal in Drive's query language.
pub fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'"))
}

/// Everything not trashed in `parent`, folders only when `folders`.
pub fn children_query(parent: &str, folders: bool) -> String {
    let mut q = format!("{} in parents and trashed = false", quote(parent));
    if folders {
        q.push_str(&format!(" and mimeType = {}", quote(FOLDER_MIME)));
    }
    q
}

/// The folder named `name` at the top of the Drive (`root`) or in `parent`.
pub fn folder_query(name: &str, parent: &str) -> String {
    format!(
        "name = {} and {}",
        quote(name),
        children_query(parent, true)
    )
}

/// The metadata that creates a file, or a folder when `folder`.
pub fn metadata(name: &str, parent: &str, folder: bool) -> String {
    let mut v = serde_json::json!({ "name": name, "parents": [parent] });
    if folder {
        v["mimeType"] = FOLDER_MIME.into();
    }
    v.to_string()
}

/// The URL that lists `query`'s matches, a page at a time.
pub fn list_url(query: &str, page: Option<&str>) -> String {
    let mut pairs = vec![
        ("q", query),
        ("fields", "nextPageToken,files(id,name)"),
        ("pageSize", "1000"),
        ("spaces", "drive"),
    ];
    if let Some(p) = page {
        pairs.push(("pageToken", p));
    }
    format!("{FILES_URL}?{}", form(&pairs))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn challenge_matches_rfc_7636() {
        assert_eq!(
            challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn consent_url_carries_everything_encoded() {
        let url = consent_url("id.apps", "http://127.0.0.1:5000", "ch", "st");
        assert!(url.starts_with("https://accounts.google.com/o/oauth2/v2/auth?client_id=id.apps&"));
        assert!(url.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A5000"));
        assert!(url.contains(
            "scope=https%3A%2F%2Fwww.googleapis.com%2Fauth%2Fdrive.file%20openid%20email"
        ));
        assert!(url.contains("code_challenge=ch&code_challenge_method=S256&state=st"));
        assert!(url.contains("access_type=offline"));
    }

    #[test]
    fn the_answer_is_read_from_the_request_line() {
        assert_eq!(
            read_answer("GET /?state=st&code=4%2F0Ab&scope=email HTTP/1.1", "st"),
            Answer::Code("4/0Ab".into())
        );
        assert_eq!(
            read_answer("GET /?error=access_denied&state=st HTTP/1.1", "st"),
            Answer::Refused("access_denied".into())
        );
    }

    #[test]
    fn strays_are_not_the_answer() {
        assert_eq!(
            read_answer("GET /favicon.ico HTTP/1.1", "st"),
            Answer::Other
        );
        assert_eq!(
            read_answer("GET /?state=old&code=x HTTP/1.1", "st"),
            Answer::Other
        );
        assert_eq!(
            read_answer("POST /?state=st&code=x HTTP/1.1", "st"),
            Answer::Other
        );
        assert_eq!(read_answer("", "st"), Answer::Other);
    }

    #[test]
    fn query_values_decode_plus_and_percent() {
        assert_eq!(query_value("a=1&b=x+y%21", "b").as_deref(), Some("x y!"));
        assert_eq!(query_value("a=1", "b"), None);
        assert_eq!(query_value("b=%zz", "b"), None);
    }

    #[test]
    fn tokens_parse_with_the_email_from_the_id_token() {
        let payload = b64url(br#"{"email":"a@b.dk","sub":"1"}"#);
        let body = format!(
            r#"{{"access_token":"ya29","expires_in":3599,"refresh_token":"1//r","id_token":"h.{payload}.s"}}"#
        );
        assert_eq!(
            parse_tokens(body.as_bytes()),
            Ok(Tokens {
                access: "ya29".into(),
                expires_in: 3599,
                refresh: Some("1//r".into()),
                email: Some("a@b.dk".into()),
            })
        );
    }

    #[test]
    fn a_refusal_says_why() {
        let body = br#"{"error":"invalid_grant","error_description":"Token has been expired or revoked."}"#;
        assert_eq!(
            parse_tokens(body),
            Err("Token has been expired or revoked.".into())
        );
        assert!(parse_tokens(b"<html>").is_err());
    }

    #[test]
    fn file_lists_parse_with_their_next_page() {
        let body = br#"{"nextPageToken":"p2","files":[{"id":"1","name":"a.hbk"},{"id":"2"}]}"#;
        let (files, next) = parse_files(body).unwrap();
        assert_eq!(
            files,
            vec![DriveFile {
                id: "1".into(),
                name: "a.hbk".into()
            }]
        );
        assert_eq!(next.as_deref(), Some("p2"));
        assert!(parse_files(br#"{"error":{}}"#).is_err());
        assert_eq!(parse_id(br#"{"id":"x"}"#).as_deref(), Some("x"));
    }

    #[test]
    fn queries_quote_their_strings() {
        assert_eq!(quote(r"it's a\b"), r"'it\'s a\\b'");
        assert_eq!(
            folder_query("Horadric backup", "root"),
            "name = 'Horadric backup' and 'root' in parents and trashed = false \
             and mimeType = 'application/vnd.google-apps.folder'"
        );
        assert!(list_url("x", Some("p")).ends_with("&pageToken=p"));
    }

    #[test]
    fn metadata_names_the_parent() {
        let v: serde_json::Value = serde_json::from_str(&metadata("m", "p", true)).unwrap();
        assert_eq!(v["parents"][0], "p");
        assert_eq!(v["mimeType"], FOLDER_MIME);
        let v: serde_json::Value = serde_json::from_str(&metadata("a/b", "p", false)).unwrap();
        assert_eq!(v["name"], "a/b");
        assert!(v.get("mimeType").is_none());
    }
}
