//! Claude Code's login on disk, and the logins Horadric keeps aside for
//! switching, see [`horadric_core::accounts`].
//!
//! The kept logins are in `accounts.dat` beside the state file, sealed with
//! DPAPI so they read back only for this Windows user. Claude Code's own
//! `.credentials.json` is plain text, so this is no weaker than what it
//! copies, and a copy of the folder elsewhere reads as nothing.

use std::fs;
use std::io;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use horadric_core::accounts::{self, Account, Accounts};
use serde_json::Value;
use windows::Win32::Foundation::{LocalFree, HLOCAL};
use windows::Win32::Security::Cryptography::{
    CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
};

use crate::{console, store};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

fn config_dir() -> Option<PathBuf> {
    std::env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from)
}

fn home() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
}

/// `.credentials.json`, in `CLAUDE_CONFIG_DIR` or `~/.claude`.
fn credentials_path() -> Option<PathBuf> {
    let dir = config_dir().or_else(|| Some(home()?.join(".claude")))?;
    Some(dir.join(".credentials.json"))
}

/// `.claude.json`, in `CLAUDE_CONFIG_DIR` or the home folder.
fn config_path() -> Option<PathBuf> {
    Some(config_dir().or_else(home)?.join(".claude.json"))
}

/// Why this instance may not switch, if it may not. A dev instance must
/// not log the real Claude Code into another account, so it switches only
/// a Claude Code of its own, one with `CLAUDE_CONFIG_DIR` set.
pub fn refused() -> Option<&'static str> {
    (horadric_hooks::dev() && config_dir().is_none())
        .then_some("A dev instance switches only with CLAUDE_CONFIG_DIR set.")
}

fn read_json(path: &Path) -> io::Result<Value> {
    let bytes = fs::read(path)?;
    serde_json::from_slice(&bytes).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// Written beside and renamed over, so Claude Code never reads half a file.
fn write_json(path: &Path, value: &Value) -> io::Result<()> {
    let tmp = path.with_extension("json.horadric-tmp");
    let mut text = serde_json::to_string_pretty(value)?;
    text.push('\n');
    fs::write(&tmp, text)?;
    fs::rename(&tmp, path)
}

/// Who is logged in now, by id, from `.claude.json` alone.
pub fn live_id() -> Option<String> {
    accounts::identify(&read_json(&config_path()?).ok()?)
}

/// The login in use now, whole.
pub fn live() -> Option<Account> {
    let credentials = read_json(&credentials_path()?).ok()?;
    let config = read_json(&config_path()?).ok()?;
    Account::capture(&credentials, &config)
}

/// When either file last changed, so the app reads them only then.
pub fn stamp() -> Option<(SystemTime, SystemTime)> {
    let at = |p: Option<PathBuf>| fs::metadata(p?).ok()?.modified().ok();
    Some((at(credentials_path())?, at(config_path())?))
}

/// Logs Claude Code in as `account`: its token into `.credentials.json`
/// and its profile into `.claude.json`, the rest of each as it was.
pub fn put(account: &Account) -> io::Result<()> {
    let missing = || io::Error::new(io::ErrorKind::NotFound, "no home folder");
    let credentials_path = credentials_path().ok_or_else(missing)?;
    let config_path = config_path().ok_or_else(missing)?;
    let mut credentials = read_json(&credentials_path).unwrap_or(Value::Null);
    let mut config = read_json(&config_path)?;
    account.put_credentials(&mut credentials);
    account.put_profile(&mut config);
    write_json(&credentials_path, &credentials)?;
    write_json(&config_path, &config)
}

/// Opens `claude auth login` in a console of its own. It is a login, not a
/// session, so it gets no pane. The app sees the new login in the files.
pub fn log_in() -> io::Result<()> {
    let claude = console::claude_program()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "claude not found on PATH"))?;
    // `start` gives it a console with working handles, which a program
    // without one cannot hand down.
    let mut command = std::process::Command::new("cmd.exe");
    command
        .raw_arg(format!(
            "/c start \"Log in to Claude\" \"{}\" auth login",
            claude.display()
        ))
        .creation_flags(CREATE_NO_WINDOW);
    let tags = [horadric_hooks::SESSION_ENV, horadric_hooks::OWNER_ENV];
    for name in console::PARENT_SESSION_ENV.iter().chain(&tags) {
        command.env_remove(name);
    }
    command.spawn().map(drop)
}

fn path() -> Option<PathBuf> {
    Some(store::dir()?.join("accounts.dat"))
}

pub fn load() -> Accounts {
    path()
        .and_then(|p| fs::read(p).ok())
        .and_then(|sealed| unseal(&sealed))
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

pub fn save(all: &Accounts) -> io::Result<()> {
    let path = path().ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no %APPDATA%"))?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let sealed = seal(&serde_json::to_vec(all)?).ok_or_else(|| io::Error::other("DPAPI"))?;
    let tmp = path.with_extension("dat.tmp");
    fs::write(&tmp, sealed)?;
    fs::rename(&tmp, &path)
}

fn blob(bytes: &[u8]) -> CRYPT_INTEGER_BLOB {
    CRYPT_INTEGER_BLOB {
        cbData: bytes.len() as u32,
        pbData: bytes.as_ptr() as *mut u8,
    }
}

/// Takes the bytes DPAPI handed out and frees its copy.
unsafe fn taken(out: CRYPT_INTEGER_BLOB) -> Vec<u8> {
    let bytes = std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec();
    let _ = LocalFree(Some(HLOCAL(out.pbData as *mut _)));
    bytes
}

fn seal(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut out = CRYPT_INTEGER_BLOB::default();
    unsafe {
        CryptProtectData(
            &blob(bytes),
            None,
            None,
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut out,
        )
        .ok()?;
        Some(taken(out))
    }
}

fn unseal(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut out = CRYPT_INTEGER_BLOB::default();
    unsafe {
        CryptUnprotectData(
            &blob(bytes),
            None,
            None,
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut out,
        )
        .ok()?;
        Some(taken(out))
    }
}
