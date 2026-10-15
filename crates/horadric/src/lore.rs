//! The lore tools of `horadric mcp`. Where the notes are kept is in
//! `horadric_hooks::lore`, what a note is and how one is found in
//! `horadric_core::lore`.
//!
//! The code index is here too: which files a working tree has, which of
//! them changed since they were read, and where what was read is kept.
//! What is read from a file and the questions asked of it are in
//! `code.rs`.

use std::cell::OnceCell;
use std::collections::HashMap;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime};

use horadric_core::pack::Touched;
use horadric_core::sanctuary::{self, Graph, Kind, Positions, QuestFiles};
use horadric_core::sky::{Project, Sky};
use horadric_core::{chronicle, lore};
use horadric_hooks::lock::Lock;
use horadric_hooks::lore::{now, Place};
use horadric_hooks::tasks as tasks_file;
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
    graph_with(place, &crate::task::finished(&place.top))
}

/// The graph of `place` with its done quests already found.
fn graph_with(place: &Place, done: &[(String, Touched)]) -> Result<Graph, String> {
    let calls = with_index(place, code::links)?;
    let files = place.listed().unwrap_or_else(|| {
        let mut found = Vec::new();
        walk(&place.top, "", &mut found);
        found
    });
    let notes = place.read()?.notes;
    let quests: Vec<QuestFiles> = done
        .iter()
        .map(|(title, touched)| QuestFiles { title, touched })
        .collect();
    Ok(Graph::build(&files, &calls, &notes, &quests))
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

/// One project of Tristram's sky: its graph and where its nodes were left.
pub struct Charted {
    pub key: String,
    pub name: String,
    pub graph: Graph,
    pub at: Positions,
}

/// Tristram's sky: every project Horadric knows as a galaxy, `projects`
/// in the galaxies' order so a star's node can be looked up, and the
/// projects whose graph could not be built, with why.
pub struct TownSky {
    pub sky: Sky,
    pub projects: Vec<Arc<Charted>>,
    pub missed: Vec<(String, String)>,
}

/// A charted project as the cache keeps it: the files its graph is read
/// out of and their times when it was built. Edits the code index has not
/// read yet are not in them, so the sky shows a project as its index last
/// saw it, which is close enough at that distance; its own sanctuary reads
/// it fresh.
struct Kept {
    sources: Vec<PathBuf>,
    stamp: Vec<Option<SystemTime>>,
    charted: Arc<Charted>,
}

static CHARTED: Mutex<Option<HashMap<String, Kept>>> = Mutex::new(None);

/// Tristram's sky as it is now, from the same projects `horadric projects`
/// lists. A project whose sources have not changed since it was last
/// charted in this process is taken as it was, so opening the town again
/// costs only a look at a few file times per project.
pub fn town_sky() -> TownSky {
    let records = OnceCell::new();
    let mut projects = Vec::new();
    let mut missed = Vec::new();
    for dir in horadric_ui::town::known_dirs() {
        let key = horadric_ui::folder_key(&dir);
        match chart(&dir, &key, &records) {
            Ok(c) => projects.push(c),
            Err(e) => missed.push((horadric_ui::project_name(&key), e)),
        }
    }
    projects.sort_by(|a, b| a.key.cmp(&b.key));
    let sky = Sky::build(
        &projects
            .iter()
            .map(|c| Project {
                key: &c.key,
                name: &c.name,
                graph: &c.graph,
                at: &c.at,
            })
            .collect::<Vec<_>>(),
    );
    TownSky {
        sky,
        projects,
        missed,
    }
}

fn stamp(sources: &[PathBuf]) -> Vec<Option<SystemTime>> {
    sources
        .iter()
        .map(|p| fs::metadata(p).and_then(|m| m.modified()).ok())
        .collect()
}

/// One project of the sky, from the cache while its sources are as they
/// were, else built, and laid out and kept when it has nodes never placed.
fn chart(
    dir: &str,
    key: &str,
    records: &OnceCell<Vec<chronicle::Record>>,
) -> Result<Arc<Charted>, String> {
    // Asking git where the project is costs a process start, more than
    // the rest of a look at a charted project, so the cache is by key.
    if let Some(k) = CHARTED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .and_then(|m| m.get(key))
    {
        if stamp(&k.sources) == k.stamp {
            return Ok(k.charted.clone());
        }
    }
    let place = Place::of(Path::new(dir))?;
    let log = tasks_file::find_list(&place.top);
    let sources = vec![
        place.code_store(),
        place.notes_store().to_path_buf(),
        place.sanctuary_store(),
        place.top.join(".git").join("index"),
        log.as_deref().map(tasks_file::file).unwrap_or_default(),
        horadric_hooks::state_dir()
            .unwrap_or_default()
            .join(chronicle::FILE),
    ];
    let before = stamp(&sources);
    let done = match &log {
        Some(log) => crate::task::finished_in(log, records.get_or_init(horadric_ui::chronicle_all)),
        None => Vec::new(),
    };
    let graph = graph_with(&place, &done)?;
    let mut at = place.positions();
    if graph.nodes.iter().any(|n| !at.at.contains_key(&n.id)) {
        at = sanctuary::layout(&graph, &at);
        place.keep_positions(&at)?;
    }
    // Building wrote the code index and keeping wrote the positions, so
    // their times are taken again; the rest as they were before reading,
    // so a change made meanwhile is seen next time.
    let mut stamp = before;
    let after = self::stamp(&sources);
    stamp[0] = after[0];
    stamp[2] = after[2];
    let charted = Arc::new(Charted {
        key: key.to_string(),
        name: horadric_ui::project_name(key),
        graph,
        at,
    });
    CHARTED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_insert_with(HashMap::new)
        .insert(
            key.to_string(),
            Kept {
                sources,
                stamp,
                charted: charted.clone(),
            },
        );
    Ok(charted)
}

/// `horadric sky`: charts every known project into Tristram's sky twice,
/// cold and then from the cache, saying what each galaxy holds and how
/// long it took, so the sky can be tried before the town's pane draws it.
pub fn sky() -> Result<(), String> {
    let t = Instant::now();
    let town = town_sky();
    let cold = t.elapsed();
    let t = Instant::now();
    let again = town_sky();
    let warm = t.elapsed();
    for (g, c) in town.sky.galaxies.iter().zip(&town.projects) {
        println!(
            "{:<28} {:>5} nodes {:>6} links  radius {:>4.0} at ({:>5.0}, {:>5.0})",
            g.name,
            c.graph.nodes.len(),
            c.graph.links.len(),
            g.radius,
            g.centre[0],
            g.centre[1]
        );
    }
    for (name, why) in &town.missed {
        println!("{name:<28} not charted: {why}");
    }
    let (_, r) = town.sky.bounds();
    println!(
        "{} galaxies, {} stars, {} links, {r:.0} across from the middle",
        town.sky.galaxies.len(),
        town.sky.stars.len(),
        town.sky.links.len()
    );
    println!(
        "charted in {cold:?}, again from the cache in {warm:?} ({} the same)",
        if again.sky == town.sky {
            "all"
        } else {
            "not all"
        }
    );
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
