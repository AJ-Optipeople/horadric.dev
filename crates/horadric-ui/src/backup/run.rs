//! The backup as the app runs it: where it goes now, when it is due, and
//! the jobs the Settings section starts. Everything here blocks, so the
//! app calls it from a thread of its own.

use super::{
    back_up, config, credential, manifest, restore_with, swap_in, Config, FolderVault, Report,
    Vault, WRONG_PASSPHRASE,
};
use crate::{google, google_drive::DriveVault};
use config::{Last, To};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// How long after a try the next may begin. A month's segment that is
/// closed never changes, so it is hashed and found the same, never sent
/// again; the one still being written is sent when its hash moved, at
/// most this often.
pub const EVERY: u64 = 3600;
/// Where a restore waits to be swapped in at the next start, while
/// nothing holds the memory open.
const PENDING: &str = "restore-pending";

/// What a job on a thread of its own tells the app when it is done.
pub enum Done {
    SignedIn(Result<String, String>),
    SignedOut,
    BackedUp(Result<Report, String>),
    Passphrase(Result<(), String>),
    Machines(Result<Vec<String>, String>),
    /// The machine, and a restore staged from its backup (with when it
    /// was made) or why not.
    Restored(String, Result<(Report, u64), String>),
}

pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Whether the app should back up now: it is switched on, can reach its
/// target and has a passphrase, and the last try was an hour or more ago.
pub fn due(config: &Config, ready: bool, now: u64) -> bool {
    config.to != To::Off
        && ready
        && config
            .last
            .as_ref()
            .is_none_or(|l| now.saturating_sub(l.at) >= EVERY)
}

/// Whether the target can be reached without asking the human anything.
pub fn ready(config: &Config) -> bool {
    credential::read(credential::PASSPHRASE).is_some()
        && match config.to {
            To::Off => false,
            To::Drive => google::account().is_some(),
            To::Folder => config.folder.as_ref().is_some_and(|f| f.is_dir()),
        }
}

/// The vault `config` points at, for this machine's own backup.
pub fn vault(config: &Config) -> Result<Box<dyn Vault>, String> {
    vault_of(config, &config::machine())
}

/// The vault of `machine`'s backup, which a restore on a new PC reads.
pub fn vault_of(config: &Config, machine: &str) -> Result<Box<dyn Vault>, String> {
    match config.to {
        To::Off => Err("backup is off".into()),
        To::Drive => Ok(Box::new(DriveVault::open(machine)?)),
        To::Folder => {
            let folder = config.folder.as_ref().ok_or("no folder picked")?;
            if !folder.is_dir() {
                return Err(format!("{} is not a folder", folder.display()));
            }
            Ok(Box::new(FolderVault::new(folder, machine)))
        }
    }
}

/// The machines that have a backup at the target.
pub fn machines(config: &Config) -> Result<Vec<String>, String> {
    match config.to {
        To::Off => Err("backup is off".into()),
        To::Drive => DriveVault::machines(),
        To::Folder => {
            let folder = config.folder.as_ref().ok_or("no folder picked")?;
            Ok(super::folder::machines(folder))
        }
    }
}

/// One backup, its result kept in `backup.json` for the Settings section.
/// Returns the config as saved.
pub fn run_once(state: &Path) -> (Config, Result<Report, String>) {
    let mut config = Config::load(state);
    let at = unix_now();
    let result = match credential::read(credential::PASSPHRASE) {
        None => Err("no passphrase is set".to_string()),
        Some(pass) => vault(&config).and_then(|v| {
            let report = back_up(state, v.as_ref(), &pass, at)?;
            let bytes = manifest(v.as_ref())?.map_or(0, |m| m.bytes());
            Ok((report, bytes))
        }),
    };
    let (kept, shown) = match result {
        Ok((report, bytes)) => (
            Last {
                at,
                files: report.files,
                bytes,
                error: None,
            },
            Ok(report),
        ),
        Err(e) => (
            Last {
                at,
                files: config.last.as_ref().map_or(0, |l| l.files),
                bytes: config.last.as_ref().map_or(0, |l| l.bytes),
                error: Some(e.clone()),
            },
            Err(e),
        ),
    };
    config.last = Some(kept);
    if let Err(e) = config.save(state) {
        eprintln!("horadric: cannot keep backup.json: {e}");
    }
    (config, shown)
}

/// Makes `new` the passphrase. With no backup at the target it is simply
/// kept. With one it must open it, or, when the one kept opens it, the
/// whole backup is sealed again under `new` (see [`rekey`]).
pub fn set_passphrase(state: &Path, config: &Config, new: &str) -> Result<(), String> {
    if new.chars().count() < 8 {
        return Err("a passphrase needs at least 8 characters".into());
    }
    let existing = if config.to == To::Off {
        None
    } else {
        manifest(vault(config)?.as_ref())?
    };
    let Some(m) = existing else {
        return credential::write(credential::PASSPHRASE, new);
    };
    if m.unlock(new).is_ok() {
        return credential::write(credential::PASSPHRASE, new);
    }
    let Some(old) = credential::read(credential::PASSPHRASE).filter(|o| m.unlock(o).is_ok()) else {
        return Err("that passphrase does not open the backup already there".into());
    };
    rekey(state, vault(config)?.as_ref(), &old, new)?;
    credential::write(credential::PASSPHRASE, new)
}

/// Seals the backup in `vault` again under `new`: it is read out under
/// `old` into a scratch folder, the files in the vault are removed and
/// backed up again from there, so what the store has since lost is kept.
pub fn rekey(state: &Path, vault: &dyn Vault, old: &str, new: &str) -> Result<(), String> {
    let scratch = state.join("backup-rekey");
    let _ = std::fs::remove_dir_all(&scratch);
    let result = (|| {
        super::restore(vault, old, &scratch)?;
        for name in vault.list()? {
            vault.delete(&name)?;
        }
        back_up(&scratch, vault, new, unix_now()).map(|_| ())
    })();
    let _ = std::fs::remove_dir_all(&scratch);
    result
}

/// Reads `machine`'s backup into a folder that is swapped in at the next
/// start. `passphrase` is tried instead of the one kept.
pub fn stage_restore(
    state: &Path,
    config: &Config,
    machine: &str,
    passphrase: Option<&str>,
) -> Result<(Report, u64), String> {
    let vault = vault_of(config, machine)?;
    let m = manifest(vault.as_ref())?
        .ok_or_else(|| format!("there is no backup of {machine} there"))?;
    let key = match passphrase {
        Some(p) => m.unlock(p)?,
        None => credential::read(credential::PASSPHRASE)
            .and_then(|p| m.unlock(&p).ok())
            .ok_or(WRONG_PASSPHRASE)?,
    };
    let scratch = state.join(format!("{PENDING}.part"));
    let pending = state.join(PENDING);
    let _ = std::fs::remove_dir_all(&scratch);
    let report = restore_with(vault.as_ref(), &m, &key, &scratch).and_then(|r| {
        if pending.exists() {
            std::fs::remove_dir_all(&pending).map_err(|e| e.to_string())?;
        }
        std::fs::rename(&scratch, &pending).map_err(|e| e.to_string())?;
        Ok(r)
    });
    if report.is_err() {
        let _ = std::fs::remove_dir_all(&scratch);
    }
    Ok((report?, m.last().unwrap_or(0)))
}

/// Swaps in a restore staged earlier, before anything has opened the
/// memory. The folders it replaces are kept beside. `None` when nothing
/// is staged.
pub fn apply_pending(state: &Path) -> Option<Result<Vec<PathBuf>, String>> {
    let pending = state.join(PENDING);
    if !pending.is_dir() {
        return None;
    }
    let stamp = horadric_core::tasks::utc(unix_now()).replace(':', "");
    Some(swap_in(state, &pending, &stamp))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(to: To, last: Option<u64>) -> Config {
        Config {
            to,
            last: last.map(|at| Last {
                at,
                files: 0,
                bytes: 0,
                error: None,
            }),
            ..Config::default()
        }
    }

    #[test]
    fn due_when_on_ready_and_an_hour_since() {
        assert!(due(&config(To::Folder, None), true, 10));
        assert!(!due(&config(To::Off, None), true, 10));
        assert!(!due(&config(To::Folder, None), false, 10));
        assert!(!due(&config(To::Folder, Some(100)), true, 100 + EVERY - 1));
        assert!(due(&config(To::Folder, Some(100)), true, 100 + EVERY));
        // A clock set back does not hold backups off for good.
        assert!(!due(&config(To::Folder, Some(500)), true, 100));
    }

    #[test]
    fn rekey_keeps_files_the_store_lost() {
        let dir = std::env::temp_dir().join(format!("horadric-rekey-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("memory")).unwrap();
        std::fs::write(dir.join("memory").join("2026-09.jsonl"), "old month").unwrap();
        let vault = super::super::MemoryVault::default();
        back_up(&dir, &vault, "first passphrase", 1).unwrap();
        std::fs::remove_file(dir.join("memory").join("2026-09.jsonl")).unwrap();
        rekey(&dir, &vault, "first passphrase", "second passphrase").unwrap();
        let m = manifest(&vault).unwrap().unwrap();
        assert!(m.unlock("first passphrase").is_err());
        let key = m.unlock("second passphrase").unwrap();
        let out = dir.join("out");
        restore_with(&vault, &m, &key, &out).unwrap();
        assert_eq!(
            std::fs::read_to_string(out.join("memory").join("2026-09.jsonl")).unwrap(),
            "old month"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
