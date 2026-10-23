//! How a quest's model follows how its runs went: one tier up when it
//! struggles, and a line per run that says how it ended, so Warriv picks
//! the next quest's model from what happened rather than a guess.
//!
//! ```text
//! - [!] Wire the new API in @wire-api-51500: which endpoint?
//!   Warriv: Model: haiku
//!   Outcome: haiku, 14m, blocked, 820k tokens
//!   Escalated: sonnet, from haiku: blocked
//! ```
//!
//! An `Escalated:` line names the model the quest starts on next. It
//! stands where a `Warriv: Model:` line would, below a human's `Model:`
//! line, which always wins: see [`crate::tasks::Task::model`]. A quest is
//! escalated once at most, and never past opus. A fix-up quest filed for a
//! quest that landed red or was sent back is a quest of its own, so it may
//! start one tier above the quest it fixes.
//!
//! An `Outcome:` line is written as a run ends: landed (green when the
//! checks ran), red, a conflict, blocked, sent back by review, or done
//! where nothing lands. They travel with the quest into the done file, and
//! [`summary`] reads them back for Warriv.

use crate::tasks::{self, Mark, Task, MODELS};

/// The word that starts an escalation's notes line.
pub const ESCALATED: &str = "Escalated:";

/// The word that starts a run's notes line.
pub const OUTCOME: &str = "Outcome:";

/// At most this many runs, the newest, go into Warriv's summary, so it
/// tells how quests go lately and stays the same size however long the
/// project has run.
pub const RECENT: usize = 100;

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

/// The step up for `task`'s model, as `(from, to)`: None when a human
/// chose its model, it names none (the default model is not known here),
/// or it is on opus already.
fn step(task: &Task) -> Option<(&'static str, &'static str)> {
    if task.notes.iter().any(|n| tasks::model_line(n).is_some()) {
        return None;
    }
    let from = task.model()?;
    Some((from, up(from)?))
}

/// The line that moves `task` one tier up after `why`, the next time it
/// starts. Once per quest, so a quest that keeps struggling never climbs
/// on its own and its notes say it once.
pub fn escalation(task: &Task, why: &Struggle) -> Option<String> {
    if task.notes.iter().any(|n| escalated_line(n).is_some()) {
        return None;
    }
    carried(task, why)
}

/// The line that starts a fix-up quest for `task` one tier above it,
/// after `why`. The fix-up is a quest of its own, so `task` having been
/// escalated already does not stop it.
pub fn carried(task: &Task, why: &Struggle) -> Option<String> {
    let (from, to) = step(task)?;
    Some(format!("{ESCALATED} {to}, from {from}: {}", why.says()))
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
    /// One of [`MODELS`], or `default` when the quest named none.
    pub model: String,
    /// Whether it ran on a model an escalation chose.
    pub escalated: bool,
    /// From its start to its end.
    pub minutes: Option<u64>,
    pub ending: Ending,
    /// What its conversations spent, when Horadric heard it.
    pub tokens: Option<u64>,
}

/// What a quest's run is called when it named no model.
pub const DEFAULT: &str = "default";

impl Run {
    /// The run `task` just had, as its notes stand before anything is
    /// written for how it ended.
    pub fn of(task: &Task, ending: Ending, minutes: Option<u64>, tokens: Option<u64>) -> Self {
        let model = task.model();
        // Escalated only when the escalation is what chose the model: a
        // human's line or a later Warriv line beats it.
        let escalated = model.is_some()
            && task
                .notes
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
            && !task.notes.iter().any(|n| tasks::model_line(n).is_some());
        Run {
            model: model.unwrap_or(DEFAULT).to_string(),
            escalated,
            minutes,
            ending,
            tokens,
        }
    }

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

/// `text` with the quest `title`'s run written under it as it ended, and,
/// when that ending is a struggle, the line that starts it one tier up
/// next time. None when the quest is not in the list.
pub fn record(
    text: &str,
    title: &str,
    ending: Ending,
    minutes: Option<u64>,
    tokens: Option<u64>,
) -> Option<String> {
    let list = tasks::parse(text);
    let task = list.iter().find(|t| t.title == title)?;
    let run = Run::of(task, ending, minutes, tokens);
    let mut lines = vec![run.line()];
    if let Some(up) = ending.struggle().and_then(|s| escalation(task, &s)) {
        lines.push(up);
    }
    let end = tasks::end_of(text, task.line);
    let out = (end..).zip(lines).fold(text.to_string(), |out, (at, l)| {
        tasks::insert_note(&out, at, &l)
    });
    Some(out)
}

/// How each quest that changed between two reads of a list ended a run,
/// by title: blocked on the human, sent back from review, or, where
/// `lands` is false so no landing will say it, done. A quest blocked on
/// something the runner can check, or on another quest, only waits, so
/// it is not counted.
pub fn endings(old: &[Task], new: &[Task], lands: bool) -> Vec<(String, Ending)> {
    let mut out = Vec::new();
    for t in new {
        let Some(before) = old.iter().find(|o| o.title == t.title) else {
            continue;
        };
        if t.mark == before.mark {
            continue;
        }
        let ending = match (before.mark, t.mark) {
            (_, Mark::Blocked) if t.wait.is_none() && t.after().is_empty() => Ending::Blocked,
            (Mark::Review, Mark::Working | Mark::Open) => Ending::SentBack,
            (_, Mark::Done) if !lands => Ending::Done,
            _ => continue,
        };
        out.push((t.title.clone(), ending));
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
        // A fix-up is a quest of its own, so it may climb from there.
        assert_eq!(
            carried(&once, &Struggle::Red("cargo test".into())).as_deref(),
            Some("Escalated: opus, from sonnet: checks red on `cargo test`")
        );
    }

    #[test]
    fn an_outcome_line_round_trips() {
        let run = Run {
            model: "sonnet".into(),
            escalated: true,
            minutes: Some(42),
            ending: Ending::Green,
            tokens: Some(4_930_000),
        };
        assert_eq!(
            run.line(),
            "Outcome: sonnet escalated, 42m, green, 4.9M tokens"
        );
        let back = Run::read(&run.line()).unwrap();
        assert_eq!(back.tokens, Some(4_900_000));
        assert_eq!(back.minutes, Some(42));
        assert!(back.escalated);
        let bare = Run {
            model: DEFAULT.into(),
            escalated: false,
            minutes: None,
            ending: Ending::SentBack,
            tokens: Some(830_400),
        };
        assert_eq!(bare.line(), "Outcome: default, sent back, 830k tokens");
        assert_eq!(Run::read(&bare.line()).unwrap().tokens, Some(830_000));
        assert_eq!(Run::read("Outcome: haiku, 5m"), None);
        assert_eq!(Run::read("Outcomes: haiku, green"), None);
    }

    #[test]
    fn a_run_is_escalated_only_while_the_escalation_chose_its_model() {
        let r = |notes| Run::of(&quest(notes), Ending::Green, None, None);
        let up = "  Warriv: Model: haiku\n  Escalated: sonnet, from haiku: blocked\n";
        assert!(r(up).escalated);
        assert_eq!(r(up).model, "sonnet");
        assert!(!r("  Warriv: Model: haiku\n").escalated);
        let overruled = "  Escalated: sonnet, from haiku: blocked\n  Warriv: Model: opus\n";
        assert!(!r(overruled).escalated);
        assert_eq!(r("").model, "default");
    }

    #[test]
    fn a_struggling_run_is_written_then_escalated_under_its_quest() {
        let text = "- [!] Q @q-1: which key?\n  Warriv: Model: haiku\n- [ ] Next\n";
        let out = record(text, "Q", Ending::Blocked, Some(14), Some(820_000)).unwrap();
        assert_eq!(
            out,
            "- [!] Q @q-1: which key?\n  Warriv: Model: haiku\n  \
             Outcome: haiku, 14m, blocked, 820k tokens\n  \
             Escalated: sonnet, from haiku: blocked\n- [ ] Next\n"
        );
        // Blocked again on sonnet: written, not escalated a second time.
        let again = record(&out, "Q", Ending::Blocked, None, None).unwrap();
        assert!(again.contains("Outcome: sonnet escalated, blocked\n"));
        assert_eq!(again.matches("Escalated:").count(), 1);
        // A good ending escalates nothing.
        let green = record(text, "Q", Ending::Green, None, None).unwrap();
        assert!(!green.contains("Escalated:"));
        assert_eq!(record(text, "Gone", Ending::Green, None, None), None);
    }

    #[test]
    fn blocks_on_the_human_and_send_backs_end_a_run_and_waits_do_not() {
        let old = parse(
            "- [/] A @a-1\n- [/] B @b-1\n- [/] C @c-1\n- [?] D @d-1\n- [/] E @e-1\n- [?] F @f-1\n",
        );
        let new = parse(
            "- [!] A @a-1: which key?\n- [!] B @b-1: later {on file: out.txt}\n\
             - [!] C @c-1: needs it\n  After: A\n- [/] D @d-1\n- [x] E @e-1\n- [x] F @f-1\n",
        );
        assert_eq!(
            endings(&old, &new, true),
            [
                ("A".to_string(), Ending::Blocked),
                ("D".to_string(), Ending::SentBack)
            ]
        );
        assert_eq!(
            endings(&old, &new, false),
            [
                ("A".to_string(), Ending::Blocked),
                ("D".to_string(), Ending::SentBack),
                ("E".to_string(), Ending::Done),
                ("F".to_string(), Ending::Done),
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
