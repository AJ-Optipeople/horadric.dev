//! The app's memory: one keeper and one model for every session's
//! `lore_recall`, on a thread of its own, so embedding never holds up a
//! window and ten sessions do not load ten models. See
//! docs/plan/memory.md, part 3.
//!
//! Started by the first recall, not with the app: the model is fetched
//! from Hugging Face the first time, and an app nobody recalls in needs
//! neither the download nor the memory. Until the model is loaded the
//! keeper answers by words.

use std::path::Path;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::OnceLock;
use std::thread;
use std::time::Duration;

use horadric_hooks::listener::RecallCall;
use horadric_hooks::lore::{now, Place};
use horadric_memory::keeper::{Answer, Keeper};
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
                        Ok(a) => json!({ "text": a.text, "given": a.given }),
                        Err(e) => json!({ "error": e }),
                    };
                    call.reply.send(body);
                }
                behind = keeper.has_model();
            }
            Some(Msg::Model(Ok(model))) => {
                keeper.set_model(*model);
                behind = true;
            }
            Some(Msg::Model(Err(e))) => eprintln!("horadric: memory has no model: {e}"),
            None => match keeper.catch_up(STEP) {
                Ok(n) => behind = n > 0,
                Err(e) => {
                    eprintln!("horadric: memory could not embed: {e}");
                    behind = false;
                }
            },
        }
    }
}

/// What `horadric mcp` asked, answered: `dir` is the folder it runs in,
/// `files` already relative to the top of its project, `known` the sigils
/// the session was told, `unroll` a hit it wants word for word.
fn answer(keeper: &mut Keeper<Embedder>, body: &Value) -> Result<Answer, String> {
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
    if let Some(id) = body["unroll"].as_str().filter(|u| !u.trim().is_empty()) {
        return keeper.unroll(place.key(), &lore, id, ask.now, |p| place.hash(p));
    }
    let known: Vec<String> = body["known"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    keeper.recall(place.key(), &lore, &ask, BUDGET, |p| place.hash(p), &known)
}
