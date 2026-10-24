//! The memory's backup: encrypted copies of the store's text files in a
//! vault, and a restore from them.
//!
//! A backup is one machine's folder in a [`Vault`]: `manifest.json` in
//! the clear (the key's salt and rounds, and per file its path, hash,
//! size and time), and per file `<path>.hbk`, AES-256-GCM sealed and
//! bound to its path. The logic here is written once against the trait.

pub mod config;
pub mod credential;
pub mod crypto;
pub mod folder;
pub mod run;
pub mod vault;

pub use config::Config;
pub use folder::FolderVault;
pub use vault::{MemoryVault, Vault};

use crate::portal_crypto::{open_bound, random, seal_bound};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const MANIFEST: &str = "manifest.json";
const VERSION: u64 = 1;
const KDF: &str = "PBKDF2-SHA256";
/// What the check in the manifest seals, so a wrong passphrase is caught
/// before a byte is written, even in a backup with no files yet.
const CHECK: &[u8] = b"horadric backup";
const CHECK_AAD: &[u8] = b"check";
const MAX_ROUNDS: u32 = 10_000_000;
/// The error a passphrase that does not fit gives, for a caller to ask
/// for another.
pub const WRONG_PASSPHRASE: &str = "that passphrase does not open this backup";
/// The two folders of the state that are backed up and swapped on restore.
pub const FOLDERS: [&str; 2] = ["memory", "lore"];

/// One file in the manifest.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub path: String,
    pub sha256: String,
    pub size: u64,
    /// When this content was backed up, Unix seconds.
    pub at: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Manifest {
    pub salt: Vec<u8>,
    pub rounds: u32,
    /// [`CHECK`] sealed under the key, hex.
    pub check: String,
    pub files: Vec<Entry>,
}

impl Manifest {
    pub fn to_json(&self) -> Value {
        json!({
            "version": VERSION,
            "kdf": {"name": KDF, "salt": crypto::hex(&self.salt), "rounds": self.rounds},
            "check": self.check,
            "files": self.files.iter().map(|f| json!({
                "path": f.path, "sha256": f.sha256, "size": f.size, "at": f.at,
            })).collect::<Vec<_>>(),
        })
    }

    pub fn parse(bytes: &[u8]) -> Result<Manifest, String> {
        let bad = |what: &str| format!("the backup's manifest is damaged ({what})");
        let v: Value = serde_json::from_slice(bytes).map_err(|e| bad(&e.to_string()))?;
        match v["version"].as_u64() {
            Some(VERSION) => {}
            Some(n) => {
                return Err(format!(
                    "the backup is format {n}, newer than this Horadric reads; update Horadric"
                ))
            }
            None => return Err(bad("no version")),
        }
        if v["kdf"]["name"] != KDF {
            return Err(bad("unknown key derivation"));
        }
        let salt = v["kdf"]["salt"]
            .as_str()
            .and_then(crypto::unhex)
            .ok_or_else(|| bad("salt"))?;
        let rounds = v["kdf"]["rounds"]
            .as_u64()
            .and_then(|r| u32::try_from(r).ok())
            // Zero would fail CNG and a huge count would hang on a damaged file.
            .filter(|r| (1..=MAX_ROUNDS).contains(r))
            .ok_or_else(|| bad("rounds"))?;
        let check = v["check"].as_str().ok_or_else(|| bad("check"))?.to_string();
        let files = v["files"]
            .as_array()
            .ok_or_else(|| bad("files"))?
            .iter()
            .map(|f| {
                Some(Entry {
                    path: f["path"].as_str().filter(|p| folder::safe(p))?.to_string(),
                    sha256: f["sha256"]
                        .as_str()
                        .filter(|s| s.len() == 64 && crypto::unhex(s).is_some())?
                        .to_string(),
                    size: f["size"].as_u64()?,
                    at: f["at"].as_u64()?,
                })
            })
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| bad("a file"))?;
        Ok(Manifest {
            salt,
            rounds,
            check,
            files,
        })
    }

    /// The key, when `passphrase` is the one this backup was made with.
    pub fn unlock(&self, passphrase: &str) -> Result<[u8; 32], String> {
        let key = crypto::derive(passphrase, &self.salt, self.rounds);
        let sealed = crypto::unhex(&self.check).unwrap_or_default();
        match open_bound(&key, &sealed, CHECK_AAD) {
            Some(plain) if plain == CHECK => Ok(key),
            _ => Err(WRONG_PASSPHRASE.into()),
        }
    }

    /// The total size of the files it holds.
    pub fn bytes(&self) -> u64 {
        self.files.iter().map(|f| f.size).sum()
    }

    /// When the newest file in it was backed up.
    pub fn last(&self) -> Option<u64> {
        self.files.iter().map(|f| f.at).max()
    }
}

/// A new, empty backup's manifest under `passphrase`, with a fresh salt.
fn fresh(passphrase: &str) -> (Manifest, [u8; 32]) {
    let mut salt = vec![0u8; 16];
    random(&mut salt);
    let key = crypto::derive(passphrase, &salt, crypto::ROUNDS);
    let check = crypto::hex(&seal_bound(&key, CHECK, CHECK_AAD));
    let manifest = Manifest {
        salt,
        rounds: crypto::ROUNDS,
        check,
        files: Vec::new(),
    };
    (manifest, key)
}

/// The vault's manifest, `None` when nothing was backed up there yet.
pub fn manifest(vault: &dyn Vault) -> Result<Option<Manifest>, String> {
    if !vault.list()?.iter().any(|n| n == MANIFEST) {
        return Ok(None);
    }
    Manifest::parse(&vault.get(MANIFEST)?).map(Some)
}

/// A file's blob is named for its content too, so a new version never
/// overwrites the one the current manifest points at: a backup cut off
/// before its manifest leaves the previous backup whole.
fn blob(entry: &Entry) -> String {
    format!("{}.{}.hbk", entry.path, &entry.sha256[..16])
}

/// Whether a path in the state folder, with `/`, is backed up: the
/// memory's text and the lore's notes. The vectors, the code index and
/// the Sanctuary's layout are rebuilt, so they are not.
pub fn chosen(rel: &str) -> bool {
    if let Some(name) = rel.strip_prefix("memory/") {
        return name.starts_with("day/")
            || name == "horadrim.json"
            || name == "transcripts.json"
            || (!name.contains('/') && name.ends_with(".jsonl"));
    }
    if let Some(name) = rel.strip_prefix("lore/") {
        return !name.contains('/')
            && name.ends_with(".json")
            && !name.ends_with(".code.json")
            && !name.ends_with(".sanctuary.json");
    }
    false
}

/// The files of `state` that are backed up, relative with `/`, sorted.
pub fn collect(state: &Path) -> Vec<String> {
    let mut all = Vec::new();
    for top in FOLDERS {
        list_files(&state.join(top), &format!("{top}/"), &mut all);
    }
    all.retain(|rel| chosen(rel) && folder::safe(rel));
    all.sort();
    all
}

fn list_files(dir: &Path, prefix: &str, out: &mut Vec<String>) {
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        let rel = format!("{prefix}{name}");
        if entry.path().is_dir() {
            list_files(&entry.path(), &format!("{rel}/"), out);
        } else {
            out.push(rel);
        }
    }
}

fn local(root: &Path, rel: &str) -> PathBuf {
    rel.split('/')
        .fold(root.to_path_buf(), |p, part| p.join(part))
}

/// What a backup or a restore did.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Report {
    /// Files the backup holds after it.
    pub files: usize,
    /// Of those, the ones sent (backup) or written (restore) this time.
    pub moved: usize,
    /// Their size before sealing.
    pub bytes: u64,
}

/// Backs `state` up into `vault`: each chosen file whose content changed
/// since the last backup is sealed and put beside its old version, then
/// the manifest, last, and only then the old versions go. A backup cut
/// off half way leaves the previous one whole. A file gone
/// from the state stays in the backup: an emptied store is exactly when
/// the backup must not follow it.
pub fn back_up(
    state: &Path,
    vault: &dyn Vault,
    passphrase: &str,
    now: u64,
) -> Result<Report, String> {
    let (mut manifest, key, new) = match manifest(vault)? {
        Some(m) => {
            let key = m.unlock(passphrase)?;
            (m, key, false)
        }
        None => {
            let (m, key) = fresh(passphrase);
            (m, key, true)
        }
    };
    let mut entries: BTreeMap<String, Entry> = manifest
        .files
        .drain(..)
        .map(|e| (e.path.clone(), e))
        .collect();
    let mut report = Report::default();
    let mut stale = Vec::new();
    for rel in collect(state) {
        // A file written as we read it is caught next time.
        let Ok(plain) = std::fs::read(local(state, &rel)) else {
            continue;
        };
        let sha256 = crypto::hex(&crypto::sha256(&plain));
        if entries.get(&rel).is_some_and(|e| e.sha256 == sha256) {
            continue;
        }
        let entry = Entry {
            path: rel.clone(),
            sha256,
            size: plain.len() as u64,
            at: now,
        };
        vault.put(&blob(&entry), &seal_bound(&key, &plain, rel.as_bytes()))?;
        report.moved += 1;
        report.bytes += plain.len() as u64;
        if let Some(old) = entries.insert(rel, entry) {
            stale.push(blob(&old));
        }
    }
    manifest.files = entries.into_values().collect();
    report.files = manifest.files.len();
    if new || report.moved > 0 {
        let text = serde_json::to_vec_pretty(&manifest.to_json()).map_err(|e| e.to_string())?;
        vault.put(MANIFEST, &text)?;
    }
    // One left behind costs space, not correctness: the manifest no
    // longer names it.
    for name in stale {
        let _ = vault.delete(&name);
    }
    Ok(report)
}

/// Decrypts the backup in `vault` into `into`, which should be a fresh
/// folder: it gets `memory\` and `lore\` as they were backed up. A wrong
/// passphrase is caught before anything is written, and a blob that is
/// damaged or swapped stops the restore.
pub fn restore(vault: &dyn Vault, passphrase: &str, into: &Path) -> Result<Report, String> {
    let manifest = manifest(vault)?.ok_or("there is no backup there")?;
    let key = manifest.unlock(passphrase)?;
    restore_with(vault, &manifest, &key, into)
}

/// [`restore`] with the manifest read and the key from its
/// [`Manifest::unlock`], which is slow on purpose and wants doing once.
pub fn restore_with(
    vault: &dyn Vault,
    manifest: &Manifest,
    key: &[u8; 32],
    into: &Path,
) -> Result<Report, String> {
    let mut report = Report {
        files: manifest.files.len(),
        ..Report::default()
    };
    for entry in &manifest.files {
        let sealed = vault.get(&blob(entry))?;
        let plain = open_bound(key, &sealed, entry.path.as_bytes())
            .ok_or_else(|| format!("{} in the backup is damaged", entry.path))?;
        if crypto::hex(&crypto::sha256(&plain)) != entry.sha256 {
            return Err(format!(
                "{} in the backup is not what was backed up",
                entry.path
            ));
        }
        let path = local(into, &entry.path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        std::fs::write(&path, &plain).map_err(|e| format!("{}: {e}", path.display()))?;
        report.moved += 1;
        report.bytes += plain.len() as u64;
    }
    Ok(report)
}

/// Swaps a restored folder in for the state's `memory\` and `lore\`. The
/// old ones are kept beside, renamed with `stamp`, never deleted; their
/// new names are returned. Nothing may hold them open: the app's memory
/// must be stopped first. When a move fails, the ones already made are
/// undone, so the store is either the old one or the restored one.
pub fn swap_in(state: &Path, restored: &Path, stamp: &str) -> Result<Vec<PathBuf>, String> {
    // Each rename made, as (from, to), to undo in reverse.
    let mut done: Vec<(PathBuf, PathBuf)> = Vec::new();
    let mut kept = Vec::new();
    let step = |from: PathBuf, to: PathBuf, done: &mut Vec<(PathBuf, PathBuf)>| {
        std::fs::rename(&from, &to)
            .map(|()| done.push((from.clone(), to)))
            .map_err(|e| format!("could not move {}: {e}", from.display()))
    };
    let result: Result<(), String> = (|| {
        for top in FOLDERS {
            let live = state.join(top);
            if live.exists() {
                let old = unused(state, &format!("{top}.before-restore-{stamp}"));
                step(live.clone(), old.clone(), &mut done)?;
                kept.push(old);
            }
            let new = restored.join(top);
            if new.exists() {
                step(new, live, &mut done)?;
            }
        }
        Ok(())
    })();
    if let Err(e) = result {
        for (from, to) in done.into_iter().rev() {
            let _ = std::fs::rename(&to, &from);
        }
        return Err(format!(
            "{e}. Nothing was changed; is Horadric or a session still using it?"
        ));
    }
    for top in FOLDERS {
        let _ = std::fs::create_dir_all(state.join(top));
    }
    let _ = std::fs::remove_dir(restored);
    // The restored lore log may stop short of numbers this brother sent.
    if let Err(e) = horadric_memory::share::renew(state) {
        eprintln!("horadric: cannot name this brother anew after the restore: {e}");
    }
    Ok(kept)
}

/// `state\name`, or with `-2`, `-3` and on when that is taken.
fn unused(state: &Path, name: &str) -> PathBuf {
    std::iter::once(state.join(name))
        .chain((2..).map(|n| state.join(format!("{name}-{n}"))))
        .find(|p| !p.exists())
        .expect("an endless supply of names")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("horadric-backup-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn write(root: &Path, rel: &str, text: &str) {
        let path = local(root, rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    /// The one blob in the vault for `path`.
    fn blob_of(vault: &MemoryVault, path: &str) -> String {
        let prefix = format!("{path}.");
        let found: Vec<String> = vault
            .list()
            .unwrap()
            .into_iter()
            .filter(|n| n.starts_with(&prefix) && n.ends_with(".hbk"))
            .collect();
        assert_eq!(found.len(), 1, "{found:?}");
        found[0].clone()
    }

    fn read(root: &Path, rel: &str) -> String {
        std::fs::read_to_string(local(root, rel)).unwrap()
    }

    #[test]
    fn chooses_the_text_and_not_what_is_rebuilt() {
        for yes in [
            "memory/2026-10.jsonl",
            "memory/horadrim.json",
            "memory/transcripts.json",
            "memory/day/2026-10-16.md",
            "lore/horadric.dev-990a2c84.json",
        ] {
            assert!(chosen(yes), "{yes}");
        }
        for no in [
            "memory/2026-10.all-MiniLM-L6-v2@1110a243.vec",
            "memory/models/x.safetensors",
            "lore/horadric.dev-990a2c84.code.json",
            "lore/horadric.dev-990a2c84.sanctuary.json",
            "settings.json",
            "memoryx/2026-10.jsonl",
        ] {
            assert!(!chosen(no), "{no}");
        }
    }

    #[test]
    fn manifest_round_trips() {
        let (mut m, _) = fresh("pass");
        m.files.push(Entry {
            path: "memory/2026-10.jsonl".into(),
            sha256: "ab".repeat(32),
            size: 3,
            at: 7,
        });
        let text = serde_json::to_vec(&m.to_json()).unwrap();
        assert_eq!(Manifest::parse(&text).unwrap(), m);
        assert!(Manifest::parse(br#"{"version":2}"#)
            .unwrap_err()
            .contains("newer"));
        assert!(Manifest::parse(b"nonsense").is_err());
    }

    #[test]
    fn backs_up_restores_and_sends_only_what_changed() {
        let state = scratch("state");
        write(&state, "memory/2026-09.jsonl", "september\n");
        write(&state, "memory/2026-10.jsonl", "october\n");
        write(&state, "memory/2026-10.x@1.vec", "vectors");
        write(&state, "memory/day/2026-10-16.md", "a day");
        write(&state, "lore/p-1.json", "[]");
        write(&state, "lore/p-1.code.json", "{}");
        let vault = MemoryVault::default();

        let first = back_up(&state, &vault, "pass", 100).unwrap();
        assert_eq!((first.files, first.moved), (4, 4));
        let names = vault.list().unwrap();
        assert!(names.contains(&blob_of(&vault, "memory/2026-10.jsonl")));
        assert!(!names
            .iter()
            .any(|n| n.contains(".vec") || n.contains(".code")));
        // The vault holds noise.
        let sealed = vault.get(&blob_of(&vault, "memory/2026-10.jsonl")).unwrap();
        assert!(!sealed.windows(7).any(|w| w == b"october"));

        let same = back_up(&state, &vault, "pass", 200).unwrap();
        assert_eq!((same.files, same.moved), (4, 0));

        write(&state, "memory/2026-10.jsonl", "october\nmore\n");
        std::fs::remove_file(local(&state, "lore/p-1.json")).unwrap();
        let again = back_up(&state, &vault, "pass", 300).unwrap();
        assert_eq!((again.files, again.moved), (4, 1));
        assert_eq!(vault.list().unwrap().len(), 5, "the old version is gone");
        let m = manifest(&vault).unwrap().unwrap();
        assert_eq!(m.last(), Some(300));
        let sept = m.files.iter().find(|f| f.path == "memory/2026-09.jsonl");
        assert_eq!(sept.unwrap().at, 100);

        assert!(back_up(&state, &vault, "wrong", 400)
            .unwrap_err()
            .contains("passphrase"));

        let into = scratch("restored");
        assert!(restore(&vault, "wrong", &into).is_err());
        assert!(!into.exists(), "a wrong passphrase writes nothing");
        let report = restore(&vault, "pass", &into).unwrap();
        assert_eq!(report.moved, 4);
        assert_eq!(read(&into, "memory/2026-10.jsonl"), "october\nmore\n");
        assert_eq!(read(&into, "memory/day/2026-10-16.md"), "a day");
        assert_eq!(read(&into, "lore/p-1.json"), "[]");

        let before = horadric_memory::share::brother(&state).unwrap();
        let kept = swap_in(&state, &into, "now").unwrap();
        assert_eq!(kept.len(), 2);
        assert_ne!(
            horadric_memory::share::brother(&state).unwrap(),
            before,
            "a restored lore is written under a new brother"
        );
        assert_eq!(read(&state, "lore/p-1.json"), "[]");
        assert!(!local(&state, "memory/2026-10.x@1.vec").exists());
        assert_eq!(
            read(&state, "memory.before-restore-now/2026-10.x@1.vec"),
            "vectors"
        );
        assert!(!into.exists());
        std::fs::remove_dir_all(&state).unwrap();
    }

    #[test]
    fn a_swapped_blob_is_refused() {
        let state = scratch("swapped");
        write(&state, "memory/2026-09.jsonl", "september\n");
        write(&state, "memory/2026-10.jsonl", "october\n");
        let vault = MemoryVault::default();
        back_up(&state, &vault, "pass", 1).unwrap();
        let sept = vault.get(&blob_of(&vault, "memory/2026-09.jsonl")).unwrap();
        vault
            .put(&blob_of(&vault, "memory/2026-10.jsonl"), &sept)
            .unwrap();
        let into = scratch("swapped-into");
        assert!(restore(&vault, "pass", &into)
            .unwrap_err()
            .contains("damaged"));
        let _ = std::fs::remove_dir_all(&into);
        std::fs::remove_dir_all(&state).unwrap();
    }

    #[test]
    fn an_empty_state_still_makes_a_backup_to_check_against() {
        let state = scratch("empty");
        let vault = MemoryVault::default();
        let r = back_up(&state, &vault, "pass", 1).unwrap();
        assert_eq!(r, Report::default());
        assert!(manifest(&vault).unwrap().unwrap().unlock("pass").is_ok());
        assert!(back_up(&state, &vault, "nope", 2).is_err());
    }

    /// Puts fail once its allowance is spent, as when a disk fills or the
    /// network drops half way.
    struct Failing(MemoryVault, std::cell::Cell<usize>);

    impl Vault for Failing {
        fn list(&self) -> Result<Vec<String>, String> {
            self.0.list()
        }
        fn get(&self, name: &str) -> Result<Vec<u8>, String> {
            self.0.get(name)
        }
        fn put(&self, name: &str, bytes: &[u8]) -> Result<(), String> {
            if self.1.get() == 0 {
                return Err("cut off".into());
            }
            self.1.set(self.1.get() - 1);
            self.0.put(name, bytes)
        }
        fn delete(&self, name: &str) -> Result<(), String> {
            self.0.delete(name)
        }
    }

    #[test]
    fn a_backup_cut_off_half_way_leaves_the_last_one_whole() {
        let state = scratch("cut");
        write(&state, "memory/2026-09.jsonl", "september");
        write(&state, "memory/2026-10.jsonl", "october");
        let vault = Failing(MemoryVault::default(), std::cell::Cell::new(usize::MAX));
        back_up(&state, &vault, "pass", 1).unwrap();
        write(&state, "memory/2026-09.jsonl", "september, changed");
        write(&state, "memory/2026-10.jsonl", "october, changed");
        vault.1.set(1);
        assert!(back_up(&state, &vault, "pass", 2).is_err());
        let into = scratch("cut-into");
        restore(&vault, "pass", &into).unwrap();
        assert_eq!(read(&into, "memory/2026-09.jsonl"), "september");
        assert_eq!(read(&into, "memory/2026-10.jsonl"), "october");
        std::fs::remove_dir_all(&into).unwrap();
        std::fs::remove_dir_all(&state).unwrap();
    }

    #[test]
    fn a_swap_that_cannot_finish_changes_nothing() {
        use std::os::windows::fs::OpenOptionsExt;
        let state = scratch("stuck");
        write(&state, "memory/2026-10.jsonl", "live");
        write(&state, "lore/p-1.json", "live");
        let restored = scratch("stuck-restored");
        write(&restored, "memory/2026-10.jsonl", "restored");
        write(&restored, "lore/p-1.json", "restored");
        // An open file in lore stops that folder moving, as a running
        // session's would.
        let held = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(local(&state, "lore/p-1.json"))
            .unwrap();
        assert!(swap_in(&state, &restored, "x").is_err());
        drop(held);
        assert_eq!(read(&state, "memory/2026-10.jsonl"), "live");
        assert_eq!(read(&state, "lore/p-1.json"), "live");
        assert_eq!(read(&restored, "memory/2026-10.jsonl"), "restored");
        assert!(!state.join("memory.before-restore-x").exists());

        // A second restore with the same stamp keeps both old copies.
        std::fs::create_dir_all(state.join("memory.before-restore-x")).unwrap();
        let kept = swap_in(&state, &restored, "x").unwrap();
        assert!(kept[0].ends_with("memory.before-restore-x-2"));
        assert_eq!(read(&state, "memory/2026-10.jsonl"), "restored");
        std::fs::remove_dir_all(&state).unwrap();
    }

    #[test]
    fn a_damaged_round_count_is_refused() {
        let (m, _) = fresh("pass");
        let mut v = m.to_json();
        v["kdf"]["rounds"] = 0.into();
        assert!(Manifest::parse(v.to_string().as_bytes()).is_err());
        v["kdf"]["rounds"] = 4_000_000_000u64.into();
        assert!(Manifest::parse(v.to_string().as_bytes()).is_err());
    }
}
