//! A vault that is a folder: a NAS, a USB disk, or one a sync client
//! carries elsewhere (a copy, never the live store, see the plan).

use super::Vault;
use std::path::{Path, PathBuf};

/// The folder every machine's backup sits in, under the one picked.
pub const ROOT: &str = "Horadric backup";

/// One machine's backup in `<picked>\Horadric backup\<machine>`.
pub struct FolderVault {
    dir: PathBuf,
}

impl FolderVault {
    pub fn new(picked: &Path, machine: &str) -> FolderVault {
        FolderVault {
            dir: picked.join(ROOT).join(machine),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn path(&self, name: &str) -> Result<PathBuf, String> {
        if !safe(name) {
            return Err(format!("{name} is not a name in a backup"));
        }
        Ok(name
            .split('/')
            .fold(self.dir.clone(), |p, part| p.join(part)))
    }
}

/// Whether a vault name stays inside the vault: relative, no `..`, no
/// empty parts, no drive or backslash to climb out by.
pub fn safe(name: &str) -> bool {
    !name.is_empty()
        && !name.contains(['\\', ':'])
        && name
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

/// The machines with a backup in the folder picked, by name.
pub fn machines(picked: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(picked.join(ROOT))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    names.sort();
    names
}

impl Vault for FolderVault {
    fn list(&self) -> Result<Vec<String>, String> {
        let mut names = Vec::new();
        walk(&self.dir, "", &mut names)?;
        names.sort();
        Ok(names)
    }

    fn get(&self, name: &str) -> Result<Vec<u8>, String> {
        let path = self.path(name)?;
        std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// Writes beside it and renames over it, so a backup cut off half way
    /// never leaves half a file under the real name.
    fn put(&self, name: &str, bytes: &[u8]) -> Result<(), String> {
        let path = self.path(name)?;
        let err = |e: std::io::Error| format!("{}: {e}", path.display());
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(err)?;
        }
        let mut part = path.clone().into_os_string();
        part.push(".part");
        let part = PathBuf::from(part);
        // On disk before the rename, so a USB disk pulled after it does not
        // hold an empty file under the real name.
        let mut file = std::fs::File::create(&part).map_err(err)?;
        std::io::Write::write_all(&mut file, bytes).map_err(err)?;
        file.sync_all().map_err(err)?;
        drop(file);
        std::fs::rename(&part, &path).map_err(err)
    }

    fn delete(&self, name: &str) -> Result<(), String> {
        let path = self.path(name)?;
        match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                Err(format!("{}: {e}", path.display()))
            }
            _ => Ok(()),
        }
    }
}

fn walk(dir: &Path, prefix: &str, names: &mut Vec<String>) -> Result<(), String> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(format!("{}: {e}", dir.display())),
    };
    for entry in entries.flatten() {
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        let rel = format!("{prefix}{name}");
        if entry.path().is_dir() {
            walk(&entry.path(), &format!("{rel}/"), names)?;
        } else if !name.ends_with(".part") {
            names.push(rel);
        }
    }
    Ok(())
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

    #[test]
    fn names_stay_inside() {
        assert!(safe("manifest.json"));
        assert!(safe("lore/horadric-1a2b.json.hbk"));
        assert!(!safe(""));
        assert!(!safe("../x"));
        assert!(!safe("a//b"));
        assert!(!safe("a\\b"));
        assert!(!safe("C:/x"));
        assert!(!safe("/x"));
    }

    #[test]
    fn folder_vault_round_trips() {
        let picked = scratch("folder");
        let v = FolderVault::new(&picked, "PC");
        assert_eq!(v.list().unwrap(), Vec::<String>::new());
        v.put("memory/day/2026-10-16.md.hbk", b"one").unwrap();
        v.put("memory/day/2026-10-16.md.hbk", b"two").unwrap();
        v.put("manifest.json", b"{}").unwrap();
        assert_eq!(v.get("memory/day/2026-10-16.md.hbk").unwrap(), b"two");
        assert_eq!(
            v.list().unwrap(),
            ["manifest.json", "memory/day/2026-10-16.md.hbk"]
        );
        v.delete("manifest.json").unwrap();
        v.delete("manifest.json").unwrap();
        assert!(v.get("manifest.json").is_err());
        assert!(v.put("../escape", b"x").is_err());
        assert_eq!(machines(&picked), ["PC"]);
        std::fs::remove_dir_all(&picked).unwrap();
    }
}
