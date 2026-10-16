//! Where backups go and how the last one went, in `backup.json` in the
//! state folder. The passphrase is never here: it is in Credential
//! Manager ([`super::credential`]).

use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

pub const FILE: &str = "backup.json";

/// How the last backup went.
#[derive(Debug, Clone, PartialEq)]
pub struct Last {
    /// Unix seconds.
    pub at: u64,
    pub files: usize,
    pub bytes: u64,
    pub error: Option<String>,
}

/// Where the app backs up to by itself. Off until the human picks.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum To {
    #[default]
    Off,
    Drive,
    Folder,
}

impl To {
    pub const ALL: [To; 3] = [To::Off, To::Drive, To::Folder];

    pub fn label(self) -> &'static str {
        match self {
            To::Off => "Off",
            To::Drive => "Google Drive",
            To::Folder => "A folder",
        }
    }

    fn name(self) -> &'static str {
        match self {
            To::Off => "off",
            To::Drive => "drive",
            To::Folder => "folder",
        }
    }

    fn from_name(name: &str) -> To {
        To::ALL
            .into_iter()
            .find(|t| t.name() == name)
            .unwrap_or_default()
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Config {
    pub to: To,
    /// The folder picked; the backup is in `Horadric backup\<machine>` in it.
    pub folder: Option<PathBuf>,
    pub last: Option<Last>,
    /// Keys this version does not know, kept as they were for the one that
    /// wrote them.
    pub rest: Map<String, Value>,
}

impl Config {
    pub fn parse(text: &str) -> Config {
        let Ok(Value::Object(mut map)) = serde_json::from_str(text) else {
            return Config::default();
        };
        let to = map
            .remove("to")
            .and_then(|v| v.as_str().map(To::from_name))
            .unwrap_or_default();
        let folder = map
            .remove("folder")
            .and_then(|v| v.as_str().map(PathBuf::from));
        let last = map.remove("last").and_then(|v| {
            Some(Last {
                at: v["at"].as_u64()?,
                files: v["files"].as_u64()? as usize,
                bytes: v["bytes"].as_u64()?,
                error: v["error"].as_str().map(str::to_string),
            })
        });
        Config {
            to,
            folder,
            last,
            rest: map,
        }
    }

    pub fn to_json(&self) -> Value {
        let mut map = self.rest.clone();
        map.insert("to".into(), json!(self.to.name()));
        if let Some(folder) = &self.folder {
            map.insert("folder".into(), json!(folder.to_string_lossy()));
        }
        if let Some(l) = &self.last {
            map.insert(
                "last".into(),
                json!({"at": l.at, "files": l.files, "bytes": l.bytes, "error": l.error}),
            );
        }
        Value::Object(map)
    }

    pub fn load(state: &Path) -> Config {
        std::fs::read_to_string(state.join(FILE))
            .map(|t| Config::parse(&t))
            .unwrap_or_default()
    }

    pub fn save(&self, state: &Path) -> Result<(), String> {
        let path = state.join(FILE);
        let text = serde_json::to_string_pretty(&self.to_json()).map_err(|e| e.to_string())?;
        std::fs::create_dir_all(state).map_err(|e| e.to_string())?;
        std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))
    }
}

/// This machine's name, which its backup's folder is called.
pub fn machine() -> String {
    std::env::var("COMPUTERNAME")
        .ok()
        .filter(|n| super::folder::safe(n))
        .unwrap_or_else(|| "this PC".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_keeps_what_it_does_not_know() {
        let text = r#"{"folder":"D:\\Backups","to":"drive","last":{"at":5,"files":2,"bytes":9,"error":null}}"#;
        let c = Config::parse(text);
        assert_eq!(c.folder.as_deref(), Some(Path::new(r"D:\Backups")));
        assert_eq!(c.last.as_ref().unwrap().files, 2);
        assert_eq!(c.to, To::Drive);
        assert!(c.rest.is_empty());
        assert_eq!(Config::parse(&c.to_json().to_string()), c);
        assert_eq!(Config::parse("junk"), Config::default());
    }
}
