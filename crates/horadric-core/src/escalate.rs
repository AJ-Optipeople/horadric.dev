//! How a quest's model follows how its runs went: one tier up when it
//! struggled and starts over, and a line per run that says how it ended,
//! so Warriv picks the next quest's model from what happened rather than
//! a guess.
//!
//! ```text
//! - [ ] Wire the new API in
//!   Warriv: Model: haiku
//!   Outcome: haiku, 14m, blocked, 820k tokens
//!   Escalated: sonnet, from haiku: blocked
//! ```
//!
//! A run is one stretch of a session on a quest: from the quest being
//! taken (or going on after a block) to it being blocked on the human,
//! sent back from review, landing, or done where nothing lands. Its
//! `Outcome:` line names the model its session was launched with, which
//! the chronicle keeps ([`crate::chronicle::Happened::Launched`]), not
//! what the notes say by then.
//!
//! A quest is escalated only where a fresh session starts on it, never
//! under a session that keeps going, so the rule is predictable: a quest
//! whose last run struggled (blocked or sent back) is escalated when it is
//! put back to open, and the fix-up quest filed for one that landed red or
//! was sent back after its session ended starts one tier above it. An
//! `Escalated:` line names that model. It stands where a `Warriv: Model:`
//! line would, below a human's `Model:` line, which always wins: see
//! [`crate::tasks::model_of`]. A quest is escalated once at most, and
//! never past opus. Any notes line that starts with `Escalated:` and a
//! model's name counts, a human's included.
//!
//! The lines travel with the quest into the done file, and [`summary`]
//! reads the `Outcome:` lines back for Warriv.

use crate::tasks::{self, Mark, Task, MODELS};

/// The word that starts an escalation's notes line.
pub const ESCALATED: &str = "Escalated:";

/// The word that starts a run's notes line.
pub const OUTCOME: &str = "Outcome:";

/// At most this many runs, the newest, go into Warriv's summary, so it
/// tells how quests go lately and stays the same size however long the
/// project has run.
pub const RECENT: usize = 100;

/// What a run is called when its session was started on no model of its
/// own, so on the agent's default.
pub const DEFAULT: &str = "default";

/// The model one tier above `model`, None at the top or for a name that
/// is not one of [`MODELS`].
pub fn up(model: &str) -> Option<&'static str> {
    let i = MODELS.iter().position(|m| *m == model)?;
    MODELS.get(i + 1).copied()
}

/// The model an `Escalated: <model>, from <model>: <why>` notes line moves
/// the quest to.
pub fn escalated_line(note: &str) -> Option<&'static str> {
    let rest = strip_word(note, ESCALATED)?;
    let name = rest
        .split(|c: char| c == ',' || c.is_whitespace())
        .find(|w| !w.is_empty())?;
    MODELS.into_iter().find(|m| m.eq_ignore_ascii_case(name))
}

/// `note` past `word`, read in any case, since a human writes these too.
fn strip_word<'a>(note: &'a str, word: &str) -> Option<&'a str> {
    let head = note.get(..word.len())?;
    head.eq_ignore_ascii_case(word)
        .then(|| note[word.len()..].trim())
}

/// Whether the model a quest with `notes` starts on is the one an
/// `Escalated:` line chose: no human line, and no Warriv line after it.
pub fn chosen_by_escalation(notes: &[String]) -> bool {
    if notes.iter().any(|n| tasks::model_line(n).is_some()) {
        return false;
    }
    notes
        .iter()
        .rev()
        .find_map(|n| {
            let warriv = n
                .strip_prefix(crate::warriv::NOTE)
                .and_then(tasks::model_line)
                .map(|_| false);
            warriv.or_else(|| escalated_line(n).map(|_| true))
        })
        .unwrap_or(false)
}

/// The model a command line starts Claude Code on: its `--model`, either
/// as two words or as `--model=<name>`, the last one counting.
pub fn launched_model(args: &[String]) -> Option<&str> {
    let mut out = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--model" {
            out = it.next().map(String::as_str).or(out);
        } else if let Some(m) = a.strip_prefix("--model=") {
            out = Some(m);
        }
    }
    out
}

/// How a run went wrong, in the words its escalation line gives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Struggle {
    /// It stopped on a question for the human.
    Blocked,
    /// A review sent it back.
    SentBack,
    /// It landed, but this check failed on it.
    Red(String),
}

impl Struggle {
    fn says(&self) -> String {
        match self {
            Struggle::Blocked => "blocked".to_string(),
            Struggle::SentBack => "sent back by review".to_string(),
            Struggle::Red(check) => format!("checks red on `{check}`"),
        }
    }
}

/// The line that moves `task` from `from` one tier up after `why`: None
/// when a human chose its model, `from` is the default model (which is
/// not known here) or opus already.
fn step(task: &Task, from: &str, why: &Struggle) -> Option<String> {
    if task.notes.iter().any(|n| tasks::model_line(n).is_some()) {
        return None;
    }
    let from = MODELS.into_iter().find(|m| *m == from)?;
    Some(format!(
        "{ESCALATED} {}, from {from}: {}",
        up(from)?,
        why.says()
    ))
}

/// The line that moves `task` one tier above the model it starts on,
/// after `why`, for its next fresh start. Once per quest, so a quest that
/// keeps struggling never climbs on its own and its notes say it once.
pub fn escalation(task: &Task, why: &Struggle) -> Option<String> {
    if task.notes.iter().any(|n| escalated_line(n).is_some()) {
        return None;
    }
    step(task, task.model()?, why)
}

/// The line that starts a fix-up quest for `task`, whose run on `ran`
/// ended after `why`, one tier above that run. The fix-up is a quest of
/// its own, so `task` having been escalated already does not stop it.
pub fn carried(task: &Task, ran: &str, why: &Struggle) -> Option<String> {
    step(task, ran, why)
}

/// How a run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ending {
    /// It landed and the project's checks passed on it.
    Green,
    /// It landed in a project with no checks.
    Landed,
    /// It was finished where nothing lands: the human approved it.
    Done,
    /// A check failed when it landed.
    Red,
    /// It did not rebase onto `main`.
    Conflict,
    Blocked,
    SentBack,
}

impl Ending {
    const ALL: [Ending; 7] = [
        Ending::Green,
        Ending::Landed,
        Ending::Done,
        Ending::Red,
        Ending::Conflict,
        Ending::Blocked,
        Ending::SentBack,
    ];

    pub fn word(self) -> &'static str {
        match self {
            Ending::Green => "green",
            Ending::Landed => "landed",
            Ending::Done => "done",
            Ending::Red => "red",
            Ending::Conflict => "conflict",
            Ending::Blocked => "blocked",
            Ending::SentBack => "sent back",
        }
    }

    /// Whether the run did what it was for. A conflict is the log's
    /// timing, not the model's work, but it did not land either.
    pub fn good(self) -> bool {
        matches!(self, Ending::Green | Ending::Landed | Ending::Done)
    }

    /// What it says of the model, when it says that the quest struggled.
    /// A red landing is answered by its fix-up quest, not here.
    fn struggle(self) -> Option<Struggle> {
        match self {
            Ending::Blocked => Some(Struggle::Blocked),
            Ending::SentBack => Some(Struggle::SentBack),
            _ => None,
        }
    }
}

/// One run of a quest, as its `Outcome:` line says it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    /// The model its session was launched with: one of [`MODELS`], or
    /// [`DEFAULT`].
    pub model: String,
    /// Whether that model was the one an escalation chose.
    pub escalated: bool,
    /// From its start to its end.
    pub minutes: Option<u64>,
    pub ending: Ending,
    /// What this run alone spent, when Horadric heard it.
    pub tokens: Option<u64>,
}

impl Run {
    /// Its notes line: `Outcome: sonnet escalated, 42m, green, 4.9M tokens`.
    pub fn line(&self) -> String {
        let mut out = format!("{OUTCOME} {}", self.model);
        if self.escalated {
            out.push_str(" escalated");
        }
        if let Some(m) = self.minutes {
            out.push_str(&format!(", {m}m"));
        }
        out.push_str(&format!(", {}", self.ending.word()));
        if let Some(t) = self.tokens {
            out.push_str(&format!(", {} tokens", count(t)));
        }
        out
    }

    /// The run an `Outcome:` notes line says. A line without a known
    /// ending is not one.
    pub fn read(note: &str) -> Option<Run> {
        let rest = strip_word(note, OUTCOME)?;
        let mut fields = rest.split(',').map(str::trim);
        let mut head = fields.next()?.split_whitespace();
        let model = head.next()?.to_ascii_lowercase();
        let escalated = head
            .next()
            .is_some_and(|w| w.eq_ignore_ascii_case("escalated"));
        let (mut minutes, mut ending, mut tokens) = (None, None, None);
        for f in fields {
            if let Some(n) = f.strip_suffix('m').and_then(|n| n.parse().ok()) {
                minutes = Some(n);
            } else if let Some(n) = f.strip_suffix("tokens").and_then(|n| uncount(n.trim())) {
                tokens = Some(n);
            } else if let Some(e) = Ending::ALL.into_iter().find(|e| e.word() == f) {
                ending = Some(e);
            }
        }
        Some(Run {
            model,
            escalated,
            minutes,
            ending: ending?,
            tokens,
        })
    }
}

/// A token count in a few characters: `830k`, `4.9M`.
fn count(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{}k", n / 1_000)
    } else {
        n.to_string()
    }
}

/// A count as [`count`] writes it, back to a number.
fn uncount(s: &str) -> Option<u64> {
    let (num, times) = match s.chars().last()? {
        'M' | 'm' => (&s[..s.len() - 1], 1_000_000.0),
        'k' | 'K' => (&s[..s.len() - 1], 1_000.0),
        _ => (s, 1.0),
    };
    let n: f64 = num.trim().parse().ok()?;
    (n >= 0.0).then(|| (n * times).round() as u64)
}

/// `text` with `note` put under the quest `title`, after its notes. None
/// when the quest is not in the list.
fn add_note(text: &str, title: &str, note: &str) -> Option<String> {
    let task = tasks::parse(text).into_iter().find(|t| t.title == title)?;
    Some(tasks::insert_note(
        text,
        tasks::end_of(text, task.line),
        note,
    ))
}

/// `text` with the quest `title`'s `run` written under it. None when the
/// quest is not in the list.
pub fn record(text: &str, title: &str, run: &Run) -> Option<String> {
    add_note(text, title, &run.line())
}

/// `text` with the quest `title`, put back to start over, escalated when
/// its last run struggled. None when nothing is to be written.
pub fn starts_over(text: &str, title: &str) -> Option<String> {
    let task = tasks::parse(text).into_iter().find(|t| t.title == title)?;
    let last = task.notes.iter().rev().find_map(|n| Run::read(n))?;
    let line = escalation(&task, &last.ending.struggle()?)?;
    add_note(text, title, &line)
}

/// What a change of a quest's mark means for its runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// A run its session was on ended so.
    Ended(Ending),
    /// It was put back, so a fresh session starts on it next.
    StartsOver,
}

/// The changes to runs between two reads of a list, by title, in the
/// order they are to be written. Only a quest a session held counts: one
/// that was working, or in review for a send back, with a holder. A block
/// on something the runner can check, or on another quest, only waits.
/// Done counts where `lands` is false, since no landing will say it.
pub fn changes(old: &[Task], new: &[Task], lands: bool) -> Vec<(String, Change)> {
    let mut out = Vec::new();
    for t in new {
        let Some(before) = old.iter().find(|o| o.title == t.title) else {
            continue;
        };
        if t.mark == before.mark || before.holder.is_none() {
            continue;
        }
        let plain_block = t.wait.is_none() && t.after().is_empty();
        let title = || t.title.clone();
        match (before.mark, t.mark) {
            (Mark::Working, Mark::Blocked) if plain_block => {
                out.push((title(), Change::Ended(Ending::Blocked)))
            }
            (Mark::Review, Mark::Working) => out.push((title(), Change::Ended(Ending::SentBack))),
            (Mark::Review, Mark::Open) => {
                out.push((title(), Change::Ended(Ending::SentBack)));
                out.push((title(), Change::StartsOver));
            }
            (Mark::Working | Mark::Blocked, Mark::Open) => out.push((title(), Change::StartsOver)),
            (Mark::Working | Mark::Review, Mark::Done) if !lands => {
                out.push((title(), Change::Ended(Ending::Done)))
            }
            _ => {}
        }
    }
    out
}

/// What Warriv is told of how quests went lately, by the model they ran
/// on: how many runs, how many did what they were for, the median time
/// and tokens. `quests` oldest first, the done file's then the log's.
/// None before any run was written.
pub fn summary(quests: &[Task]) -> Option<String> {
    let runs: Vec<Run> = quests
        .iter()
        .flat_map(|t| t.notes.iter().filter_map(|n| Run::read(n)))
        .collect();
    let runs = &runs[runs.len().saturating_sub(RECENT)..];
    if runs.is_empty() {
        return None;
    }
    let mut out = format!(
        "How the last {} quest runs went, by the model they ran on (good: landed green, \
         or approved where nothing lands):\n",
        runs.len()
    );
    for model in MODELS.into_iter().chain([DEFAULT]) {
        let these: Vec<&Run> = runs.iter().filter(|r| r.model == model).collect();
        if these.is_empty() {
            continue;
        }
        let good = these.iter().filter(|r| r.ending.good()).count();
        out.push_str(&format!(
            "- {model}: {} runs, {}% good",
            these.len(),
            good * 100 / these.len()
        ));
        if let Some(m) = median(these.iter().filter_map(|r| r.minutes)) {
            out.push_str(&format!(", median {m}m"));
        }
        if let Some(t) = median(these.iter().filter_map(|r| r.tokens)) {
            out.push_str(&format!(", median {} tokens", count(t)));
        }
        out.push('\n');
    }
    let escalated = runs.iter().filter(|r| r.escalated).count();
    if escalated > 0 {
        out.push_str(&format!("{escalated} of them ran after an escalation.\n"));
    }
    Some(out)
}

/// The middle value, the lower of the two middles for an even count.
fn median(values: impl Iterator<Item = u64>) -> Option<u64> {
    let mut v: Vec<u64> = values.collect();
    v.sort_unstable();
    v.get(v.len().checked_sub(1)? / 2).copied()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::parse;

    fn quest(notes: &str) -> Task {
        parse(&format!("- [/] Q @q-1\n{notes}")).remove(0)
    }

    fn notes(s: &str) -> Vec<String> {
        s.lines().map(str::to_string).collect()
    }

    fn run(model: &str, ending: Ending) -> Run {
        Run {
            model: model.into(),
            escalated: false,
            minutes: None,
            ending,
            tokens: None,
        }
    }

    #[test]
    fn a_model_steps_one_tier_up_and_stops_at_opus() {
        assert_eq!(up("haiku"), Some("sonnet"));
        assert_eq!(up("sonnet"), Some("opus"));
        assert_eq!(up("opus"), None);
        assert_eq!(up("gpt"), None);
    }

    #[test]
    fn an_escalated_line_names_the_model_it_moves_to() {
        assert_eq!(
            escalated_line("Escalated: sonnet, from haiku: blocked"),
            Some("sonnet")
        );
        assert_eq!(escalated_line("escalated:  OPUS"), Some("opus"));
        // A human's line in the same words counts too.
        assert_eq!(escalated_line("Escalated: opus please"), Some("opus"));
        assert_eq!(escalated_line("Escalated: gpt, from haiku"), None);
        assert_eq!(escalated_line("Escalated"), None);
        assert_eq!(escalated_line("Model: opus"), None);
    }

    #[test]
    fn an_escalation_stands_where_warrivs_line_does_and_the_human_still_wins() {
        let t = quest("  Warriv: Model: haiku\n  Escalated: sonnet, from haiku: blocked\n");
        assert_eq!(t.model(), Some("sonnet"));
        let t = quest(
            "  Warriv: Model: haiku\n  Escalated: sonnet, from haiku: blocked\n  \
             Warriv: Model: opus\n",
        );
        assert_eq!(t.model(), Some("opus"));
        let t = quest("  Model: haiku\n  Escalated: sonnet, from haiku: blocked\n");
        assert_eq!(t.model(), Some("haiku"));
    }

    #[test]
    fn the_escalation_chose_the_model_only_while_nothing_overrules_it() {
        assert!(chosen_by_escalation(&notes(
            "Warriv: Model: haiku\nEscalated: sonnet, from haiku: blocked"
        )));
        assert!(!chosen_by_escalation(&notes("Warriv: Model: haiku")));
        assert!(!chosen_by_escalation(&notes(
            "Escalated: sonnet, from haiku: blocked\nWarriv: Model: opus"
        )));
        assert!(!chosen_by_escalation(&notes(
            "Model: haiku\nEscalated: sonnet, from haiku: blocked"
        )));
    }

    #[test]
    fn the_launched_model_is_the_last_model_flag() {
        let args = |s: &str| s.split(' ').map(str::to_string).collect::<Vec<_>>();
        assert_eq!(
            launched_model(&args("--resume x --model haiku")),
            Some("haiku")
        );
        assert_eq!(
            launched_model(&args("--model haiku --model=opus")),
            Some("opus")
        );
        assert_eq!(launched_model(&args("--resume x")), None);
        assert_eq!(launched_model(&args("--model")), None);
    }

    #[test]
    fn a_quest_escalates_once_never_past_opus_nor_over_a_human() {
        let blocked = Struggle::Blocked;
        assert_eq!(
            escalation(&quest("  Warriv: Model: haiku\n"), &blocked).as_deref(),
            Some("Escalated: sonnet, from haiku: blocked")
        );
        assert_eq!(
            escalation(&quest("  Warriv: Model: sonnet\n"), &Struggle::SentBack).as_deref(),
            Some("Escalated: opus, from sonnet: sent back by review")
        );
        assert_eq!(
            escalation(&quest("  Warriv: Model: opus\n"), &blocked),
            None
        );
        assert_eq!(escalation(&quest("  Model: haiku\n"), &blocked), None);
        assert_eq!(escalation(&quest(""), &blocked), None);
        let once = quest("  Warriv: Model: haiku\n  Escalated: sonnet, from haiku: blocked\n");
        assert_eq!(escalation(&once, &blocked), None);
        // A fix-up is a quest of its own, so it may climb from the run's
        // model, whatever the notes say by now.
        let red = Struggle::Red("cargo test".into());
        assert_eq!(
            carried(&once, "sonnet", &red).as_deref(),
            Some("Escalated: opus, from sonnet: checks red on `cargo test`")
        );
        assert_eq!(
            carried(&once, "haiku", &red).as_deref(),
            Some("Escalated: sonnet, from haiku: checks red on `cargo test`")
        );
        assert_eq!(carried(&once, DEFAULT, &red), None);
        assert_eq!(carried(&quest("  Model: haiku\n"), "haiku", &red), None);
    }

    #[test]
    fn an_outcome_line_round_trips() {
        let r = Run {
            model: "sonnet".into(),
            escalated: true,
            minutes: Some(42),
            ending: Ending::Green,
            tokens: Some(4_930_000),
        };
        assert_eq!(
            r.line(),
            "Outcome: sonnet escalated, 42m, green, 4.9M tokens"
        );
        let back = Run::read(&r.line()).unwrap();
        assert_eq!(back.tokens, Some(4_900_000));
        assert_eq!(back.minutes, Some(42));
        assert!(back.escalated);
        let bare = Run {
            tokens: Some(830_400),
            ..run(DEFAULT, Ending::SentBack)
        };
        assert_eq!(bare.line(), "Outcome: default, sent back, 830k tokens");
        assert_eq!(Run::read(&bare.line()).unwrap().tokens, Some(830_000));
        assert_eq!(
            run("haiku", Ending::Blocked).line(),
            "Outcome: haiku, blocked"
        );
        assert_eq!(Run::read("Outcome: haiku, 5m"), None);
        assert_eq!(Run::read("Outcomes: haiku, green"), None);
    }

    #[test]
    fn a_run_is_written_under_its_quest_and_escalates_nothing_there() {
        let text = "- [!] Q @q-1: which key?\n  Warriv: Model: haiku\n- [ ] Next\n";
        let out = record(text, "Q", &run("haiku", Ending::Blocked)).unwrap();
        assert_eq!(
            out,
            "- [!] Q @q-1: which key?\n  Warriv: Model: haiku\n  \
             Outcome: haiku, blocked\n- [ ] Next\n"
        );
        assert_eq!(record(text, "Gone", &run("haiku", Ending::Green)), None);
    }

    #[test]
    fn a_quest_put_back_after_a_struggle_starts_over_one_tier_up_once() {
        let text = "- [ ] Q\n  Warriv: Model: haiku\n  Outcome: haiku, blocked\n- [ ] Next\n";
        let up = starts_over(text, "Q").unwrap();
        assert_eq!(
            up,
            "- [ ] Q\n  Warriv: Model: haiku\n  Outcome: haiku, blocked\n  \
             Escalated: sonnet, from haiku: blocked\n- [ ] Next\n"
        );
        // Put back again after another struggle: said once only.
        let again = record(&up, "Q", &run("sonnet", Ending::SentBack)).unwrap();
        assert_eq!(starts_over(&again, "Q"), None);
        // A run that went well, or none at all, is no reason.
        let fine = "- [ ] Q\n  Warriv: Model: haiku\n  Outcome: haiku, conflict\n";
        assert_eq!(starts_over(fine, "Q"), None);
        assert_eq!(starts_over("- [ ] Q\n  Warriv: Model: haiku\n", "Q"), None);
    }

    #[test]
    fn only_a_held_run_ends_and_waits_do_not() {
        let old = parse(
            "- [/] A @a-1\n- [/] B @b-1\n- [/] C @c-1\n- [?] D @d-1\n- [/] E @e-1\n\
             - [?] F @f-1\n- [ ] G\n- [ ] H\n- [!] I @i-1: why\n- [?] J @j-1\n- [!] K @k-1: why\n",
        );
        let new = parse(
            "- [!] A @a-1: which key?\n- [!] B @b-1: later {on file: out.txt}\n\
             - [!] C @c-1: needs it\n  After: A\n- [/] D @d-1\n- [x] E @e-1\n- [x] F @f-1\n\
             - [!] G: no run\n- [x] H\n- [ ] I\n- [ ] J\n- [!] K @k-1: handed on\n",
        );
        let sb = Change::Ended(Ending::SentBack);
        let held = |lands| changes(&old, &new, lands);
        let s = |t: &str, c| (t.to_string(), c);
        assert_eq!(
            held(true),
            [
                s("A", Change::Ended(Ending::Blocked)),
                s("D", sb),
                s("I", Change::StartsOver),
                s("J", sb),
                s("J", Change::StartsOver),
            ]
        );
        assert_eq!(
            held(false),
            [
                s("A", Change::Ended(Ending::Blocked)),
                s("D", sb),
                s("E", Change::Ended(Ending::Done)),
                s("F", Change::Ended(Ending::Done)),
                s("I", Change::StartsOver),
                s("J", sb),
                s("J", Change::StartsOver),
            ]
        );
    }

    #[test]
    fn the_summary_counts_the_recent_runs_by_model() {
        assert_eq!(summary(&parse("- [x] Q\n  Model: haiku\n")), None);
        let log = parse(
            "- [x] A\n  Outcome: haiku, 10m, blocked, 1.0M tokens\n  \
             Outcome: sonnet escalated, 30m, green, 3.0M tokens\n\
             - [x] B\n  Outcome: haiku, 6m, green, 500k tokens\n\
             - [x] C\n  Outcome: opus, 40m, red\n  Outcome: default, done\n",
        );
        assert_eq!(
            summary(&log).unwrap(),
            "How the last 5 quest runs went, by the model they ran on (good: landed green, \
             or approved where nothing lands):\n\
             - haiku: 2 runs, 50% good, median 6m, median 500k tokens\n\
             - sonnet: 1 runs, 100% good, median 30m, median 3.0M tokens\n\
             - opus: 1 runs, 0% good, median 40m\n\
             - default: 1 runs, 100% good\n\
             1 of them ran after an escalation.\n"
        );
    }

    #[test]
    fn the_summary_keeps_to_the_newest_runs() {
        let mut text = String::from("- [x] Old\n  Outcome: opus, red\n");
        for _ in 0..RECENT {
            text.push_str("- [x] New\n  Outcome: haiku, green\n");
        }
        let s = summary(&parse(&text)).unwrap();
        assert!(s.contains(&format!("- haiku: {RECENT} runs, 100% good")));
        assert!(!s.contains("opus"));
    }
}
