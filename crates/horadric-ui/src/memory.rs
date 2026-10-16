//! The app's memory: one keeper and one model for every session's
//! `lore_recall`, on a thread of its own, so embedding never holds up a
//! window and ten sessions do not load ten models. See
//! docs/plan/memory.md, part 3.
//!
//! Started by the first recall or the first transcript to keep. The model
//! is fetched from Hugging Face by the first recall only, so an app
//! nobody recalls in keeps its dialogue (part 4) without the download or
//! the memory it takes. Until the model is loaded the keeper answers by
//! words.
//!
//! The code index lives in the `horadric` binary, so it hands over what
//! the memory keeps of it with [`set_code_source`]. A recall owes the
//! project's code a sync, done when nothing is asked, so a project the
//! index has never read does not hold the first recall up.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::OnceLock;
use std::thread;
use std::time::{Duration, Instant};

use horadric_hooks::listener::{RecallCall, Reply};
use horadric_hooks::lore::{now, Place};
use horadric_memory::keeper::{Answer, CodeFile, Keeper, Theirs, Unasked};
use horadric_memory::recall::{self, Ask};
use horadric_memory::transcripts::Format;
use horadric_memory::{models, Embedder, Kind};
use serde_json::{json, Value};

/// The model memory runs on: see docs/plan/memory.md, part 1.
const MODEL: Kind = Kind::MiniLm;

/// How long a recall may spend embedding notes the model has not seen,
/// well inside what `horadric mcp` waits.
const BUDGET: Duration = Duration::from_secs(3);

/// The same for a recall on a prompt, which the human waits on: the
/// listener gives up on it after three seconds.
const PROMPT_BUDGET: Duration = Duration::from_secs(1);

/// How long a recall nobody asked for may embed, and how long the quest
/// it is for waits on it: a quest is started from the window's thread,
/// and notes the model has not seen are caught up in the background
/// anyway.
const UNASKED_BUDGET: Duration = Duration::from_millis(500);
const UNASKED_WAIT: Duration = Duration::from_millis(1500);

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

/// A transcript that has grown: a hook said so, with the folder the
/// session is in.
pub struct Keep {
    pub path: PathBuf,
    pub cwd: String,
    pub format: Format,
}

/// Who a recall answers.
enum To {
    /// A session's `lore_recall`, by `horadric mcp`.
    Mcp(Reply),
    /// Horadric itself, for a prompt the agent did not ask it for, by the
    /// time it stops waiting. One found later is not worked out: quests
    /// started in a burst would queue recalls nobody reads, ahead of the
    /// sessions' own.
    Unasked(Sender<Result<Answer, String>>, Instant),
}

enum Msg {
    /// What was asked, and who to answer.
    Recall(Value, To),
    Keep(Keep),
    Model(Result<Box<Embedder>, String>),
}

/// What a session was given by recalls on its prompts: the hits, never
/// shown it again, and the sigils, told once. `horadric mcp` keeps its
/// own for `lore_recall`.
#[derive(Default)]
struct Given {
    seen: HashSet<String>,
    known: Vec<String>,
}

/// Whether recall may look in other projects' memory: the Settings switch,
/// off until turned on.
static ACROSS: AtomicBool = AtomicBool::new(false);

pub fn set_across(on: bool) {
    ACROSS.store(on, Ordering::Relaxed);
}

fn sender() -> &'static Sender<Msg> {
    static TX: OnceLock<Sender<Msg>> = OnceLock::new();
    TX.get_or_init(start)
}

/// Answers a recall on the memory's thread, starting it the first time.
pub fn ask(call: RecallCall) {
    // A send that fails drops the call, which closes its reply: the server
    // hears so at once and answers by words.
    let _ = sender().send(Msg::Recall(call.body, To::Mcp(call.reply)));
}

/// What memory recalls of `query` for the project `dir` is in, the strong
/// hits only (part 9), for a prompt the agent did not ask it for. None
/// when nothing is strong enough, or the memory took too long.
pub fn unasked(dir: &Path, query: &str) -> Option<String> {
    let (tx, rx) = mpsc::channel();
    let body = json!({ "dir": dir.to_string_lossy(), "query": query, "strong": true });
    let until = Instant::now() + UNASKED_WAIT;
    sender()
        .send(Msg::Recall(body, To::Unasked(tx, until)))
        .ok()?;
    match rx.recv_timeout(UNASKED_WAIT) {
        Ok(Ok(a)) if a.text != recall::NOTHING => Some(a.text),
        Ok(Ok(_)) => None,
        Ok(Err(e)) => {
            eprintln!("horadric: memory could not recall for a quest: {e}");
            None
        }
        Err(_) => None,
    }
}

/// Keeps what a transcript gained, on the memory's thread. Told by the
/// Stop, PreCompact and SessionEnd hooks.
pub fn keep(job: Keep) {
    let _ = sender().send(Msg::Keep(job));
}

fn start() -> Sender<Msg> {
    let (tx, rx) = mpsc::channel();
    let Some(state) = horadric_hooks::state_dir() else {
        return tx;
    };
    let loaded = tx.clone();
    thread::spawn(move || run(&state, rx, loaded));
    tx
}

/// What is on disk now, to keep once, newest first: the first start of
/// the memory reads it a file at a time, resting between.
fn backlog() -> Vec<(PathBuf, Format)> {
    let claude = horadric_hooks::transcript::files()
        .into_iter()
        .map(|p| (p, Format::Claude));
    let codex = horadric_hooks::codex::files()
        .into_iter()
        .map(|p| (p, Format::Codex));
    // Reversed, since the loop takes from the end.
    let mut all: Vec<_> = claude.chain(codex).collect();
    all.reverse();
    all
}

/// The project a folder belongs to, asked of git once per folder: the
/// transcripts of a day name a handful.
#[derive(Default)]
struct Projects(HashMap<String, Option<String>>);

impl Projects {
    fn of(&mut self, cwd: &str) -> Option<String> {
        self.0
            .entry(cwd.to_lowercase())
            .or_insert_with(|| Place::of(Path::new(cwd)).ok().map(|p| p.key().to_string()))
            .clone()
    }
}

fn load(models: &Path) -> Result<Embedder, String> {
    let dir = models::ensure(MODEL, models, crate::net::get)?;
    Embedder::load(MODEL, &dir)
}

fn run(state: &Path, rx: Receiver<Msg>, loaded: Sender<Msg>) {
    let dir = state.join("memory");
    let models = state.join("models");
    let mut keeper: Keeper<Embedder> = match Keeper::open(&dir) {
        Ok(k) => k,
        Err(e) => {
            eprintln!("horadric: memory in {} cannot open: {e}", dir.display());
            return;
        }
    };
    let mut backlog = backlog();
    let mut projects = Projects::default();
    let mut loading = false;
    let mut behind = !backlog.is_empty();
    // The folder a recall came from whose code is owed a sync, and when
    // each was last synced.
    let mut owed: Option<String> = None;
    let mut synced: HashMap<String, Instant> = HashMap::new();
    let mut sessions: HashMap<String, Given> = HashMap::new();
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
            Some(Msg::Recall(body, to)) => {
                // The model is wanted from the first recall on, not before.
                if !loading {
                    loading = true;
                    let (models, loaded) = (models.clone(), loaded.clone());
                    thread::spawn(move || {
                        let _ = loaded.send(Msg::Model(load(&models).map(Box::new)));
                    });
                }
                match to {
                    To::Mcp(reply) if reply.wanted() => {
                        let wanted = || reply.wanted();
                        let a = answer(&mut keeper, &body, BUDGET, &mut sessions, wanted);
                        reply.send(match a {
                            Ok(a) => json!({ "text": a.text, "given": a.given }),
                            Err(e) => json!({ "error": e }),
                        });
                    }
                    To::Mcp(_) => {}
                    To::Unasked(reply, until) if Instant::now() < until => {
                        let a = answer(&mut keeper, &body, UNASKED_BUDGET, &mut sessions, || true);
                        let _ = reply.send(a);
                    }
                    To::Unasked(..) => {}
                }
                if let Some(dir) = body["dir"].as_str() {
                    if CODE.get().is_some()
                        && synced.get(dir).is_none_or(|t| t.elapsed() >= SYNC_EVERY)
                    {
                        owed = Some(dir.to_string());
                    }
                }
                behind = keeper.has_model() || !backlog.is_empty() || owed.is_some();
            }
            Some(Msg::Keep(job)) => {
                kept(
                    &mut keeper,
                    &mut projects,
                    &job.path,
                    job.format,
                    Some(&job.cwd),
                );
            }
            Some(Msg::Model(Ok(model))) => {
                keeper.set_model(*model);
                behind = true;
            }
            Some(Msg::Model(Err(e))) => eprintln!("horadric: memory has no model: {e}"),
            // What was said before the app ran comes first: text is cheap to
            // keep, and the embedding of it can wait.
            None => {
                if let Some(dir) = owed.take() {
                    synced.insert(dir.clone(), Instant::now());
                    if let Err(e) = sync_code(&mut keeper, &dir) {
                        eprintln!("horadric: memory could not read the code of {dir}: {e}");
                    }
                    behind = true;
                    continue;
                }
                if let Some((path, format)) = backlog.pop() {
                    kept(&mut keeper, &mut projects, &path, format, None);
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

fn kept(
    keeper: &mut Keeper<Embedder>,
    projects: &mut Projects,
    path: &Path,
    format: Format,
    cwd: Option<&str>,
) {
    if let Err(e) = keeper.ingest(path, format, cwd, |c| projects.of(c), now()) {
        eprintln!("horadric: memory could not keep {}: {e}", path.display());
    }
}

/// What `horadric mcp` asked, answered: `dir` is the folder it runs in,
/// `files` already relative to the top of its project, `known` the sigils
/// the session was told, `unroll` a hit it wants word for word. Or what
/// the listener asks on a session's prompt (`prompt` true, with the
/// Horadric `session` and the agent's own `agent_session`), answered
/// only with what clears the bar and the session was not shown. With
/// `strong`, only the hits strong enough to show an agent that did not
/// ask.
fn answer(
    keeper: &mut Keeper<Embedder>,
    body: &Value,
    budget: Duration,
    sessions: &mut HashMap<String, Given>,
    wanted: impl Fn() -> bool,
) -> Result<Answer, String> {
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
        history: body["history"].as_bool() == Some(true),
        least: if body["strong"] == true {
            recall::STRONG
        } else {
            0.0
        },
    };
    let theirs = |key: &str| horadric_hooks::lore::of_key(key).ok();
    let across: Option<Theirs> = ACROSS.load(Ordering::Relaxed).then_some(&theirs);
    if let Some(id) = body["unroll"].as_str().filter(|u| !u.trim().is_empty()) {
        return keeper.unroll(place.key(), &lore, id, across.is_some(), ask.now, |p| {
            place.hash(p)
        });
    }
    if body["prompt"].as_bool() == Some(true) {
        // Keyed by the agent's own session: a `/clear` starts a new one,
        // whose context holds none of what the old one was given.
        let agent_session = body["agent_session"].as_str().filter(|s| !s.is_empty());
        let session = agent_session
            .or(body["session"].as_str())
            .ok_or("say which session")?;
        let given = sessions.entry(session.to_string()).or_default();
        let unasked = Unasked {
            seen: &given.seen,
            session: agent_session,
        };
        let mut a = keeper.recall(
            place.key(),
            &lore,
            &ask,
            across,
            PROMPT_BUDGET,
            |p| place.hash(p),
            &given.known,
            Some(unasked),
        )?;
        // A reply the listener gave up on never reached the agent, so what
        // it held was neither shown nor told.
        if wanted() {
            given.seen.extend(a.shown.iter().cloned());
            given.known.extend(a.given.iter().cloned());
        }
        if !a.text.is_empty() {
            a.text = format!("{}\n\n{}", recall::OFFERED, a.text);
        }
        return Ok(a);
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
    keeper.recall(
        place.key(),
        &lore,
        &ask,
        across,
        budget,
        |p| place.hash(p),
        &known,
        None,
    )
}
