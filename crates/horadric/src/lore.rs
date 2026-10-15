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
use std::sync::Mutex;
use std::time::{Instant, SystemTime};

use horadric_core::lore;
use horadric_core::sanctuary::{self, Graph, Kind, QuestFiles};
use horadric_hooks::lock::Lock;
use horadric_hooks::lore::{now, Place};
use serde_json::Value;

use crate::code::{self, FileIndex, Lang, Store};

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
    let marks = place.marks(files(&place, args)?);
    let commit = place.commit();
    let id = place.change(|l| l.add(text, marks, &commit, now(), &replaces))?;
    Ok(format!(
        "Kept as #{id}. Every session of this project can find it with lore_recall."
    ))
}

fn recall(args: &Value) -> Result<String, String> {
    let query = args.get("query").and_then(Value::as_str).unwrap_or("");
    let place = Place::here()?;
    let files = files(&place, args)?;
    let lore = place.read()?;
    let found = lore.recall(query, &files, |p| place.hash(p));
    Ok(lore::describe(&found, now()))
}

fn find(args: &Value) -> Result<String, String> {
    let name = args.get("name").and_then(Value::as_str).unwrap_or("");
    with_index(&Place::here()?, |files| code::find(files, name))
}

/// The Arcane Sanctuary's graph of the project `place` is in: every file
/// of the tree, the calls between them from the code index, the notes and
/// the done quests.
pub fn graph(place: &Place) -> Result<Graph, String> {
    let calls = with_index(place, code::links)?;
    let files = place.listed().unwrap_or_else(|| {
        let mut found = Vec::new();
        walk(&place.top, "", &mut found);
        found
    });
    let notes = place.read()?.notes;
    let done = crate::task::finished(&place.top);
    let quests: Vec<QuestFiles> = done
        .iter()
        .map(|(title, touched)| QuestFiles { title, touched })
        .collect();
    Ok(Graph::build(&files, &calls, &notes, &quests))
}

/// The graph of the project `dir` is in, for the app's sanctuary pane.
pub fn graph_at(dir: &Path) -> Result<Graph, String> {
    graph(&Place::of(dir)?)
}

/// `horadric sanctuary`: lays the graph out from where it was left and
/// keeps it, saying how large it is and how long that took, so the layout
/// can be tried on a real project before the pane draws it.
pub fn sanctuary() -> Result<(), String> {
    let place = Place::here()?;
    let t = Instant::now();
    let g = graph(&place)?;
    let built = t.elapsed();
    let t = Instant::now();
    let at = sanctuary::layout(&g, &place.positions());
    let laid = t.elapsed();
    place.keep_positions(&at)?;
    let count = |k| g.nodes.iter().filter(|n| n.kind == k).count();
    let orphans = g.nodes.iter().filter(|n| n.weight == 0).count();
    println!(
        "{} files, {} notes, {} quests, {} links, {orphans} with none",
        count(Kind::File),
        count(Kind::Note),
        count(Kind::Quest),
        g.links.len()
    );
    println!("built in {built:?}, laid out in {laid:?}");
    println!("kept in {}", place.sanctuary_store().display());
    Ok(())
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
    let files: Vec<(String, &FileIndex)> = tree
        .into_iter()
        .filter_map(|(rel, k)| m.read.get(&k).map(|f| (rel, f)))
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
