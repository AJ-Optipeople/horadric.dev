//! Warriv's look back: while it drives, the first round after 04:00 also
//! reads the last day of the chronicle for what keeps going wrong. A
//! single conflict or a red check is a fix-up quest already; the same
//! thing twice is a pattern, which wants a quest that stops it happening
//! and a rule in Warriv's memory. This finds the patterns, so the round's
//! prompt carries them rather than the chronicle itself.

use std::collections::BTreeMap;

use crate::chronicle::{Command, Happened, Outcome, Record};
use crate::merge::Failure;
use crate::tasks::one_line;
use crate::warriv::Kind;

/// The time of day the look back falls due, in seconds after local
/// midnight: after the night's work, before the human is back.
pub const AT: u64 = 4 * 60 * 60;

/// How far back a look back reads at most, in seconds.
pub const DAY: u64 = 24 * 60 * 60;

/// Something that happened more than once since the last look back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pattern {
    /// Rebases that stopped on a conflict, `times` in all, on these quests.
    Conflicts { times: usize, quests: Vec<String> },
    /// The check `check` went red `times` in all, on these quests.
    Red {
        check: String,
        times: usize,
        quests: Vec<String>,
    },
    /// Sessions that stopped without reporting done or blocked, `times` in
    /// all, on these quests.
    Silent { times: usize, quests: Vec<String> },
    /// The quest `quest` came back from review `times` times.
    SentBack { quest: String, times: usize },
}

impl Pattern {
    /// The line the round's prompt gives it.
    pub fn line(&self) -> String {
        match self {
            Pattern::Conflicts { times, quests } => format!(
                "{times} merges stopped on a conflict, on {}: quests are stepping on the same \
                 files. Find out which, and order or split the quests that touch them.",
                list(quests)
            ),
            Pattern::Red {
                check,
                times,
                quests,
            } => format!(
                "The check `{check}` went red {times} times, on {}: it is flaky or something \
                 the quests share is broken. Find out which, and make it pass steadily.",
                list(quests)
            ),
            Pattern::Silent { times, quests } => format!(
                "{times} times a session stopped without reporting done or blocked, on {}: \
                 read what they ended with, and make the quests or the prompt clearer.",
                list(quests)
            ),
            Pattern::SentBack { quest, times } => format!(
                "\"{}\" came back from review {times} times: its quest says too little of \
                 what done means, or the work is harder than it looks.",
                one_line(quest)
            ),
        }
    }
}

fn list(quests: &[String]) -> String {
    quests
        .iter()
        .map(|q| format!("\"{}\"", one_line(q)))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The start of the latest 04:00 local at or before `now`, `offset` being
/// how far the local clock is ahead of UTC.
fn last_four(now: u64, offset: i64) -> u64 {
    let local = now as i64 + offset - AT as i64;
    now.saturating_sub(local.rem_euclid(DAY as i64) as u64)
}

/// When the project's last look back was told, from the chronicle.
pub fn last(records: &[Record], project: &str) -> Option<u64> {
    records
        .iter()
        .filter(|r| r.project == project)
        .filter(|r| match &r.what {
            Happened::WarrivWoke { events, .. } => events.iter().any(|e| e.kind == Kind::LookBack),
            _ => false,
        })
        .map(|r| r.at)
        .max()
}

/// Whether a look back is due at `now`: none was told since the latest
/// 04:00. Kept in the chronicle, so a reload never brings a second one.
pub fn due(records: &[Record], project: &str, now: u64, offset: i64) -> bool {
    last(records, project).is_none_or(|at| at < last_four(now, offset))
}

/// Where a look back at `now` starts reading: the last one, but no more
/// than a [`DAY`] back.
pub fn since(records: &[Record], project: &str, now: u64) -> u64 {
    let day = now.saturating_sub(DAY);
    last(records, project).map_or(day, |at| at.max(day))
}

/// The patterns in the project's chronicle from `since` on: what happened
/// twice or more.
pub fn patterns(records: &[Record], project: &str, since: u64) -> Vec<Pattern> {
    let mine: Vec<&Record> = records.iter().filter(|r| r.project == project).collect();
    let recent = || mine.iter().copied().filter(|r| r.at >= since);
    let mut out = Vec::new();

    let mut conflicts = Tally::default();
    let mut red: BTreeMap<String, Tally> = BTreeMap::new();
    for r in recent() {
        if let Happened::NotMerged { failure, .. } = &r.what {
            match failure {
                Failure::Conflict => conflicts.add(&r.title),
                Failure::Red(check) => red.entry(check.clone()).or_default().add(&r.title),
                _ => {}
            }
        }
    }
    if conflicts.times >= 2 {
        out.push(Pattern::Conflicts {
            times: conflicts.times,
            quests: conflicts.quests,
        });
    }
    for (check, t) in red.into_iter().filter(|(_, t)| t.times >= 2) {
        out.push(Pattern::Red {
            check,
            times: t.times,
            quests: t.quests,
        });
    }

    let mut silent = Tally::default();
    for r in recent() {
        if let Happened::WarrivWoke { events, .. } = &r.what {
            for e in events.iter().filter(|e| e.kind == Kind::Asks) {
                silent.add(&e.quest);
            }
        }
    }
    if silent.times >= 2 {
        out.push(Pattern::Silent {
            times: silent.times,
            quests: silent.quests,
        });
    }

    for (quest, times) in sent_back(&mine, since) {
        out.push(Pattern::SentBack { quest, times });
    }
    out
}

/// How often each quest came back from review, over its whole story, for
/// those that came back since `since` and twice or more. A review sends a
/// quest back with `quest fix`, which Warriv's reviewer runs; a session
/// that takes a quest again after review is one too, which is how the
/// human sends it back. A reviewer's fix to a live session is both, so a
/// quest counts the larger of the two.
fn sent_back(mine: &[&Record], since: u64) -> Vec<(String, usize)> {
    // Per title: fixes, retakes, and when it last came back.
    let mut seen: BTreeMap<&str, (usize, usize, u64)> = BTreeMap::new();
    let mut in_review: BTreeMap<&str, bool> = BTreeMap::new();
    for r in mine {
        let title = r.title.as_str();
        match &r.what {
            Happened::WarrivRan {
                command: Command::Fix,
                ..
            } => {
                let e = seen.entry(title).or_default();
                e.0 += 1;
                e.2 = e.2.max(r.at);
            }
            Happened::Marked { mark, .. } => {
                in_review.insert(title, *mark == Outcome::Review);
            }
            Happened::Accepted { .. } => {
                let reviewed = in_review.insert(title, false) == Some(true);
                if reviewed {
                    let e = seen.entry(title).or_default();
                    e.1 += 1;
                    e.2 = e.2.max(r.at);
                }
            }
            _ => {}
        }
    }
    seen.into_iter()
        .filter(|(t, _)| !t.is_empty())
        .map(|(t, (fixes, retakes, at))| (t, fixes.max(retakes), at))
        .filter(|&(_, times, at)| times >= 2 && at >= since)
        .map(|(t, times, _)| (t.to_string(), times))
        .collect()
}

/// How often something happened, and on which quests, in order.
#[derive(Default)]
struct Tally {
    times: usize,
    quests: Vec<String>,
}

impl Tally {
    fn add(&mut self, quest: &str) {
        self.times += 1;
        if !quest.is_empty() && !self.quests.iter().any(|q| q == quest) {
            self.quests.push(quest.to_string());
        }
    }
}

/// The day in a line, for the round's prompt beside the patterns: how
/// many quests were done and merged, merges that failed, and Warriv's
/// wakes.
pub fn day(records: &[Record], project: &str, since: u64) -> String {
    let recent: Vec<&Record> = records
        .iter()
        .filter(|r| r.project == project && r.at >= since)
        .collect();
    let count = |f: &dyn Fn(&Happened) -> bool| recent.iter().filter(|r| f(&r.what)).count();
    let done = count(&|h| {
        matches!(
            h,
            Happened::Marked {
                mark: Outcome::Done,
                ..
            }
        )
    });
    let merged = count(&|h| matches!(h, Happened::Merged { .. }));
    let failed = count(&|h| matches!(h, Happened::NotMerged { .. }));
    let mut wakes: Vec<&str> = recent
        .iter()
        .filter_map(|r| match &r.what {
            Happened::WarrivWoke { wake, .. } => Some(wake.as_str()),
            _ => None,
        })
        .collect();
    wakes.sort();
    wakes.dedup();
    format!(
        "{} completed, {merged} merged, {failed} did not merge by itself, {}",
        plural(done, "quest"),
        plural(wakes.len(), "Warriv session")
    )
}

fn plural(n: usize, word: &str) -> String {
    match n {
        1 => format!("1 {word}"),
        n => format!("{n} {word}s"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chronicle::Woken;

    fn rec(at: u64, title: &str, what: Happened) -> Record {
        Record {
            at,
            project: "p".into(),
            quest: format!("q-{title}"),
            title: title.into(),
            what,
        }
    }

    fn not_merged(at: u64, title: &str, failure: Failure) -> Record {
        rec(
            at,
            title,
            Happened::NotMerged {
                branch: title.into(),
                failure,
            },
        )
    }

    fn woke(at: u64, kinds: &[(Kind, &str)]) -> Record {
        rec(
            at,
            "",
            Happened::WarrivWoke {
                wake: format!("warriv-{at}"),
                conversation: String::new(),
                events: kinds
                    .iter()
                    .map(|(k, q)| Woken {
                        kind: *k,
                        quest: q.to_string(),
                    })
                    .collect(),
            },
        )
    }

    fn marked(at: u64, title: &str, mark: Outcome) -> Record {
        rec(
            at,
            title,
            Happened::Marked {
                mark,
                reason: String::new(),
            },
        )
    }

    fn accepted(at: u64, title: &str) -> Record {
        rec(at, title, Happened::Accepted { notes: Vec::new() })
    }

    fn fix(at: u64, title: &str) -> Record {
        rec(
            at,
            title,
            Happened::WarrivRan {
                wake: "warriv-review".into(),
                conversation: String::new(),
                command: Command::Fix,
                text: "no tests".into(),
            },
        )
    }

    #[test]
    fn due_once_a_day_from_four_in_the_morning() {
        // Local is UTC+2; 2026-10-05 00:00 UTC is 02:00 local.
        let midnight = 1_791_158_400;
        let offset = 2 * 3600;
        let at = |h: u64| midnight + h * 3600 - 2 * 3600;
        assert!(due(&[], "p", at(3), offset));
        let looked = [woke(at(4) + 60, &[(Kind::Round, ""), (Kind::LookBack, "")])];
        assert!(!due(&looked, "p", at(5), offset));
        assert!(!due(&looked, "p", at(27), offset), "03:00 the next day");
        assert!(due(&looked, "p", at(28), offset), "04:00 the next day");
        assert!(due(&looked, "other", at(5), offset));
        // A round alone is no look back.
        let round = [woke(at(5), &[(Kind::Round, "")])];
        assert!(due(&round, "p", at(6), offset));
    }

    #[test]
    fn reads_from_the_last_look_back_at_most_a_day() {
        assert_eq!(since(&[], "p", 100_000), 100_000 - DAY);
        let looked = [woke(90_000, &[(Kind::LookBack, "")])];
        assert_eq!(since(&looked, "p", 100_000), 90_000);
        assert_eq!(since(&looked, "p", 200_000), 200_000 - DAY);
    }

    #[test]
    fn one_of_anything_is_no_pattern() {
        let records = [
            not_merged(10, "A", Failure::Conflict),
            not_merged(11, "B", Failure::Red("cargo test".into())),
            woke(12, &[(Kind::Asks, "C")]),
            fix(13, "D"),
        ];
        assert!(patterns(&records, "p", 0).is_empty());
    }

    #[test]
    fn repeated_conflicts_and_a_check_red_twice() {
        let records = [
            not_merged(10, "A", Failure::Conflict),
            not_merged(11, "B", Failure::Conflict),
            not_merged(12, "A", Failure::Conflict),
            not_merged(13, "C", Failure::Red("cargo test".into())),
            not_merged(14, "D", Failure::Red("cargo test".into())),
            not_merged(15, "E", Failure::Red("cargo clippy".into())),
            not_merged(16, "F", Failure::Refused),
        ];
        assert_eq!(
            patterns(&records, "p", 0),
            vec![
                Pattern::Conflicts {
                    times: 3,
                    quests: vec!["A".into(), "B".into()]
                },
                Pattern::Red {
                    check: "cargo test".into(),
                    times: 2,
                    quests: vec!["C".into(), "D".into()]
                },
            ]
        );
    }

    #[test]
    fn only_since_and_only_this_project() {
        let mut other = not_merged(20, "B", Failure::Conflict);
        other.project = "q".into();
        let records = [
            not_merged(5, "A", Failure::Conflict),
            not_merged(20, "A", Failure::Conflict),
            other,
        ];
        assert!(patterns(&records, "p", 10).is_empty());
        assert_eq!(patterns(&records, "p", 0).len(), 1);
    }

    #[test]
    fn sessions_that_stopped_without_reporting() {
        let records = [
            woke(10, &[(Kind::Asks, "A"), (Kind::Round, "")]),
            woke(20, &[(Kind::Asks, "B")]),
        ];
        assert_eq!(
            patterns(&records, "p", 0),
            vec![Pattern::Silent {
                times: 2,
                quests: vec!["A".into(), "B".into()]
            }]
        );
    }

    #[test]
    fn a_quest_sent_back_twice() {
        // The reviewer's fixes, with a live session taking it again each time.
        let records = [
            marked(10, "A", Outcome::Review),
            fix(11, "A"),
            accepted(12, "A"),
            marked(13, "A", Outcome::Review),
            fix(14, "A"),
            accepted(15, "A"),
            // Taken again after a review, by the human, once only.
            marked(16, "B", Outcome::Review),
            accepted(17, "B"),
            // Blocked and taken again is no review.
            marked(18, "C", Outcome::Blocked),
            accepted(19, "C"),
            marked(20, "C", Outcome::Blocked),
            accepted(21, "C"),
        ];
        assert_eq!(
            patterns(&records, "p", 0),
            vec![Pattern::SentBack {
                quest: "A".into(),
                times: 2
            }]
        );
        // Sent back before and once since still counts both.
        assert_eq!(patterns(&records, "p", 13).len(), 1);
        assert!(patterns(&records, "p", 16).is_empty());
    }

    #[test]
    fn retakes_by_the_human_count_too() {
        let records = [
            marked(10, "A", Outcome::Review),
            accepted(11, "A"),
            marked(12, "A", Outcome::Review),
            accepted(13, "A"),
        ];
        assert_eq!(
            patterns(&records, "p", 0),
            vec![Pattern::SentBack {
                quest: "A".into(),
                times: 2
            }]
        );
    }

    #[test]
    fn each_pattern_reads_as_a_line() {
        let red = Pattern::Red {
            check: "cargo test".into(),
            times: 2,
            quests: vec!["C".into(), "D".into()],
        };
        assert_eq!(
            red.line(),
            "The check `cargo test` went red 2 times, on \"C\", \"D\": it is flaky or something \
             the quests share is broken. Find out which, and make it pass steadily."
        );
        let back = Pattern::SentBack {
            quest: "A".into(),
            times: 2,
        };
        assert!(back
            .line()
            .starts_with("\"A\" came back from review 2 times"));
    }

    #[test]
    fn the_day_in_a_line() {
        let records = [
            marked(10, "A", Outcome::Done),
            rec(
                11,
                "A",
                Happened::Merged {
                    branch: "a".into(),
                    checked: String::new(),
                },
            ),
            not_merged(12, "B", Failure::Conflict),
            woke(13, &[(Kind::Round, "")]),
            woke(14, &[(Kind::Asks, "C")]),
        ];
        assert_eq!(
            day(&records, "p", 0),
            "1 quest completed, 1 merged, 1 did not merge by itself, 2 Warriv sessions"
        );
    }
}
