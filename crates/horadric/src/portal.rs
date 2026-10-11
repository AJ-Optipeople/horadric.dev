//! `horadric portal`: Town Portal, Horadric from a phone. Turns it on or
//! off and shows the pairing link. The running app looks at `portal.json`
//! and connects to the relay to match. See `docs/PORTAL.md`.

use horadric_core::portal::{Config, Subs};
use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

pub fn run(args: &[String]) -> Result<(), String> {
    let dir = horadric_hooks::state_dir().ok_or("cannot find %APPDATA%")?;
    let mut config = Config::load(&dir);
    let save = |c: &Config| {
        c.save(&dir)
            .map_err(|e| format!("cannot write {}: {e}", dir.join("portal.json").display()))
    };
    match args.first().map(String::as_str) {
        None => {
            let mut c = config.take().unwrap_or_else(fresh);
            c.enabled = true;
            save(&c)?;
            println!("Town Portal is on, through {}.", c.relay);
            println!("Open this on your phone, or scan the QR code in the browser:\n");
            println!("{}", c.link(&pc_name()));
            open(&c.pair_url(&pc_name()));
        }
        Some("off") => {
            if let Some(mut c) = config {
                c.enabled = false;
                save(&c)?;
            }
            println!("Town Portal is off.");
        }
        Some("rekey") => {
            let mut c = config.ok_or("Town Portal was never turned on")?;
            c.key = fresh().key;
            save(&c)?;
            Subs::clear(&dir).map_err(|e| format!("cannot forget the phones' push: {e}"))?;
            println!("A new key: every phone is unpaired. Pair again with:\n");
            println!("{}", c.link(&pc_name()));
        }
        Some("link") => {
            let c = config.ok_or("Town Portal was never turned on")?;
            println!("{}", c.link(&pc_name()));
        }
        Some("relay") => {
            let mut c = config.unwrap_or_else(fresh);
            let url = args
                .get(1)
                .ok_or("usage: horadric portal relay wss://HOST")?;
            if !(url.starts_with("wss://") || url.starts_with("ws://")) {
                return Err(format!("`{url}` is not a ws:// or wss:// address"));
            }
            c.relay = url.trim_end_matches('/').to_string();
            save(&c)?;
            println!(
                "Town Portal goes through {} now. Pair again with:\n",
                c.relay
            );
            println!("{}", c.link(&pc_name()));
        }
        Some(other) => return Err(format!("unknown portal command `{other}`")),
    }
    Ok(())
}

fn fresh() -> Config {
    let mut random = [0u8; 64];
    horadric_ui::portal_crypto::random(&mut random);
    Config::fresh(&random)
}

fn pc_name() -> String {
    std::env::var("COMPUTERNAME").unwrap_or_else(|_| "PC".into())
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
