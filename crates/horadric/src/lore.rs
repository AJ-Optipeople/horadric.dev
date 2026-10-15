//! The lore tools of `horadric mcp`: where a project's notes are kept and
//! how they are read and written. What a note is and how one is found is
//! in `horadric_core::lore`.
//!
//! The code index is here too: which files a project has, and which of
//! them changed since they were read. What is read from a file and the
//! questions asked of it are in `code.rs`.
//!
//! Every session of a project runs its own `horadric mcp`, so two may write
//! at once. A lock file beside the notes keeps them taking turns, and a
//! write goes to a new file first, so a reader never sees half of one.

use std::collections::HashMap;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use horadric_core::lore::{self, Lore, Mark};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;

use crate::code::{self, FileIndex, Lang, Store};

/// How long a lock is waited for before it is taken as left by a process
/// that died holding it.
const LOCK_WAIT: Duration = Duration::from_secs(3);

/// How many files of a project the code index reads at most, so a folder
/// that is not a project (a home folder) does not take minutes.
const MOST_FILES: usize = 20_000;

/// Answers a lore tool, or None when `name` is not one.
pub fn tool(name: &str, args: &Value) -> Option<Result<String, String>> {
    match name {
        "lore_note" => Some(note(args)),
        "lore_recall" => Some(recall(args)),
        "lore_find" => Some(find(args)),
        "lore_calls" => Some(calls(args)),
        "lore_outline" => Some(outline(args)),
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

fn find(args: &Value) -> Result<String, String> {
    let name = args.get("name").and_then(Value::as_str).unwrap_or("");
    with_index(&Place::here()?, |files| code::find(files, name))
}

fn calls(args: &Value) -> Result<String, String> {
    let name = args.get("name").and_then(Value::as_str).unwrap_or("");
    let which = args.get("which").and_then(Value::as_str).unwrap_or("both");
    with_index(&Place::here()?, |files| code::calls(files, name, which))
}

/// One file needs no index: it is read now, which is as quick as looking
/// it up and always fresh.
fn outline(args: &Value) -> Result<String, String> {
    let file = args.get("file").and_then(Value::as_str).unwrap_or("");
    if file.is_empty() {
        return Err("say which file: file".into());
    }
    let place = Place::here()?;
    let top = place.top.to_string_lossy();
    let rel = lore::relative(file, &top, &place.cwd.to_string_lossy())
        .ok_or_else(|| format!("{file} is not in this project ({top})"))?;
    let lang = Lang::of(&rel)
        .ok_or_else(|| format!("{rel}: the index reads Rust, TypeScript, JavaScript and Python"))?;
    let bytes = fs::read(place.top.join(&rel)).map_err(|e| format!("{rel}: {e}"))?;
    Ok(code::outline(&rel, &code::read(lang, &bytes)))
}

/// What this process knows of the working tree between calls, so a file
/// whose time and size did not change is not read again.
struct Memory {
    top: PathBuf,
    /// A file's time, size and key when it was last read.
    stamps: HashMap<String, (SystemTime, u64, String)>,
    /// What was read, by key: from the store and since.
    read: HashMap<String, FileIndex>,
    /// Whether the store has heard which files this tree has.
    told: bool,
}

static MEMORY: Mutex<Option<Memory>> = Mutex::new(None);

/// Answers a question with the index of the working tree: what any
/// session of the project read before, and what changed since read now.
fn with_index<T>(place: &Place, ask: impl FnOnce(&code::Files) -> T) -> Result<T, String> {
    let mut memory = MEMORY.lock().unwrap_or_else(|e| e.into_inner());
    if memory.as_ref().is_none_or(|m| m.top != place.top) {
        let store: Store = read_json(&place.code_store())?;
        let read = if store.version == code::VERSION {
            store
                .files
                .into_iter()
                .map(|(k, kept)| (k, kept.index))
                .collect()
        } else {
            HashMap::new()
        };
        *memory = Some(Memory {
            top: place.top.clone(),
            stamps: HashMap::new(),
            read,
            told: false,
        });
    }
    let m = memory.as_mut().expect("memory was just filled");
    let mut fresh = Vec::new();
    let mut tree: Vec<(String, String)> = Vec::new();
    for rel in place.code_files() {
        let Some(lang) = Lang::of(&rel) else {
            continue;
        };
        let full = place.top.join(&rel);
        let Ok(meta) = fs::metadata(&full) else {
            continue;
        };
        if !meta.is_file() || meta.len() > code::LARGEST {
            continue;
        }
        let time = meta.modified().ok();
        let known = m
            .stamps
            .get(&rel)
            .filter(|(t, len, _)| Some(*t) == time && *len == meta.len());
        let key = match known {
            Some((_, _, k)) => k.clone(),
            None => {
                let Ok(bytes) = fs::read(&full) else {
                    continue;
                };
                let k = code::key(lang, &bytes);
                if !m.read.contains_key(&k) {
                    let index = code::read(lang, &bytes);
                    fresh.push((k.clone(), index.clone()));
                    m.read.insert(k.clone(), index);
                }
                if let Some(t) = time {
                    m.stamps.insert(rel.clone(), (t, meta.len(), k.clone()));
                }
                k
            }
        };
        tree.push((rel, key));
    }
    if !fresh.is_empty() || !m.told {
        let had: Vec<&str> = tree.iter().map(|(_, k)| k.as_str()).collect();
        // The answer does not need the store, so a store that can not be
        // written now is written after the next question.
        let stored = change_json(&place.code_store(), |s: &mut Store| {
            s.merge(fresh, &had, now());
            Ok(())
        });
        m.told = stored.is_ok();
    }
    let files: Vec<(String, &FileIndex)> = tree
        .into_iter()
        .filter_map(|(rel, k)| m.read.get(&k).map(|f| (rel, f)))
        .collect();
    Ok(ask(&files))
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

    /// Where the code index is kept, beside the notes.
    fn code_store(&self) -> PathBuf {
        self.store.with_extension("code.json")
    }

    /// The files of the working tree, relative to its top: what git has
    /// or would have, so ignored folders (`target`, `node_modules`) are
    /// left out.
    fn code_files(&self) -> Vec<String> {
        let listed = git(
            &self.top,
            &[
                "ls-files",
                "-z",
                "--cached",
                "--others",
                "--exclude-standard",
            ],
        );
        let mut files: Vec<String> = match listed {
            Some(out) => out
                .split('\0')
                .filter(|s| !s.is_empty())
                .take(MOST_FILES)
                .map(String::from)
                .collect(),
            None => {
                let mut found = Vec::new();
                walk(&self.top, "", &mut found);
                found
            }
        };
        // A file in a merge is listed once per side.
        files.dedup();
        files
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
        read_json(&self.store)
    }

    fn change<T>(&self, f: impl FnOnce(&mut Lore) -> Result<T, String>) -> Result<T, String> {
        change_json(&self.store, f)
    }
}

/// Every file below `dir` that is not in a hidden or build folder, for a
/// project that is not in git.
fn walk(dir: &Path, rel: &str, found: &mut Vec<String>) {
    const SKIP: &[&str] = &[
        "node_modules",
        "target",
        "dist",
        "build",
        "out",
        "venv",
        "__pycache__",
    ];
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        if found.len() >= MOST_FILES {
            return;
        }
        let name = e.file_name().to_string_lossy().into_owned();
        let path = if rel.is_empty() {
            name.clone()
        } else {
            format!("{rel}/{name}")
        };
        match e.file_type() {
            Ok(t) if t.is_dir() => {
                if !name.starts_with('.') && !SKIP.contains(&name.as_str()) {
                    walk(&e.path(), &path, found);
                }
            }
            Ok(t) if t.is_file() => found.push(path),
            _ => {}
        }
    }
}

fn read_json<S: DeserializeOwned + Default>(path: &Path) -> Result<S, String> {
    match fs::read_to_string(path) {
        Ok(s) => serde_json::from_str(&s)
            .map_err(|e| format!("{} could not be read: {e}", path.display())),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(S::default()),
        Err(e) => Err(e.to_string()),
    }
}

/// Reads a file, changes it and writes it back, with no other session
/// writing in between.
fn change_json<S: Serialize + DeserializeOwned + Default, T>(
    path: &Path,
    f: impl FnOnce(&mut S) -> Result<T, String>,
) -> Result<T, String> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let _lock = Lock::take(&path.with_extension("lock"))?;
    let mut value: S = read_json(path)?;
    let out = f(&mut value)?;
    let json = serde_json::to_string(&value).map_err(|e| e.to_string())?;
    let new = path.with_extension("new");
    fs::write(&new, json).map_err(|e| e.to_string())?;
    fs::rename(&new, path).map_err(|e| e.to_string())?;
    Ok(out)
}

/// A lock file, removed when dropped.
struct Lock(PathBuf);

impl Lock {
    fn take(path: &Path) -> Result<Lock, String> {
        let mut waited = Duration::ZERO;
        let step = Duration::from_millis(20);
        loop {
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)
            {
                Ok(_) => return Ok(Lock(path.to_path_buf())),
                Err(e) if e.kind() == ErrorKind::AlreadyExists => {
                    if waited >= LOCK_WAIT {
                        // Its holder died: no write takes this long.
                        let _ = fs::remove_file(path);
                        waited = Duration::ZERO;
                    }
                    thread::sleep(step);
                    waited += step;
                }
                Err(e) => return Err(e.to_string()),
            }
        }
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
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
