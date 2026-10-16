//! Where a backup goes. The backup logic is written once against
//! [`Vault`]; a folder and Google Drive each implement it.

use std::collections::BTreeMap;
use std::sync::Mutex;

/// One machine's backup folder, wherever it lives.
pub trait Vault {
    /// Every file in this machine's backup folder, by relative name.
    fn list(&self) -> Result<Vec<String>, String>;
    fn get(&self, name: &str) -> Result<Vec<u8>, String>;
    /// Writes or replaces.
    fn put(&self, name: &str, bytes: &[u8]) -> Result<(), String>;
    fn delete(&self, name: &str) -> Result<(), String>;
}

/// A vault in memory, for tests of the logic above any real one.
#[derive(Default)]
pub struct MemoryVault {
    pub files: Mutex<BTreeMap<String, Vec<u8>>>,
}

impl Vault for MemoryVault {
    fn list(&self) -> Result<Vec<String>, String> {
        Ok(self.files.lock().unwrap().keys().cloned().collect())
    }

    fn get(&self, name: &str) -> Result<Vec<u8>, String> {
        self.files
            .lock()
            .unwrap()
            .get(name)
            .cloned()
            .ok_or_else(|| format!("{name} is not in the backup"))
    }

    fn put(&self, name: &str, bytes: &[u8]) -> Result<(), String> {
        self.files
            .lock()
            .unwrap()
            .insert(name.to_string(), bytes.to_vec());
        Ok(())
    }

    fn delete(&self, name: &str) -> Result<(), String> {
        self.files.lock().unwrap().remove(name);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_vault_puts_replaces_and_deletes() {
        let v = MemoryVault::default();
        v.put("a/b.hbk", b"one").unwrap();
        v.put("a/b.hbk", b"two").unwrap();
        v.put("manifest.json", b"{}").unwrap();
        assert_eq!(v.get("a/b.hbk").unwrap(), b"two");
        assert_eq!(v.list().unwrap(), ["a/b.hbk", "manifest.json"]);
        v.delete("a/b.hbk").unwrap();
        assert!(v.get("a/b.hbk").is_err());
    }
}
