//! The lore tools of `horadric mcp`: where a project's notes are kept and
//! how they are read and written. What a note is and how one is found is
//! in `horadric_core::lore`.
//!
//! Every session of a project runs its own `horadric mcp`, so two may write
//! at once. A lock file beside the notes keeps them taking turns, and a
//! write goes to a new file first, so a reader never sees half of one.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use horadric_core::lore::{self, Lore, Mark};
use horadric_hooks::lock::Lock;
use serde_json::Value;

/// Answers a lore tool, or None when `name` is not one.
pub fn tool(name: &str, args: &Value) -> Option<Result<String, String>> {
    match name {
        "lore_note" => Some(note(args)),
        "lore_recall" => Some(recall(args)),
        _ => None,
    }
}

fn note(args: &Value) -> Result<String, String> {
    let text = args.get("text").and_then(Value::as_str).unwrap_or("");
    let replaces: Vec<u64> = list(args, "replaces")
        .iter()
        .filter_map(Value::as_u64)
        .collect();
    let place = Place::here()?;
    let files = place.files(args)?;
    let marks = files
        .into_iter()
        .map(|path| Mark {
            hash: place.hash(&path),
            path,
        })
        .collect();
    let commit = place.commit();
    let id = place.change(|l| l.add(text, marks, &commit, now(), &replaces))?;
    Ok(format!(
        "Kept as #{id}. Every session of this project can find it with lore_recall."
    ))
}

fn recall(args: &Value) -> Result<String, String> {
    let query = args.get("query").and_then(Value::as_str).unwrap_or("");
    let place = Place::here()?;
    let files = place.files(args)?;
    let lore = place.read()?;
    let found = lore.recall(query, &files, |p| place.hash(p));
    Ok(lore::describe(&found, now()))
}

fn list<'a>(args: &'a Value, key: &str) -> &'a [Value] {
    args.get(key)
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The project this session works in, and where its notes are.
struct Place {
    /// The top of the working tree the session is in, which files are
    /// read from: a worktree's own, so a note sees the files it was about.
    top: PathBuf,
    cwd: PathBuf,
    /// The notes, shared by every worktree of the project.
    store: PathBuf,
}

impl Place {
    fn here() -> Result<Place, String> {
        let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
        let out = git(
            &cwd,
            &[
                "rev-parse",
                "--path-format=absolute",
                "--git-common-dir",
                "--show-toplevel",
            ],
        );
        let mut lines = out.as_deref().unwrap_or("").lines().map(str::trim);
        let (key, top) = match (lines.next(), lines.next()) {
            (Some(common), Some(top)) if !top.is_empty() => {
                let common = common.replace('\\', "/");
                let root = common.strip_suffix("/.git").unwrap_or(top);
                (root.to_string(), PathBuf::from(top))
            }
            _ => (cwd.to_string_lossy().into_owned(), cwd.clone()),
        };
        let key = key.replace('\\', "/").trim_end_matches('/').to_lowercase();
        let dir = horadric_hooks::state_dir()
            .ok_or("there is no folder to keep notes in")?
            .join("lore");
        Ok(Place {
            top,
            cwd,
            store: dir.join(lore::file_name(&key)),
        })
    }

    /// The `files` a tool was given, relative to the top.
    fn files(&self, args: &Value) -> Result<Vec<String>, String> {
        let top = self.top.to_string_lossy();
        let cwd = self.cwd.to_string_lossy();
        let mut out = Vec::new();
        for f in list(args, "files").iter().filter_map(Value::as_str) {
            let rel = lore::relative(f, &top, &cwd)
                .ok_or_else(|| format!("{f} is not in this project ({top})"))?;
            if !out.contains(&rel) {
                out.push(rel);
            }
        }
        Ok(out)
    }

    fn hash(&self, rel: &str) -> Option<u64> {
        fs::read(self.top.join(rel)).ok().map(|b| lore::hash(&b))
    }

    fn commit(&self) -> String {
        git(&self.top, &["rev-parse", "HEAD"])
            .map(|s| s.trim().to_string())
            .unwrap_or_default()
    }

    fn read(&self) -> Result<Lore, String> {
        match fs::read_to_string(&self.store) {
            Ok(s) => serde_json::from_str(&s)
                .map_err(|e| format!("{} could not be read: {e}", self.store.display())),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(Lore::default()),
            Err(e) => Err(e.to_string()),
        }
    }

    /// Reads the notes, changes them and writes them back, with no other
    /// session writing in between.
    fn change<T>(&self, f: impl FnOnce(&mut Lore) -> Result<T, String>) -> Result<T, String> {
        if let Some(dir) = self.store.parent() {
            fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let _lock = Lock::take(&self.store.with_extension("lock")).map_err(|e| e.to_string())?;
        let mut lore = self.read()?;
        let out = f(&mut lore)?;
        let json = serde_json::to_string_pretty(&lore).map_err(|e| e.to_string())?;
        let new = self.store.with_extension("new");
        fs::write(&new, json).map_err(|e| e.to_string())?;
        fs::rename(&new, &self.store).map_err(|e| e.to_string())?;
        Ok(out)
    }
}

fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = horadric_hooks::no_window(&mut Command::new("git"))
        .arg("--no-optional-locks")
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}
