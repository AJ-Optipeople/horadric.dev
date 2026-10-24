//! The lore tools of `horadric mcp`. Where the notes are kept is in
//! `horadric_hooks::lore`, what a note is and how one is found in
//! `horadric_core::lore`.
//!
//! The code index is here too: which files a working tree has, which of
//! them changed since they were read, and where what was read is kept.
//! What is read from a file and the questions asked of it are in
//! `code.rs`.

use std::collections::HashMap;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, SystemTime};

use horadric_core::lore::Origin;
use horadric_hooks::lock::Lock;
use horadric_hooks::lore::{now, Place};
use horadric_hooks::{client, COMMAND_HEADER, OWNER_ENV, RECALL_PATH};
use horadric_memory::distill;
use horadric_memory::horadrim::{self, Tongue};
use horadric_memory::keeper;
use horadric_memory::parts::Tree;
use horadric_memory::recall::{self, Candidate};
use horadric_memory::store::Store as MemoryStore;
use serde_json::Value;

use crate::code::{self, FileIndex, Lang, Store};

#[cfg(windows)]
#[path = "lore_sky.rs"]
mod sky;
#[cfg(windows)]
pub use sky::{graph_at, sanctuary, sky, town_graph};

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
    let sources: Vec<String> = list(args, "sources")
        .iter()
        .filter_map(Value::as_str)
        .map(|s| s.trim().to_string())
        .collect();
    let place = Place::here()?;
    let marks = place.marks(files(&place, args)?);
    let commit = place.commit();
    // The passages are looked up before the note is kept, so a note never
    // names one the memory does not hold, and is true from the earliest.
    let since = if sources.is_empty() {
        None
    } else {
        let dir = horadric_hooks::state_dir()
            .ok_or("no state folder to find the passages in")?
            .join("memory");
        distill::earliest(&dir, &sources)?
    };
    let origin = Origin { since, sources };
    // The symbols it names are taken as they are now, so the note goes
    // stale when they change, not whenever anything in their files does.
    let parts: Vec<horadric_core::lore::Part> = marks
        .iter()
        .filter_map(|m| {
            let bytes = fs::read(place.top.join(&m.path)).ok()?;
            let pieces = code::pieces(&m.path, &bytes)?;
            Some(horadric_core::lore::parts_of(text, &m.path, &pieces))
        })
        .flatten()
        .collect();
    let id = place.change(|l| {
        let id = l.add_from(text, marks, &commit, now(), &replaces, origin)?;
        l.set_parts(id, parts);
        Ok(id)
    })?;
    Ok(format!(
        "Kept as #{id}. Every session of this project can find it with lore_recall."
    ))
}

/// Asks the app, which holds the model and finds by meaning; when it is
/// not running, or too busy to answer in time, finds by words here.
fn recall(args: &Value) -> Result<String, String> {
    let query = args.get("query").and_then(Value::as_str).unwrap_or("");
    let kind_name = args.get("kind").and_then(Value::as_str).unwrap_or("");
    let kind = match kind_name {
        "" => None,
        k => Some(
            recall::kind_named(k)
                .ok_or_else(|| format!("kind is one of note, quest, dialogue or code, not {k}"))?,
        ),
    };
    let unroll = args.get("unroll").and_then(Value::as_str).unwrap_or("");
    let history = args.get("history").and_then(Value::as_bool) == Some(true);
    let place = Place::here()?;
    let files = files(&place, args)?;
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    let body = serde_json::json!({
        "dir": cwd.to_string_lossy(),
        "query": query,
        "files": files,
        "kind": kind_name,
        "unroll": unroll,
        "known": known().clone(),
        "history": history,
    });
    if let Some((text, given)) = ask_app(&body) {
        known().extend(given);
        if unroll.trim().is_empty() {
            handed(&text);
        }
        return Ok(text);
    }
    let lore = place.read()?;
    if !unroll.trim().is_empty() {
        // Inscribed hits come only from the app, so what is unrolled here
        // is a note, a passage the app kept, or the Lexicon.
        let dir = horadric_hooks::state_dir()
            .ok_or("no state folder")?
            .join("memory");
        if unroll.trim() == "lexicon" {
            let a = keeper::lexicon(&Tongue::load(&dir).map_err(|e| e.to_string())?, place.key());
            known().extend(a.given);
            return Ok(a.text);
        }
        let store = MemoryStore::open(&dir).map_err(|e| e.to_string())?;
        return keeper::unroll(&store, place.key(), &lore, unroll, false, now(), |p| {
            place.hash(p)
        });
    }
    let ask = recall::Ask {
        query: query.to_string(),
        files,
        kind,
        now: now(),
        history,
        least: 0.0,
    };
    let mut cands: Vec<Candidate> = lore.notes.iter().map(Candidate::note).collect();
    if history {
        cands.extend(lore.past.iter().map(Candidate::past));
    }
    let tree = Tree::new(&place.top, Some(code::pieces));
    let found = recall::rank(&ask, &cands, &[], |i| {
        !recall::stale(&cands[i], &tree).is_empty()
    });
    if found.is_empty() {
        return Ok(recall::NOTHING.to_string());
    }
    let text = horadrim::plain(&recall::hits(&cands, &found, ask.now, &tree));
    handed(&text);
    Ok(text)
}

/// Logs the hits of a `lore_recall` answer as handed to this session, so
/// `horadric quest spend` can tell whether they were used. One `horadric
/// mcp` runs per session, and Claude Code passes it its conversation id.
fn handed(text: &str) {
    // A test's recalls are not a session's, and the state folder is real.
    if cfg!(test) {
        return;
    }
    let var = |k: &str| std::env::var(k).unwrap_or_default();
    horadric_hooks::given::keep(
        horadric_core::spend::uptake::ASKED,
        &var(horadric_hooks::SESSION_ENV),
        &var("CLAUDE_CODE_SESSION_ID"),
        text,
    );
}

/// How long a recall waits on the app: a little more than the listener
/// waits on the memory, so its timeout is the one heard.
const RECALL_WAIT: Duration = Duration::from_secs(12);

/// The sigils of Horadrim this session was told, so the app tells each
/// once. One `horadric mcp` runs per session, so its memory is the
/// session's.
fn known() -> MutexGuard<'static, Vec<String>> {
    static KNOWN: Mutex<Vec<String>> = Mutex::new(Vec::new());
    KNOWN.lock().unwrap_or_else(|e| e.into_inner())
}

/// What the app's memory recalls of `query` for the project `dir` is in,
/// the strong hits only, as a quest's prompt gets them: None inside when
/// nothing is strong enough, None outside when the app did not answer.
/// Without the model nothing is strong, so finding by words here would
/// show what no quest gets.
#[cfg(windows)]
pub fn recall_strong(dir: &Path, query: &str) -> Option<Option<String>> {
    let body = serde_json::json!({ "dir": dir.to_string_lossy(), "query": query, "strong": true });
    ask_app(&body).map(|(text, _)| Some(text).filter(|t| t != recall::NOTHING))
}

/// The app's answer to a recall and the sigils it told, None when there
/// is none to be had.
fn ask_app(body: &Value) -> Option<(String, Vec<String>)> {
    let port = std::env::var(OWNER_ENV)
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or_else(horadric_hooks::port);
    let headers = [(COMMAND_HEADER, "recall")];
    let (status, text) =
        client::ask(port, RECALL_PATH, &headers, &body.to_string(), RECALL_WAIT).ok()?;
    if status != 200 {
        return None;
    }
    let answer: Value = serde_json::from_str(&text).ok()?;
    let given = answer["given"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    Some((answer.get("text")?.as_str()?.to_string(), given))
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
    let rel = place.relative([file])?.remove(0);
    let lang = Lang::of(&rel)
        .ok_or_else(|| format!("{rel}: the index reads Rust, TypeScript, JavaScript and Python"))?;
    let bytes = fs::read(place.top.join(&rel)).map_err(|e| format!("{rel}: {e}"))?;
    Ok(code::outline(&rel, &code::read(lang, &bytes)))
}

/// The `files` a tool was given, relative to the top.
fn files(place: &Place, args: &Value) -> Result<Vec<String>, String> {
    place.relative(list(args, "files").iter().filter_map(Value::as_str))
}

fn list<'a>(args: &'a Value, key: &str) -> &'a [Value] {
    args.get(key)
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
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
    with_keyed(place, |keyed| {
        let files: Vec<(String, &FileIndex)> =
            keyed.iter().map(|(rel, _, f)| (rel.clone(), *f)).collect();
        ask(&files)
    })
}

/// What the memory keeps of the project's code: for each file the index
/// has read, a text for each of its symbols, and the hash of its bytes,
/// which is what a note's marks hold too.
pub fn code_files(dir: &Path) -> Result<Vec<horadric_memory::keeper::CodeFile>, String> {
    with_keyed(&Place::of(dir)?, |keyed| {
        keyed
            .iter()
            .filter_map(|(rel, key, f)| {
                let hash = u64::from_str_radix(key.rsplit(':').next()?, 16).ok()?;
                Some(horadric_memory::keeper::CodeFile {
                    path: rel.clone(),
                    hash,
                    symbols: code::passages(rel, f),
                })
            })
            .filter(|f| !f.symbols.is_empty())
            .collect()
    })
}

/// [`with_index`] with each file's key as well.
fn with_keyed<T>(
    place: &Place,
    ask: impl FnOnce(&[(String, String, &FileIndex)]) -> T,
) -> Result<T, String> {
    let mut memory = MEMORY.lock().unwrap_or_else(|e| e.into_inner());
    if memory.as_ref().is_none_or(|m| m.top != place.top) {
        // An index that can not be read is only a cache: it starts again.
        let store = read_store(&place.code_store()).unwrap_or_default();
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
    let listed = place.listed().unwrap_or_else(|| {
        let mut found = Vec::new();
        walk(&place.top, "", &mut found);
        found
    });
    let mut fresh = Vec::new();
    let mut tree: Vec<(String, String)> = Vec::new();
    for rel in listed.into_iter().take(MOST_FILES) {
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
        m.told = change_store(&place.code_store(), |s| s.merge(fresh, &had, now())).is_ok();
    }
    let files: Vec<(String, String, &FileIndex)> = tree
        .into_iter()
        .filter_map(|(rel, k)| m.read.get(&k).map(|f| (rel, k, f)))
        .collect();
    Ok(ask(&files))
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

fn read_store(path: &Path) -> Result<Store, String> {
    match fs::read_to_string(path) {
        Ok(s) => serde_json::from_str(&s)
            .map_err(|e| format!("{} could not be read: {e}", path.display())),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(Store::default()),
        Err(e) => Err(e.to_string()),
    }
}

/// Reads the code index, changes it and writes it back, with no other
/// session writing in between, the way the notes are written.
fn change_store(path: &Path, f: impl FnOnce(&mut Store)) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let _lock = Lock::take(&path.with_extension("lock")).map_err(|e| e.to_string())?;
    // An index that can not be read is only a cache: it starts again.
    let mut store = read_store(path).unwrap_or_default();
    f(&mut store);
    let json = serde_json::to_string(&store).map_err(|e| e.to_string())?;
    let new = path.with_extension("new");
    fs::write(&new, json).map_err(|e| e.to_string())?;
    fs::rename(&new, path).map_err(|e| e.to_string())
}
