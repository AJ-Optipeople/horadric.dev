//! The chronicle: every quest a project has ever taken, for the quest log
//! window. The quest list forgets a quest's story the moment it is done
//! (its row leaves the tile, its session closes), and the journal keeps a
//! week. So the app, and `horadric quest` from inside a session, append a
//! line to `chronicle.jsonl` for each thing that happens to a quest, kept
//! for good, and this turns those lines into quests and the branching
//! diagram the window draws.
//!
//! A quest is known by its holder, the `@id` the list gives it, which is
//! the id of the session that worked it (or of its batch of tombs). A quest
//! added by a session working another quest is that quest's child; one
//! added by any other session, or by the human, grows from the trunk, the
//! project's main line, which every quest converges back into once done.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::journal::{self, Commit};
use crate::tasks::{Mark, Task};
use crate::{tombs, Agent};

/// The file, beside `state.json`.
pub const FILE: &str = "chronicle.jsonl";

/// One thing that happened to a quest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    /// When, in Unix seconds.
    pub at: u64,
    /// The project key.
    #[serde(default)]
    pub project: String,
    /// The holder of the quest, empty for [`Happened::Added`], which comes
    /// before the quest has one.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub quest: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title: String,
    #[serde(flatten)]
    pub what: Happened,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "what", rename_all = "snake_case")]
pub enum Happened {
    /// `horadric quest add` ran in the session `by`, which makes the quest
    /// `title` a child of the quest `by` works, if it works one, or else
    /// of the conversation `by` held, which sits on the main line.
    Added {
        by: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        conversation: String,
    },
    /// A session took the quest.
    Accepted {
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        notes: Vec<String>,
    },
    /// The list's mark for the quest changed to review, blocked or done,
    /// or back to open, which is a quest put back.
    Marked {
        mark: Outcome,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        reason: String,
    },
    /// The agent's own word on what the quest achieved, from `horadric
    /// quest done "..."`.
    Summary { text: String },
    /// A turn of the quest's session ended: its last message, and where
    /// its conversation is, to read or carry on later.
    Turn {
        #[serde(default, skip_serializing_if = "String::is_empty")]
        line: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        conversation: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        cwd: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        name: String,
    },
    /// The commits made under the quest, newest first.
    Commits { commits: Vec<Commit> },
    /// The quest's branch went into the main tree.
    Merged { branch: String },
}

/// Where a quest stands, or how it ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// Its session is on it.
    Working,
    /// The agent says it is done and the human has not looked yet.
    Review,
    Blocked,
    Done,
    /// Put back in the list, its session ended.
    Returned,
}

impl Outcome {
    pub fn of(mark: Mark) -> Self {
        match mark {
            Mark::Working => Self::Working,
            Mark::Review => Self::Review,
            Mark::Blocked => Self::Blocked,
            Mark::Done => Self::Done,
            Mark::Open => Self::Returned,
        }
    }

    /// The word the window shows.
    pub fn word(self) -> &'static str {
        match self {
            Self::Working => "in progress",
            Self::Review => "awaiting review",
            Self::Blocked => "blocked",
            Self::Done => "completed",
            Self::Returned => "put back",
        }
    }

    /// Whether the quest's line has ended. A quest in review still holds
    /// its session, so it is not over until approved.
    pub fn over(self) -> bool {
        matches!(self, Self::Done | Self::Returned)
    }
}

impl Record {
    /// The line the file holds for it, newline included.
    pub fn line(&self) -> String {
        let mut s = serde_json::to_string(self).unwrap_or_default();
        s.push('\n');
        s
    }
}

/// Every line of the file that reads. A torn line, from a crash mid write,
/// is left out rather than losing the rest.
pub fn parse(text: &str) -> Vec<Record> {
    text.lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

/// The quest a session id belongs to: a tomb's is its batch's.
pub fn quest_of(session: &str) -> &str {
    tombs::of(session).map_or(session, |(batch, _)| batch)
}

/// The item of `list` the session works, if it works one. A tomb's item
/// is held by its batch until the human picks, and by the winner after.
pub fn worked_by<'a>(list: &'a [Task], session: &str) -> Option<&'a Task> {
    let quest = quest_of(session);
    list.iter()
        .find(|t| t.holder.as_deref().is_some_and(|h| quest_of(h) == quest))
}

/// The records for what changed between two reads of a project's list:
/// a quest accepted, marked, or put back. Like [`journal::marks`], items
/// are matched by title, since lines move as the list is edited.
pub fn marks(project: &str, old: &[Task], new: &[Task], now: u64) -> Vec<Record> {
    let mut out = Vec::new();
    for t in new {
        let Some(before) = old.iter().find(|o| o.title == t.title) else {
            continue;
        };
        let holder = t.holder.clone().or_else(|| before.holder.clone());
        let Some(quest) = holder else { continue };
        let record = |what| Record {
            at: now,
            project: project.to_string(),
            quest: quest.clone(),
            title: t.title.clone(),
            what,
        };
        if t.mark == Mark::Working && (before.mark != Mark::Working || before.holder != t.holder) {
            out.push(record(Happened::Accepted {
                notes: t.notes.clone(),
            }));
        } else if t.mark != before.mark && t.mark != Mark::Working {
            out.push(record(Happened::Marked {
                mark: Outcome::of(t.mark),
                reason: t.reason.clone().unwrap_or_default(),
            }));
        }
    }
    out
}

/// One quest, as the window shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Quest {
    /// Its holder, which is how records find it.
    pub id: String,
    pub title: String,
    pub notes: Vec<String>,
    /// The name its session's tile had.
    pub name: String,
    /// When it was accepted, None for a quest older than the chronicle,
    /// known only from the list.
    pub accepted: Option<u64>,
    /// When it was last marked.
    pub ended: Option<u64>,
    pub outcome: Outcome,
    /// Why it is blocked.
    pub reason: String,
    /// The agent's own summary.
    pub summary: String,
    /// The last thing its session said.
    pub last: String,
    /// The conversation id and its folder, to read it or carry it on.
    pub conversation: Option<(String, String)>,
    pub commits: Vec<Commit>,
    /// The branch it was merged from, when it had one.
    pub merged: Option<String>,
    /// The quest it grew out of, an index into the same list.
    pub parent: Option<usize>,
    /// The session, not a quest's, that added it, for "added by".
    pub added_by: Option<String>,
    /// The conversation that session held, which the quest branches from
    /// when it is on the main line.
    pub added_in: Option<String>,
    /// Not a quest but a conversation no quest holds: a main session, drawn
    /// on the main line itself. Its `accepted` is when it started, `ended`
    /// when it was last touched.
    pub main: bool,
    /// Whose conversation it is, which says how to carry it on.
    pub agent: Agent,
}

impl Quest {
    fn new(id: &str, title: &str) -> Self {
        Self {
            id: id.to_string(),
            title: title.to_string(),
            notes: Vec::new(),
            name: String::new(),
            accepted: None,
            ended: None,
            outcome: Outcome::Working,
            reason: String::new(),
            summary: String::new(),
            last: String::new(),
            conversation: None,
            commits: Vec::new(),
            merged: None,
            parent: None,
            added_by: None,
            added_in: None,
            main: false,
            agent: Agent::Claude,
        }
    }

    /// The one line on what came of it: the agent's summary, else why it
    /// is blocked, else the last thing its session said.
    pub fn result(&self) -> &str {
        [&self.summary, &self.reason, &self.last]
            .into_iter()
            .find(|s| !s.trim().is_empty())
            .map_or("", |s| s.as_str())
    }
}

/// Every quest of `project`, oldest first: those the chronicle heard of,
/// then those the list holds that it did not (quests worked before the
/// chronicle, or while no app ran), in the list's order. The journal, which
/// keeps a week, fills in what it knows of those.
pub fn quests(
    records: &[Record],
    journal: &[journal::Entry],
    project: &str,
    list: &[Task],
) -> Vec<Quest> {
    let mut out: Vec<Quest> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    // Who added which title, waiting for the quest to be accepted.
    let mut added: HashMap<&str, (&str, &str)> = HashMap::new();
    for r in records.iter().filter(|r| r.project == project) {
        if let Happened::Added { by, conversation } = &r.what {
            added.insert(r.title.as_str(), (by.as_str(), conversation.as_str()));
            continue;
        }
        if r.quest.is_empty() {
            continue;
        }
        let quest = quest_of(&r.quest).to_string();
        let i = find(&mut out, &mut index, &quest, &r.title);
        let q = &mut out[i];
        if !r.title.is_empty() {
            q.title = r.title.clone();
        }
        match &r.what {
            Happened::Added { .. } => {}
            Happened::Accepted { notes } => {
                q.accepted.get_or_insert(r.at);
                q.notes = notes.clone();
                q.outcome = Outcome::Working;
                q.ended = None;
                if let Some((by, conversation)) = added.get(r.title.as_str()) {
                    q.added_by = Some(by.to_string());
                    q.added_in = Some(conversation.to_string()).filter(|c| !c.is_empty());
                }
            }
            Happened::Marked { mark, reason } => {
                q.outcome = *mark;
                q.reason = reason.clone();
                q.ended = Some(r.at);
            }
            Happened::Summary { text } => q.summary = text.clone(),
            Happened::Turn {
                line,
                conversation,
                cwd,
                name,
            } => {
                if !line.is_empty() {
                    q.last = line.clone();
                }
                if !conversation.is_empty() {
                    q.conversation = Some((conversation.clone(), cwd.clone()));
                }
                if !name.is_empty() {
                    q.name = name.clone();
                }
            }
            Happened::Commits { commits } => q.commits = commits.clone(),
            Happened::Merged { branch } => q.merged = Some(branch.clone()),
        }
    }
    for t in list {
        let (Some(holder), true) = (&t.holder, t.mark != Mark::Open) else {
            continue;
        };
        let known = index.contains_key(holder.as_str());
        let i = find(&mut out, &mut index, holder, &t.title);
        let q = &mut out[i];
        if q.notes.is_empty() {
            q.notes = t.notes.clone();
        }
        // The list is the truth of where a quest stands now.
        if !known || q.outcome != Outcome::Returned {
            q.outcome = Outcome::of(t.mark);
        }
        if let Some(why) = t.reason.as_ref().filter(|_| t.mark == Mark::Blocked) {
            q.reason = why.clone();
        }
        if !known {
            from_journal(q, journal, project);
        }
    }
    link(&mut out);
    out
}

/// The quest `id` in `out`, added with `title` when it is not there yet.
fn find(out: &mut Vec<Quest>, index: &mut HashMap<String, usize>, id: &str, title: &str) -> usize {
    *index.entry(id.to_string()).or_insert_with(|| {
        out.push(Quest::new(id, title));
        out.len() - 1
    })
}

/// What the journal's week says of a quest the chronicle never heard of.
fn from_journal(q: &mut Quest, journal: &[journal::Entry], project: &str) {
    use journal::What;
    for e in journal {
        let ours = e.project == project && quest_of(&e.session) == q.id;
        let titled = |t: &str| e.project == project && t == q.title;
        match &e.what {
            What::Started { title } if titled(title) => {
                q.accepted.get_or_insert(e.at);
            }
            What::Finished { title, commits } if titled(title) => {
                q.ended = Some(e.at);
                if !commits.is_empty() {
                    q.commits = commits.clone();
                }
            }
            What::Review { title } | What::Blocked { title, .. } if titled(title) => {
                q.ended = Some(e.at);
            }
            What::Done { line } if ours && !line.is_empty() => q.last = line.clone(),
            What::Merged { branch, title } if titled(title) => q.merged = Some(branch.clone()),
            _ => {}
        }
        if ours && !e.name.is_empty() {
            q.name = e.name.clone();
        }
    }
}

/// Finds each quest's parent: the quest worked by the session that added
/// it. Only a quest accepted earlier can be one, which keeps the tree a
/// tree whatever the records say.
fn link(quests: &mut [Quest]) {
    for i in 0..quests.len() {
        let Some(by) = quests[i].added_by.clone() else {
            continue;
        };
        let by = quest_of(&by);
        if let Some(p) = quests[..i].iter().position(|q| q.id == by) {
            quests[i].parent = Some(p);
            quests[i].added_by = None;
        }
    }
}

/// A conversation the agent kept for the project's folder, as the app
/// found it on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Talk {
    pub id: String,
    pub title: String,
    /// When it started and when it was last touched, in Unix seconds.
    pub started: u64,
    pub touched: u64,
    pub cwd: String,
    pub agent: Agent,
}

/// `quests` with each of `talks` that no quest holds put among them as a
/// conversation on the main line, and each quest added in one of those
/// made its child, so it branches from the conversation's dot.
pub fn with_talks(mut quests: Vec<Quest>, talks: &[Talk]) -> Vec<Quest> {
    let held = |id: &str| {
        quests
            .iter()
            .any(|q| q.conversation.as_ref().is_some_and(|(c, _)| c == id))
    };
    let free: Vec<&Talk> = talks.iter().filter(|t| !held(&t.id)).collect();
    for t in free {
        let mut q = Quest::new(&t.id, &t.title);
        q.accepted = Some(t.started);
        q.ended = Some(t.touched);
        q.outcome = Outcome::Done;
        q.conversation = Some((t.id.clone(), t.cwd.clone()));
        q.main = true;
        q.agent = t.agent;
        quests.push(q);
    }
    for i in 0..quests.len() {
        if quests[i].main || quests[i].parent.is_some() {
            continue;
        }
        let Some(c) = quests[i].added_in.clone() else {
            continue;
        };
        if let Some(p) = quests.iter().position(|q| q.main && q.id == c) {
            quests[i].parent = Some(p);
            quests[i].added_by = None;
        }
    }
    quests
}

/// One row of the branching diagram, a quest, newest at the top. Lane 0
/// is the trunk, the project's main line, drawn the whole way down; every
/// quest has a lane of its own from where it branched until it converged.
///
/// Each row is a band with a dot at its middle. Lines run between bands
/// from bottom (older) to top (newer):
/// - `through`: lanes drawn the full height of the band.
/// - the quest's own lane rises from `from` at the band's bottom edge to
///   the dot, and from the dot on up to the top edge when `up`.
/// - `ends`: lanes that end in this band, other than the dot's own: they
///   rise from the bottom edge to the middle, then converge into the
///   trunk at the top edge when merged, or stop with a cap.
/// - `end`: how the dot's own lane ends here, when it does.
/// - `forks`: on a conversation's row, which sits on the trunk, the lanes
///   of the quests it added, leaving its dot for the top edge and running
///   on up to the quest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// Index into the quests.
    pub quest: usize,
    pub lane: usize,
    /// The lane it branched from, the trunk or its parent's.
    pub from: usize,
    pub up: bool,
    pub end: Option<End>,
    pub through: Vec<usize>,
    pub ends: Vec<(usize, End)>,
    /// Each lane forking here, and the quest it leads to.
    pub forks: Vec<(usize, usize)>,
}

/// How a lane ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum End {
    /// Into the trunk: the quest is done and its work is in.
    Converge,
    /// Stopped: put back, blocked or waiting, its work not in the main
    /// line. A quest still in progress never ends; its lane runs to the
    /// top.
    Cap,
}

/// The diagram for `quests` as [`quests`] and [`with_talks`] gave them,
/// and how many lanes it takes, the trunk included. Rows come newest
/// first. A conversation on the main line takes a row on the trunk and no
/// lane; a quest it added has its lane from the conversation's row.
pub fn graph(quests: &[Quest]) -> (Vec<Row>, usize) {
    let n = quests.len();
    // Oldest first, by acceptance; the unknown keep their order, first.
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by_key(|&i| (quests[i].accepted.unwrap_or(0), i));
    let mut row_of = vec![0; n];
    for (r, &q) in order.iter().enumerate() {
        row_of[q] = r;
    }
    let main = |q: usize| quests[q].main;
    // The row each quest's lane starts on: its own, or the row of the
    // conversation that added it, when that came first.
    let mut start = row_of.clone();
    for q in 0..n {
        if let Some(p) = quests[q].parent.filter(|&p| main(p)) {
            start[q] = row_of[p].min(row_of[q]);
        }
    }
    // The last row each quest's lane reaches: the last quest accepted
    // before it ended, and never short of its children's rows. A quest
    // that has not ended runs to the newest row.
    let mut last = vec![0; n];
    for (r, &q) in order.iter().enumerate() {
        let quest = &quests[q];
        last[q] = match (quest.main, quest.outcome, quest.ended) {
            (true, ..) => r,
            (_, Outcome::Working, _) => n.saturating_sub(1),
            (_, _, Some(end)) => order
                .iter()
                .enumerate()
                .skip(r)
                .take_while(|(_, &o)| quests[o].accepted.is_some_and(|a| a < end) || o == q)
                .map(|(r, _)| r)
                .last()
                .unwrap_or(r),
            (_, _, None) => r,
        };
    }
    for &q in order.iter().rev() {
        if let Some(p) = quests[q].parent.filter(|&p| !main(p)) {
            last[p] = last[p].max(row_of[q]);
        }
    }
    // The quests whose lanes start on each row, in row order.
    let mut starting: Vec<Vec<usize>> = vec![Vec::new(); n];
    for &q in order.iter().filter(|&&q| !main(q)) {
        starting[start[q]].push(q);
    }
    // Lanes, lowest free first, held from a quest's start to its last.
    let mut lane = vec![0; n];
    let mut busy: Vec<Option<usize>> = Vec::new();
    for (r, here) in starting.iter().enumerate() {
        for slot in busy.iter_mut() {
            if slot.is_some_and(|h| last[h] < r) {
                *slot = None;
            }
        }
        for &q in here {
            let free = busy.iter().position(Option::is_none).unwrap_or_else(|| {
                busy.push(None);
                busy.len() - 1
            });
            busy[free] = Some(q);
            lane[q] = free + 1;
        }
    }
    let width = busy.len() + 1;
    let end_of = |q: usize| -> Option<End> {
        match quests[q].outcome {
            Outcome::Working => None,
            Outcome::Done => Some(End::Converge),
            _ => Some(End::Cap),
        }
    };
    let top = n.saturating_sub(1);
    let mut rows = Vec::with_capacity(n);
    for (r, &q) in order.iter().enumerate() {
        let mut through = Vec::new();
        let mut ends = Vec::new();
        for &o in &order {
            if o == q || main(o) || start[o] >= r || last[o] < r {
                continue;
            }
            let ending = row_of[o] < r && last[o] == r;
            if ending && !(last[o] == top && end_of(o).is_none()) {
                ends.push((lane[o], end_of(o).unwrap_or(End::Cap)));
            } else {
                through.push(lane[o]);
            }
        }
        through.sort_unstable();
        ends.sort_unstable_by_key(|(l, _)| *l);
        if main(q) {
            let forks = starting[r].iter().map(|&c| (lane[c], c)).collect();
            rows.push(Row {
                quest: q,
                lane: 0,
                from: 0,
                up: false,
                end: None,
                through,
                ends,
                forks,
            });
            continue;
        }
        let open = last[q] > r || (r == top && end_of(q).is_none());
        let from = match quests[q].parent {
            // Its lane rose here from the conversation's dot.
            _ if start[q] < r => lane[q],
            Some(p) => lane[p],
            None => 0,
        };
        rows.push(Row {
            quest: q,
            lane: lane[q],
            from,
            up: open,
            end: (!open).then(|| end_of(q).unwrap_or(End::Cap)),
            through,
            ends,
            forks: Vec::new(),
        });
    }
    rows.reverse();
    (rows, width)
}

/// A conversation's transcript, Claude Code's JSON lines, as text to read:
/// each prompt and reply in full, each tool call on one line. Lines that
/// are not a message, or not JSON, are left out.
pub fn transcript_text(jsonl: &str) -> String {
    let mut out = String::new();
    let mut last = "";
    for line in jsonl.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let kind = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
        if kind != "user" && kind != "assistant" {
            continue;
        }
        if v.get("isMeta").and_then(|m| m.as_bool()) == Some(true) {
            continue;
        }
        let Some(content) = v.get("message").and_then(|m| m.get("content")) else {
            continue;
        };
        let who = if kind == "user" { "## You" } else { "## Agent" };
        let mut said = Vec::new();
        match content {
            serde_json::Value::String(s) => said.push(s.trim().to_string()),
            serde_json::Value::Array(parts) => {
                for p in parts {
                    match p.get("type").and_then(|t| t.as_str()) {
                        Some("text") => {
                            if let Some(t) = p.get("text").and_then(|t| t.as_str()) {
                                said.push(t.trim().to_string());
                            }
                        }
                        Some("tool_use") => said.push(tool_line(p)),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
        said.retain(|s| !s.is_empty());
        if said.is_empty() {
            continue;
        }
        // A reply split by tool results stays under one head.
        if who != last {
            out.push_str(&format!(
                "
{who}

"
            ));
            last = who;
        }
        for s in said {
            out.push_str(&s);
            out.push('\n');
        }
    }
    out.trim_start().to_string()
}

/// One line for a tool call: its name and the input that says most.
fn tool_line(part: &serde_json::Value) -> String {
    let name = part.get("name").and_then(|n| n.as_str()).unwrap_or("tool");
    let input = part.get("input");
    let detail = [
        "command",
        "file_path",
        "pattern",
        "url",
        "description",
        "prompt",
    ]
    .iter()
    .find_map(|k| input?.get(*k)?.as_str())
    .unwrap_or("");
    let detail: String = crate::tasks::one_line(detail).chars().take(160).collect();
    if detail.is_empty() {
        format!("> {name}")
    } else {
        format!("> {name}: {detail}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(at: u64, quest: &str, title: &str, what: Happened) -> Record {
        Record {
            at,
            project: "p".into(),
            quest: quest.into(),
            title: title.into(),
            what,
        }
    }

    fn accepted(at: u64, quest: &str, title: &str) -> Record {
        rec(at, quest, title, Happened::Accepted { notes: vec![] })
    }

    fn marked(at: u64, quest: &str, mark: Outcome) -> Record {
        rec(
            at,
            quest,
            "",
            Happened::Marked {
                mark,
                reason: String::new(),
            },
        )
    }

    fn task(line: usize, mark: Mark, title: &str, holder: Option<&str>) -> Task {
        Task {
            line,
            mark,
            title: title.into(),
            holder: holder.map(Into::into),
            reason: None,
            wait: None,
            notes: vec![],
        }
    }

    #[test]
    fn records_round_trip_and_torn_lines_are_left_out() {
        let r = rec(
            5,
            "a-1",
            "A",
            Happened::Turn {
                line: "All green.".into(),
                conversation: "c1".into(),
                cwd: "C:/p".into(),
                name: "A".into(),
            },
        );
        let text = format!("{}{{\"at\":", r.line());
        assert_eq!(parse(&text), vec![r]);
    }

    #[test]
    fn a_session_works_the_item_its_quest_holds() {
        let list = [
            task(0, Mark::Open, "A", None),
            task(1, Mark::Working, "B", Some("b-1")),
            task(2, Mark::Working, "C", Some("c-1.x3")),
            task(3, Mark::Done, "D", Some("d-1.x2.2")),
        ];
        let title = |s: &str| worked_by(&list, s).map(|t| t.title.as_str());
        assert_eq!(title("b-1"), Some("B"));
        assert_eq!(title("c-1.x3.3"), Some("C"));
        assert_eq!(
            title("d-1.x2.1"),
            Some("D"),
            "a losing tomb is still its batch's"
        );
        assert_eq!(title("b-11"), None);
        assert_eq!(title("quest-giver-5"), None);
    }

    #[test]
    fn a_change_of_mark_is_a_record() {
        let old = [
            task(0, Mark::Open, "A", None),
            task(1, Mark::Working, "B", Some("b-1")),
        ];
        let mut new = [
            task(0, Mark::Working, "A", Some("a-1")),
            task(1, Mark::Blocked, "B", Some("b-1")),
        ];
        new[0].notes = vec!["note".into()];
        new[1].reason = Some("needs a key".into());
        let r = marks("p", &old, &new, 9);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].quest, "a-1");
        assert_eq!(
            r[0].what,
            Happened::Accepted {
                notes: vec!["note".into()]
            }
        );
        assert_eq!(
            r[1].what,
            Happened::Marked {
                mark: Outcome::Blocked,
                reason: "needs a key".into()
            }
        );
        // Put back: open again, the holder only in the old read.
        let back = marks("p", &new[..1], &[task(0, Mark::Open, "A", None)], 10);
        assert_eq!(back[0].quest, "a-1");
        assert!(matches!(
            back[0].what,
            Happened::Marked {
                mark: Outcome::Returned,
                ..
            }
        ));
        assert!(marks("p", &new, &new, 11).is_empty());
    }

    #[test]
    fn a_quest_gathers_its_records_and_its_child() {
        let records = [
            accepted(10, "a-1", "A"),
            rec(
                11,
                "",
                "B",
                Happened::Added {
                    by: "a-1".into(),
                    conversation: String::new(),
                },
            ),
            rec(
                12,
                "a-1",
                "",
                Happened::Summary {
                    text: "Did A".into(),
                },
            ),
            marked(13, "a-1", Outcome::Done),
            accepted(14, "b-1.x2", "B"),
            rec(
                15,
                "b-1.x2.2",
                "",
                Happened::Turn {
                    line: "Tomb two".into(),
                    conversation: "c2".into(),
                    cwd: "C:/w".into(),
                    name: String::new(),
                },
            ),
        ];
        let q = quests(&records, &[], "p", &[]);
        assert_eq!(q.len(), 2);
        assert_eq!(q[0].title, "A");
        assert_eq!(q[0].result(), "Did A");
        assert_eq!(q[0].outcome, Outcome::Done);
        assert_eq!(q[0].ended, Some(13));
        assert_eq!(q[1].id, "b-1.x2");
        assert_eq!(q[1].parent, Some(0));
        assert_eq!(q[1].result(), "Tomb two");
        assert_eq!(q[1].conversation, Some(("c2".into(), "C:/w".into())));
        assert!(quests(&records, &[], "other", &[]).is_empty());
    }

    #[test]
    fn a_quest_added_by_a_main_session_grows_from_the_trunk() {
        let records = [
            rec(
                1,
                "",
                "A",
                Happened::Added {
                    by: "quest-giver-5".into(),
                    conversation: String::new(),
                },
            ),
            accepted(2, "a-1", "A"),
        ];
        let q = quests(&records, &[], "p", &[]);
        assert_eq!(q[0].parent, None);
        assert_eq!(q[0].added_by.as_deref(), Some("quest-giver-5"));
    }

    #[test]
    fn the_list_fills_in_quests_from_before_the_chronicle() {
        let mut blocked = task(2, Mark::Blocked, "C", Some("c-1"));
        blocked.reason = Some("no key".into());
        let list = [
            task(0, Mark::Done, "A", Some("a-1")),
            task(1, Mark::Open, "B", None),
            blocked,
        ];
        let journal = [journal::Entry {
            at: 7,
            session: "a-1".into(),
            name: "Tile A".into(),
            project: "p".into(),
            what: journal::What::Done {
                line: "A is in.".into(),
            },
        }];
        let q = quests(&[], &journal, "p", &list);
        assert_eq!(q.len(), 2);
        assert_eq!((q[0].outcome, q[0].result()), (Outcome::Done, "A is in."));
        assert_eq!(q[0].name, "Tile A");
        assert_eq!((q[1].outcome, q[1].result()), (Outcome::Blocked, "no key"));
    }

    fn quest(id: &str, accepted: u64, ended: Option<u64>, outcome: Outcome) -> Quest {
        let mut q = Quest::new(id, id);
        q.accepted = Some(accepted);
        q.ended = ended;
        q.outcome = outcome;
        q
    }

    #[test]
    fn quests_one_after_another_share_a_lane() {
        let q = [
            quest("a", 1, Some(2), Outcome::Done),
            quest("b", 3, Some(4), Outcome::Done),
        ];
        let (rows, width) = graph(&q);
        assert_eq!(width, 2);
        assert_eq!(rows[0].quest, 1, "newest first");
        assert!(rows.iter().all(|r| r.lane == 1 && r.from == 0));
        assert!(rows.iter().all(|r| r.end == Some(End::Converge) && !r.up));
        assert!(rows
            .iter()
            .all(|r| r.through.is_empty() && r.ends.is_empty()));
    }

    #[test]
    fn overlapping_quests_take_lanes_side_by_side() {
        let q = [
            quest("a", 1, Some(5), Outcome::Done),
            quest("b", 2, Some(3), Outcome::Returned),
            quest("c", 4, None, Outcome::Working),
        ];
        let (rows, width) = graph(&q);
        assert_eq!(width, 3);
        // Newest first: c, b, a.
        let (c, b, a) = (&rows[0], &rows[1], &rows[2]);
        assert_eq!((a.lane, a.up, a.end), (1, true, None));
        assert_eq!((b.lane, b.end, b.up), (2, Some(End::Cap), false));
        assert_eq!(b.through, vec![1]);
        // b's lane is free again by the time c starts, and a ends there.
        assert_eq!((c.lane, c.up, c.end), (2, true, None));
        assert_eq!(c.ends, vec![(1, End::Converge)]);
    }

    #[test]
    fn a_child_branches_from_its_parent_which_reaches_it() {
        let mut child = quest("b", 5, Some(6), Outcome::Done);
        child.parent = Some(0);
        let q = [quest("a", 1, Some(2), Outcome::Done), child];
        let (rows, width) = graph(&q);
        assert_eq!(width, 3);
        let (b, a) = (&rows[0], &rows[1]);
        assert_eq!((a.lane, a.up, a.end), (1, true, None));
        assert_eq!((b.lane, b.from), (2, 1));
        assert_eq!(b.ends, vec![(1, End::Converge)]);
    }

    fn talk(id: &str, started: u64, touched: u64) -> Talk {
        Talk {
            id: id.into(),
            title: id.to_uppercase(),
            started,
            touched,
            cwd: "C:/p".into(),
            agent: Agent::Claude,
        }
    }

    #[test]
    fn conversations_no_quest_holds_join_the_main_line_and_adopt_their_quests() {
        let records = [
            rec(
                3,
                "",
                "A",
                Happened::Added {
                    by: "main-7".into(),
                    conversation: "t1".into(),
                },
            ),
            accepted(4, "a-1", "A"),
            rec(
                5,
                "a-1",
                "",
                Happened::Turn {
                    line: String::new(),
                    conversation: "held".into(),
                    cwd: "C:/p".into(),
                    name: String::new(),
                },
            ),
        ];
        let q = quests(&records, &[], "p", &[]);
        assert_eq!(q[0].added_in.as_deref(), Some("t1"));
        let q = with_talks(q, &[talk("t1", 1, 9), talk("held", 4, 6)]);
        assert_eq!(q.len(), 2, "the conversation a quest holds is the quest's");
        assert!(q[1].main && q[1].id == "t1");
        assert_eq!((q[1].accepted, q[1].ended), (Some(1), Some(9)));
        assert_eq!(q[0].parent, Some(1));
        assert_eq!(q[0].added_by, None);
        // Added in a conversation not on the main line: from the trunk.
        let q = with_talks(quests(&records, &[], "p", &[]), &[talk("t2", 1, 2)]);
        assert_eq!(q[0].parent, None);
        assert_eq!(q[0].added_by.as_deref(), Some("main-7"));
    }

    #[test]
    fn a_conversation_sits_on_the_trunk_and_its_quest_forks_from_its_dot() {
        let mut child = quest("b", 5, Some(6), Outcome::Done);
        child.parent = Some(2);
        let q = with_talks(
            vec![quest("a", 3, Some(4), Outcome::Done), child],
            &[talk("t", 1, 9)],
        );
        let (rows, width) = graph(&q);
        // Oldest first: t, a, b; drawn newest first.
        let (b, a, t) = (&rows[0], &rows[1], &rows[2]);
        assert_eq!(width, 3);
        assert_eq!((t.quest, t.lane, t.up, t.end), (2, 0, false, None));
        assert_eq!(t.forks, vec![(1, 1)]);
        // b's lane passes a on its way up, a takes the next lane.
        assert_eq!(a.lane, 2);
        assert_eq!(a.through, vec![1]);
        assert_eq!((b.lane, b.from), (1, 1));
        assert!(b.ends.is_empty() && b.through.is_empty());
    }

    #[test]
    fn a_transcript_reads_as_prompts_replies_and_tool_lines() {
        let jsonl = [
            r#"{"type":"user","message":{"role":"user","content":"Fix the clock"}}"#,
            r#"{"type":"user","isMeta":true,"message":{"content":"caveat"}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Looking."},{"type":"tool_use","name":"Bash","input":{"command":"cargo test\n--all"}}]}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","content":"ok"}]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Done."}]}}"#,
            r#"{"type":"summary","summary":"x"}"#,
            "not json",
        ]
        .join("\n");
        assert_eq!(
            transcript_text(&jsonl),
            "## You\n\nFix the clock\n\n## Agent\n\nLooking.\n> Bash: cargo test --all\nDone.\n"
        );
    }
}
