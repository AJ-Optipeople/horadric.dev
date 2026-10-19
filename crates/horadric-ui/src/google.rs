//! Sign in with Google, for the backup to Google Drive. OAuth 2.0 for
//! installed apps with PKCE: the default browser opens on Google's consent
//! page (Google refuses sign in inside an embedded web view, so never the
//! browser pane) and comes back to a listener on 127.0.0.1. The refresh
//! token goes to Credential Manager, under a name with the state folder's
//! so a dev instance has its own.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

use horadric_core::google::{self, Answer, Tokens};
use horadric_core::portal::b64url;
use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

use crate::{credential, net, portal_crypto};

/// The "Horadric desktop" client in the human's Google Cloud project
/// `horadric-511810`. For an installed app Google treats the secret as
/// public: PKCE is what keeps a stolen code useless.
const CLIENT_ID: &str = "693724238201-dlmjnsqhhg2ndfb24gv2n9bolmgoee9d.apps.googleusercontent.com";
const CLIENT_SECRET: &str = "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";

/// How long the listener waits for the human to finish in the browser.
const SIGN_IN_WAIT: Duration = Duration::from_secs(300);

/// The id and secret, which `HORADRIC_GOOGLE_CLIENT_ID` and `_SECRET`
/// override to try another Cloud project.
fn client() -> (String, String) {
    let var = |k: &str, default: &str| std::env::var(k).unwrap_or_else(|_| default.into());
    (
        var("HORADRIC_GOOGLE_CLIENT_ID", CLIENT_ID),
        var("HORADRIC_GOOGLE_CLIENT_SECRET", CLIENT_SECRET),
    )
}

/// The Credential Manager name for this Horadric's refresh token.
fn target() -> String {
    format!("Horadric Google Drive ({})", horadric_hooks::state_name())
}

/// The signed in account's email, or `None` when signed out.
pub fn account() -> Option<String> {
    credential::read(&target()).map(|(email, _)| email)
}

/// Opens the consent page in the default browser and waits, up to five
/// minutes, for the human to say yes. Blocking: call it from a thread.
/// Returns the account's email.
pub fn sign_in() -> Result<String, String> {
    let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| format!("cannot listen: {e}"))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let redirect = format!("http://127.0.0.1:{port}");
    let verifier = b64url(&random::<32>());
    let state = b64url(&random::<16>());
    let (id, secret) = client();
    open_browser(&google::consent_url(
        &id,
        &redirect,
        &google::challenge(&verifier),
        &state,
    ));

    let code = wait_for_code(&listener, &state)?;
    let answer = net::request(
        "POST",
        google::TOKEN_URL,
        &[("Content-Type", "application/x-www-form-urlencoded")],
        google::form(&[
            ("code", &code),
            ("client_id", &id),
            ("client_secret", &secret),
            ("redirect_uri", &redirect),
            ("grant_type", "authorization_code"),
            ("code_verifier", &verifier),
        ])
        .as_bytes(),
        1 << 20,
    )?;
    let tokens = google::parse_tokens(&answer.body)?;
    if !tokens.grants_drive() {
        return Err(
            "Google was not given Drive: sign in again and tick the box that lets \
                    Horadric see, edit and create its own Drive files"
                .into(),
        );
    }
    let refresh = tokens
        .refresh
        .ok_or("Google sent no refresh token, so the backup could not run on its own")?;
    let email = tokens.email.unwrap_or_else(|| "Google account".into());
    credential::write(&target(), &email, refresh.as_bytes())?;
    Ok(email)
}

/// Takes connections until one brings this sign in's answer, and tells
/// the browser it can be closed.
fn wait_for_code(listener: &TcpListener, state: &str) -> Result<String, String> {
    listener
        .set_nonblocking(true)
        .map_err(|e| format!("cannot listen: {e}"))?;
    let start = Instant::now();
    loop {
        let stream = match listener.accept() {
            Ok((s, _)) => s,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if start.elapsed() > SIGN_IN_WAIT {
                    return Err("sign in timed out: nothing came back from the browser".into());
                }
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
            Err(e) => return Err(format!("cannot listen: {e}")),
        };
        let Some(line) = request_line(&stream) else {
            continue;
        };
        match google::read_answer(&line, state) {
            Answer::Code(code) => {
                reply(
                    stream,
                    "You are signed in",
                    "Horadric backs up to your Google Drive now. You can close this tab.",
                );
                return Ok(code);
            }
            Answer::Refused(why) => {
                reply(stream, "Not signed in", "Google said no, so Horadric does not back up to Google Drive. You can close this tab.");
                return Err(format!("Google refused: {why}"));
            }
            Answer::Other => reply_not_found(stream),
        }
    }
}

fn request_line(stream: &TcpStream) -> Option<String> {
    stream.set_nonblocking(false).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    // The headers are read to the end too: closing a socket with unread
    // data resets it, and the browser would show that, not the page.
    let mut header = String::new();
    while reader.read_line(&mut header).ok()? > 2 {
        header.clear();
    }
    Some(line.trim_end().to_string())
}

fn reply(mut stream: TcpStream, title: &str, text: &str) {
    let page = format!(
        "<!doctype html><meta charset=utf-8><title>Horadric</title>\
         <body style=\"font:16px system-ui;background:#111;color:#ddd;display:grid;\
         place-items:center;height:90vh\"><div><h1 style=\"font-weight:500\">{title}</h1>\
         <p>{text}</p></div>"
    );
    let _ = write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{page}",
        page.len()
    );
}

fn reply_not_found(mut stream: TcpStream) {
    let _ = stream
        .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
}

/// Forgets the account: Google revokes the token, Credential Manager
/// drops it. Signed out even when Google cannot be reached.
pub fn sign_out() {
    if let Some((_, refresh)) = credential::read(&target()) {
        let token = String::from_utf8_lossy(&refresh).into_owned();
        let _ = net::request(
            "POST",
            google::REVOKE_URL,
            &[("Content-Type", "application/x-www-form-urlencoded")],
            google::form(&[("token", &token)]).as_bytes(),
            1 << 16,
        );
    }
    credential::delete(&target());
}

/// An access token, refreshed from the stored refresh token when it is
/// about to run out.
pub struct Session {
    access: String,
    until: Instant,
}

impl Session {
    pub fn new() -> Result<Session, String> {
        let mut s = Session {
            access: String::new(),
            until: Instant::now(),
        };
        s.refresh()?;
        Ok(s)
    }

    pub fn token(&mut self) -> Result<&str, String> {
        if Instant::now() >= self.until {
            self.refresh()?;
        }
        Ok(&self.access)
    }

    /// Gets a fresh access token, also after a 401 says the old one died
    /// early.
    pub fn refresh(&mut self) -> Result<(), String> {
        let (_, refresh) = credential::read(&target()).ok_or("not signed in to Google")?;
        let refresh = String::from_utf8_lossy(&refresh).into_owned();
        let (id, secret) = client();
        let answer = net::request(
            "POST",
            google::TOKEN_URL,
            &[("Content-Type", "application/x-www-form-urlencoded")],
            google::form(&[
                ("client_id", &id),
                ("client_secret", &secret),
                ("refresh_token", &refresh),
                ("grant_type", "refresh_token"),
            ])
            .as_bytes(),
            1 << 20,
        )?;
        let Tokens {
            access, expires_in, ..
        } = google::parse_tokens(&answer.body).map_err(|why| {
            // Only a refused grant means the sign in is gone; anything
            // else is Google having a bad moment, and the next try works.
            if answer.status == 400 || answer.status == 401 {
                format!("Google sign in no longer works ({why}); sign in again")
            } else {
                format!("Google answered {}: {why}", answer.status)
            }
        })?;
        self.access = access;
        self.until = Instant::now() + Duration::from_secs(expires_in.saturating_sub(60));
        Ok(())
    }
}

fn random<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    portal_crypto::random(&mut b);
    b
}

fn open_browser(url: &str) {
    unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            &HSTRING::from(url),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        );
    }
}
