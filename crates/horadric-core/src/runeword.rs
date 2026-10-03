//! Runewords: runes cast on one session one after another, a turn each,
//! the way runes in the right order make a runeword. "Test, review, merge"
//! given to a session tells it to test, has a reviewer read its work and
//! tells it to answer the review, then merges its branch.
//!
//! Runes and the cube's recipes are one system. A rune is an action on a
//! session; a recipe casts one on what the cube holds at once, a runeword
//! casts several on one session over time. Review and merge are the same
//! actions in both.
//!
//! Which step comes next is decided here from the session's phase, and
//! the reviewer's while one reads, so the app only carries it out.
//!
//! The Runetome's stones are runewords too, read from a project's config
//! and from one file every project shares. Their steps go beyond turns:
//! keystrokes written into the session's terminal, and commands run in
//! its folder, so a stone of only commands needs no session at all.

use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::cube::{self, Subject};
use crate::session::Phase;
use crate::tasks::one_line;

mod edit;
mod stone;
pub use edit::unwrite;
pub use stone::{
    ask_text, carve, fingerprint, name, reforge_prompt, smith_prompt, tip, Carving, Stroke,
    EDGE_POINTS, EMPTY_TIP, RUNES,
};

/// One action a runeword casts on its session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Rune {
    /// The session runs the tests, fixes what fails and commits.
    Test,
    /// A new session reviews its work, then the session answers the review.
    Review,
    /// Its branch is merged into what the main tree has checked out.
    Merge,
    /// Anything else in a config's runeword: told to the session as it is.
    Say(String),
    /// Keystrokes written into the session's terminal, as [`keys`] reads
    /// them. Done once written; it waits for no turn.
    Keys(String),
    /// A command run with `cmd /c` in the project's folder, or the
    /// session's worktree. Done when it exits, and a code other than 0
    /// stops the runeword. `show` runs it in a pane on the stage instead
    /// of hidden.
    Run { command: String, show: bool },
}

impl Rune {
    /// A word from a config: the three runes by name, anything else said
    /// to the session. None for an empty word.
    pub fn parse(word: &str) -> Option<Rune> {
        let word = one_line(word);
        Some(match word.to_lowercase().as_str() {
            "" => return None,
            "test" => Rune::Test,
            "review" => Rune::Review,
            "merge" => Rune::Merge,
            _ => Rune::Say(word),
        })
    }

    /// Its word on a tile and in a runeword's name.
    pub fn word(&self) -> String {
        match self {
            Rune::Test => "test".into(),
            Rune::Review => "review".into(),
            Rune::Merge => "merge".into(),
            Rune::Say(text) | Rune::Keys(text) => cut(text, WORD_CHARS),
            Rune::Run { command, .. } => cut(command, WORD_CHARS),
        }
    }

    /// Whether it is cast on a session. Only a command runs without one.
    pub fn needs_session(&self) -> bool {
        !matches!(self, Rune::Run { .. })
    }

    /// What it does, in full, for a stone's tooltip.
    pub fn describe(&self) -> String {
        match self {
            Rune::Test => "test".into(),
            Rune::Review => "review".into(),
            Rune::Merge => "merge".into(),
            Rune::Say(text) => format!("say \"{text}\""),
            Rune::Keys(spec) => format!("keys {spec}"),
            Rune::Run {
                command,
                show: false,
            } => format!("run {command}"),
            Rune::Run {
                command,
                show: true,
            } => format!("run {command} (shown)"),
        }
    }

    /// One step of a stone in a config: a word as [`Rune::parse`] reads
    /// it, or an object with one of `say`, `keys` or `run`, the last with
    /// `"show": true` if it should be watched.
    fn from_value(v: &Value) -> Result<Rune, String> {
        if let Some(word) = v.as_str() {
            return Rune::parse(word).ok_or_else(|| "a step is empty".to_string());
        }
        let Some(m) = v.as_object() else {
            return Err(format!("a step is a word or an object, not {v}"));
        };
        let text = |key: &str| -> Result<String, String> {
            match m.get(key) {
                Some(Value::String(t)) if !t.trim().is_empty() => Ok(t.clone()),
                _ => Err(format!("\"{key}\" wants some text")),
            }
        };
        let kinds: Vec<&str> = ["say", "keys", "run"]
            .into_iter()
            .filter(|k| m.contains_key(*k))
            .collect();
        let rune = match kinds.as_slice() {
            ["say"] => Rune::Say(one_line(&text("say")?)),
            ["keys"] => {
                let spec = text("keys")?;
                keys(&spec)?;
                Rune::Keys(spec)
            }
            ["run"] => Rune::Run {
                command: text("run")?.trim().to_string(),
                show: match m.get("show") {
                    None => false,
                    Some(Value::Bool(b)) => *b,
                    Some(_) => return Err("\"show\" is true or false".into()),
                },
            },
            [] => {
                let names: Vec<&str> = m.keys().map(String::as_str).collect();
                return Err(format!(
                    "a step has one of say, keys or run, not {}",
                    names.join(", ")
                ));
            }
            _ => return Err(format!("a step has one kind, not {}", kinds.join(" and "))),
        };
        if let Some(extra) = m
            .keys()
            .find(|k| !kinds.contains(&k.as_str()) && !(k.as_str() == "show" && kinds == ["run"]))
        {
            return Err(format!("a {} step has no \"{extra}\"", kinds[0]));
        }
        Ok(rune)
    }
}

/// How long a said rune's word may be on a tile.
const WORD_CHARS: usize = 24;

fn cut(text: &str, most: usize) -> String {
    if text.chars().count() <= most {
        return text.to_string();
    }
    let cut: String = text.chars().take(most).collect();
    format!("{}...", cut.trim_end())
}

/// Where a runeword is in its current rune.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Step {
    /// Waiting for the session to be at rest, to cast the rune.
    Due,
    /// Told the session at `at`; the rune is done when that turn ends.
    /// `heard` is when its prompt went in, so a later one is the human's.
    Told {
        at: SystemTime,
        #[serde(default)]
        heard: Option<SystemTime>,
    },
    /// A reviewer started at `at` writes its review to `file`.
    Reviewing {
        reviewer: String,
        file: String,
        at: SystemTime,
    },
    /// Keystrokes are being written, a moment apart, so each lands as
    /// typed rather than as one paste.
    Typing,
    /// A command runs. It writes its exit code beside `file` when it is
    /// done, and hidden, its output, so a reload finds out how it went.
    /// `pane` is the session it is shown in, when it is.
    Running {
        file: String,
        #[serde(default)]
        pane: Option<String>,
    },
}

/// A runeword given to a session, and how far it has got.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Runeword {
    pub name: String,
    pub runes: Vec<Rune>,
    /// The rune being cast.
    pub at: usize,
    pub step: Step,
}

impl Runeword {
    pub fn new(name: &str, runes: Vec<Rune>) -> Runeword {
        Runeword {
            name: name.to_string(),
            runes,
            at: 0,
            step: Step::Due,
        }
    }

    /// What its tile says: the rune being cast and how far along it is.
    pub fn progress(&self) -> String {
        match self.runes.get(self.at) {
            Some(r) => format!("{} {}/{}", r.word(), self.at + 1, self.runes.len()),
            None => "done".into(),
        }
    }

    /// The same where there is no room for the rune's word.
    pub fn progress_short(&self) -> String {
        match self.runes.get(self.at) {
            Some(_) => format!("rune {}/{}", self.at + 1, self.runes.len()),
            None => "done".into(),
        }
    }

    /// On to the next rune.
    pub fn advance(&mut self) {
        self.at += 1;
        self.step = Step::Due;
    }
}

/// A runeword's name made from its runes: "Test, review, merge".
pub fn named(runes: &[Rune]) -> String {
    let words: Vec<String> = runes.iter().map(Rune::word).collect();
    let mut name = words.join(", ");
    if let Some(first) = name.get(..1) {
        name = first.to_uppercase() + &name[1..];
    }
    name
}

/// Runewords by name, as a project offers them.
pub type Offered = Vec<(String, Vec<Rune>)>;

/// The stones every project has, by label, with what each is for: one or
/// two of each kind of step, each worth a click on the first day, so the
/// tome shows what a stone can do before anyone makes one.
fn built_in() -> Vec<(&'static str, &'static str, Vec<Rune>)> {
    let say = |t: &str| Rune::Say(t.into());
    let keys = |k: &str| Rune::Keys(k.into());
    vec![
        (
            "Approve",
            "Presses Enter: yes to the question the agent is asking. Works while another stone runs.",
            vec![keys("{Enter}")],
        ),
        (
            "Interrupt",
            "Presses Esc: the agent stops what it is doing and waits for you.",
            vec![keys("Esc")],
        ),
        (
            "Recap",
            "Asks the agent where it is up to.",
            vec![say(
                "In three short sentences: what you have done, what is left, and anything you need from me.",
            )],
        ),
        (
            "Commit",
            "Has the agent commit its work so far.",
            vec![say(
                "Commit what you have changed so far, staging only the files you changed, with a message that says why.",
            )],
        ),
        (
            "Fresh start",
            "Clears the conversation, then has the agent find its bearings again.",
            vec![
                keys("/clear{Enter}"),
                say("Read the README and any agent instructions in this project, then tell me in a few lines where the work is and what you would do next."),
            ],
        ),
        (
            "Second opinion",
            "A new agent reviews this one's work, then this one answers the review.",
            vec![Rune::Review],
        ),
        (
            "Open folder",
            "Opens the project's folder in Explorer. Needs no session.",
            vec![Rune::Run {
                command: "start \"\" .".into(),
                show: false,
            }],
        ),
    ]
}

/// Where a stone comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    BuiltIn,
    /// The project's `.horadric/config.json`.
    Project,
    /// The file every project shares.
    Global,
}

/// A stone in the Runetome: a runeword by its label, or why it does not
/// parse, kept so a stone with a mistake shows cracked rather than gone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stone {
    pub label: String,
    pub steps: Steps,
    pub source: Source,
    /// What it is for, in a sentence, when it says: `"about"` beside its
    /// `"steps"`. Empty when it does not.
    pub about: String,
}

impl Stone {
    /// Its steps, when they parse.
    pub fn runes(&self) -> Option<&[Rune]> {
        self.steps.as_deref().ok()
    }

    /// Whether it casts on the project rather than on a session.
    pub fn sessionless(&self) -> bool {
        self.runes().is_some_and(sessionless)
    }
}

/// Whether a runeword of these steps casts on the project rather than on
/// a session: only commands, which need no session.
pub fn sessionless(runes: &[Rune]) -> bool {
    !runes.is_empty() && !runes.iter().any(Rune::needs_session)
}

/// Whether a runeword of these steps is only keystrokes, which wait for
/// no turn, so it can be typed into a session casting another runeword
/// (an answer to a permission prompt) without stopping that one.
pub fn only_keys(runes: &[Rune]) -> bool {
    !runes.is_empty() && runes.iter().all(|r| matches!(r, Rune::Keys(_)))
}

/// A stone's steps, or why they do not parse.
pub type Steps = Result<Vec<Rune>, String>;

/// The stones in one file, by label, in the file's order, each with its
/// steps or why they do not parse. Empty for an empty file or one without
/// `runewords`, and an error for one that is not JSON. Both forms are read:
///
/// ```json
/// { "runewords": {
///     "Fresh start": { "steps": [ { "keys": "/clear{Enter}" },
///                                 { "say": "Take the next quest" } ] },
///     "Ship": ["test", "Update the changelog", "merge"] } }
/// ```
pub fn parse(text: &str) -> Result<Vec<(String, Steps)>, String> {
    Ok(written(text)?
        .into_iter()
        .map(|(label, steps, _)| (label, steps))
        .collect())
}

/// What [`parse`] reads, each stone with its `about` as well.
fn written(text: &str) -> Result<Vec<(String, Steps, String)>, String> {
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }
    let v: Value = serde_json::from_str(text).map_err(|e| format!("not JSON: {e}"))?;
    let words = match v.get("runewords") {
        None | Some(Value::Null) => return Ok(Vec::new()),
        Some(Value::Object(m)) => m,
        Some(_) => return Err("\"runewords\" is an object of stones by label".into()),
    };
    Ok(words
        .iter()
        .filter_map(|(label, value)| {
            let label = one_line(label);
            let about = value
                .get("about")
                .and_then(Value::as_str)
                .map(one_line)
                .unwrap_or_default();
            (!label.is_empty()).then(|| (label, steps_of(value), about))
        })
        .collect())
}

/// One stone's steps: a list, or an object with `steps`.
fn steps_of(v: &Value) -> Steps {
    let list = match v {
        Value::Array(list) => list,
        Value::Object(m) => match m.get("steps") {
            Some(Value::Array(list)) => list,
            Some(_) => return Err("\"steps\" is a list".into()),
            None => return Err("a stone has \"steps\"".into()),
        },
        _ => return Err("a stone is a list of steps or {\"steps\": [...]}".into()),
    };
    let runes = list
        .iter()
        .enumerate()
        .map(|(i, step)| Rune::from_value(step).map_err(|e| format!("step {}: {e}", i + 1)))
        .collect::<Result<Vec<Rune>, String>>()?;
    if runes.is_empty() {
        return Err("it has no steps".into());
    }
    Ok(runes)
}

/// Every stone a project has, as the Runetome lays them out: the built in
/// ones, then the project's (`project`, its config's text), then the ones
/// every project shares (`global`). A built in one that a file repeats
/// step for step is left to the file, which named it. A file that is not
/// JSON adds nothing.
pub fn stones(project: &str, global: &str) -> Vec<Stone> {
    let mut theirs: Vec<Stone> = Vec::new();
    for (text, source) in [(project, Source::Project), (global, Source::Global)] {
        for (label, steps, about) in written(text).unwrap_or_default() {
            theirs.push(Stone {
                label,
                steps,
                source,
                about,
            });
        }
    }
    let mut out: Vec<Stone> = built_in()
        .into_iter()
        .filter(|(label, _, runes)| {
            !theirs
                .iter()
                .any(|s| s.label == *label || s.runes() == Some(runes.as_slice()))
        })
        .map(|(label, about, runes)| Stone {
            label: label.into(),
            steps: Ok(runes),
            source: Source::BuiltIn,
            about: about.into(),
        })
        .collect();
    out.extend(theirs);
    out
}

/// The stones in the order the human dragged them to, `order` being
/// labels. A stone the order does not name keeps its place among the
/// others after every one it names, so a new stone shows last, beside
/// the empty stone that made it.
pub fn arrange(mut stones: Vec<Stone>, order: &[String]) -> Vec<Stone> {
    stones.sort_by_key(|s| {
        order
            .iter()
            .position(|l| *l == s.label)
            .unwrap_or(usize::MAX)
    });
    stones
}

/// The labels after the stone at `from` was dragged to the place `to`,
/// the others closing up behind it. A place past the end is the last one.
pub fn moved(labels: &[String], from: usize, to: usize) -> Vec<String> {
    let mut out = labels.to_vec();
    if from < out.len() {
        let label = out.remove(from);
        out.insert(to.min(out.len()), label);
    }
    out
}

/// Every stone that parses, by label, in the tome's order.
pub fn offered(stones: Vec<Stone>) -> Offered {
    stones
        .into_iter()
        .filter_map(|s| Some((s.label, s.steps.ok()?)))
        .collect()
}

/// Keystrokes as a `keys` step writes them, in pieces written a moment
/// apart so each lands as typed: a run of text is one, each key in braces
/// one of its own. The whole of `spec` may be one key (`"Esc"`,
/// `"Ctrl+C"`), or text with keys in braces (`"/clear{Enter}"`), where
/// `{{` and `}}` are braces.
pub fn keys(spec: &str) -> Result<Vec<Vec<u8>>, String> {
    if spec.trim().is_empty() {
        return Err("no keys".into());
    }
    if !spec.contains(['{', '}']) {
        if let Some(bytes) = chord(spec.trim()) {
            return Ok(vec![bytes]);
        }
    }
    let mut out: Vec<Vec<u8>> = Vec::new();
    let mut text = String::new();
    let mut chars = spec.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '{' | '}' if chars.peek() == Some(&c) => {
                chars.next();
                text.push(c);
            }
            '{' => {
                let mut name = String::new();
                loop {
                    match chars.next() {
                        Some('}') => break,
                        Some(c) => name.push(c),
                        None => return Err(format!("\"{{{name}\" is not closed")),
                    }
                }
                let key = chord(name.trim()).ok_or_else(|| format!("no key called \"{name}\""))?;
                if !text.is_empty() {
                    out.push(std::mem::take(&mut text).into_bytes());
                }
                out.push(key);
            }
            c => text.push(c),
        }
    }
    if !text.is_empty() {
        out.push(text.into_bytes());
    }
    Ok(out)
}

/// One key with any of Ctrl, Alt and Shift before it, as a terminal sends
/// it: `"Enter"`, `"Ctrl+C"`, `"Shift+Tab"`, `"Alt+Up"`. None for what is
/// not one.
fn chord(spec: &str) -> Option<Vec<u8>> {
    let mut parts: Vec<&str> = spec.split('+').map(str::trim).collect();
    // "Ctrl++" is Ctrl and the plus key.
    if spec.len() > 1 && spec.ends_with("++") {
        parts.truncate(parts.len() - 2);
        parts.push("+");
    }
    let (key, mods) = parts.split_last()?;
    let (mut ctrl, mut alt, mut shift) = (false, false, false);
    for m in mods {
        match m.to_lowercase().as_str() {
            "ctrl" | "control" => ctrl = true,
            "alt" => alt = true,
            "shift" => shift = true,
            _ => return None,
        }
    }
    let lower = key.to_lowercase();
    // xterm's modifier parameter: 1, and 1 more for Shift, 2 Alt, 4 Ctrl.
    let param = 1 + u8::from(shift) + 2 * u8::from(alt) + 4 * u8::from(ctrl);
    let letter = |l: u8| -> Vec<u8> {
        match param {
            1 => vec![0x1b, b'[', l],
            _ => format!("\x1b[1;{param}{}", l as char).into_bytes(),
        }
    };
    let tilde = |n: u8| -> Vec<u8> {
        match param {
            1 => format!("\x1b[{n}~").into_bytes(),
            _ => format!("\x1b[{n};{param}~").into_bytes(),
        }
    };
    match lower.as_str() {
        "up" => return Some(letter(b'A')),
        "down" => return Some(letter(b'B')),
        "right" => return Some(letter(b'C')),
        "left" => return Some(letter(b'D')),
        "home" => return Some(letter(b'H')),
        "end" => return Some(letter(b'F')),
        "insert" | "ins" => return Some(tilde(2)),
        "delete" | "del" => return Some(tilde(3)),
        "pageup" | "pgup" => return Some(tilde(5)),
        "pagedown" | "pgdn" => return Some(tilde(6)),
        f if f.len() > 1 && f.starts_with('f') && f[1..].bytes().all(|b| b.is_ascii_digit()) => {
            let n: u8 = f[1..].parse().ok().filter(|n| (1..=12).contains(n))?;
            let p = b'P' + n.min(5) - 1;
            return Some(match n {
                1..=4 if param == 1 => vec![0x1b, b'O', p],
                1..=4 => format!("\x1b[1;{param}{}", p as char).into_bytes(),
                _ => tilde([15, 17, 18, 19, 20, 21, 23, 24][usize::from(n - 5)]),
            });
        }
        _ => {}
    }
    let mut out = Vec::new();
    if alt {
        out.push(0x1b);
    }
    match lower.as_str() {
        "enter" | "return" if shift => out.extend_from_slice(b"\x1b\r"),
        "enter" | "return" => out.push(b'\r'),
        "tab" if shift => out.extend_from_slice(b"\x1b[Z"),
        "tab" => out.push(b'\t'),
        "esc" | "escape" => out.push(0x1b),
        "backspace" | "bksp" if ctrl => out.push(0x08),
        "backspace" | "bksp" => out.push(0x7f),
        "space" if ctrl => out.push(0),
        "space" => out.push(b' '),
        _ => {
            let mut chars = key.chars();
            let (Some(c), None) = (chars.next(), chars.next()) else {
                return None;
            };
            if ctrl {
                out.push(match c.to_ascii_lowercase() {
                    l @ 'a'..='z' => l as u8 & 0x1f,
                    '@' | '2' => 0,
                    '[' => 0x1b,
                    '\\' => 0x1c,
                    ']' => 0x1d,
                    '^' | '6' => 0x1e,
                    '_' | '-' => 0x1f,
                    _ => return None,
                });
            } else {
                let c = if shift { c.to_ascii_uppercase() } else { c };
                let mut buf = [0; 4];
                out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            }
        }
    }
    Some(out)
}

/// A runeword cast on a project rather than on a session: a stone of only
/// `run` steps. Saved beside the sessions, so it goes on through a reload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OnProject {
    /// The project's key.
    pub project: String,
    pub word: Runeword,
}

/// How a `run` step's command went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ran {
    /// It exited with `code`. `last` is its last line of output, when its
    /// output was kept.
    Exited { code: i32, last: String },
    /// It is gone without saying how it ended: its pane was closed.
    Gone,
}

/// A command's exit code as its file holds it. None until it is written.
pub fn exit_code(text: &str) -> Option<i32> {
    text.trim().parse().ok()
}

/// The last line of a command's output with anything in it, cut short for
/// a toast.
pub fn last_line(output: &str) -> String {
    let line = output
        .lines()
        .map(str::trim)
        .rfind(|l| !l.is_empty())
        .unwrap_or_default();
    cut(line, LAST_CHARS)
}

/// How much of a failed command's last line a toast shows.
const LAST_CHARS: usize = 160;

/// Why a command that failed stopped its runeword. The toast names the
/// command already, by the step it was at.
fn failed(code: i32, last: &str) -> String {
    match last {
        "" => format!("it exited with {code}"),
        _ => format!("it exited with {code}: {last}"),
    }
}

/// A session as the next step looks at it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seen {
    pub phase: Phase,
    /// When its phase began.
    pub since: SystemTime,
    /// When its last prompt went in, if one did since the app started.
    pub prompted: Option<SystemTime>,
    /// Keystrokes a `keys` step gave it are still to be written.
    pub typing: bool,
}

impl Seen {
    /// A turn that ended after `at`.
    fn finished_after(&self, at: SystemTime) -> bool {
        self.phase == Phase::Done && self.since > at
    }
}

/// What the app does next for a runeword.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Act {
    /// Nothing yet: the session or its reviewer is not there yet.
    Wait,
    /// Cast this rune now.
    Cast(Rune),
    /// The told prompt went in at this time.
    Heard(SystemTime),
    /// The rune is done: on to the next.
    Next,
    /// The review is written: tell the session to answer it.
    Answer,
    /// Every rune is cast.
    Complete,
    /// It cannot go on, and why.
    Stop(String),
}

/// The next step for `word`, cast on a session seen as `session` or on
/// its project with none, with its reviewer seen as `reviewer` while one
/// reads (None when it is gone), and how the command of a `run` step went
/// once it is known.
pub fn act(
    word: &Runeword,
    session: Option<&Seen>,
    reviewer: Option<&Seen>,
    ran: Option<&Ran>,
) -> Act {
    match session.map(|s| &s.phase) {
        Some(Phase::Ended) => return Act::Stop("the session ended".into()),
        Some(Phase::Paused) => return Act::Wait,
        _ => {}
    }
    let Some(session) = session else {
        return sessionless_act(word, ran);
    };
    match &word.step {
        Step::Due => match word.runes.get(word.at) {
            None => Act::Complete,
            // Keys and commands wait for no turn: Esc is for a session
            // that is busy, and a command does not type to it.
            Some(rune @ (Rune::Keys(_) | Rune::Run { .. })) => Act::Cast(rune.clone()),
            // At rest, and not waiting on the human: telling it now would
            // land in the middle of what it is doing.
            Some(rune) if matches!(session.phase, Phase::Done | Phase::Idle) => {
                Act::Cast(rune.clone())
            }
            Some(_) => Act::Wait,
        },
        // A turn the human cuts short (Esc, or No to a permission) sends
        // no Stop, so the next turn to end would be one the human asked
        // for. A prompt after the told one says the human took over, and
        // the runeword stops rather than count that turn as the rune or
        // tell the rune again over what the human is doing.
        Step::Told { heard: Some(h), .. } if session.prompted.is_some_and(|p| p > *h) => {
            Act::Stop("you took over from it".into())
        }
        Step::Told { at, .. } if session.finished_after(*at) => Act::Next,
        Step::Told { at, heard: None } => match session.prompted {
            Some(p) if p > *at => Act::Heard(p),
            _ => Act::Wait,
        },
        Step::Told { .. } => Act::Wait,
        Step::Reviewing { at, .. } => match reviewer {
            None => Act::Stop("the reviewer ended before it wrote its review".into()),
            Some(r) if r.phase == Phase::Ended => {
                Act::Stop("the reviewer ended before it wrote its review".into())
            }
            Some(r) if r.finished_after(*at) => Act::Answer,
            Some(_) => Act::Wait,
        },
        Step::Typing if session.typing => Act::Wait,
        Step::Typing => Act::Next,
        Step::Running { .. } => ran_act(ran),
    }
}

/// The next step of a runeword cast on a project, which only runs
/// commands.
fn sessionless_act(word: &Runeword, ran: Option<&Ran>) -> Act {
    match (&word.step, word.runes.get(word.at)) {
        (Step::Due, None) => Act::Complete,
        (Step::Due, Some(rune @ Rune::Run { .. })) => Act::Cast(rune.clone()),
        (Step::Due, Some(rune)) => Act::Stop(format!("{} needs a session", rune.word())),
        (Step::Running { .. }, _) => ran_act(ran),
        _ => Act::Stop("it has no session".into()),
    }
}

/// A running command's step: on once it exits cleanly, stopped otherwise.
fn ran_act(ran: Option<&Ran>) -> Act {
    match ran {
        None => Act::Wait,
        Some(Ran::Exited { code: 0, .. }) => Act::Next,
        Some(Ran::Exited { code, last }) => Act::Stop(failed(*code, last)),
        Some(Ran::Gone) => Act::Stop("its pane was closed".into()),
    }
}

/// What the test rune tells the session.
pub fn test_prompt() -> String {
    "Run this project's tests. Fix whatever fails, and commit once they all pass. \
     If there are no tests, say so and change nothing."
        .into()
}

/// What the reviewer of one session is asked: to read its diff and write
/// the review to `file`.
pub fn review_prompt(subject: &Subject, base: &str, file: &str) -> String {
    format!(
        "Review the work of a session. {} Read the diff. Say what it does, what is wrong \
         or risky with file and line, and what is missing. Write your review to \"{file}\" \
         and change no other file.",
        cube::diff_of(subject, base)
    )
}

/// What the session is told once its review is written.
pub fn answer_prompt(file: &str) -> String {
    format!(
        "A reviewer read your work and wrote its review to \"{file}\". Read it, fix what \
         you agree with, and commit. Say which points you left alone and why."
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::WaitReason;
    use std::time::Duration;

    fn t(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn seen(phase: Phase, since: u64) -> Seen {
        Seen {
            phase,
            since: t(since),
            prompted: None,
            typing: false,
        }
    }

    /// The next step on a session, with no command running.
    fn on(w: &Runeword, s: &Seen, reviewer: Option<&Seen>) -> Act {
        act(w, Some(s), reviewer, None)
    }

    fn word(runes: &[Rune]) -> Runeword {
        Runeword::new(&named(runes), runes.to_vec())
    }

    #[test]
    fn words_parse_to_runes_and_the_rest_is_said() {
        assert_eq!(Rune::parse(" Test "), Some(Rune::Test));
        assert_eq!(Rune::parse("REVIEW"), Some(Rune::Review));
        assert_eq!(Rune::parse("merge"), Some(Rune::Merge));
        assert_eq!(Rune::parse("   "), None);
        assert_eq!(
            Rune::parse("Update\nthe changelog"),
            Some(Rune::Say("Update the changelog".into()))
        );
    }

    #[test]
    fn a_name_and_progress_come_from_the_runes() {
        let mut w = word(&[Rune::Test, Rune::Review, Rune::Merge]);
        assert_eq!(w.name, "Test, review, merge");
        assert_eq!(w.progress(), "test 1/3");
        w.advance();
        assert_eq!(w.progress(), "review 2/3");
        assert_eq!(w.progress_short(), "rune 2/3");
        assert_eq!(w.step, Step::Due);
        w.advance();
        w.advance();
        assert_eq!(w.progress(), "done");
        let said = Rune::Say("Write the release notes for this version".into());
        assert_eq!(said.word(), "Write the release notes...");
        assert_eq!(
            named(&[said, Rune::Merge]),
            "Write the release notes..., merge"
        );
    }

    const BUILT_IN_LABELS: [&str; 7] = [
        "Approve",
        "Interrupt",
        "Recap",
        "Commit",
        "Fresh start",
        "Second opinion",
        "Open folder",
    ];

    #[test]
    fn the_tome_lays_out_built_in_then_project_then_global_stones() {
        let config = r#"{ "runewords": {
            "Ship": ["test", "Update the changelog", "merge"],
            "Empty": [],
            "Again": ["review"],
            "Commit": { "about": "Ours", "steps": ["Commit it"] }
        } }"#;
        let global = r#"{ "runewords": {
            "Open the site": { "steps": [ { "run": "start http://localhost:3000" } ] }
        } }"#;
        let stones = stones(config, global);
        let labels: Vec<(&str, Source)> = stones
            .iter()
            .map(|s| (s.label.as_str(), s.source))
            .collect();
        // Its own "Again" is the built in second opinion, and its own
        // "Commit" takes the built in one's place, so neither is there
        // twice.
        assert_eq!(
            labels,
            [
                ("Approve", Source::BuiltIn),
                ("Interrupt", Source::BuiltIn),
                ("Recap", Source::BuiltIn),
                ("Fresh start", Source::BuiltIn),
                ("Open folder", Source::BuiltIn),
                ("Again", Source::Project),
                ("Commit", Source::Project),
                ("Empty", Source::Project),
                ("Ship", Source::Project),
                ("Open the site", Source::Global),
            ]
        );
        assert_eq!(stones[6].about, "Ours");
        assert_eq!(stones[7].steps, Err("it has no steps".into()));
        assert!(stones[9].sessionless());
        assert!(!stones[8].sessionless());
        // The menu offers only what parses.
        let offered = offered(stones);
        assert_eq!(offered.len(), 9);
        assert_eq!(
            offered[7].1,
            [
                Rune::Test,
                Rune::Say("Update the changelog".into()),
                Rune::Merge
            ]
        );
        let plain = super::stones("", "");
        let names: Vec<&str> = plain.iter().map(|s| s.label.as_str()).collect();
        assert_eq!(names, BUILT_IN_LABELS);
        // A broken file adds nothing, and takes nothing from the other.
        assert_eq!(super::stones("{ not json", global).len(), 8);
    }

    #[test]
    fn every_built_in_stone_says_what_it_is_for_and_parses_as_written() {
        for s in super::stones("", "") {
            assert!(!s.about.is_empty(), "{}", s.label);
            let runes = s.runes().unwrap();
            for r in runes {
                if let Rune::Keys(spec) = r {
                    assert!(keys(spec).is_ok(), "{spec}");
                }
            }
        }
        let approve = &super::stones("", "")[0];
        assert!(only_keys(approve.runes().unwrap()));
        assert!(super::stones("", "")[6].sessionless());
    }

    #[test]
    fn a_dragged_order_lays_out_the_tome_and_new_stones_go_last() {
        let labels = |s: &[Stone]| s.iter().map(|s| s.label.clone()).collect::<Vec<_>>();
        let global = r#"{ "runewords": { "Mine": ["say hi"], "Yours": ["say yo"] } }"#;
        let plain = super::stones("", global);
        assert_eq!(arrange(plain.clone(), &[]), plain);
        let order = vec!["Yours".to_string(), "Approve".to_string()];
        let laid = labels(&arrange(plain.clone(), &order));
        assert_eq!(laid[..2], ["Yours", "Approve"]);
        // The rest keep the order they had, Mine still after the built in.
        let rest: Vec<String> = labels(&plain)
            .into_iter()
            .filter(|l| !order.contains(l))
            .collect();
        assert_eq!(laid[2..], rest[..]);
        assert_eq!(laid.last().map(String::as_str), Some("Mine"));
    }

    #[test]
    fn a_moved_stone_takes_its_new_place_and_the_others_close_up() {
        let v = |s: &[&str]| s.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let abcd = v(&["a", "b", "c", "d"]);
        assert_eq!(moved(&abcd, 0, 2), v(&["b", "c", "a", "d"]));
        assert_eq!(moved(&abcd, 3, 0), v(&["d", "a", "b", "c"]));
        assert_eq!(moved(&abcd, 1, 1), abcd);
        assert_eq!(moved(&abcd, 1, 99), v(&["a", "c", "d", "b"]));
        assert_eq!(moved(&abcd, 9, 0), abcd);
    }

    #[test]
    fn both_forms_and_every_step_kind_parse() {
        let text = r#"{ "runewords": {
            "Fresh start": { "steps": [ { "keys": "/clear{Enter}" },
                                        { "say": "Read docs/PLAN.md\n and go" } ] },
            "Watch": { "steps": [ { "run": " npm test ", "show": true }, "Review" ] },
            "Ship": ["test", { "run": "git push" }, "merge"]
        } }"#;
        let parsed = parse(text).unwrap();
        assert_eq!(
            parsed,
            [
                (
                    "Fresh start".to_string(),
                    Ok(vec![
                        Rune::Keys("/clear{Enter}".into()),
                        Rune::Say("Read docs/PLAN.md and go".into()),
                    ])
                ),
                (
                    "Ship".to_string(),
                    Ok(vec![
                        Rune::Test,
                        Rune::Run {
                            command: "git push".into(),
                            show: false
                        },
                        Rune::Merge,
                    ])
                ),
                (
                    "Watch".to_string(),
                    Ok(vec![
                        Rune::Run {
                            command: "npm test".into(),
                            show: true
                        },
                        Rune::Review,
                    ])
                ),
            ]
        );
        assert_eq!(parse(""), Ok(Vec::new()));
        assert_eq!(parse(r#"{ "mode": "auto" }"#), Ok(Vec::new()));
        assert!(parse("{ nope").unwrap_err().starts_with("not JSON"));
        assert!(parse(r#"{ "runewords": [] }"#).is_err());
    }

    #[test]
    fn a_stone_that_does_not_parse_says_why() {
        let reason = |stone: &str| -> String {
            let text = format!(r#"{{ "runewords": {{ "X": {stone} }} }}"#);
            parse(&text).unwrap()[0].1.clone().unwrap_err()
        };
        assert_eq!(
            reason("3"),
            "a stone is a list of steps or {\"steps\": [...]}"
        );
        assert_eq!(reason("{}"), "a stone has \"steps\"");
        assert_eq!(reason(r#"{"steps": "test"}"#), "\"steps\" is a list");
        assert_eq!(reason(r#"["test", ""]"#), "step 2: a step is empty");
        assert_eq!(
            reason("[1]"),
            "step 1: a step is a word or an object, not 1"
        );
        assert_eq!(
            reason(r#"[{"keys": "{Enterr}"}]"#),
            "step 1: no key called \"Enterr\""
        );
        assert_eq!(
            reason(r#"[{"say": " "}]"#),
            "step 1: \"say\" wants some text"
        );
        assert_eq!(
            reason(r#"[{"say": "a", "run": "b"}]"#),
            "step 1: a step has one kind, not say and run"
        );
        assert_eq!(
            reason(r#"[{"type": "a"}]"#),
            "step 1: a step has one of say, keys or run, not type"
        );
        assert_eq!(
            reason(r#"[{"say": "a", "show": true}]"#),
            "step 1: a say step has no \"show\""
        );
        assert_eq!(
            reason(r#"[{"run": "a", "show": "yes"}]"#),
            "step 1: \"show\" is true or false"
        );
    }

    #[test]
    fn keys_read_as_a_terminal_sends_them() {
        let one = |spec: &str| keys(spec).unwrap().concat();
        assert_eq!(one("Esc"), b"\x1b");
        assert_eq!(one(" escape "), b"\x1b");
        assert_eq!(one("Ctrl+C"), [3]);
        assert_eq!(one("ctrl + c"), [3]);
        assert_eq!(one("Enter"), b"\r");
        assert_eq!(one("Shift+Tab"), b"\x1b[Z");
        assert_eq!(one("Alt+x"), b"\x1bx");
        assert_eq!(one("Up"), b"\x1b[A");
        assert_eq!(one("Ctrl+Left"), b"\x1b[1;5D");
        assert_eq!(one("Delete"), b"\x1b[3~");
        assert_eq!(one("Shift+PageUp"), b"\x1b[5;2~");
        assert_eq!(one("F1"), b"\x1bOP");
        assert_eq!(one("F5"), b"\x1b[15~");
        assert_eq!(one("F12"), b"\x1b[24~");
        assert_eq!(one("Alt++"), b"\x1b+");
        // Text, with keys in braces as pieces of their own.
        assert_eq!(
            keys("/clear{Enter}").unwrap(),
            [b"/clear".to_vec(), b"\r".to_vec()]
        );
        assert_eq!(
            keys("{Esc}{Esc}git log{Enter}").unwrap(),
            [
                b"\x1b".to_vec(),
                b"\x1b".to_vec(),
                b"git log".to_vec(),
                b"\r".to_vec()
            ]
        );
        assert_eq!(keys("a {{b}} c").unwrap(), [b"a {b} c".to_vec()]);
        // A word that is no key is typed.
        assert_eq!(keys("hello world").unwrap(), [b"hello world".to_vec()]);
        assert_eq!(keys("1+1").unwrap(), [b"1+1".to_vec()]);
        assert!(keys("  ").is_err());
        assert_eq!(keys("{Enter").unwrap_err(), "\"{Enter\" is not closed");
        assert_eq!(keys("{Hyper+A}").unwrap_err(), "no key called \"Hyper+A\"");
        assert!(keys("{F13}").is_err());
    }

    #[test]
    fn keys_are_written_at_once_and_done_once_written() {
        let mut w = word(&[Rune::Keys("Esc".into()), Rune::Test]);
        // Esc is for a session that is busy, so it waits for no rest.
        assert_eq!(
            on(&w, &seen(Phase::Working, 5), None),
            Act::Cast(Rune::Keys("Esc".into()))
        );
        w.step = Step::Typing;
        let typing = Seen {
            typing: true,
            ..seen(Phase::Working, 5)
        };
        assert_eq!(on(&w, &typing, None), Act::Wait);
        assert_eq!(on(&w, &seen(Phase::Working, 5), None), Act::Next);
        assert_eq!(on(&w, &seen(Phase::Paused, 5), None), Act::Wait);
    }

    #[test]
    fn a_command_goes_on_when_it_exits_cleanly() {
        let run = Rune::Run {
            command: "npm test".into(),
            show: false,
        };
        let mut w = word(&[run.clone(), Rune::Merge]);
        let s = seen(Phase::Working, 5);
        assert_eq!(act(&w, Some(&s), None, None), Act::Cast(run.clone()));
        w.step = Step::Running {
            file: "C:/r/1".into(),
            pane: None,
        };
        assert_eq!(act(&w, Some(&s), None, None), Act::Wait);
        let clean = Ran::Exited {
            code: 0,
            last: String::new(),
        };
        assert_eq!(act(&w, Some(&s), None, Some(&clean)), Act::Next);
        let failing = Ran::Exited {
            code: 1,
            last: "2 tests failed".into(),
        };
        assert_eq!(
            act(&w, Some(&s), None, Some(&failing)),
            Act::Stop("it exited with 1: 2 tests failed".into())
        );
        assert_eq!(
            act(&w, Some(&s), None, Some(&Ran::Gone)),
            Act::Stop("its pane was closed".into())
        );
    }

    #[test]
    fn a_runeword_of_only_keys_waits_for_no_turn() {
        let keys = |k: &str| Rune::Keys(k.into());
        assert!(only_keys(&[keys("1")]));
        assert!(only_keys(&[keys("1"), keys("{enter}")]));
        assert!(!only_keys(&[keys("1"), Rune::Say("x".into())]));
        assert!(!only_keys(&[Rune::Test]));
        assert!(!only_keys(&[]));
    }

    #[test]
    fn a_runeword_of_commands_needs_no_session() {
        let run = |c: &str| Rune::Run {
            command: c.into(),
            show: false,
        };
        let runes = [run("a"), run("b")];
        assert!(sessionless(&runes));
        assert!(!sessionless(&[run("a"), Rune::Say("x".into())]));
        assert!(!sessionless(&[]));
        let mut w = word(&runes);
        assert_eq!(act(&w, None, None, None), Act::Cast(run("a")));
        w.step = Step::Running {
            file: "f".into(),
            pane: None,
        };
        assert_eq!(act(&w, None, None, None), Act::Wait);
        let clean = Ran::Exited {
            code: 0,
            last: String::new(),
        };
        assert_eq!(act(&w, None, None, Some(&clean)), Act::Next);
        w.advance();
        w.advance();
        assert_eq!(act(&w, None, None, None), Act::Complete);
        let said = word(&[Rune::Say("hi".into())]);
        assert_eq!(
            act(&said, None, None, None),
            Act::Stop("hi needs a session".into())
        );
    }

    #[test]
    fn a_command_s_last_line_and_code_read_from_its_files() {
        assert_eq!(exit_code("0\r\n"), Some(0));
        assert_eq!(exit_code("-1"), Some(-1));
        assert_eq!(exit_code(""), None);
        assert_eq!(last_line("one\r\n  two  \r\n\r\n"), "two");
        assert_eq!(last_line(""), "");
        assert_eq!(last_line(&"x".repeat(200)).chars().count(), 163);
    }

    #[test]
    fn a_runeword_on_a_project_survives_a_save() {
        let mut w = word(&[Rune::Run {
            command: "start ms-settings:".into(),
            show: true,
        }]);
        w.step = Step::Running {
            file: "C:/r/1".into(),
            pane: Some("rune-1".into()),
        };
        let on = OnProject {
            project: "c:/p".into(),
            word: w,
        };
        let json = serde_json::to_string(&on).unwrap();
        assert_eq!(serde_json::from_str::<OnProject>(&json).unwrap(), on);
        let typing = serde_json::to_string(&Step::Typing).unwrap();
        assert_eq!(serde_json::from_str::<Step>(&typing).unwrap(), Step::Typing);
    }

    #[test]
    fn a_step_describes_itself_in_full() {
        assert_eq!(Rune::Say("Go on".into()).describe(), "say \"Go on\"");
        assert_eq!(Rune::Keys("Ctrl+C".into()).describe(), "keys Ctrl+C");
        let shown = Rune::Run {
            command: "npm run dev".into(),
            show: true,
        };
        assert_eq!(shown.describe(), "run npm run dev (shown)");
        assert_eq!(shown.word(), "npm run dev");
        assert!(!shown.needs_session());
        assert!(Rune::Keys("Esc".into()).needs_session());
    }

    #[test]
    fn a_rune_is_cast_only_on_a_session_at_rest() {
        let w = word(&[Rune::Test, Rune::Merge]);
        assert_eq!(on(&w, &seen(Phase::Done, 5), None), Act::Cast(Rune::Test));
        assert_eq!(on(&w, &seen(Phase::Idle, 5), None), Act::Cast(Rune::Test));
        assert_eq!(on(&w, &seen(Phase::Working, 5), None), Act::Wait);
        let asking = Phase::Waiting(WaitReason::Permission);
        assert_eq!(on(&w, &seen(asking, 5), None), Act::Wait);
        assert_eq!(on(&w, &seen(Phase::Paused, 5), None), Act::Wait);
    }

    #[test]
    fn a_told_rune_is_done_when_a_later_turn_ends() {
        let mut w = word(&[Rune::Test, Rune::Merge]);
        w.step = Step::Told {
            at: t(10),
            heard: None,
        };
        // The turn it was told in has not ended yet.
        assert_eq!(on(&w, &seen(Phase::Done, 5), None), Act::Wait);
        assert_eq!(on(&w, &seen(Phase::Working, 11), None), Act::Wait);
        assert_eq!(on(&w, &seen(Phase::Done, 20), None), Act::Next);
    }

    #[test]
    fn a_prompt_after_the_told_one_stops_the_runeword() {
        let mut w = word(&[Rune::Test, Rune::Merge]);
        w.step = Step::Told {
            at: t(10),
            heard: None,
        };
        let working = |prompted: u64| Seen {
            prompted: Some(t(prompted)),
            ..seen(Phase::Working, prompted)
        };
        // A prompt from before the telling is not the told one.
        assert_eq!(on(&w, &working(8), None), Act::Wait);
        assert_eq!(on(&w, &working(11), None), Act::Heard(t(11)));
        w.step = Step::Told {
            at: t(10),
            heard: Some(t(11)),
        };
        assert_eq!(on(&w, &working(11), None), Act::Wait);
        // The human cut the turn short and asked for something else.
        assert_eq!(
            on(&w, &working(40), None),
            Act::Stop("you took over from it".into())
        );
        // Even once that turn has ended.
        let done = Seen {
            prompted: Some(t(40)),
            ..seen(Phase::Done, 50)
        };
        assert!(matches!(on(&w, &done, None), Act::Stop(_)));
        // The told turn ending is the rune done, heard or not.
        let own = Seen {
            prompted: Some(t(11)),
            ..seen(Phase::Done, 30)
        };
        assert_eq!(on(&w, &own, None), Act::Next);
    }

    #[test]
    fn a_told_step_saved_before_heard_existed_still_reads() {
        let w: Step = serde_json::from_str(
            r#"{"told":{"at":{"secs_since_epoch":10,"nanos_since_epoch":0}}}"#,
        )
        .unwrap();
        assert_eq!(
            w,
            Step::Told {
                at: t(10),
                heard: None
            }
        );
    }

    #[test]
    fn a_review_is_answered_once_the_reviewer_finished() {
        let mut w = word(&[Rune::Review]);
        w.step = Step::Reviewing {
            reviewer: "r".into(),
            file: "review.md".into(),
            at: t(10),
        };
        let s = seen(Phase::Done, 5);
        assert_eq!(on(&w, &s, Some(&seen(Phase::Idle, 10))), Act::Wait);
        assert_eq!(on(&w, &s, Some(&seen(Phase::Working, 12))), Act::Wait);
        assert_eq!(on(&w, &s, Some(&seen(Phase::Done, 30))), Act::Answer);
        assert!(matches!(on(&w, &s, None), Act::Stop(_)));
        assert!(matches!(
            on(&w, &s, Some(&seen(Phase::Ended, 30))),
            Act::Stop(_)
        ));
    }

    #[test]
    fn it_completes_after_the_last_rune_and_stops_with_its_session() {
        let mut w = word(&[Rune::Merge]);
        w.advance();
        assert_eq!(on(&w, &seen(Phase::Done, 5), None), Act::Complete);
        assert_eq!(
            on(&w, &seen(Phase::Ended, 5), None),
            Act::Stop("the session ended".into())
        );
    }

    #[test]
    fn it_survives_a_save() {
        let mut w = word(&[Rune::Test, Rune::Say("Tidy up".into())]);
        w.step = Step::Reviewing {
            reviewer: "r".into(),
            file: "C:/r.md".into(),
            at: t(10),
        };
        let json = serde_json::to_string(&w).unwrap();
        assert_eq!(serde_json::from_str::<Runeword>(&json).unwrap(), w);
    }

    #[test]
    fn the_prompts_name_the_diff_and_the_review_file() {
        let s = Subject {
            name: "login".into(),
            dir: "C:/p.login".into(),
            branch: Some("login".into()),
        };
        let p = review_prompt(&s, "main", "C:/reviews/login.md");
        assert!(p.contains("diff main...login"), "{p}");
        assert!(p.contains("\"C:/reviews/login.md\""));
        assert!(!p.contains('\n'));
        assert!(answer_prompt("C:/r.md").contains("\"C:/r.md\""));
        assert!(!test_prompt().contains('\n'));
    }
}
