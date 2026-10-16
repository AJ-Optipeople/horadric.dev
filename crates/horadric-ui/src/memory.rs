//! The app's memory: one keeper and one model for every session's
//! `lore_recall`, on a thread of its own, so embedding never holds up a
//! window and ten sessions do not load ten models. See
//! docs/plan/memory.md, part 3.
//!
//! Started by the first recall, not with the app: the model is fetched
//! from Hugging Face the first time, and an app nobody recalls in needs
//! neither the download nor the memory. Until the model is loaded the
//! keeper answers by words.
//!
//! The code index lives in the `horadric` binary, so it hands over what
//! the memory keeps of it with [`set_code_source`]. A recall owes the
//! project's code a sync, done when nothing is asked, so a project the
//! index has never read does not hold the first recall up.

use std::collections::HashMap;
use std::path::Path;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::OnceLock;
use std::thread;
use std::time::{Duration, Instant};

use horadric_hooks::listener::RecallCall;
use horadric_hooks::lore::{now, Place};
use horadric_memory::keeper::{CodeFile, Keeper};
use horadric_memory::recall::{self, Ask};
use horadric_memory::{models, Embedder, Kind};
use serde_json::{json, Value};

/// The model memory runs on: see docs/plan/memory.md, part 1.
const MODEL: Kind = Kind::MiniLm;

/// How long a recall may spend embedding notes the model has not seen,
/// well inside what `horadric mcp` waits.
const BUDGET: Duration = Duration::from_secs(3);

/// How many chunks the background embeds between looks for a recall,
/// and how long it rests after: the model takes every core while it runs,
/// and sessions share them.
const STEP: usize = 2;
const REST: Duration = Duration::from_millis(100);

/// How long a project's code is left alone after a sync: looking is cheap
/// (a stat of each file) but not free, and recalls come in bursts.
const SYNC_EVERY: Duration = Duration::from_secs(30);

/// The code of the project a folder is in, as the memory keeps it.
pub type CodeSource = fn(&Path) -> Result<Vec<CodeFile>, String>;

static CODE: OnceLock<CodeSource> = OnceLock::new();

/// Gives the memory the function that reads a project's code index.
/// Without it the memory finds notes and passages but no code.
pub fn set_code_source(f: CodeSource) {
    let _ = CODE.set(f);
}

enum Msg {
    Recall(RecallCall),
    Model(Result<Box<Embedder>, String>),
}

/// Answers a recall on the memory's thread, starting it the first time.
pub fn ask(call: RecallCall) {
    static TX: OnceLock<Sender<Msg>> = OnceLock::new();
    // A send that fails drops the call, which closes its reply: the server
    // hears so at once and answers by words.
    let _ = TX.get_or_init(start).send(Msg::Recall(call));
}

fn start() -> Sender<Msg> {
    let (tx, rx) = mpsc::channel();
    let Some(state) = horadric_hooks::state_dir() else {
        return tx;
    };
    let loaded = tx.clone();
    let models = state.join("models");
    thread::spawn(move || {
        let _ = loaded.send(Msg::Model(load(&models).map(Box::new)));
    });
    thread::spawn(move || run(&state.join("memory"), rx));
    tx
}

fn load(models: &Path) -> Result<Embedder, String> {
    let dir = models::ensure(MODEL, models, crate::net::get)?;
    Embedder::load(MODEL, &dir)
}

fn run(dir: &Path, rx: Receiver<Msg>) {
    let mut keeper: Keeper<Embedder> = match Keeper::open(dir) {
        Ok(k) => k,
        Err(e) => {
            eprintln!("horadric: memory in {} cannot open: {e}", dir.display());
            return;
        }
    };
    let mut behind = false;
    // The folder a recall came from whose code is owed a sync, and when
    // each was last synced.
    let mut owed: Option<String> = None;
    let mut synced: HashMap<String, Instant> = HashMap::new();
    loop {
        let msg = if behind {
            match rx.recv_timeout(REST) {
                Ok(m) => Some(m),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => return,
            }
        } else {
            match rx.recv() {
                Ok(m) => Some(m),
                Err(_) => return,
            }
        };
        match msg {
            Some(Msg::Recall(call)) => {
                if call.reply.wanted() {
                    let body = match answer(&mut keeper, &call.body) {
                        Ok(text) => json!({ "text": text }),
                        Err(e) => json!({ "error": e }),
                    };
                    call.reply.send(body);
                }
                if let Some(dir) = call.body["dir"].as_str() {
                    if CODE.get().is_some()
                        && synced.get(dir).is_none_or(|t| t.elapsed() >= SYNC_EVERY)
                    {
                        owed = Some(dir.to_string());
                    }
                }
                behind = keeper.has_model() || owed.is_some();
            }
            Some(Msg::Model(Ok(model))) => {
                keeper.set_model(*model);
                behind = true;
            }
            Some(Msg::Model(Err(e))) => eprintln!("horadric: memory has no model: {e}"),
            None => {
                if let Some(dir) = owed.take() {
                    synced.insert(dir.clone(), Instant::now());
                    if let Err(e) = sync_code(&mut keeper, &dir) {
                        eprintln!("horadric: memory could not read the code of {dir}: {e}");
                    }
                    behind = true;
                    continue;
                }
                match keeper.catch_up(STEP) {
                    Ok(n) => behind = n > 0,
                    Err(e) => {
                        eprintln!("horadric: memory could not embed: {e}");
                        behind = false;
                    }
                }
            }
        }
    }
}

fn sync_code(keeper: &mut Keeper<Embedder>, dir: &str) -> Result<(), String> {
    let source = CODE.get().ok_or("no code source")?;
    let place = Place::of(Path::new(dir))?;
    let files = source(Path::new(dir))?;
    keeper
        .sync_code(place.key(), &files, now())
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// What `horadric mcp` asked, answered: `dir` is the folder it runs in,
/// `files` already relative to the top of its project.
fn answer(keeper: &mut Keeper<Embedder>, body: &Value) -> Result<String, String> {
    let dir = body["dir"].as_str().ok_or("say which folder: dir")?;
    let place = Place::of(Path::new(dir))?;
    let lore = place.read()?;
    let kind = match body["kind"].as_str().filter(|k| !k.is_empty()) {
        Some(k) => Some(recall::kind_named(k).ok_or_else(|| format!("no kind {k}"))?),
        None => None,
    };
    let ask = Ask {
        query: body["query"].as_str().unwrap_or("").to_string(),
        files: body["files"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default(),
        kind,
        now: now(),
    };
    keeper.recall(place.key(), &lore, &ask, BUDGET, |p| place.hash(p))
}
