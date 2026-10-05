//! Warriv, the project's orchestrator: an agent session Horadric wakes when
//! something in the quest log needs judgment rather than a rule, and that
//! plans for the human. A quest blocked on a question, a session that
//! stopped without reporting, an `After:` line that names nothing, a merge
//! that failed, a log in auto mode with nothing ready to start: each of
//! those is an [`Event`]. Horadric is the plumbing that wakes it; Warriv
//! answers with `quest` commands, and its memory is the log itself, the
//! `Warriv:` notes lines it writes under each quest.
//!
//! This is the part that decides: which events wake it, how often it may
//! wake, what it is told, and the events that wait while it works. The UI
//! starts the session and types into terminals.

use std::collections::{BTreeSet, VecDeque};

use serde_json::Value;

use crate::merge::{add_fix_up, FixUp};
use crate::tasks::{
    end_of, find, insert_note, insert_with_notes, item_line, one_line, parse, readiness,
    replace_line, set_mark, Mark, Mode, Ready, Task, ASSUMED,
};

/// What a blocked quest's reason starts with when Warriv handed it to the
/// human. Such a quest needs the human, and never wakes Warriv again.
pub const HANDED: &str = "Warriv asks: ";

/// What a Warriv session's id starts with.
pub const ID: &str = "warriv";

/// Whether the session `id` is a Warriv, whose `quest` commands are its
/// own changes and whose notes are marked as its.
pub fn is_warriv(id: &str) -> bool {
    id.strip_prefix(ID)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('-'))
}

/// What a notes line Warriv wrote starts with, so the next wake reads what
/// was decided before.
pub const NOTE: &str = "Warriv: ";

/// At most this many wakes a project in [`HOUR`], then events go to the
/// human until an hour has passed since the first of them.
pub const WAKES_AN_HOUR: usize = 6;

/// The span the wakes are counted in, in seconds.
pub const HOUR: u64 = 60 * 60;

/// At most this many quests filed for the aims a wake.
pub const FILE_AT_MOST: usize = 8;

/// Wakes in a row for the aims that filed nothing, before the aims go to
/// the human as a question.
pub const DRY_WAKES: usize = 3;

/// What the notes line starts with on a quest Warriv filed for an aim.
pub const FILED: &str = "Filed by Warriv for: ";

/// The longest last turn of a holding session put in a prompt, in
/// characters. Its end says what it asks, so that is what is kept.
const LAST_TURN: usize = 2000;

/// Whether the project has Warriv on: `"orchestrator": true` in its
/// `config.json`. Off by default, and then every event goes to the human.
pub fn on(config: &str) -> bool {
    serde_json::from_str::<Value>(config)
        .ok()
        .and_then(|v| v.get("orchestrator")?.as_bool())
        .unwrap_or(false)
}

/// "Warriv drives" switched on for a project: Warriv runs it alone, the
/// orchestrator on whatever its config says, with no budget on its wakes.
/// The human's to flip, kept in `state.json` so a reload keeps driving.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Drive {
    /// "and ships public": it may cut a public release unattended, not
    /// only ship local.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub ships_public: bool,
}

/// Whether Warriv hears a project's events: its config turns it on, and so
/// does driving.
pub fn orchestrates(config: bool, drives: bool) -> bool {
    config || drives
}

/// What an errand due while a limit is too full to start anything does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Full {
    /// Not run this time; it comes round at its next due time.
    Skip,
    /// Run once the limit has reset.
    Wait,
}

/// A due errand in the 90 % hold. Driving, nobody is there to run it
/// later by hand, so it waits for the reset instead of being skipped.
pub fn when_full(drives: bool) -> Full {
    if drives {
        Full::Wait
    } else {
        Full::Skip
    }
}

/// The words of the quests tile's Warriv line: what it is about, after
/// "Warriv drives" while it does, which is never left out.
pub fn line(watch: Option<String>, drive: Option<Drive>) -> Option<String> {
    let Some(d) = drive else {
        return watch;
    };
    let head = if d.ships_public {
        "Warriv drives and ships public"
    } else {
        "Warriv drives"
    };
    Some(
        match watch.as_deref().and_then(|w| w.strip_prefix("Warriv")) {
            Some(rest) => format!("{head},{}", rest.trim_start_matches(':')),
            None => head.to_string(),
        },
    )
}

/// What woke Warriv.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// A quest went `[!]` on the human, not on another quest or a check.
    Blocked,
    /// A session on a quest stopped again after it was asked whether it is
    /// finished, which makes it a question for somebody.
    Asks,
    /// A quest's `After:` lines name nothing, several quests, or itself.
    Tangled,
    /// A finished quest did not merge into `main` by itself.
    Merge,
    /// The log runs in auto mode, nothing is in hand, and every quest left
    /// waits.
    Stalled,
    /// The same with an aim open, even with no quest left: Warriv files the
    /// next quests toward it.
    Dry,
}

impl Kind {
    /// Whether Warriv could have caused it by changing the log. One that
    /// shows up while it works is its own doing, and does not wake it.
    pub fn can_be_own(self) -> bool {
        matches!(self, Kind::Tangled | Kind::Stalled)
    }

    fn says(self) -> &'static str {
        match self {
            Kind::Blocked => "is blocked",
            Kind::Asks => "stopped without saying it is completed",
            Kind::Tangled => "can not start: its After: lines are tangled",
            Kind::Merge => "is completed, but did not merge into main by itself",
            Kind::Stalled | Kind::Dry => "",
        }
    }
}

/// Something in a project's quest log that wakes Warriv.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub kind: Kind,
    /// The quest it is about, empty for one about the whole log.
    pub title: String,
    /// What the quest's session said, or what is wrong.
    pub detail: String,
}

impl Event {
    /// Stays the same while the event holds, so each is heard once, and a
    /// quest blocked again for another reason is heard anew.
    pub fn key(&self) -> String {
        format!("{:?}\n{}\n{}", self.kind, self.title, self.detail)
    }
}

/// The events a project's log holds now, given the titles of the quests
/// whose sessions stopped after the nudge and the aims still open. A merge
/// that failed is not in the log, so the UI adds it to the [`Desk`] when it
/// happens.
pub fn events(tasks: &[Task], mode: Mode, asks: &[String], aims: &[String]) -> Vec<Event> {
    let ready = readiness(tasks);
    let mut out = Vec::new();
    for (t, r) in tasks.iter().zip(&ready) {
        let reason = t.reason.clone().unwrap_or_default();
        if t.mark == Mark::Blocked && t.wait.is_none() && !reason.starts_with(HANDED) {
            out.push(Event {
                kind: Kind::Blocked,
                title: t.title.clone(),
                detail: reason,
            });
        } else if t.mark == Mark::Working && asks.contains(&t.title) {
            out.push(Event {
                kind: Kind::Asks,
                title: t.title.clone(),
                detail: String::new(),
            });
        }
        if matches!(t.mark, Mark::Open | Mark::Blocked) && r.tangled() {
            out.push(Event {
                kind: Kind::Tangled,
                title: t.title.clone(),
                detail: r.why(),
            });
        }
    }
    if let Some(e) = stalled(tasks, mode, &ready, aims) {
        out.push(e);
    }
    out
}

/// A log in auto mode that has nothing in hand and nothing ready to start,
/// while quests are left or an aim is open. A quest blocked on the human is
/// the reason then, and an event of its own.
fn stalled(tasks: &[Task], mode: Mode, ready: &[Ready], aims: &[String]) -> Option<Event> {
    if mode != Mode::Auto
        || tasks.iter().any(|t| {
            matches!(t.mark, Mark::Working | Mark::Review)
                || (t.mark == Mark::Blocked && t.wait.is_none())
        })
    {
        return None;
    }
    let mut waiting = Vec::new();
    for (t, r) in tasks.iter().zip(ready) {
        match t.mark {
            Mark::Open if t.title.trim().is_empty() => {}
            Mark::Open if *r == Ready::Yes => return None,
            Mark::Open | Mark::Blocked => waiting.push(one_line(&t.title)),
            _ => {}
        }
    }
    let waits = match waiting.is_empty() {
        true => "No quest is left to start.".to_string(),
        false => format!("Waiting: {}.", waiting.join("; ")),
    };
    if !aims.is_empty() {
        return Some(Event {
            kind: Kind::Dry,
            title: String::new(),
            detail: waits,
        });
    }
    (!waiting.is_empty()).then(|| Event {
        kind: Kind::Stalled,
        title: String::new(),
        detail: format!("Nothing is in hand and no quest is ready to start. {waits}"),
    })
}

/// The notes line on a quest Warriv files for `aim`.
pub fn filed(aim: &str) -> String {
    format!("{FILED}{}", one_line(aim))
}

/// How many quests in the log Warriv filed for an aim, so a wake that
/// filed nothing is known by the count staying put.
pub fn filed_count(tasks: &[Task]) -> usize {
    tasks
        .iter()
        .filter(|t| t.notes.iter().any(|n| n.starts_with(FILED)))
        .count()
}

/// What a project's Warriv has heard: the events already taken, the ones
/// waiting to be told, and when it woke this last hour.
#[derive(Debug, Default, Clone)]
pub struct Desk {
    seen: BTreeSet<String>,
    queue: Vec<Event>,
    wakes: Vec<u64>,
    /// The budget ran out and the human was told so, until it frees up.
    tired: bool,
    /// Events that went to the human: the budget was spent, or Warriv
    /// ended its turn with them still holding. Kept while they hold.
    human: Vec<Event>,
    /// Wakes in a row for the aims that filed nothing.
    dry: usize,
    /// Warriv drives the project, which lifts the budget on its wakes.
    driving: bool,
}

/// How a wake given the dry log ended.
#[derive(Debug, PartialEq, Eq)]
pub enum Dried {
    /// The log is no longer dry, so the count starts over.
    Moved,
    /// Still dry: it is heard again, for another wake.
    Again,
    /// The third wake in a row that changed nothing: the aims are the
    /// human's while the log stays as it is.
    Human,
}

/// What to do with the events waiting.
#[derive(Debug, PartialEq, Eq)]
pub enum Wake {
    /// Nothing waits.
    Nothing,
    /// Tell Warriv these.
    Go(Vec<Event>),
    /// The budget is spent: these go to the human, who is told why the
    /// first time.
    Tired { events: Vec<Event>, first: bool },
}

impl Desk {
    /// Takes the events the log holds now. A new one waits for Warriv,
    /// unless it showed up while Warriv was `working` and could be its own
    /// doing. One that no longer holds is forgotten, so it is heard again
    /// should it come back.
    pub fn hear(&mut self, now: Vec<Event>, working: bool) {
        let keys: BTreeSet<String> = now.iter().map(Event::key).collect();
        self.seen.retain(|k| keys.contains(k));
        self.human.retain(|e| keys.contains(&e.key()));
        self.queue
            .retain(|e| e.kind == Kind::Merge || keys.contains(&e.key()));
        for e in now {
            if !self.seen.insert(e.key()) {
                continue;
            }
            // Its own doing is nothing it would settle, so the human hears it.
            if working && e.kind.can_be_own() {
                self.hand(e);
                continue;
            }
            self.queue.push(e);
        }
    }

    /// An event that is not in the log, a failed merge, heard once.
    pub fn push(&mut self, e: Event) {
        if !self.queue.contains(&e) {
            self.queue.push(e);
        }
    }

    /// An event Warriv could not settle, which is the human's while it
    /// holds and never goes to Warriv again.
    pub fn hand(&mut self, e: Event) {
        if !self.human.contains(&e) {
            self.human.push(e);
        }
    }

    /// The quests among the events `now` that Warriv has, so they need
    /// nobody else: every one but those that went to the human. A quest
    /// Warriv handed on is no event, so it is the human's too.
    pub fn holding(&self, now: &[Event]) -> BTreeSet<String> {
        now.iter()
            .filter(|e| !e.title.is_empty() && !self.human.contains(e))
            .map(|e| e.title.clone())
            .collect()
    }

    /// A wake given the dry log `e` ended. `holds` is whether the log is
    /// still dry the same way, `moved` whether it filed a quest or the open
    /// aims changed meanwhile. Still dry, the event is forgotten so the next
    /// look hears it again, until [`DRY_WAKES`] in a row that moved nothing
    /// hand it on.
    pub fn dry_ended(&mut self, e: Event, holds: bool, moved: bool) -> Dried {
        if !holds {
            self.dry = 0;
            return Dried::Moved;
        }
        self.dry = if moved { 0 } else { self.dry + 1 };
        if self.dry >= DRY_WAKES {
            self.dry = 0;
            self.hand(e);
            return Dried::Human;
        }
        self.seen.remove(&e.key());
        Dried::Again
    }

    /// Whether Warriv drives the project now. Driving, it may wake as
    /// often as events come: what keeps it from running away is that each
    /// event is heard once and its own doing never wakes it.
    pub fn drive(&mut self, on: bool) {
        self.driving = on;
    }

    /// Whether events wait.
    pub fn waiting(&self) -> bool {
        !self.queue.is_empty()
    }

    /// Takes every waiting event, as one wake at `now` when the budget
    /// allows, else for the human.
    pub fn wake(&mut self, now: u64) -> Wake {
        if self.queue.is_empty() {
            return Wake::Nothing;
        }
        let events = std::mem::take(&mut self.queue);
        self.wakes.retain(|&at| at + HOUR > now);
        if self.wakes.len() >= WAKES_AN_HOUR && !self.driving {
            let first = !std::mem::replace(&mut self.tired, true);
            for e in &events {
                self.hand(e.clone());
            }
            return Wake::Tired { events, first };
        }
        self.tired = false;
        self.wakes.push(now);
        Wake::Go(events)
    }

    /// What Warriv is about at `now`, for the quests tile: settling the
    /// `open` events its awake session has not answered and those waiting
    /// for its next stop, or resting with its wakes spent. None when it
    /// sleeps with wakes left.
    pub fn watch(&self, open: Option<usize>, now: u64) -> Option<Watch> {
        if let Some(open) = open {
            return Some(Watch::Settling(open + self.queue.len()));
        }
        let spent: Vec<u64> = self
            .wakes
            .iter()
            .copied()
            .filter(|&at| at + HOUR > now)
            .collect();
        if spent.len() < WAKES_AN_HOUR || self.driving {
            return None;
        }
        spent.into_iter().min().map(|first| Watch::Rests {
            until: first + HOUR,
        })
    }
}

/// What the quests tile's line says of Warriv.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Watch {
    /// Awake, with this many events in hand.
    Settling(usize),
    /// Its wakes are spent until this unix time.
    Rests { until: u64 },
}

impl Watch {
    /// The line's words. `local` is the seconds since local midnight at
    /// the unix time `now`, which turns `until` into a time of day.
    pub fn words(self, local: u64, now: u64) -> String {
        match self {
            Watch::Settling(0) => "Warriv: settling".to_string(),
            Watch::Settling(n) => format!("Warriv: settling {n}"),
            Watch::Rests { until } => {
                let secs = (local as i64 + until as i64 - now as i64).rem_euclid(86_400);
                format!(
                    "Warriv rests until {:02}:{:02}",
                    secs / 3600,
                    secs % 3600 / 60
                )
            }
        }
    }

    /// Whether it is at work, which reads in the working colour.
    pub fn working(self) -> bool {
        matches!(self, Watch::Settling(_))
    }
}

/// An event with what Warriv needs to judge it: the quest's notes, which
/// hold what it decided before, and the holding session's last turn.
#[derive(Debug, Clone, Default)]
pub struct Brief {
    pub event: Option<Event>,
    pub notes: Vec<String>,
    pub last_turn: Option<String>,
    /// The open aims, for a dry log.
    pub aims: Vec<String>,
}

/// What Warriv is told beside its first prompt, every time it starts: who
/// it is, what it may do, and how. `horadric` is how to run this Horadric
/// from its shell, `file` the log's path from the project folder.
pub fn system_prompt(horadric: &str, file: &str) -> String {
    format!(
        "You are Warriv, the caravan master of this project. Many agent sessions work \
         the quests in the quest log, {file}, by themselves, and Horadric wakes you when \
         one of them can not go on, so the human hears only what needs a human. You plan; \
         you do not build. Change no code, start no session, and never edit {file} \
         directly: every change goes through `{horadric} quest`, which is the one command \
         you may run without asking. {AS_WRITTEN}\n\n\
         What you may do:\n\
         - Answer a quest's session: `{horadric} quest tell \"<title>\" \"<message>\"`. \
         Horadric types it into the session once it is between turns, and a blocked quest \
         goes on. Once per quest each time you are woken.\n\
         - Write down what you decided under the quest, which is how the next wake knows: \
         `{horadric} quest note \"<title>\" \"<what you decided and why>\"`, which Horadric marks as \
         yours. Do this for \
         every event you settle.\n\
         - Add quests: `{horadric} quest add \"<title>\" --notes \"<notes>\"`, with \
         `--below \"<title>\"` to put it right under another, and `--after \"<title>\"` \
         once for each quest it needs first. Split a quest that is too big by telling its \
         session to do only the first part and adding the rest.\n\
         - Pick each quest's model: a quest you add, or one you meet in an event, with no \
         `Model:` line gets one, `--notes \"Model: <name>\"` on a quest you add and \
         `{horadric} quest note \"<title>\" \"Model: <name>\"` on one there already. \
         `haiku` for small, plain work (a rename, a string, a test for code already \
         there), `sonnet` for most, `opus` for design, hard bugs and anything that spans \
         the code. Never change a `Model:` line you did not write: that one is the human's.\n\
         - Hand a quest to the human when only the human can settle it: `{horadric} quest \
         blocked \"<question>\" --quest \"<title>\"`. Word the question so it can be \
         answered in one line, and say what you tried in a note first.\n\
         - Read anything: the log, `{horadric} quest list`, the code, git.\n\n\
         The human may write where the work is going as `Aim:` lines at the top of {file}. \
         When the log runs dry with an aim open, you plan ahead:\n\
         - File the next quests toward the first open aim, at most {FILE_AT_MOST} a wake, in \
         the order they should be done: `{horadric} quest add \"<title>\" --notes \"<what to \
         build, where its reasoning lives, how to check it>\" --notes \"{FILED}<the aim>\" \
         --after \"<title>\"`, with an `--after` for each quest it needs first. Each is one \
         session's work, small enough to finish and check in one go. Never a title already in \
         the log, done or not.\n\
         - When what the aim names is all done, mark it: `{horadric} quest aim done \"<the \
         aim>\"`. Read the log, the plan and git before you decide either way.\n\n\
         Answer from what is written: the quest's notes, the plan and docs, the code, and \
         what other quests found. Hand on what needs the human's taste, money, accounts or \
         a decision the docs leave open. When you have settled every event, stop. Do not ask \
         questions in this chat: nobody reads it, and the session closes when your turn ends."
    )
}

/// What Warriv is told besides, while it drives: the human is away, so
/// it settles what can be changed later itself and writes it down, and
/// hands on only what can not be undone or only the human can know.
pub fn driven_prompt(horadric: &str) -> String {
    format!(
        "You drive this project: the human is away and the list goes on without \
         them. When a session asks something that can be changed later (a name, a \
         layout, which of two fixes), do not hand it on: decide, tell the session, and \
         write it down with `{horadric} quest note \"<title>\" \"{ASSUMED} <what you chose \
         and why>\"`. The human reads every such line when they come back and \
         overrules in one line what they would have chosen otherwise. Hand a quest to \
         the human only for a choice that can not be undone (deleting data, \
         publishing, spending money, an account) or that only the human can know. A \
         quest handed on no longer stops the list: the rest of the caravan moves."
    )
}

/// What the human's one line against an assumption does, by how its
/// quest stands now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Overrule {
    /// Its session still holds it: told, as `quest tell` would.
    Tell(String),
    /// Nobody took it yet: a notes line its session will read.
    Note(String),
    /// It is done or gone: a new quest to change it, at the end of the log.
    Quest { title: String, notes: String },
}

/// The human overrules `assumed`, made on the quest `title`, with
/// `answer`. `mark` is the quest's mark now, none when it left the log.
pub fn overrule(mark: Option<Mark>, title: &str, assumed: &str, answer: &str) -> Overrule {
    let (assumed, answer) = (one_line(assumed), one_line(answer));
    let said = format!("You assumed \u{201C}{assumed}\u{201D}; instead: {answer}");
    match mark {
        Some(m) if m.held() => Overrule::Tell(said),
        Some(Mark::Open) => Overrule::Note(human_note(&said)),
        _ => Overrule::Quest {
            title: one_line(&format!("Overrule on {title}")),
            notes: format!(
                "While it was done, it assumed \u{201C}{assumed}\u{201D}.\nThe human answers: {answer}"
            ),
        },
    }
}

/// The first prompt of a wake: each event with its quest's notes and the
/// holding session's last turn.
pub fn prompt(briefs: &[Brief]) -> String {
    let mut out = String::from("Horadric woke you for this:\n");
    for b in briefs {
        out.push('\n');
        out.push_str(&brief(b));
    }
    out
}

/// What Warriv is told at a stop when more happened while it worked. Typed
/// into its terminal, which ends a prompt at a newline, so on one line.
pub fn more(briefs: &[Brief]) -> String {
    let each: Vec<String> = briefs.iter().map(|b| one_line(&brief(b))).collect();
    format!(
        "While you worked, more happened. {} Settle these the same way, then stop.",
        each.join(" ")
    )
}

fn brief(b: &Brief) -> String {
    let Some(e) = &b.event else {
        return String::new();
    };
    let mut out = match e.kind {
        Kind::Stalled => format!("- The log is stalled. {}\n", e.detail),
        Kind::Dry => {
            let mut s = format!(
                "- The log has run dry: nothing is in hand and nothing is ready to start. {}\n  \
                 The aims still open, in order:\n",
                e.detail
            );
            for a in &b.aims {
                s.push_str(&format!("    Aim: {a}\n"));
            }
            s.push_str(&format!(
                "  Read what the first aim points at, then file the next quests toward it, at \
                 most {FILE_AT_MOST}, each with an `--after` for every quest it needs first and \
                 the notes line `{FILED}<the aim>`. Or, if it is all done, mark it reached.\n"
            ));
            s
        }
        k => {
            let mut s = format!("- The quest \"{}\" {}", one_line(&e.title), k.says());
            if e.detail.is_empty() {
                s.push_str(".\n");
            } else {
                s.push_str(&format!(": {}\n", e.detail.trim()));
            }
            s
        }
    };
    if !b.notes.is_empty() {
        out.push_str("  Its notes:\n");
        for n in &b.notes {
            out.push_str(&format!("    {n}\n"));
        }
    }
    if let Some(turn) = b
        .last_turn
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
    {
        out.push_str(&format!(
            "  Its session's last turn ended with:\n    {}\n",
            tail(turn, LAST_TURN).replace('\n', "\n    ")
        ));
    }
    out
}

/// The last `n` characters of `s`, marked as cut when they are.
fn tail(s: &str, n: usize) -> String {
    let count = s.chars().count();
    if count <= n {
        return s.to_string();
    }
    let kept: String = s.chars().skip(count - n).collect();
    format!("\u{2026}{kept}")
}

/// What a quest's session is told when Warriv answers it, typed into its
/// terminal, so on one line.
pub fn told(horadric: &str, message: &str) -> String {
    answer(
        "Warriv, who plans this project's quests, answers",
        horadric,
        message,
    )
}

/// What a quest's session is told when the human answers it from the card
/// that greets them back.
pub fn answered(horadric: &str, message: &str) -> String {
    answer("The human answers", horadric, message)
}

fn answer(who: &str, horadric: &str, message: &str) -> String {
    format!(
        "{who}: {} Go on with this quest, and when it is finished, commit your work and run \
         `{horadric} quest done \"<one short line on what you achieved>\"`.",
        one_line(message)
    )
}

/// What the human is told once when Warriv has woken too often this hour.
pub fn tired(project: &str) -> String {
    format!(
        "Warriv woke {WAKES_AN_HOUR} times in {project} this hour, so what happens next \
         comes to you until the hour is over."
    )
}

/// What the human is asked when Warriv woke [`DRY_WAKES`] times in a row
/// for the aims and filed nothing.
pub fn stuck(aims: &[String]) -> String {
    let aim = aims.first().map(|a| one_line(a)).unwrap_or_default();
    format!(
        "Warriv woke {DRY_WAKES} times for \"{aim}\" and filed nothing. What comes next, \
         or is it reached?"
    )
}

/// The notes line that keeps what Warriv decided.
pub fn note(text: &str) -> String {
    format!("{NOTE}{}", one_line(text))
}

/// The notes line for the human's answer to a quest whose session is gone,
/// which its next session reads.
pub fn human_note(text: &str) -> String {
    format!("The human answers: {}", one_line(text))
}

/// The reason a quest Warriv hands to the human is blocked with.
pub fn handed(question: &str) -> String {
    format!("{HANDED}{}", one_line(question))
}

/// The question Warriv handed on, when `reason` is one.
pub fn question(reason: &str) -> Option<&str> {
    reason.strip_prefix(HANDED)
}

/// The quest `name` means in the log `text`, or why there is none.
fn quest(text: &str, name: &str) -> Result<Task, String> {
    let tasks = parse(text);
    match find(&tasks, name) {
        Ok(i) => Ok(tasks[i].clone()),
        Err(0) => Err(format!("no quest is called \"{}\"", one_line(name))),
        Err(_) => Err(format!(
            "\"{}\" matches more than one quest",
            one_line(name)
        )),
    }
}

/// `text` with a notes line under the quest `name`, after its other notes.
pub fn add_note(text: &str, name: &str, line: &str) -> Result<String, String> {
    let t = quest(text, name)?;
    Ok(insert_note(text, end_of(text, t.line), line))
}

/// `text` with the quest `name` handed to the human: blocked, its holder
/// kept, with the question as its reason. Only a quest a session holds
/// can wait on the human that way.
pub fn hand_on(text: &str, name: &str, question: &str) -> Result<String, String> {
    let t = quest(text, name)?;
    let holder = match (t.mark, t.holder.as_deref()) {
        (Mark::Done, _) => return Err(format!("\"{}\" is completed", t.title)),
        (_, Some(h)) if t.mark.held() => h,
        _ => {
            return Err(format!(
                "no session holds \"{}\"; add a note to it instead",
                t.title
            ))
        }
    };
    // The human answers from the notes, so what was tried is there first.
    if !t.notes.iter().any(|n| n.trim_start().starts_with(NOTE)) {
        return Err(format!(
            "say what you tried first, with `quest note \"{}\" \"...\"`",
            one_line(&t.title)
        ));
    }
    let line = item_line(
        Mark::Blocked,
        &t.title,
        Some(holder),
        Some(&handed(question)),
        None,
    );
    replace_line(text, t.line, &line).ok_or_else(|| "the log changed".to_string())
}

/// `text` with a new open quest and its notes right under the quest
/// `name` and its notes.
pub fn add_below(text: &str, name: &str, title: &str, notes: &str) -> Result<String, String> {
    let t = quest(text, name)?;
    Ok(insert_with_notes(text, end_of(text, t.line), title, notes))
}

/// What a reviewer session's id starts with. It is a Warriv too, so its
/// notes are marked as Warriv's and it may hand a quest to the human.
pub const REVIEWER: &str = "warriv-review";

/// Whether the session `id` is a reviewer, which reads a finished quest
/// in "Warriv reviews" mode before it lands.
pub fn is_reviewer(id: &str) -> bool {
    id.strip_prefix(REVIEWER)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('-'))
}

/// The longest diff put in a reviewer's prompt, in characters. Its start
/// is kept, and the reviewer is told how to read the rest. The prompt goes
/// on a command line, which Windows ends at 32767.
const DIFF: usize = 20_000;

/// The quests waiting for a review: completed by their session, `[?]`,
/// in list order. A quest in tombs is the human's pick, not a review.
pub fn to_review(tasks: &[Task]) -> Vec<String> {
    tasks
        .iter()
        .filter(|t| t.mark == Mark::Review && !t.title.trim().is_empty())
        .filter(|t| {
            t.holder
                .as_deref()
                .is_none_or(|h| crate::tombs::count(h).is_none())
        })
        .map(|t| t.title.clone())
        .collect()
}

/// A project's reviews: the one being read, the ones queued behind it, and
/// every quest already taken while it stays `[?]`, so a reviewer that ends
/// without a verdict leaves it to the human rather than starting again.
#[derive(Debug, Default, Clone)]
pub struct Reviews {
    seen: BTreeSet<String>,
    queue: VecDeque<String>,
    current: Option<String>,
}

impl Reviews {
    /// Takes the quests `[?]` now. A new one queues; one that is no longer
    /// `[?]` is forgotten, so it is read again should it come back.
    pub fn hear(&mut self, now: &[String]) {
        self.seen.retain(|t| now.contains(t));
        self.queue.retain(|t| now.contains(t));
        for t in now {
            if self.seen.insert(t.clone()) {
                self.queue.push_back(t.clone());
            }
        }
    }

    /// The quest to read next, when no review is under way: one reviewer a
    /// project at a time.
    pub fn take(&mut self) -> Option<String> {
        if self.current.is_some() {
            return None;
        }
        self.current = self.queue.pop_front();
        self.current.clone()
    }

    /// The review under way ended, however it ended.
    pub fn done(&mut self) {
        self.current = None;
    }

    /// Whether `title` is being read or waits to be, so nobody else needs
    /// to hear of it.
    pub fn pending(&self, title: &str) -> bool {
        self.current.as_deref() == Some(title) || self.queue.iter().any(|t| t == title)
    }

    /// The mode is not "Warriv reviews" any more: nothing waits. The review
    /// under way ends at its stop.
    pub fn stop(&mut self) {
        self.queue.clear();
        self.seen.clear();
    }
}

/// What a reviewer reads: the quest, its notes, where its work is and the
/// diff of that work against what it lands on.
#[derive(Debug, Clone, Default)]
pub struct Review {
    pub title: String,
    pub notes: Vec<String>,
    /// The quest's branch, none when it worked in the main tree.
    pub branch: Option<String>,
    /// What the main tree has checked out, where it lands.
    pub into: String,
    pub diff: String,
}

/// What a reviewer is told beside its first prompt: who it is and the
/// three ways a review ends.
pub fn review_system_prompt(horadric: &str, file: &str) -> String {
    format!(
        "You are Warriv, the caravan master of this project, reviewing a quest from the \
         quest log, {file}. An agent session finished it and Horadric woke you to read its \
         work before it lands, so the human hears only what needs a human. You review; you \
         do not build. Change no code, run no checks (landing runs the project's checks \
         itself), and never edit {file} directly.\n\n\
         Read the quest, its notes and its diff, and as much of the code around it as you \
         need. Then end the review one of three ways:\n\
         - It does what the quest asks and nothing is wrong with it: `{horadric} quest pass \
         \"<title>\"`. It lands on main as the human's approval would.\n\
         - Something is wrong or missing that its session can fix: `{horadric} quest fix \
         \"<title>\" \"<what is wrong, concretely: the file, the case, what it should do>\"`. \
         Horadric tells its session, or files a fix-up quest when the session is gone.\n\
         - Only the human can say: first `{horadric} quest note \"<title>\" \"<what you \
         saw>\"`, then `{horadric} quest blocked \"<question>\" --quest \"<title>\"`, worded \
         so it can be answered in one line.\n\n\
         Judge what the quest asked for, not your own taste: style the project does not ask \
         for is no reason to send it back. Run exactly one of the three, then stop. {AS_WRITTEN} Do not ask \
         questions in this chat: nobody reads it, and the session closes when your turn ends."
    )
}

/// The first prompt of a review: the quest, its notes, and its diff.
pub fn review_prompt(r: &Review) -> String {
    let mut out = format!("Review the quest \"{}\".\n", one_line(&r.title));
    if !r.notes.is_empty() {
        out.push_str("\nIts notes:\n");
        for n in &r.notes {
            out.push_str(&format!("  {n}\n"));
        }
    }
    let Some(branch) = &r.branch else {
        out.push_str(&format!(
            "\nIt worked in the main tree, on {}, so its work is in the commits there; \
             read `git log` to find them.\n",
            r.into
        ));
        return out;
    };
    let range = format!("{}...{branch}", r.into);
    if r.diff.trim().is_empty() {
        out.push_str(&format!(
            "\nIts branch {branch} has no changes against {}: `git diff {range}` is empty.\n",
            r.into
        ));
        return out;
    }
    out.push_str(&format!(
        "\nIts work is on the branch {branch}. `git diff {range}` says:\n\n```diff\n{}\n```\n",
        head(r.diff.trim_end(), DIFF)
    ));
    if r.diff.trim_end().chars().count() > DIFF {
        out.push_str(&format!(
            "\nThe diff is cut there. Run `git diff {range}` for the rest.\n"
        ));
    }
    out
}

/// The first `n` characters of `s`, marked as cut when they are.
fn head(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    let kept: String = s.chars().take(n).collect();
    format!("{kept}\u{2026}")
}

/// `text` with the quest `name` passed: completed, its holder kept, so it
/// lands as the human's approval would. Only a quest waiting for review
/// can pass.
pub fn pass(text: &str, name: &str) -> Result<String, String> {
    let t = reviewed(text, name)?;
    set_mark(text, t.line, &t.title, Mark::Done).ok_or_else(|| "the log changed".to_string())
}

/// The quest `name` means, when it waits for review.
pub fn reviewed(text: &str, name: &str) -> Result<Task, String> {
    let t = quest(text, name)?;
    if t.mark != Mark::Review {
        return Err(format!("\"{}\" is not waiting for review", t.title));
    }
    Ok(t)
}

/// What a quest's session is told when its review sends it back, typed
/// into its terminal, so on one line.
pub fn sent_back(horadric: &str, what: &str) -> String {
    format!(
        "Warriv reviewed this quest and sends it back: {} Fix that, commit your work, and \
         run `{horadric} quest done \"<one short line on what you achieved>\"` again.",
        one_line(what)
    )
}

/// The quest that fixes what a review of `title` found, for when its
/// session is gone: its work is on `branch`, or in the main tree.
pub fn fix_up(title: &str, branch: Option<&str>, into: &str, what: &str) -> FixUp {
    let mut notes = vec![format!(
        "Warriv reviewed \"{}\" and sent it back: {}",
        one_line(title),
        one_line(what)
    )];
    match branch {
        Some(b) => notes.push(format!(
            "Its work is on the branch `{b}`, not in `{into}`. Take its commits onto your \
             branch with `git cherry-pick {into}..{b}`, fix what the review found, make every \
             check in `.horadric/config.json` pass, and commit. Then delete it with `git \
             branch -D {b}`. Your branch lands like any finished quest and carries its work."
        )),
        None => notes.push(format!("Its work is already in `{into}`; fix it there.")),
    }
    notes.push("The quests after the reviewed one wait for this one too.".to_string());
    FixUp {
        title: format!("Fix what review found in {}", branch.unwrap_or(title)),
        notes: notes.join("\n"),
    }
}

/// `text` with the quest `name`, whose session is gone, sent back: it is
/// completed, as a failed merge leaves its quest, and `fix` goes right
/// below it, the quests after it waiting for that too.
pub fn send_back(text: &str, name: &str, fix: &FixUp) -> Result<String, String> {
    let t = reviewed(text, name)?;
    let done = set_mark(text, t.line, &t.title, Mark::Done)
        .ok_or_else(|| "the log changed".to_string())?;
    Ok(add_fix_up(&done, &t.title, fix).unwrap_or(done))
}

/// Warriv's memory, from the project folder: what one session leaves the
/// next, committed with the project so it travels with the repository.
pub const MEMORY: &str = ".horadric/warriv.md";

/// Past this many lines the memory is compacted, so it stays something a
/// session reads whole before it starts.
pub const MEMORY_LINES: usize = 200;

/// What a line in the memory's Open part starts with when it holds the
/// human's answer to a question Warriv handed on.
pub const ANSWERED: &str = "- Answered: ";

const RULES: &str = "## Rules";
const LATELY: &str = "## Lately";
const OPEN: &str = "## Open";

/// A memory with nothing in it yet.
pub fn blank_memory() -> String {
    format!("# Warriv's memory\n\n{RULES}\n\n{LATELY}\n\n{OPEN}\n")
}

/// Whether the memory has grown past [`MEMORY_LINES`].
pub fn compact_due(memory: &str) -> bool {
    memory.lines().count() > MEMORY_LINES
}

/// How many answers from the human wait in the memory to become rules.
fn answers(memory: &str) -> usize {
    memory
        .lines()
        .filter(|l| l.trim_start().starts_with(ANSWERED))
        .count()
}

/// What every Warriv session is told about its memory, `file` from the
/// project folder, given what the memory holds now, or `None` when there
/// is none yet. Read and written by the session itself, so the prompt
/// says how rather than carrying it.
pub fn memory_prompt(file: &str, memory: Option<&str>) -> String {
    let mut out = format!(
        "Your memory is {file}, kept between your sessions. Read it before anything \
         else and follow its rules. Write to it last, before you stop, with your file \
         tools; it is the one file you edit that way. Horadric commits it when you stop, \
         so do not commit it yourself. It has three parts:\n\
         - {RULES}: how this project wants things done, one line each. Add one when the \
         human settles a kind of question, or when you learn what every later session \
         should know.\n\
         - {LATELY}: what you did and why, newest first, one line each that starts with \
         the date, like `- 2026-10-05 Told \"Port\" to use 4210, as the plan says.` Add a \
         line for this session even when it changed nothing.\n\
         - {OPEN}: what you wait to see, one line each: the questions you handed to the \
         human and what to check next time. Remove a line once it is settled."
    );
    let Some(memory) = memory.filter(|m| !m.trim().is_empty()) else {
        out.push_str(
            "\nIt does not exist yet: create it with a `# Warriv's memory` title and \
             those three headings.",
        );
        return out;
    };
    match answers(memory) {
        0 => {}
        n => out.push_str(&format!(
            "\n{OPEN} holds {} starting `{}`: the human's answer to a question you \
             handed on. Write each as a rule under {RULES}, worded so the same kind of \
             question never has to be asked again, then remove the line.",
            if n == 1 { "a line" } else { "lines" },
            ANSWERED.trim_start_matches("- ").trim_end()
        )),
    }
    if compact_due(memory) {
        out.push_str(&format!(
            "\nIt is {} lines, past {MEMORY_LINES}: compact it before you stop. Keep every \
             rule, merging those that say the same, and keep {OPEN}. Keep the newest twenty \
             lines of {LATELY} as they are and fold the older ones into a line a day or a \
             week, until the file is under {} lines.",
            memory.lines().count(),
            MEMORY_LINES * 3 / 4
        ));
    }
    out
}

/// `memory` with the human's answer to the question Warriv handed on
/// about `title` added to its Open part, for the next wake to make a rule
/// of. `None` when the quest went on without words Horadric heard.
pub fn with_answer(memory: &str, title: &str, question: &str, answer: Option<&str>) -> String {
    let line = match answer.map(one_line).filter(|a| !a.is_empty()) {
        Some(a) => format!(
            "{ANSWERED}on \"{}\" you asked \"{}\". The human said: {a}",
            one_line(title),
            one_line(question)
        ),
        None => format!(
            "{ANSWERED}on \"{}\" you asked \"{}\". The human settled it without words \
             Horadric heard: read the quest and its notes for how.",
            one_line(title),
            one_line(question)
        ),
    };
    let memory = if memory.trim().is_empty() {
        blank_memory()
    } else {
        memory.to_string()
    };
    let mut lines: Vec<&str> = memory.lines().collect();
    let at = match lines.iter().position(|l| l.trim_end() == OPEN) {
        Some(head) => {
            // After the part's last line, before the next heading.
            let end = lines[head + 1..]
                .iter()
                .position(|l| l.starts_with("## "))
                .map_or(lines.len(), |i| head + 1 + i);
            let last = lines[head + 1..end]
                .iter()
                .rposition(|l| !l.trim().is_empty())
                .map_or(head, |i| head + 1 + i);
            if last > head {
                last + 1
            } else {
                if !lines.get(head + 1).is_some_and(|l| l.trim().is_empty()) {
                    lines.insert(head + 1, "");
                }
                head + 2
            }
        }
        None => {
            if lines.last().is_some_and(|l| !l.trim().is_empty()) {
                lines.push("");
            }
            lines.push(OPEN);
            lines.push("");
            lines.len()
        }
    };
    lines.insert(at, &line);
    if lines.get(at + 1).is_some_and(|l| l.starts_with("## ")) {
        lines.insert(at + 1, "");
    }
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

/// The flags that let a session run `horadric quest` without asking,
/// through either shell tool Claude Code has on Windows. Only the
/// command as written matches: `& "..."` or a `cd` before it asks.
pub fn quest_tools(horadric: &str) -> Vec<String> {
    let h = horadric.trim_matches('"');
    vec![
        "--allowedTools".to_string(),
        format!("Bash({h} quest:*)"),
        format!("PowerShell({h} quest:*)"),
    ]
}

/// What a prompt says so the quest commands run without asking.
const AS_WRITTEN: &str = "Run it with the Bash tool, exactly as written, one command at a \
     time, never behind `&`, `cd` or `&&`: anything else stops for the human's permission.";

/// What a quest's notes line starts with when it says where the quest
/// came from: a link, a message's id, a pull request.
pub const FROM: &str = "From: ";

/// Every `From:` a quest log's notes hold, in the log's order, once each,
/// so an errand knows what it filed before.
pub fn froms(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for t in parse(text) {
        for n in &t.notes {
            let from = n.trim().strip_prefix(FROM.trim_end()).map(str::trim);
            if let Some(f) = from.filter(|f| !f.is_empty()) {
                if !out.iter().any(|o| o == f) {
                    out.push(f.to_string());
                }
            }
        }
    }
    out
}

/// How many `From:` lines an errand's system prompt lists, the newest
/// ones, so a long log does not make a long prompt.
const FROMS_LISTED: usize = 200;

/// The system prompt of an errand's session: a stone named `label` cast
/// unattended on a clock, in a project whose quest log is `file`, which
/// holds the `From:` lines `froms` already. What it finds becomes
/// quests, each saying where it came from, and nothing is filed twice.
pub fn errand_prompt(horadric: &str, file: &str, label: &str, froms: &[String]) -> String {
    let mut out = format!(
        "You run the errand \"{label}\" for Horadric, which casts it on a clock while \
         nobody watches. Horadric shows the human's coding agent sessions and runs the \
         project's quest log, {file}: the work agent sessions take on one quest at a time. \
         Your steps come as prompts, one turn each; do what each says, and the session \
         closes after the last. Do not ask questions in this chat: nobody reads it.\n\n\
         What you find that needs doing becomes a quest, not work you do here:\n\
         - Add one with `{horadric} quest add \"<title>\" --notes \"{FROM}<where it came \
         from>\"`, the title a short line of what to do. Where it came from is a link, a \
         message's id, an issue or a pull request: something that names that one finding \
         and no other. Add `--after \"<title>\"` for a quest it needs done first.\n\
         - Say more under it with `{horadric} quest note \"<title>\" \"<line>\"`.\n\
         - Read the log with `{horadric} quest list`, or the file itself. Never edit the \
         file directly.\n\
         - File nothing whose `{FROM}` line is in the log already, done or not: that one \
         was filed by an earlier cast. One finding is one quest.\n\
         {AS_WRITTEN}"
    );
    let listed = &froms[froms.len().saturating_sub(FROMS_LISTED)..];
    if listed.is_empty() {
        out.push_str("\n\nThe log has no `From:` lines yet.");
    } else {
        out.push_str("\n\nThe `From:` lines in the log when this cast began:");
        for f in listed {
            out.push_str("\n- ");
            out.push_str(f);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::parse;

    #[test]
    fn the_quest_commands_are_allowed_in_both_shells() {
        assert_eq!(
            quest_tools("\"C:/h/horadric.exe\""),
            [
                "--allowedTools",
                "Bash(C:/h/horadric.exe quest:*)",
                "PowerShell(C:/h/horadric.exe quest:*)"
            ]
        );
        assert!(system_prompt("h", "f").contains("never behind `&`"));
        assert!(errand_prompt("h", "f", "l", &[]).contains("never behind `&`"));
    }

    #[test]
    fn froms_are_read_from_every_quest_once_each() {
        let log = "- [ ] Fix the login\n  From: https://x/issues/4\n  Model: haiku\n\
                   - [x] Typo on the page\n  From:   slack 1234  \n\
                   - [ ] Again\n  From: https://x/issues/4\n  From:\n\
                   - [ ] Mine\n  Comes from: nothing\n";
        assert_eq!(froms(log), ["https://x/issues/4", "slack 1234"]);
        assert!(froms("").is_empty());
    }

    #[test]
    fn an_errand_is_told_the_quest_commands_the_from_rule_and_what_is_filed() {
        let froms = vec!["https://x/issues/4".to_string()];
        let p = errand_prompt("horadric", ".horadric/tasks.md", "Feedback", &froms);
        assert!(p.contains("\"Feedback\""));
        assert!(p.contains("horadric quest add \"<title>\" --notes \"From: "));
        assert!(p.contains("horadric quest note"));
        assert!(p.contains(".horadric/tasks.md"));
        assert!(p.contains("File nothing whose `From: ` line is in the log already"));
        assert!(p.ends_with("began:\n- https://x/issues/4"));
        assert!(errand_prompt("h", "f", "l", &[]).ends_with("no `From:` lines yet."));
    }

    #[test]
    fn an_errand_lists_only_the_newest_froms() {
        let froms: Vec<String> = (0..FROMS_LISTED + 5).map(|i| format!("id {i}")).collect();
        let p = errand_prompt("h", "f", "l", &froms);
        assert!(!p.contains("- id 4\n"));
        assert!(p.contains("- id 5\n"));
        assert!(p.ends_with(&format!("- id {}", FROMS_LISTED + 4)));
    }

    #[test]
    fn it_is_off_unless_the_config_says_so() {
        assert!(!on(""));
        assert!(!on("{}"));
        assert!(!on(r#"{"orchestrator": false}"#));
        assert!(!on(r#"{"orchestrator": "yes"}"#));
        assert!(on(r#"{"tasks": {"mode": "auto"}, "orchestrator": true}"#));
    }

    fn list(text: &str) -> Vec<Task> {
        parse(text)
    }

    #[test]
    fn a_quest_blocked_on_the_human_wakes_it_and_one_that_waits_does_not() {
        let t = list(
            "- [!] A @a-1: which port?\n\
             - [!] B @b-1: needs A {after}\n  After: A\n\
             - [!] C @c-1: later {until: 2026-10-01T14:05Z}\n\
             - [!] D @d-1: Warriv asks: which account pays?\n",
        );
        let e = events(&t, Mode::Manual, &[], &[]);
        assert_eq!(
            e,
            vec![Event {
                kind: Kind::Blocked,
                title: "A".into(),
                detail: "which port?".into()
            }]
        );
    }

    #[test]
    fn a_session_that_asks_after_the_nudge_wakes_it() {
        let t = list("- [/] A @a-1\n- [/] B @b-1\n");
        let e = events(&t, Mode::Review, &["B".into()], &[]);
        assert_eq!(e.len(), 1);
        assert_eq!((e[0].kind, e[0].title.as_str()), (Kind::Asks, "B"));
    }

    #[test]
    fn a_tangled_quest_wakes_it() {
        let t =
            list("- [ ] A\n  After: Nothing like it\n- [ ] B\n  After: C\n- [ ] C\n  After: B\n");
        let e = events(&t, Mode::Manual, &[], &[]);
        let kinds: Vec<(Kind, &str)> = e.iter().map(|e| (e.kind, e.title.as_str())).collect();
        assert_eq!(
            kinds,
            vec![
                (Kind::Tangled, "A"),
                (Kind::Tangled, "B"),
                (Kind::Tangled, "C")
            ]
        );
        assert_eq!(e[0].detail, "No quest is called \"Nothing like it\".");
    }

    #[test]
    fn a_log_in_auto_mode_with_nothing_ready_is_stalled() {
        let waiting =
            "- [x] A\n- [!] B @b-1: later {until: 2030-01-01T00:00Z}\n- [ ] C\n  After: B\n";
        let e = events(&list(waiting), Mode::Auto, &[], &[]);
        assert_eq!(e.last().unwrap().kind, Kind::Stalled);
        assert!(e.last().unwrap().detail.ends_with("Waiting: B; C."));
        // Not in review mode, not with a quest ready, not with one in hand.
        assert!(events(&list(waiting), Mode::Review, &[], &[])
            .iter()
            .all(|e| e.kind != Kind::Stalled));
        let ready = "- [!] B @b-1: needs a key\n- [ ] D\n";
        assert!(events(&list(ready), Mode::Auto, &[], &[])
            .iter()
            .all(|e| e.kind != Kind::Stalled));
        let in_hand = "- [/] A @a-1\n- [ ] C\n  After: A\n";
        assert!(events(&list(in_hand), Mode::Auto, &[], &[]).is_empty());
        // One blocked on the human is the event, not the stall.
        let on_human = "- [!] B @b-1: needs a key\n- [ ] C\n  After: B\n";
        let kinds: Vec<Kind> = events(&list(on_human), Mode::Auto, &[], &[])
            .iter()
            .map(|e| e.kind)
            .collect();
        assert_eq!(kinds, vec![Kind::Blocked]);
        // A finished log is not stalled.
        assert!(events(&list("- [x] A\n"), Mode::Auto, &[], &[]).is_empty());
    }

    #[test]
    fn a_dry_log_with_an_aim_open_wakes_it_even_with_nothing_left() {
        let aims = ["Ship the API".to_string()];
        let dry = |text: &str, mode| {
            events(&list(text), mode, &[], &aims)
                .into_iter()
                .find(|e| e.kind == Kind::Dry)
        };
        let empty = dry("", Mode::Auto).unwrap();
        assert_eq!(empty.detail, "No quest is left to start.");
        assert!(empty.title.is_empty());
        assert!(dry("- [x] A\n", Mode::Auto).is_some());
        let waiting = "- [!] B @b-1: later {until: 2030-01-01T00:00Z}\n";
        assert_eq!(dry(waiting, Mode::Auto).unwrap().detail, "Waiting: B.");
        // No stall besides it.
        assert_eq!(events(&list(waiting), Mode::Auto, &[], &aims).len(), 1);
        // Not outside auto mode, not with work ready, in hand or on the human.
        assert!(dry("", Mode::Review).is_none());
        assert!(dry("- [ ] A\n", Mode::Auto).is_none());
        assert!(dry("- [/] A @a-1\n", Mode::Auto).is_none());
        assert!(dry("- [!] A @a-1: which key?\n", Mode::Auto).is_none());
        assert!(dry("- [!] A @a-1: Warriv asks: which key?\n", Mode::Auto).is_none());
        // No aim, and an empty log is no event at all.
        assert!(events(&list(""), Mode::Auto, &[], &[]).is_empty());
    }

    fn dry_log() -> Event {
        Event {
            kind: Kind::Dry,
            title: String::new(),
            detail: "No quest is left to start.".into(),
        }
    }

    #[test]
    fn a_dry_log_is_not_its_own_doing() {
        let mut d = Desk::default();
        d.hear(vec![blocked("A", "x")], false);
        assert!(matches!(d.wake(0), Wake::Go(_)));
        d.hear(vec![dry_log()], true);
        assert_eq!(d.wake(1), Wake::Go(vec![dry_log()]));
    }

    #[test]
    fn three_dry_wakes_in_a_row_that_move_nothing_go_to_the_human() {
        let mut d = Desk::default();
        let wake = |d: &mut Desk, at| {
            d.hear(vec![dry_log()], false);
            assert_eq!(d.wake(at), Wake::Go(vec![dry_log()]));
        };
        wake(&mut d, 0);
        assert_eq!(d.dry_ended(dry_log(), true, false), Dried::Again);
        wake(&mut d, 1);
        // Filing something starts the count over.
        assert_eq!(d.dry_ended(dry_log(), true, true), Dried::Again);
        wake(&mut d, 2);
        assert_eq!(d.dry_ended(dry_log(), true, false), Dried::Again);
        wake(&mut d, 3);
        assert_eq!(d.dry_ended(dry_log(), true, false), Dried::Again);
        wake(&mut d, 4);
        assert_eq!(d.dry_ended(dry_log(), true, false), Dried::Human);
        // The human's now: heard, not queued, not held by Warriv.
        d.hear(vec![dry_log()], false);
        assert_eq!(d.wake(5), Wake::Nothing);
        assert!(d.holding(&[dry_log()]).is_empty());
        // A log that is no longer dry starts over too.
        let mut d = Desk::default();
        wake(&mut d, 0);
        assert_eq!(d.dry_ended(dry_log(), false, false), Dried::Moved);
    }

    #[test]
    fn the_dry_prompt_carries_the_aims_and_the_rules() {
        let p = prompt(&[Brief {
            event: Some(dry_log()),
            aims: vec!["Ship the API".into(), "Write the docs".into()],
            ..Brief::default()
        }]);
        assert!(p.contains(
            "- The log has run dry: nothing is in hand and nothing is ready to start. \
             No quest is left to start.\n"
        ));
        assert!(p.contains("    Aim: Ship the API\n    Aim: Write the docs\n"));
        assert!(p.contains("at most 8"));
        assert!(p.contains("`Filed by Warriv for: <the aim>`"));
        assert!(p.contains("mark it reached"));
        assert!(!more(&[Brief {
            event: Some(dry_log()),
            aims: vec!["A".into()],
            ..Brief::default()
        }])
        .contains('\n'));
    }

    #[test]
    fn quests_filed_for_an_aim_are_counted_by_their_note() {
        assert_eq!(filed("Ship\nthe API"), "Filed by Warriv for: Ship the API");
        let t = list(
            "- [ ] A\n  Filed by Warriv for: Ship\n  After: B\n- [x] B\n  Filed by Warriv for: Ship\n- [ ] C\n  Warriv: no\n",
        );
        assert_eq!(filed_count(&t), 2);
    }

    #[test]
    fn the_human_is_asked_about_the_first_aim() {
        let s = stuck(&["Ship\nthe API".into(), "Docs".into()]);
        assert!(s.contains("\"Ship the API\""));
        assert!(!s.contains("Docs"));
    }

    fn blocked(title: &str, why: &str) -> Event {
        Event {
            kind: Kind::Blocked,
            title: title.into(),
            detail: why.into(),
        }
    }

    #[test]
    fn each_event_is_heard_once_while_it_holds() {
        let mut d = Desk::default();
        d.hear(vec![blocked("A", "which port?")], false);
        assert_eq!(d.wake(0), Wake::Go(vec![blocked("A", "which port?")]));
        d.hear(vec![blocked("A", "which port?")], false);
        assert_eq!(d.wake(1), Wake::Nothing);
        // Blocked again for another reason is new.
        d.hear(vec![blocked("A", "which host?")], false);
        assert_eq!(d.wake(2), Wake::Go(vec![blocked("A", "which host?")]));
        // Gone and back is new too.
        d.hear(vec![], false);
        d.hear(vec![blocked("A", "which host?")], false);
        assert!(matches!(d.wake(3), Wake::Go(_)));
    }

    #[test]
    fn events_that_arrive_while_it_works_wait_for_the_next_stop() {
        let mut d = Desk::default();
        d.hear(vec![blocked("A", "x")], false);
        assert!(matches!(d.wake(0), Wake::Go(_)));
        d.hear(vec![blocked("A", "x"), blocked("B", "y")], true);
        d.hear(
            vec![blocked("A", "x"), blocked("B", "y"), blocked("C", "z")],
            true,
        );
        assert!(d.waiting());
        assert_eq!(
            d.wake(5),
            Wake::Go(vec![blocked("B", "y"), blocked("C", "z")])
        );
        // One settled before the stop is not told.
        d.hear(vec![blocked("D", "w")], true);
        d.hear(vec![], true);
        assert_eq!(d.wake(6), Wake::Nothing);
    }

    #[test]
    fn it_is_never_woken_by_its_own_changes() {
        let mut d = Desk::default();
        let tangled = Event {
            kind: Kind::Tangled,
            title: "B".into(),
            detail: "No quest is called \"X\".".into(),
        };
        d.hear(vec![tangled.clone()], true);
        assert_eq!(d.wake(0), Wake::Nothing);
        // Still there once it closed: heard already, so nobody wakes it.
        d.hear(vec![tangled.clone()], false);
        assert_eq!(d.wake(1), Wake::Nothing);
        // The same thing while it sleeps is somebody else's.
        let mut d = Desk::default();
        d.hear(vec![tangled.clone()], false);
        assert_eq!(d.wake(0), Wake::Go(vec![tangled]));
    }

    #[test]
    fn what_it_could_not_settle_is_the_humans_while_it_holds() {
        let mut d = Desk::default();
        let now = vec![blocked("A", "x"), blocked("B", "y")];
        d.hear(now.clone(), false);
        assert!(matches!(d.wake(0), Wake::Go(_)));
        assert_eq!(d.holding(&now), ["A", "B"].map(String::from).into());
        // Its turn ended with A still blocked.
        d.hand(blocked("A", "x"));
        d.hear(now.clone(), false);
        assert_eq!(d.holding(&now), ["B"].map(String::from).into());
        assert_eq!(d.wake(1), Wake::Nothing);
        // Blocked again for another reason, it is Warriv's again.
        let again = vec![blocked("A", "z")];
        d.hear(again.clone(), false);
        assert_eq!(d.holding(&again), ["A"].map(String::from).into());
        // A whole-log event is no quest's.
        let stalled = Event {
            kind: Kind::Stalled,
            title: String::new(),
            detail: "Waiting: A.".into(),
        };
        assert!(d.holding(&[stalled]).is_empty());
    }

    #[test]
    fn its_own_tangle_and_what_comes_while_it_rests_go_to_the_human() {
        let mut d = Desk::default();
        let tangled = Event {
            kind: Kind::Tangled,
            title: "B".into(),
            detail: "No quest is called \"X\".".into(),
        };
        d.hear(vec![tangled.clone()], true);
        assert!(d.holding(&[tangled]).is_empty());
        let mut d = Desk::default();
        for at in 0..WAKES_AN_HOUR as u64 {
            d.hear(vec![blocked(&at.to_string(), "?")], false);
            assert!(matches!(d.wake(at), Wake::Go(_)));
        }
        let late = vec![blocked("late", "?")];
        d.hear(late.clone(), false);
        assert!(matches!(d.wake(10), Wake::Tired { .. }));
        assert!(d.holding(&late).is_empty());
    }

    #[test]
    fn a_failed_merge_waits_until_told_even_though_the_log_does_not_hold_it() {
        let mut d = Desk::default();
        let merge = Event {
            kind: Kind::Merge,
            title: "A".into(),
            detail: "CONFLICT".into(),
        };
        d.push(merge.clone());
        d.push(merge.clone());
        d.hear(vec![], true);
        assert_eq!(d.wake(0), Wake::Go(vec![merge]));
    }

    #[test]
    fn six_wakes_an_hour_then_the_human_hears_why_once() {
        let mut d = Desk::default();
        for i in 0..WAKES_AN_HOUR as u64 {
            d.hear(vec![blocked("A", &i.to_string())], false);
            assert!(matches!(d.wake(i * 60), Wake::Go(_)));
        }
        d.hear(vec![blocked("A", "late")], false);
        assert_eq!(
            d.wake(400),
            Wake::Tired {
                events: vec![blocked("A", "late")],
                first: true
            }
        );
        d.hear(vec![blocked("A", "later")], false);
        assert!(matches!(d.wake(500), Wake::Tired { first: false, .. }));
        // An hour after the first wake, one is free again.
        d.hear(vec![blocked("A", "next hour")], false);
        assert!(matches!(d.wake(HOUR), Wake::Go(_)));
    }

    #[test]
    fn driving_lifts_the_six_wakes_and_it_never_rests() {
        let mut d = Desk::default();
        d.drive(true);
        for i in 0..WAKES_AN_HOUR as u64 * 3 {
            d.hear(vec![blocked("A", &i.to_string())], false);
            assert!(matches!(d.wake(i * 60), Wake::Go(_)));
        }
        assert_eq!(d.watch(None, 2000), None);
        // Each event is still heard once.
        assert_eq!(d.wake(2000), Wake::Nothing);
        // Stopped, the budget counts the wakes it had.
        d.drive(false);
        d.hear(vec![blocked("A", "after the stop")], false);
        assert!(matches!(d.wake(2100), Wake::Tired { first: true, .. }));
    }

    #[test]
    fn driving_turns_the_orchestrator_on_and_errands_wait_out_the_hold() {
        assert!(!orchestrates(false, false));
        assert!(orchestrates(true, false));
        assert!(orchestrates(false, true));
        assert_eq!(when_full(false), Full::Skip);
        assert_eq!(when_full(true), Full::Wait);
    }

    #[test]
    fn the_tile_says_warriv_drives_whatever_else_it_says() {
        let drive = Some(Drive::default());
        let public = Some(Drive { ships_public: true });
        assert_eq!(line(None, None), None);
        assert_eq!(
            line(Some("Warriv: settling 2".into()), None).as_deref(),
            Some("Warriv: settling 2")
        );
        assert_eq!(line(None, drive).as_deref(), Some("Warriv drives"));
        assert_eq!(
            line(None, public).as_deref(),
            Some("Warriv drives and ships public")
        );
        assert_eq!(
            line(Some("Warriv: settling 2".into()), drive).as_deref(),
            Some("Warriv drives, settling 2")
        );
        assert_eq!(
            line(Some("Warriv: settling".into()), public).as_deref(),
            Some("Warriv drives and ships public, settling")
        );
    }

    #[test]
    fn a_drive_keeps_in_json_with_ships_public_only_when_on() {
        let off = serde_json::to_string(&Drive::default()).unwrap();
        assert_eq!(off, "{}");
        let on: Drive = serde_json::from_str(r#"{"ships_public": true}"#).unwrap();
        assert!(on.ships_public);
        assert_eq!(
            serde_json::from_str::<Drive>("{}").unwrap(),
            Drive::default()
        );
    }

    #[test]
    fn the_tile_says_what_it_settles_and_when_it_rests() {
        let mut d = Desk::default();
        assert_eq!(d.watch(None, 0), None);
        d.hear(vec![blocked("A", "x")], false);
        assert!(matches!(d.wake(0), Wake::Go(_)));
        assert_eq!(d.watch(Some(1), 1), Some(Watch::Settling(1)));
        // What waits for its next stop is in hand too.
        d.hear(vec![blocked("A", "x"), blocked("B", "y")], true);
        assert_eq!(d.watch(Some(1), 2), Some(Watch::Settling(2)));
        assert!(Watch::Settling(2).working());
        // Asleep with wakes left, it says nothing.
        let mut d = Desk::default();
        for i in 0..WAKES_AN_HOUR as u64 - 1 {
            d.hear(vec![blocked("A", &i.to_string())], false);
            assert!(matches!(d.wake(100 + i * 60), Wake::Go(_)));
        }
        assert_eq!(d.watch(None, 400), None);
        d.hear(vec![blocked("A", "last")], false);
        assert!(matches!(d.wake(500), Wake::Go(_)));
        // Spent, it rests until the first wake is an hour old.
        let rests = Watch::Rests { until: 100 + HOUR };
        assert_eq!(d.watch(None, 600), Some(rests));
        assert!(!rests.working());
        assert_eq!(d.watch(None, 100 + HOUR), None);
    }

    #[test]
    fn the_line_reads_plainly() {
        assert_eq!(Watch::Settling(2).words(0, 0), "Warriv: settling 2");
        assert_eq!(Watch::Settling(0).words(0, 0), "Warriv: settling");
        // 20:40 local now, free again an hour on.
        let local = 20 * 3600 + 40 * 60;
        let rests = Watch::Rests {
            until: 1_000 + HOUR,
        };
        assert_eq!(rests.words(local, 1_000), "Warriv rests until 21:40");
        // Past midnight.
        let late = 23 * 3600 + 30 * 60;
        assert_eq!(rests.words(late, 1_000), "Warriv rests until 00:30");
    }

    #[test]
    fn the_first_prompt_gives_the_event_the_notes_and_the_last_turn() {
        let p = prompt(&[
            Brief {
                event: Some(blocked("Serve the API", "which port?")),
                notes: vec![
                    "The port is in docs/PORTS.md.".into(),
                    "Warriv: tried".into(),
                ],
                last_turn: Some("I built it.\nWhich port should it listen on?".into()),
                aims: Vec::new(),
            },
            Brief {
                event: Some(Event {
                    kind: Kind::Stalled,
                    title: String::new(),
                    detail: "Waiting: B.".into(),
                }),
                ..Brief::default()
            },
        ]);
        assert_eq!(
            p,
            "Horadric woke you for this:\n\n\
             - The quest \"Serve the API\" is blocked: which port?\n\
             \x20 Its notes:\n\
             \x20   The port is in docs/PORTS.md.\n\
             \x20   Warriv: tried\n\
             \x20 Its session's last turn ended with:\n\
             \x20   I built it.\n\
             \x20   Which port should it listen on?\n\
             \n\
             - The log is stalled. Waiting: B.\n"
        );
    }

    #[test]
    fn a_long_last_turn_keeps_its_end() {
        let long = format!("{}Which port?", "x".repeat(3 * LAST_TURN));
        let p = prompt(&[Brief {
            event: Some(blocked("A", "")),
            notes: Vec::new(),
            last_turn: Some(long),
            aims: Vec::new(),
        }]);
        assert!(p.contains("- The quest \"A\" is blocked.\n"));
        assert!(p.contains("\u{2026}x"));
        assert!(p.trim_end().ends_with("Which port?"));
        assert!(p.chars().count() < LAST_TURN + 200);
    }

    #[test]
    fn what_more_happened_is_one_line() {
        let m = more(&[Brief {
            event: Some(blocked("A", "x")),
            notes: vec!["n".into()],
            last_turn: Some("a\nb".into()),
            aims: Vec::new(),
        }]);
        assert!(!m.contains('\n'));
        assert!(m.starts_with("While you worked, more happened. - The quest \"A\" is blocked: x"));
    }

    #[test]
    fn the_system_prompt_names_every_command_it_may_run() {
        let p = system_prompt("hx", ".horadric/quests.md");
        for c in [
            "hx quest tell \"<title>\" \"<message>\"",
            "hx quest note \"<title>\"",
            "hx quest add \"<title>\"",
            "--below \"<title>\"",
            "--after \"<title>\"",
            "hx quest blocked \"<question>\" --quest \"<title>\"",
            "hx quest aim done \"<the aim>\"",
            "--notes \"Filed by Warriv for: <the aim>\"",
            "hx quest note \"<title>\" \"Model: <name>\"",
            "Never change a `Model:` line you did not write",
        ] {
            assert!(p.contains(c), "{c}");
        }
        assert!(p.contains("Change no code, start no session"));
    }

    #[test]
    fn an_overrule_goes_where_the_quest_is_now() {
        let said = "You assumed \u{201C}port 4100\u{201D}; instead: use 4200";
        for m in [Mark::Working, Mark::Review, Mark::Blocked] {
            assert_eq!(
                overrule(Some(m), "Serve", "port 4100", "use 4200"),
                Overrule::Tell(said.into())
            );
        }
        assert_eq!(
            overrule(Some(Mark::Open), "Serve", "port 4100", "use\n4200"),
            Overrule::Note(format!("The human answers: {said}"))
        );
        for m in [Some(Mark::Done), None] {
            let Overrule::Quest { title, notes } = overrule(m, "Serve", "port 4100", "use 4200")
            else {
                panic!("a done quest gets a new one");
            };
            assert_eq!(title, "Overrule on Serve");
            assert!(notes.ends_with("\nThe human answers: use 4200"));
            // Its notes must not read as an assumption themselves.
            assert!(notes.lines().all(|l| crate::tasks::assumed(l).is_none()));
        }
    }

    #[test]
    fn driving_warriv_assumes_what_can_be_undone() {
        let p = driven_prompt("hx");
        assert!(p.contains("`hx quest note \"<title>\" \"Assumed: "));
        assert!(p.contains("can not be undone"));
        assert!(!p.contains("  "));
    }

    #[test]
    fn a_told_session_hears_warriv_on_one_line_and_how_to_report() {
        let t = told("hx", "Use port\n4100.");
        assert!(t.starts_with("Warriv, who plans this project's quests, answers: Use port 4100."));
        assert!(t.contains("`hx quest done"));
        assert!(!t.contains('\n'));
        assert_eq!(
            human_note(
                "Blue,
not green."
            ),
            "The human answers: Blue, not green."
        );
        let a = answered("hx", "Blue.");
        assert!(a.starts_with("The human answers: Blue. Go on with this quest"));
        assert!(a.contains("`hx quest done"));
    }

    #[test]
    fn its_sessions_are_known_by_their_id() {
        assert!(is_warriv("warriv"));
        assert!(is_warriv("warriv-51234"));
        assert!(!is_warriv("warrivs-1"));
        assert!(!is_warriv("fix-warriv-1"));
    }

    #[test]
    fn its_marks_in_the_log_read_back() {
        assert_eq!(note("split\ninto two"), "Warriv: split into two");
        assert_eq!(handed("Which\naccount?"), "Warriv asks: Which account?");
        assert!(handed("x").starts_with(HANDED));
        assert_eq!(question(&handed("Which card?")), Some("Which card?"));
        assert_eq!(question("which card?"), None);
    }

    #[test]
    fn a_note_goes_under_the_quest_after_its_other_notes() {
        let text = "- [!] Serve the API @s-1: which port?
  Use docs.
- [ ] Next
";
        assert_eq!(
            add_note(text, "Serve", &note("told it 4100")).unwrap(),
            "- [!] Serve the API @s-1: which port?
  Use docs.
  Warriv: told it 4100
- [ ] Next
"
        );
        assert!(add_note(text, "Nothing", "x").is_err());
    }

    #[test]
    fn a_held_quest_is_handed_to_the_human_with_the_question() {
        let text = "- [!] Pay for it @p-1: which card?
  n
  Warriv: no card in the docs
- [/] Work @w-1
  Warriv: asked the docs
- [ ] Open
- [x] Old @o-1
";
        assert_eq!(
            hand_on(text, "Pay for it", "Which card pays?").unwrap(),
            "- [!] Pay for it @p-1: Warriv asks: Which card pays?
  n
  Warriv: no card in the docs
- [/] Work @w-1
  Warriv: asked the docs
- [ ] Open
- [x] Old @o-1
"
        );
        assert!(hand_on(text, "Work", "?").unwrap().contains(
            "- [!] Work @w-1: Warriv asks: ?
"
        ));
        assert!(hand_on(text, "Open", "?").is_err());
        assert!(hand_on(text, "Old", "?").is_err());
        // Not before the notes say what Warriv tried.
        let untried = "- [!] Pay @p-1: which card?
  n
";
        assert!(hand_on(untried, "Pay", "?")
            .unwrap_err()
            .starts_with("say what you tried first"));
        // Handed on, it no longer wakes Warriv.
        let handed = hand_on(text, "Pay", "Which card?").unwrap();
        assert!(events(&parse(&handed), Mode::Manual, &[], &[]).is_empty());
    }

    #[test]
    fn a_quest_added_below_another_goes_after_its_notes() {
        let text = "- [/] Big @b-1
  notes
- [ ] Last
";
        assert_eq!(
            add_below(text, "Big", "Second half", "After: Big").unwrap(),
            "- [/] Big @b-1
  notes
- [ ] Second half
  After: Big
- [ ] Last
"
        );
        assert_eq!(
            add_below(
                "- [ ] Only
",
                "Only",
                "Two",
                ""
            )
            .unwrap(),
            "- [ ] Only
- [ ] Two
"
        );
    }
    #[test]
    fn reviewers_are_known_by_their_id_and_are_warrivs() {
        assert!(is_reviewer("warriv-review-4100"));
        assert!(is_warriv("warriv-review-4100"));
        assert!(!is_reviewer("warriv-4100"));
        assert!(!is_reviewer("warriv-reviews-a-mode"));
    }

    #[test]
    fn a_quest_going_to_review_is_one_to_read_but_tombs_are_not() {
        let t = list("- [?] A @a-1\n- [/] B @b-1\n- [?] C @c-1.x3\n- [x] D @d-1\n- [?] E @e-1\n");
        assert_eq!(to_review(&t), ["A", "E"]);
    }

    fn titles(t: &[&str]) -> Vec<String> {
        t.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn one_review_at_a_time_and_the_rest_queue() {
        let mut r = Reviews::default();
        r.hear(&titles(&["A", "B"]));
        assert_eq!(r.take().as_deref(), Some("A"));
        // Under way, nothing else starts.
        r.hear(&titles(&["A", "B", "C"]));
        assert_eq!(r.take(), None);
        assert!(r.pending("A") && r.pending("C"));
        r.done();
        assert_eq!(r.take().as_deref(), Some("B"));
        r.done();
        assert_eq!(r.take().as_deref(), Some("C"));
        r.done();
        assert_eq!(r.take(), None);
    }

    #[test]
    fn a_quest_is_read_once_while_it_waits_and_again_when_it_comes_back() {
        let mut r = Reviews::default();
        r.hear(&titles(&["A"]));
        assert_eq!(r.take().as_deref(), Some("A"));
        // The reviewer ended without a verdict: the human's now.
        r.done();
        r.hear(&titles(&["A"]));
        assert_eq!(r.take(), None);
        assert!(!r.pending("A"));
        // Sent back, worked on, finished again: a new review.
        r.hear(&[]);
        r.hear(&titles(&["A"]));
        assert_eq!(r.take().as_deref(), Some("A"));
    }

    #[test]
    fn a_quest_approved_while_it_queued_is_not_read() {
        let mut r = Reviews::default();
        r.hear(&titles(&["A", "B"]));
        assert_eq!(r.take().as_deref(), Some("A"));
        r.hear(&titles(&["A"]));
        r.done();
        assert_eq!(r.take(), None);
        // Out of the mode, nothing waits, and back in it reads again.
        r.hear(&titles(&["C"]));
        r.stop();
        assert_eq!(r.take(), None);
        r.hear(&titles(&["C"]));
        assert_eq!(r.take().as_deref(), Some("C"));
    }

    fn review(diff: &str) -> Review {
        Review {
            title: "Serve the API".into(),
            notes: vec!["Port 4100.".into()],
            branch: Some("serve-the-api".into()),
            into: "main".into(),
            diff: diff.into(),
        }
    }

    #[test]
    fn the_review_prompt_gives_the_quest_its_notes_and_its_diff() {
        let p = review_prompt(&review("+fn serve() {}\n"));
        assert_eq!(
            p,
            "Review the quest \"Serve the API\".\n\n\
             Its notes:\n\
             \x20 Port 4100.\n\n\
             Its work is on the branch serve-the-api. `git diff main...serve-the-api` says:\n\n\
             ```diff\n+fn serve() {}\n```\n"
        );
        let empty = review_prompt(&review(""));
        assert!(empty.contains("has no changes against main"));
        let trunk = review_prompt(&Review {
            branch: None,
            ..review("")
        });
        assert!(trunk.contains("It worked in the main tree, on main"));
    }

    #[test]
    fn a_long_diff_keeps_its_start_and_says_how_to_read_the_rest() {
        let long = format!("+start\n{}", "x".repeat(2 * DIFF));
        let p = review_prompt(&review(&long));
        assert!(p.contains("+start"));
        assert!(p.contains("x\u{2026}\n```"));
        assert!(p
            .trim_end()
            .ends_with("Run `git diff main...serve-the-api` for the rest."));
        assert!(p.chars().count() < DIFF + 500);
    }

    #[test]
    fn the_review_system_prompt_names_the_three_endings() {
        let p = review_system_prompt("hx", ".horadric/tasks.md");
        for c in [
            "hx quest pass \"<title>\"",
            "hx quest fix \"<title>\" \"<what is wrong",
            "hx quest note \"<title>\"",
            "hx quest blocked \"<question>\" --quest \"<title>\"",
        ] {
            assert!(p.contains(c), "{c}");
        }
        assert!(p.contains("run no checks"));
    }

    #[test]
    fn a_pass_completes_a_quest_waiting_for_review_and_keeps_its_holder() {
        let text = "- [?] Serve the API @serve-1\n  n\n- [/] Work @w-1\n";
        assert_eq!(
            pass(text, "Serve").unwrap(),
            "- [x] Serve the API @serve-1\n  n\n- [/] Work @w-1\n"
        );
        assert!(pass(text, "Work")
            .unwrap_err()
            .contains("is not waiting for review"));
        assert!(pass(text, "Nothing").is_err());
    }

    #[test]
    fn a_session_sent_back_hears_why_on_one_line_and_how_to_report() {
        let t = sent_back("hx", "The port is\n4100, not 80.");
        assert!(t.starts_with(
            "Warriv reviewed this quest and sends it back: The port is 4100, not 80."
        ));
        assert!(t.contains("`hx quest done"));
        assert!(!t.contains('\n'));
    }

    #[test]
    fn sent_back_with_its_session_gone_files_a_fix_up_below() {
        let text = "- [?] Serve the API @serve-1\n  n\n- [ ] Next\n  After: Serve the API\n";
        let fix = fix_up("Serve the API", Some("serve-the-api"), "main", "No tests.");
        assert_eq!(fix.title, "Fix what review found in serve-the-api");
        assert!(fix.notes.contains("sent it back: No tests."));
        assert!(fix.notes.contains("git cherry-pick main..serve-the-api"));
        let out = send_back(text, "Serve the API", &fix).unwrap();
        let t = parse(&out);
        assert_eq!(t[0].mark, Mark::Done);
        assert_eq!(t[1].title, fix.title);
        assert_eq!(t[1].mark, Mark::Open);
        assert_eq!(
            t[2].after(),
            ["Fix what review found in serve-the-api", "Serve the API"]
        );
        assert!(send_back(&out, "Serve the API", &fix).is_err());
        let trunk = fix_up("Serve", None, "main", "x");
        assert_eq!(trunk.title, "Fix what review found in Serve");
        assert!(trunk.notes.contains("already in `main`"));
    }

    #[test]
    fn the_memory_prompt_says_how_to_read_and_write_it() {
        let p = memory_prompt(MEMORY, Some(&blank_memory()));
        assert!(p.starts_with("Your memory is .horadric/warriv.md,"));
        for part in ["## Rules", "## Lately", "## Open"] {
            assert!(p.contains(part), "{part}");
        }
        assert!(p.contains("Read it before anything else"));
        assert!(p.contains("Write to it last"));
        assert!(p.contains("do not commit it yourself"));
        assert!(!p.contains("does not exist"));
        assert!(!p.contains("compact"));
        assert!(!p.contains("Answered"));
    }

    #[test]
    fn with_no_memory_it_is_told_to_make_one() {
        for m in [None, Some(""), Some("  \n")] {
            assert!(memory_prompt(MEMORY, m).contains("It does not exist yet"));
        }
    }

    #[test]
    fn a_memory_past_two_hundred_lines_is_compacted() {
        let mut m = blank_memory();
        while m.lines().count() < MEMORY_LINES {
            m.push_str("- 2026-10-05 Did a thing.\n");
        }
        assert!(!compact_due(&m));
        assert!(!memory_prompt(MEMORY, Some(&m)).contains("compact it"));
        m.push_str("- 2026-10-05 One more.\n");
        assert!(compact_due(&m));
        let p = memory_prompt(MEMORY, Some(&m));
        assert!(p.contains("It is 201 lines, past 200: compact it before you stop."));
        assert!(p.contains("under 150 lines"));
    }

    #[test]
    fn an_answer_goes_under_open_and_the_prompt_makes_it_a_rule() {
        let m = with_answer(
            &blank_memory(),
            "Port",
            "Which\nport?",
            Some("4210,\nalways"),
        );
        assert_eq!(
            m,
            "# Warriv's memory\n\n## Rules\n\n## Lately\n\n## Open\n\n\
             - Answered: on \"Port\" you asked \"Which port?\". The human said: 4210, always\n"
        );
        let p = memory_prompt(MEMORY, Some(&m));
        assert!(p.contains("## Open holds a line starting `Answered:`"));
        assert!(p.contains("Write each as a rule under ## Rules"));
        let two = with_answer(&m, "Pay", "Which account?", None);
        assert!(memory_prompt(MEMORY, Some(&two)).contains("holds lines starting"));
        assert!(two.ends_with("Horadric heard: read the quest and its notes for how.\n"));
    }

    #[test]
    fn an_answer_goes_after_what_open_holds_and_before_the_next_part() {
        let m = "# M\n\n## Open\n\n- Watch CI.\n\n## Rules\n\n- Be brief.\n";
        assert_eq!(
            with_answer(m, "A", "Q?", Some("Yes")),
            "# M\n\n## Open\n\n- Watch CI.\n\
             - Answered: on \"A\" you asked \"Q?\". The human said: Yes\n\n\
             ## Rules\n\n- Be brief.\n"
        );
        assert_eq!(
            with_answer("# M\n\n## Open\n## Rules\n", "A", "Q?", Some("Yes")),
            "# M\n\n## Open\n\n- Answered: on \"A\" you asked \"Q?\". The human said: Yes\n\n\
             ## Rules\n"
        );
        assert_eq!(
            with_answer("# M\n\n## Rules\n- Be brief.\n", "A", "Q?", Some("Yes")),
            "# M\n\n## Rules\n- Be brief.\n\n## Open\n\n\
             - Answered: on \"A\" you asked \"Q?\". The human said: Yes\n"
        );
        assert!(with_answer("", "A", "Q?", None).starts_with(&blank_memory()));
    }
}
