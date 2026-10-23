//! Pairing a phone with Town Portal, the one way both `horadric portal`
//! and the app's own button do it: turn the portal on, make a one-time
//! code and show it as a QR code in the browser. The protocol is in
//! `docs/PORTAL.md`.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use horadric_core::portal::Config;
use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

use crate::portal_crypto::random;

/// Turns the portal on in `dir`, making its config the first time, opens
/// a new pairing code and shows its QR page. Gives the config as saved,
/// whose `link` is the code as a link.
pub fn pair_a_phone(dir: &Path) -> Result<Config, String> {
    let mut c = Config::load(dir).unwrap_or_else(|| Config::fresh(&random32()));
    c.enabled = true;
    c.open_ticket(&random32(), now());
    c.save(dir)
        .map_err(|e| format!("cannot write {}: {e}", dir.join("portal.json").display()))?;
    if let Some(url) = c.pair_url(&pc_name()) {
        open(&url);
    }
    Ok(c)
}

pub fn random32() -> [u8; 32] {
    let mut bytes = [0u8; 32];
    random(&mut bytes);
    bytes
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The name a phone or a brother shows for this PC. `HORADRIC_PC_NAME`
/// overrides it, so two dev instances on one machine can be told apart.
pub fn pc_name() -> String {
    std::env::var("HORADRIC_PC_NAME")
        .ok()
        .filter(|n| !n.trim().is_empty())
        .or_else(|| std::env::var("COMPUTERNAME").ok())
        .unwrap_or_else(|| "PC".into())
}

fn open(url: &str) {
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
