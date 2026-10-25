//! Signing a node in to the brotherhood (see docs/plan/the-brotherhood.md,
//! "Linux nodes on the relay"). A server has no browser, so it takes
//! WorkOS's device code flow: it prints a short code and a link, the human
//! confirms it in any browser, and the node polls until they have. The
//! refresh token goes in `workos.json` in the state folder, readable by
//! its owner alone, since a server has no Credential Manager. The pure
//! half is `horadric_core::workos`.
//!
//! The running node refreshes the access token its relay link carries.
//! Refresh tokens rotate on every use, so a refresh holds a lock in this
//! process and a file lock across processes: two refreshing with one token
//! would sign the second out. The token in hand has a lock of its own,
//! never held through a refresh, so whoever only reads it or drops it
//! (`renew`, from a terminal) never waits on WorkOS.

use std::fs::{File, OpenOptions};
use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::sync::{Mutex, TryLockError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use horadric_core::portal::write_private;
use horadric_core::workos::{self, Polled, User};
use horadric_link::http;

/// The file the refresh token is kept in, in the state folder.
const FILE: &str = "workos.json";

/// Taken around every read and write of [`FILE`] that may rotate it.
const LOCK: &str = "workos.lock";

/// The access token in hand and when it runs out. Held only to read or
/// set it.
static ACCESS: Mutex<Option<(String, u64)>> = Mutex::new(None);

/// Held through a refresh, so this process refreshes once at a time.
/// [`Locked`] does the same across processes.
static REFRESHING: Mutex<()> = Mutex::new(());

/// A dev instance signs in with WorkOS's staging environment, so it groups
/// with a Windows dev instance signed in there.
fn client() -> &'static str {
    workos::client_id(horadric_hooks::dev())
}

fn dir() -> Result<PathBuf, String> {
    horadric_hooks::state_dir().ok_or_else(|| "cannot find the state folder".to_string())
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// An exclusive lock on [`LOCK`], let go when dropped.
struct Locked(File);

impl Locked {
    fn take() -> Result<Locked, String> {
        let dir = dir()?;
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let f = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join(LOCK))
            .map_err(|e| format!("cannot lock the sign in: {e}"))?;
        if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX) } != 0 {
            return Err("cannot lock the sign in".into());
        }
        Ok(Locked(f))
    }
}

impl Drop for Locked {
    fn drop(&mut self) {
        unsafe {
            libc::flock(self.0.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

/// The refresh token and who it is, as kept.
fn read() -> Option<(String, User)> {
    let text = std::fs::read_to_string(dir().ok()?.join(FILE)).ok()?;
    workos::read_kept_file(&text, client())
}

fn keep(refresh: &str, user: &User) -> Result<(), String> {
    let path = dir()?.join(FILE);
    write_private(&path, workos::kept_file(refresh, user, client()).as_bytes())
        .map_err(|e| format!("cannot keep the sign in in {}: {e}", path.display()))
}

/// Who is signed in here, without asking WorkOS.
pub(super) fn user() -> Option<User> {
    read().map(|(_, u)| u)
}

/// Signs in with the device code flow, telling the human in the terminal
/// what to open and which code to see there. Blocks until they confirm,
/// refuse, or the code runs out.
pub(super) fn device() -> Result<User, String> {
    let answer = http::post_form(workos::DEVICE_URL, &workos::device_body(client()), 1 << 16)
        .map_err(|e| format!("cannot reach WorkOS: {e}"))?;
    let device = workos::parse_device(&answer.body)
        .map_err(|why| format!("WorkOS gave no code ({}): {why}", answer.status))?;
    println!();
    println!("To sign this node in, open this link in any browser:");
    println!();
    println!("  {}", device.link());
    println!();
    println!("and check that it shows the code  {}", device.user_code);
    if horadric_hooks::dev() {
        println!("(a dev node: WorkOS's staging environment)");
    }
    println!();
    println!("Waiting for you to confirm it there...");
    let started = Instant::now();
    let mut interval = device.interval;
    loop {
        std::thread::sleep(Duration::from_secs(interval));
        let left = device
            .expires_in
            .saturating_sub(started.elapsed().as_secs());
        let body = workos::poll_body(client(), &device.device_code);
        let poll = match http::post_form(workos::AUTHENTICATE_URL, &body, 1 << 20) {
            Ok(a) => workos::parse_poll(&a.body),
            // The network came and went: the code is still good, so poll
            // on until it runs out.
            Err(e) if left > interval => {
                eprintln!("horadric: cannot reach WorkOS, trying again: {e}");
                continue;
            }
            Err(e) => return Err(format!("cannot reach WorkOS: {e}")),
        };
        match workos::after_poll(poll, interval, left) {
            Polled::Wait(next) => interval = next,
            Polled::Stop(why) => return Err(why),
            Polled::Signed(auth) => {
                let user = auth.user.ok_or("WorkOS did not say who signed in")?;
                {
                    let _locked = Locked::take()?;
                    keep(&auth.refresh, &user)?;
                }
                hold(auth.access);
                return Ok(user);
            }
        }
    }
}

/// Forgets the account here. The WorkOS session itself ends when its
/// refresh token is never used again.
pub(super) fn sign_out() -> Result<bool, String> {
    let _locked = Locked::take()?;
    forget_access();
    let path = dir()?.join(FILE);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// The access token in hand, if it may be handed out now: see
/// [`workos::usable`].
fn in_hand(refreshing: bool) -> Option<String> {
    let held = ACCESS.lock().unwrap_or_else(|e| e.into_inner());
    held.as_ref()
        .filter(|(_, exp)| workos::usable(*exp, now(), refreshing))
        .map(|(token, _)| token.clone())
}

/// A good access token, refreshed and the rotated refresh token kept when
/// the one in hand is about to run out. Blocks while it refreshes, unless
/// another caller is refreshing already and the one in hand has not run
/// out yet.
pub(super) fn access_token() -> Result<String, String> {
    if let Some(token) = in_hand(false) {
        return Ok(token);
    }
    let _one = match REFRESHING.try_lock() {
        Ok(one) => one,
        Err(TryLockError::Poisoned(e)) => e.into_inner(),
        Err(TryLockError::WouldBlock) => {
            if let Some(token) = in_hand(true) {
                return Ok(token);
            }
            REFRESHING.lock().unwrap_or_else(|e| e.into_inner())
        }
    };
    // Whoever held it before may have just refreshed.
    if let Some(token) = in_hand(false) {
        return Ok(token);
    }
    let _locked = Locked::take()?;
    // Read under the file lock: another process may have rotated it.
    let (refresh, user) = read().ok_or("not signed in")?;
    let body = workos::refresh_body(client(), &refresh);
    let answer = http::post_form(workos::AUTHENTICATE_URL, &body, 1 << 20)
        .map_err(|e| format!("cannot reach WorkOS: {e}"))?;
    let auth = workos::parse_auth(&answer.body).map_err(|why| {
        format!(
            "WorkOS refused to renew the sign in ({}): {why}; run `horadric node signin` again",
            answer.status
        )
    })?;
    keep(&auth.refresh, auth.user.as_ref().unwrap_or(&user))?;
    hold(auth.access.clone());
    Ok(auth.access)
}

/// Drops the access token in hand, so the next [`access_token`]
/// refreshes: for one the relay refused (4401), and after a sign in or
/// out by another process.
pub(super) fn forget_access() {
    *ACCESS.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

fn hold(access: String) {
    let exp = workos::expiry(&access).unwrap_or(0);
    *ACCESS.lock().unwrap_or_else(|e| e.into_inner()) = Some((access, exp));
}
