//! Signing in to the brotherhood with WorkOS AuthKit, from the tray. The
//! default browser opens on AuthKit (never the browser pane, as for
//! Google) and comes back to a listener on 127.0.0.1 at a fixed port,
//! since WorkOS matches the redirect exactly. The refresh token goes to
//! Credential Manager under a name with the state folder's, so a dev
//! instance signs in on its own. The pure half is in
//! `horadric-core/src/workos.rs`.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use horadric_core::google::Answer;
use horadric_core::portal::b64url;
use horadric_core::workos::{self, User};
use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{
    MessageBoxW, MB_ICONWARNING, MB_OK, MB_SETFOREGROUND, SW_SHOWNORMAL,
};

use crate::{credential, net, portal_crypto};

/// How long the listener waits for the human to finish in the browser.
/// Long enough to make an account and click an email link.
const SIGN_IN_WAIT: Duration = Duration::from_secs(600);

/// The access token in hand and when it runs out. Behind a lock that is
/// held through a refresh: the refresh token rotates, so two threads
/// refreshing with the same one would sign the second out.
static ACCESS: Mutex<Option<(String, u64)>> = Mutex::new(None);

/// A sign in is waiting on the browser, so a second click does not try
/// to listen on the same port.
static SIGNING_IN: AtomicBool = AtomicBool::new(false);

/// The Credential Manager name for this Horadric's refresh token.
fn target() -> String {
    format!("Horadric WorkOS ({})", horadric_hooks::state_name())
}

/// Who is signed in, or `None` when signed out. Read from Credential
/// Manager, so it asks no one and is cheap enough for a menu.
pub fn user() -> Option<User> {
    let (email, blob) = credential::read(&target())?;
    let (_, id) = workos::read_stored(&blob)?;
    Some(User { id, email })
}

/// Starts a sign in on a thread of its own, says why when it fails, and
/// tells the portal link to connect again as the signed in PC when it
/// works.
pub fn sign_in_in_background() {
    if SIGNING_IN.swap(true, Ordering::SeqCst) {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("workos-sign-in".into())
        .spawn(|| {
            let done = sign_in();
            SIGNING_IN.store(false, Ordering::SeqCst);
            match done {
                Ok(_) => crate::portal_link::renew(),
                Err(why) => warn("Not signed in", &why),
            }
        });
    if spawned.is_err() {
        SIGNING_IN.store(false, Ordering::SeqCst);
    }
}

/// Opens AuthKit in the default browser and waits, up to ten minutes, for
/// the human to finish. Blocking: call it from a thread.
pub fn sign_in() -> Result<User, String> {
    let dev = horadric_hooks::dev();
    let listener = TcpListener::bind(("127.0.0.1", workos::port(dev))).map_err(|e| {
        format!(
            "cannot listen on port {}, which WorkOS sends the browser back to: {e}",
            workos::port(dev)
        )
    })?;
    let verifier = b64url(&random::<32>());
    let state = b64url(&random::<16>());
    open_browser(&workos::authorize_url(
        workos::client_id(dev),
        &workos::redirect_uri(dev),
        &workos::challenge(&verifier),
        &state,
    ));
    let code = wait_for_code(&listener, &state)?;
    let auth = authenticate(&workos::code_body(
        workos::client_id(horadric_hooks::dev()),
        &code,
        &verifier,
    ))?;
    let user = auth.user.ok_or("WorkOS did not say who signed in")?;
    credential::write(
        &target(),
        &user.email,
        &workos::stored(&auth.refresh, &user),
    )?;
    keep(auth.access);
    Ok(user)
}

/// Forgets the account here. The WorkOS session itself ends when its
/// refresh token is never used again.
pub fn sign_out() {
    credential::delete(&target());
    *ACCESS.lock().unwrap_or_else(|e| e.into_inner()) = None;
    crate::portal_link::renew();
}

/// A good access token, refreshed and the rotated refresh token kept when
/// the one in hand is about to run out. Blocking when it refreshes.
pub fn access_token() -> Result<String, String> {
    let mut held = ACCESS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((token, exp)) = held.as_ref() {
        if workos::fresh(*exp, now()) {
            return Ok(token.clone());
        }
    }
    let (email, blob) = credential::read(&target()).ok_or("not signed in")?;
    let (refresh, id) = workos::read_stored(&blob).ok_or("not signed in")?;
    let auth = authenticate(&workos::refresh_body(
        workos::client_id(horadric_hooks::dev()),
        &refresh,
    ))?;
    let user = auth.user.unwrap_or(User { id, email });
    credential::write(
        &target(),
        &user.email,
        &workos::stored(&auth.refresh, &user),
    )?;
    let exp = workos::expiry(&auth.access).unwrap_or(0);
    *held = Some((auth.access.clone(), exp));
    Ok(auth.access)
}

/// Drops the access token in hand, so the next [`access_token`]
/// refreshes. For a token the relay refused (4401) while this clock still
/// thought it good.
pub fn forget_access() {
    *ACCESS.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

fn keep(access: String) {
    let exp = workos::expiry(&access).unwrap_or(0);
    *ACCESS.lock().unwrap_or_else(|e| e.into_inner()) = Some((access, exp));
}

fn authenticate(body: &str) -> Result<workos::Auth, String> {
    let answer = net::request(
        "POST",
        workos::AUTHENTICATE_URL,
        &[("Content-Type", "application/x-www-form-urlencoded")],
        body.as_bytes(),
        1 << 20,
    )?;
    workos::parse_auth(&answer.body)
        .map_err(|why| format!("WorkOS answered {}: {why}", answer.status))
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
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
        match workos::read_answer(&line, state) {
            Answer::Code(code) => {
                reply(
                    stream,
                    "You are signed in",
                    "This PC can find its brothers now. You can close this tab.",
                );
                return Ok(code);
            }
            Answer::Refused(why) => {
                reply(
                    stream,
                    "Not signed in",
                    "Horadric was not signed in. You can close this tab.",
                );
                return Err(format!("WorkOS refused: {why}"));
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

/// A plain message box, which is safe off the UI thread where the sign in
/// ends.
fn warn(title: &str, text: &str) {
    unsafe {
        MessageBoxW(
            None,
            &HSTRING::from(text),
            &HSTRING::from(title),
            MB_OK | MB_ICONWARNING | MB_SETFOREGROUND,
        );
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
