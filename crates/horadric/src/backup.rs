//! `horadric backup now|status|restore`: the memory's encrypted backup to
//! a folder, and back. The logic is in [`horadric_ui::backup`]; this is
//! the passphrase prompt, the folder picked, and the words.

use horadric_ui::backup::{self, config, credential, Config, FolderVault, Vault};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const USAGE: &str = "\
horadric backup now [--folder DIR]
horadric backup status [--folder DIR]
horadric backup restore [--folder DIR] [--from MACHINE]";

pub fn run(args: &[String]) -> Result<(), String> {
    let state = horadric_hooks::state_dir().ok_or("no state folder (APPDATA is not set)")?;
    let mut config = Config::load(&state);
    let folder = flag(args, "--folder")?.map(PathBuf::from);
    match args.first().map(String::as_str) {
        Some("now") => now(&state, &mut config, folder),
        Some("status") => status(&config, folder),
        Some("restore") => restore(&state, &config, folder, flag(args, "--from")?),
        _ => Err(format!("usage:\n{USAGE}")),
    }
}

fn flag(args: &[String], name: &str) -> Result<Option<String>, String> {
    match args.iter().position(|a| a == name) {
        None => Ok(None),
        Some(i) => args
            .get(i + 1)
            .cloned()
            .map(Some)
            .ok_or_else(|| format!("{name} needs a value")),
    }
}

fn picked(config: &Config, folder: Option<PathBuf>) -> Result<PathBuf, String> {
    folder.or_else(|| config.folder.clone()).ok_or_else(|| {
        "no backup folder yet: pick one with `horadric backup now --folder DIR`".into()
    })
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn now(state: &Path, config: &mut Config, folder: Option<PathBuf>) -> Result<(), String> {
    let picked = picked(config, folder)?;
    if !picked.is_dir() {
        return Err(format!("{} is not a folder", picked.display()));
    }
    let vault = FolderVault::new(&picked, &config::machine());
    let passphrase = match credential::read(credential::PASSPHRASE) {
        Some(p) => p,
        None => {
            let p = choose_passphrase(&vault)?;
            credential::write(credential::PASSPHRASE, &p)?;
            p
        }
    };
    config.folder = Some(picked);
    let at = unix_now();
    let mut result = backup::back_up(state, &vault, &passphrase, at);
    // The passphrase kept here is not the one this folder's backup was
    // made with (another folder picked, or it was changed elsewhere).
    if result
        .as_ref()
        .is_err_and(|e| e == backup::WRONG_PASSPHRASE)
        && std::io::IsTerminal::is_terminal(&std::io::stdin())
    {
        let p = choose_passphrase(&vault)?;
        credential::write(credential::PASSPHRASE, &p)?;
        result = backup::back_up(state, &vault, &p, at);
    }
    config.last = Some(match &result {
        Ok(r) => config::Last {
            at,
            files: r.files,
            bytes: backup::manifest(&vault)
                .ok()
                .flatten()
                .map_or(0, |m| m.bytes()),
            error: None,
        },
        Err(e) => config::Last {
            at,
            files: config.last.as_ref().map_or(0, |l| l.files),
            bytes: config.last.as_ref().map_or(0, |l| l.bytes),
            error: Some(e.clone()),
        },
    });
    config.save(state)?;
    let report = result?;
    println!(
        "Backed up to {}: {} sent ({}), {} in the backup.",
        vault.dir().display(),
        files(report.moved),
        size(report.bytes),
        files(report.files)
    );
    Ok(())
}

/// The first backup's passphrase: asked twice, or, when this machine's
/// backup is already there (the passphrase was forgotten here, not
/// there), asked once and checked against it.
fn choose_passphrase(vault: &dyn Vault) -> Result<String, String> {
    if let Some(m) = backup::manifest(vault)? {
        let p = ask("The passphrase this backup was made with: ")?;
        m.unlock(&p)?;
        return Ok(p);
    }
    println!(
        "Choose a passphrase for the backup. It is kept in Windows Credential Manager\n\
         so backups run by themselves. Write it down somewhere else too: without it\n\
         the backup cannot be read, by you or by anyone."
    );
    let p = ask("Passphrase: ")?;
    if p.chars().count() < 8 {
        return Err("a passphrase needs at least 8 characters".into());
    }
    if ask("Again: ")? != p {
        return Err("the two passphrases differ".into());
    }
    Ok(p)
}

fn status(config: &Config, folder: Option<PathBuf>) -> Result<(), String> {
    let picked = picked(config, folder)?;
    let machine = config::machine();
    println!("Folder:     {}", picked.display());
    println!(
        "Passphrase: {}",
        if credential::read(credential::PASSPHRASE).is_some() {
            "kept in Credential Manager"
        } else {
            "not set"
        }
    );
    if let Some(last) = &config.last {
        match &last.error {
            None => println!("Last run:   {}", horadric_core::tasks::utc(last.at)),
            Some(e) => println!(
                "Last run:   {}, failed: {e}",
                horadric_core::tasks::utc(last.at)
            ),
        }
    }
    let machines = backup::folder::machines(&picked);
    if machines.is_empty() {
        println!("No backup there yet.");
    }
    for name in machines {
        let vault = FolderVault::new(&picked, &name);
        let here = if name == machine { " (this PC)" } else { "" };
        match backup::manifest(&vault) {
            Ok(Some(m)) => println!(
                "{name}{here}: {}, {}, last backed up {}",
                files(m.files.len()),
                size(m.bytes()),
                m.last().map_or("never".into(), horadric_core::tasks::utc)
            ),
            Ok(None) => println!("{name}{here}: empty"),
            Err(e) => println!("{name}{here}: {e}"),
        }
    }
    Ok(())
}

fn restore(
    state: &Path,
    config: &Config,
    folder: Option<PathBuf>,
    from: Option<String>,
) -> Result<(), String> {
    if crate::running() {
        return Err(format!(
            "Horadric is running and its memory is in use. Quit it from the tray \
             (sessions pause), run this again, then start it. {}",
            crate::WHERE
        ));
    }
    let picked = picked(config, folder)?;
    let machine = from.unwrap_or_else(config::machine);
    let vault = FolderVault::new(&picked, &machine);
    let manifest = backup::manifest(&vault)?
        .ok_or_else(|| format!("there is no backup of {machine} in {}", picked.display()))?;
    // Each unlock is 600,000 rounds, so the key is made once and kept.
    let stored = credential::read(credential::PASSPHRASE).and_then(|p| manifest.unlock(&p).ok());
    let key = match stored {
        Some(key) => key,
        None => manifest.unlock(&ask(&format!("The passphrase of {machine}'s backup: "))?)?,
    };
    let stamp = horadric_core::tasks::utc(unix_now()).replace(':', "");
    let fresh = state.join(format!("restore-{stamp}"));
    let swapped = backup::restore_with(&vault, &manifest, &key, &fresh)
        .and_then(|_| backup::swap_in(state, &fresh, &stamp));
    let kept = match swapped {
        Ok(kept) => kept,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&fresh);
            return Err(e);
        }
    };
    println!(
        "Restored {} from {machine}'s backup of {}.",
        files(manifest.files.len()),
        manifest
            .last()
            .map_or("never".into(), horadric_core::tasks::utc)
    );
    for old in kept {
        println!("The one it replaced is kept in {}.", old.display());
    }
    println!("The memory is embedded again in the background once Horadric starts.");
    Ok(())
}

fn files(n: usize) -> String {
    if n == 1 {
        "1 file".into()
    } else {
        format!("{n} files")
    }
}

fn size(bytes: u64) -> String {
    match bytes {
        b if b >= 1 << 20 => format!("{:.1} MB", b as f64 / (1 << 20) as f64),
        b if b >= 1 << 10 => format!("{} KB", b >> 10),
        b => format!("{b} bytes"),
    }
}

/// Reads a line from the terminal without echoing it, or from stdin as
/// it comes when that is not a terminal.
fn ask(prompt: &str) -> Result<String, String> {
    print!("{prompt}");
    let _ = std::io::stdout().flush();
    let _quiet = Quiet::new();
    let mut line = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut line)
        .map_err(|e| e.to_string())?;
    println!();
    let line = line.trim_end_matches(['\r', '\n']).to_string();
    if line.is_empty() {
        return Err("no passphrase given".into());
    }
    Ok(line)
}

/// Turns the console's echo off while it lives.
struct Quiet(Option<(windows::Win32::Foundation::HANDLE, u32)>);

impl Quiet {
    fn new() -> Quiet {
        use windows::Win32::System::Console::{
            GetConsoleMode, GetStdHandle, SetConsoleMode, CONSOLE_MODE, ENABLE_ECHO_INPUT,
            STD_INPUT_HANDLE,
        };
        unsafe {
            let Ok(input) = GetStdHandle(STD_INPUT_HANDLE) else {
                return Quiet(None);
            };
            let mut mode = CONSOLE_MODE::default();
            if GetConsoleMode(input, &mut mode).is_err() {
                return Quiet(None);
            }
            let _ = SetConsoleMode(input, CONSOLE_MODE(mode.0 & !ENABLE_ECHO_INPUT.0));
            Quiet(Some((input, mode.0)))
        }
    }
}

impl Drop for Quiet {
    fn drop(&mut self) {
        if let Some((input, mode)) = self.0 {
            unsafe {
                let _ = windows::Win32::System::Console::SetConsoleMode(
                    input,
                    windows::Win32::System::Console::CONSOLE_MODE(mode),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_read_plainly() {
        assert_eq!(size(12), "12 bytes");
        assert_eq!(size(4096), "4 KB");
        assert_eq!(size(17 << 20), "17.0 MB");
        assert_eq!(files(1), "1 file");
        assert_eq!(files(3), "3 files");
    }

    #[test]
    fn flags_need_values() {
        let args: Vec<String> = ["now", "--folder"].map(String::from).to_vec();
        assert!(flag(&args, "--folder").is_err());
        assert_eq!(flag(&args, "--from").unwrap(), None);
    }
}
