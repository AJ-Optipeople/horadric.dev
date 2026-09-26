//! What happened while nobody watched, for the catch-up ("Stay a while and
//! listen"). The registry only knows the present, so the app appends a line
//! to `journal.jsonl` for each thing worth telling, and this turns the lines
//! since a time into what the catch-up says.
//!
//! Only the last word on a session or an item counts: a session that waited,
//! was answered and finished its turn is one line, not three. What asks
//! something of you (waiting, a review, an unread turn, a block) is told
//! only while it still holds, which the caller knows and this does not.

use serde::{Deserialize, Serialize};

use crate::tasks::{Mark, Task};
use crate::usage::format_until;

/// The file, beside `state.json`.
pub const FILE: &str = "journal.jsonl";

/// How long lines are kept, in seconds.
pub const KEEP: u64 = 7 * 86_400;

/// One thing worth telling.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// When, in Unix seconds.
    pub at: u64,
    /// The Horadric id of the session it is about, empty for none.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub session: String,
    /// The session's name as its tile showed it then.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// The project key, empty for what is the account's.
    #[serde(default)]
    pub project: String,
    #[serde(flatten)]
    pub what: What,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "what", rename_all = "snake_case")]
pub enum What {
    /// The session started waiting on you, and why.
    Waiting {
        #[serde(default)]
        line: String,
    },
    /// The session finished its turn, and its last message.
    Done {
        #[serde(default)]
        line: String,
    },
    /// The session's process ended.
    Ended,
    /// A session took a task item.
    Started { title: String },
    /// An item's agent says it is done and wants a look.
    Review { title: String },
    Blocked {
        title: String,
        #[serde(default)]
        reason: String,
    },
    /// An item was marked done, and the commits made under it, newest
    /// first.
    Finished {
        title: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        commits: Vec<Commit>,
    },
    /// A finished item's branch went into the main tree.
    Merged { branch: String, title: String },
    /// A usage limit ran out, until then in Unix seconds.
    Limit { until: u64 },
}

impl Entry {
    /// The line the file holds for it, newline included.
    pub fn line(&self) -> String {
        let mut s = serde_json::to_string(self).unwrap_or_default();
        s.push('\n');
        s
    }

    /// The task item it is about, if it is about one.
    fn item(&self) -> Option<&str> {
        match &self.what {
            What::Started { title }
            | What::Review { title }
            | What::Blocked { title, .. }
            | What::Finished { title, .. } => Some(title),
            _ => None,
        }
    }

    fn phase(&self) -> bool {
        matches!(
            self.what,
            What::Waiting { .. } | What::Done { .. } | What::Ended
        )
    }
}

/// Every line of the file that reads. A torn last line, from a crash mid
/// write, is left out rather than losing the rest.
pub fn parse(text: &str) -> Vec<Entry> {
    text.lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

/// The file without the lines older than [`KEEP`], or None when none are.
pub fn trimmed(text: &str, now: u64) -> Option<String> {
    let oldest = now.saturating_sub(KEEP);
    let entries = parse(text);
    let stale = entries.iter().any(|e| e.at < oldest);
    let torn = text.lines().filter(|l| !l.trim().is_empty()).count() != entries.len();
    (stale || torn).then(|| {
        entries
            .iter()
            .filter(|e| e.at >= oldest)
            .map(Entry::line)
            .collect()
    })
}

/// A commit, as `git log` told it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Commit {
    /// The short hash.
    pub hash: String,
    pub subject: String,
}

/// The format [`commits`] reads: `git log --format=` this.
pub const LOG_FORMAT: &str = "%h%x09%s";

/// The commits in what `git log --format=`[`LOG_FORMAT`] printed.
pub fn commits(log: &str) -> Vec<Commit> {
    log.lines()
        .filter_map(|l| {
            let (hash, subject) = l.split_once('\t')?;
            let hash = hash.trim();
            (!hash.is_empty()).then(|| Commit {
                hash: hash.to_string(),
                subject: subject.trim().to_string(),
            })
        })
        .collect()
}

/// When the item `title` of `project` was last taken, which is where its
/// commits begin. None when the journal never heard it taken.
pub fn started_at(entries: &[Entry], project: &str, title: &str) -> Option<u64> {
    entries
        .iter()
        .rev()
        .find(|e| {
            e.project == project && matches!(&e.what, What::Started { title: t } if t == title)
        })
        .map(|e| e.at)
}

/// What a finished item's line says of its commits: the one, or how many
/// and their subjects, newest first.
fn commits_detail(commits: &[Commit]) -> String {
    match commits {
        [] => String::new(),
        [c] => format!("{} {}", c.hash, c.subject),
        _ => format!(
            "{} commits: {}",
            commits.len(),
            commits
                .iter()
                .map(|c| c.subject.as_str())
                .collect::<Vec<_>>()
                .join(" \u{b7} ")
        ),
    }
}

/// The lines for what changed between two reads of a project's task list:
/// an item taken, sent for review, blocked or finished. Items are matched
/// by title, since lines move as the list is edited.
pub fn marks(project: &str, old: &[Task], new: &[Task], now: u64) -> Vec<Entry> {
    new.iter()
        .filter_map(|t| {
            let before = old.iter().find(|o| o.title == t.title)?;
            if before.mark == t.mark {
                return None;
            }
            let title = t.title.clone();
            let what = match t.mark {
                Mark::Working => What::Started { title },
                Mark::Review => What::Review { title },
                Mark::Blocked => What::Blocked {
                    title,
                    reason: t.reason.clone().unwrap_or_default(),
                },
                Mark::Done => What::Finished {
                    title,
                    commits: Vec::new(),
                },
                Mark::Open => return None,
            };
            Some(Entry {
                at: now,
                session: t
                    .holder
                    .clone()
                    .or_else(|| before.holder.clone())
                    .unwrap_or_default(),
                name: String::new(),
                project: project.to_string(),
                what,
            })
        })
        .collect()
}

/// Where a line goes in the catch-up, in the order you act on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Section {
    Waiting,
    Review,
    Unread,
    Blocked,
    /// What simply happened and asks nothing.
    Happened,
}

/// One line of the catch-up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub section: Section,
    pub at: u64,
    /// The session a click on it shows, empty for none.
    pub session: String,
    pub text: String,
    /// Fainter, after the text.
    pub detail: String,
}

/// A project's lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    /// The project key, empty for the account's lines.
    pub project: String,
    pub lines: Vec<Line>,
}

/// What happened since `since`, grouped by project. `still` says whether
/// what an entry asks of you still holds: the session still waits, the
/// turn is still unread, the item is still in review or blocked. Empty
/// when nothing happened worth telling.
pub fn summary(
    entries: &[Entry],
    since: u64,
    now: u64,
    still: impl Fn(&Entry) -> bool,
) -> Vec<Group> {
    let recent: Vec<&Entry> = entries.iter().filter(|e| e.at >= since).collect();
    let last = |i: usize, same: &dyn Fn(&Entry) -> bool| !recent[i + 1..].iter().any(|e| same(e));
    let mut lines: Vec<(String, Line)> = Vec::new();
    for (i, e) in recent.iter().enumerate() {
        let line = if e.phase() {
            if !last(i, &|o| o.phase() && o.session == e.session) {
                continue;
            }
            phase_line(e, &still)
        } else if let Some(title) = e.item() {
            if !last(i, &|o| o.project == e.project && o.item() == Some(title)) {
                continue;
            }
            item_line(e, title, &still)
        } else {
            match &e.what {
                What::Merged { branch, title } => Some(Line {
                    section: Section::Happened,
                    at: e.at,
                    session: String::new(),
                    text: format!("Merged {branch}"),
                    detail: crate::tasks::one_line(title),
                }),
                What::Limit { until } => {
                    if !last(i, &|o| o.what == e.what) {
                        continue;
                    }
                    let detail = if *until <= now {
                        format!("reset {} ago", format_until(now - until))
                    } else {
                        format!("resets in {}", format_until(until - now))
                    };
                    Some(Line {
                        section: Section::Happened,
                        at: e.at,
                        session: String::new(),
                        text: "Usage limit reached".to_string(),
                        detail,
                    })
                }
                _ => None,
            }
        };
        if let Some(l) = line {
            lines.push((e.project.clone(), l));
        }
    }
    let mut groups: Vec<Group> = Vec::new();
    for (project, line) in lines {
        match groups.iter_mut().find(|g| g.project == project) {
            Some(g) => g.lines.push(line),
            None => groups.push(Group {
                project,
                lines: vec![line],
            }),
        }
    }
    for g in &mut groups {
        g.lines.sort_by_key(|l| (l.section, l.at));
    }
    // The project with the most pressing line first, and of two equally
    // pressing, the one that has waited longer.
    groups.sort_by_key(|g| g.lines.first().map(|l| (l.section, l.at)));
    groups
}

fn phase_line(e: &Entry, still: &impl Fn(&Entry) -> bool) -> Option<Line> {
    let (section, detail) = match &e.what {
        What::Waiting { line } if still(e) => (Section::Waiting, line.clone()),
        What::Done { line } if still(e) => (Section::Unread, line.clone()),
        What::Ended => (Section::Happened, "ended".to_string()),
        _ => return None,
    };
    Some(Line {
        section,
        at: e.at,
        session: e.session.clone(),
        text: e.name.clone(),
        detail: crate::tasks::one_line(&detail),
    })
}

fn item_line(e: &Entry, title: &str, still: &impl Fn(&Entry) -> bool) -> Option<Line> {
    let (section, text, detail) = match &e.what {
        What::Review { .. } if still(e) => (Section::Review, title.to_string(), String::new()),
        What::Blocked { reason, .. } if still(e) => {
            (Section::Blocked, title.to_string(), reason.clone())
        }
        What::Finished { commits, .. } => (
            Section::Happened,
            format!("Finished: {title}"),
            commits_detail(commits),
        ),
        What::Started { .. } => (
            Section::Happened,
            format!("Started: {title}"),
            String::new(),
        ),
        _ => return None,
    };
    Some(Line {
        section,
        at: e.at,
        session: e.session.clone(),
        text: crate::tasks::one_line(&text),
        detail: crate::tasks::one_line(&detail),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(at: u64, session: &str, project: &str, what: What) -> Entry {
        Entry {
            at,
            session: session.to_string(),
            name: session.to_uppercase(),
            project: project.to_string(),
            what,
        }
    }

    fn waiting(line: &str) -> What {
        What::Waiting {
            line: line.to_string(),
        }
    }

    fn done(line: &str) -> What {
        What::Done {
            line: line.to_string(),
        }
    }

    fn texts(groups: &[Group]) -> Vec<(String, Vec<(Section, String)>)> {
        groups
            .iter()
            .map(|g| {
                (
                    g.project.clone(),
                    g.lines
                        .iter()
                        .map(|l| (l.section, l.text.clone()))
                        .collect(),
                )
            })
            .collect()
    }

    #[test]
    fn a_line_reads_back_and_a_torn_one_is_left_out() {
        let e = entry(5, "a", "c:/p", waiting("allow Bash?"));
        let text = format!("{}{{\"at\":6,\"wha", e.line());
        assert_eq!(parse(&text), vec![e]);
        let limit = entry(7, "", "", What::Limit { until: 99 });
        assert!(!limit.line().contains("session"));
        assert_eq!(parse(&limit.line()), vec![limit]);
    }

    #[test]
    fn trimming_drops_the_lines_older_than_a_week() {
        let now = KEEP + 100;
        let old = entry(50, "a", "p", What::Ended);
        let new = entry(150, "b", "p", What::Ended);
        let text = old.line() + &new.line();
        assert_eq!(trimmed(&text, now), Some(new.line()));
        assert_eq!(trimmed(&new.line(), now), None);
        assert_eq!(trimmed(&(new.line() + "{torn"), now), Some(new.line()));
    }

    #[test]
    fn only_a_sessions_last_word_counts() {
        let entries = [
            entry(10, "a", "p", waiting("allow Bash?")),
            entry(20, "a", "p", done("All green.")),
        ];
        let groups = summary(&entries, 0, 30, |_| true);
        assert_eq!(
            texts(&groups),
            [("p".into(), vec![(Section::Unread, "A".into())])]
        );
        assert_eq!(groups[0].lines[0].detail, "All green.");
    }

    #[test]
    fn what_no_longer_holds_is_not_told() {
        let entries = [
            entry(10, "a", "p", waiting("allow Bash?")),
            entry(20, "b", "p", done("Done.")),
        ];
        assert!(summary(&entries, 0, 30, |_| false).is_empty());
        let only_b = summary(&entries, 0, 30, |e| e.session == "b");
        assert_eq!(
            texts(&only_b),
            [("p".into(), vec![(Section::Unread, "B".into())])]
        );
    }

    #[test]
    fn lines_before_the_time_are_left_out() {
        let entries = [
            entry(10, "a", "p", What::Ended),
            entry(20, "b", "p", What::Ended),
        ];
        let groups = summary(&entries, 15, 30, |_| true);
        assert_eq!(
            texts(&groups),
            [("p".into(), vec![(Section::Happened, "B".into())])]
        );
    }

    #[test]
    fn lines_go_in_the_order_you_act_on_them_and_waiting_oldest_first() {
        let item = |t: &str| t.to_string();
        let entries = [
            entry(1, "a", "p", What::Ended),
            entry(2, "b", "p", done("ok")),
            entry(3, "c", "p", waiting("question")),
            entry(
                4,
                "d",
                "p",
                What::Blocked {
                    title: item("Four"),
                    reason: "keys".into(),
                },
            ),
            entry(
                5,
                "e",
                "p",
                What::Review {
                    title: item("Five"),
                },
            ),
            entry(6, "f", "p", waiting("permission")),
        ];
        let groups = summary(&entries, 0, 10, |_| true);
        assert_eq!(
            texts(&groups),
            [(
                "p".into(),
                vec![
                    (Section::Waiting, "C".into()),
                    (Section::Waiting, "F".into()),
                    (Section::Review, "Five".into()),
                    (Section::Unread, "B".into()),
                    (Section::Blocked, "Four".into()),
                    (Section::Happened, "A".into()),
                ]
            )]
        );
        assert_eq!(groups[0].lines[4].detail, "keys");
    }

    #[test]
    fn an_item_is_told_once_as_it_ended_up() {
        let t = || "Add the thing".to_string();
        let entries = [
            entry(1, "a", "p", What::Started { title: t() }),
            entry(2, "a", "p", What::Review { title: t() }),
            entry(
                3,
                "a",
                "p",
                What::Finished {
                    title: t(),
                    commits: Vec::new(),
                },
            ),
            entry(
                4,
                "b",
                "p",
                What::Started {
                    title: "Other".into(),
                },
            ),
        ];
        let groups = summary(&entries, 0, 10, |_| true);
        assert_eq!(
            texts(&groups),
            [(
                "p".into(),
                vec![
                    (Section::Happened, "Finished: Add the thing".into()),
                    (Section::Happened, "Started: Other".into()),
                ]
            )]
        );
    }

    #[test]
    fn the_same_title_in_two_projects_is_two_items() {
        let entries = [
            entry(
                1,
                "a",
                "p",
                What::Finished {
                    title: "Ship".into(),
                    commits: Vec::new(),
                },
            ),
            entry(
                2,
                "b",
                "q",
                What::Finished {
                    title: "Ship".into(),
                    commits: Vec::new(),
                },
            ),
        ];
        assert_eq!(summary(&entries, 0, 10, |_| true).len(), 2);
    }

    #[test]
    fn the_most_pressing_project_comes_first() {
        let entries = [
            entry(1, "a", "calm", What::Ended),
            entry(2, "b", "urgent", waiting("question")),
        ];
        let groups = summary(&entries, 0, 10, |_| true);
        let order: Vec<&str> = groups.iter().map(|g| g.project.as_str()).collect();
        assert_eq!(order, ["urgent", "calm"]);
    }

    #[test]
    fn a_limit_says_when_it_reset_or_resets() {
        let entries = [
            entry(1, "", "", What::Limit { until: 3601 }),
            entry(2, "", "", What::Limit { until: 3601 }),
        ];
        let before = summary(&entries, 0, 1, |_| true);
        assert_eq!(before[0].lines.len(), 1);
        assert_eq!(before[0].lines[0].detail, "resets in 1 h 00 min");
        let after = summary(&entries, 0, 3601 + 120, |_| true);
        assert_eq!(after[0].lines[0].detail, "reset 2 min ago");
    }

    #[test]
    fn a_merge_names_the_branch_and_the_item() {
        let entries = [entry(
            1,
            "",
            "p",
            What::Merged {
                branch: "add-x".into(),
                title: "Add x".into(),
            },
        )];
        let l = &summary(&entries, 0, 10, |_| true)[0].lines[0];
        assert_eq!(
            (l.text.as_str(), l.detail.as_str()),
            ("Merged add-x", "Add x")
        );
    }

    #[test]
    fn a_changed_mark_is_a_line_and_an_unchanged_or_new_item_is_not() {
        let task = |line: usize, mark: Mark, title: &str, holder: Option<&str>| Task {
            line,
            mark,
            title: title.to_string(),
            holder: holder.map(str::to_string),
            reason: (mark == Mark::Blocked).then(|| "needs keys".to_string()),
            notes: Vec::new(),
        };
        let old = [
            task(0, Mark::Open, "One", None),
            task(1, Mark::Working, "Two", Some("s2")),
            task(2, Mark::Working, "Three", Some("s3")),
            task(3, Mark::Open, "Four", None),
        ];
        let new = [
            task(0, Mark::Open, "New", None),
            task(1, Mark::Working, "One", Some("s1")),
            task(2, Mark::Blocked, "Two", Some("s2")),
            task(3, Mark::Done, "Three", None),
            task(4, Mark::Open, "Four", None),
        ];
        let got = marks("p", &old, &new, 9);
        let whats: Vec<(&str, &What)> = got.iter().map(|e| (e.session.as_str(), &e.what)).collect();
        assert_eq!(
            whats,
            [
                (
                    "s1",
                    &What::Started {
                        title: "One".into()
                    }
                ),
                (
                    "s2",
                    &What::Blocked {
                        title: "Two".into(),
                        reason: "needs keys".into()
                    }
                ),
                (
                    "s3",
                    &What::Finished {
                        title: "Three".into(),
                        commits: Vec::new(),
                    }
                ),
            ]
        );
        assert!(got.iter().all(|e| e.project == "p" && e.at == 9));
    }

    #[test]
    fn a_log_reads_into_commits_and_a_line_without_a_tab_is_left_out() {
        let log = "b8f81ef\tRound the panes\n\n9e439e8\tMerge: a\tb \ngarbage\n";
        let got = commits(log);
        assert_eq!(
            got,
            [
                Commit {
                    hash: "b8f81ef".into(),
                    subject: "Round the panes".into()
                },
                Commit {
                    hash: "9e439e8".into(),
                    subject: "Merge: a\tb".into()
                },
            ]
        );
    }

    #[test]
    fn an_item_starts_where_it_was_last_taken_in_its_own_project() {
        let started = |title: &str| What::Started {
            title: title.into(),
        };
        let entries = [
            entry(1, "a", "p", started("One")),
            entry(2, "b", "q", started("One")),
            entry(3, "c", "p", started("Two")),
            entry(4, "d", "p", started("One")),
        ];
        assert_eq!(started_at(&entries, "p", "One"), Some(4));
        assert_eq!(started_at(&entries, "q", "One"), Some(2));
        assert_eq!(started_at(&entries, "p", "Three"), None);
    }

    #[test]
    fn a_finished_item_tells_its_commits() {
        let c = |hash: &str, subject: &str| Commit {
            hash: hash.into(),
            subject: subject.into(),
        };
        let finished = |commits: Vec<Commit>| What::Finished {
            title: "Add x".into(),
            commits,
        };
        let detail = |what: What| {
            summary(&[entry(1, "a", "p", what)], 0, 10, |_| true)[0].lines[0]
                .detail
                .clone()
        };
        assert_eq!(detail(finished(Vec::new())), "");
        assert_eq!(
            detail(finished(vec![c("abc1234", "Add x")])),
            "abc1234 Add x"
        );
        assert_eq!(
            detail(finished(vec![c("2", "Test x"), c("1", "Add x")])),
            "2 commits: Test x \u{b7} Add x"
        );
        let e = entry(1, "a", "p", finished(vec![c("1", "Add x")]));
        assert_eq!(parse(&e.line()), vec![e]);
        let bare = entry(1, "a", "p", finished(Vec::new()));
        assert!(!bare.line().contains("commits"));
    }
}
