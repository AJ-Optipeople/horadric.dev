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

use std::collections::BTreeSet;

use serde_json::Value;

use crate::tasks::{
    end_of, find, insert_note, insert_with_notes, item_line, one_line, parse, readiness,
    replace_line, Mark, Mode, Ready, Task,
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

/// What woke Warriv.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
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
        if self.wakes.len() >= WAKES_AN_HOUR {
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
         you may run without asking. Run it with the Bash tool, exactly as written, one \
         command at a time, never after `cd` or `&&`: anything else stops for the human's \
         permission.\n\n\
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
    format!(
        "Warriv, who plans this project's quests, answers: {} Go on with this quest, and \
         when it is finished, commit your work and run `{horadric} quest done \"<one short \
         line on what you achieved>\"`.",
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::parse;

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
        ] {
            assert!(p.contains(c), "{c}");
        }
        assert!(p.contains("Change no code, start no session"));
    }

    #[test]
    fn a_told_session_hears_warriv_on_one_line_and_how_to_report() {
        let t = told("hx", "Use port\n4100.");
        assert!(t.starts_with("Warriv, who plans this project's quests, answers: Use port 4100."));
        assert!(t.contains("`hx quest done"));
        assert!(!t.contains('\n'));
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
}
