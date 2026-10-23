//! Undoing a session's last turn.
//!
//! When a turn starts, the session's working tree is written into a commit
//! no branch points at, under `refs/horadric/turns/<session>/<n>/start`,
//! and again under `.../end` when it stops. Undo puts back the files the
//! turn changed, as the start had them.
//!
//! Sessions share one tree in trunk mode, so a turn's diff can hold other
//! sessions' work too. Only files this turn can be held to are put back:
//! every file the diff shows when no other session in the tree was mid
//! turn meanwhile, otherwise only the files its own tools wrote. A file
//! changed again since the turn ended is someone else's now, and refuses
//! the whole undo rather than be clobbered.
//!
//! This module is the reasoning only: the git calls are in the hooks
//! crate's `checkpoint`.

use std::collections::{BTreeSet, HashMap};
use std::time::{Duration, SystemTime};

use serde_json::{json, Value};

use crate::overlap::key;
use crate::rarity::WRITERS;
use crate::HookEvent;

/// How many turns of a session keep their checkpoints. Older ones are
/// deleted as new ones come, so the refs never pile up.
pub const KEEP: usize = 20;

/// Where every checkpoint lives.
pub const ROOT: &str = "refs/horadric/turns/";

/// A turn with no stop heard for this long is taken to be over, so a
/// session interrupted mid turn does not make every other session's turn
/// look shared forever.
const OPEN_LIFE: Duration = Duration::from_secs(4 * 60 * 60);

/// The session's id as a ref name may hold it.
pub fn safe(session: &str) -> String {
    let s: String = session
        .chars()
        .map(|c| match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' => c,
            _ => '_',
        })
        .collect();
    if s.is_empty() {
        "_".into()
    } else {
        s
    }
}

/// The folder of refs for every turn of `session`, ending in a slash.
pub fn prefix(session: &str) -> String {
    format!("{ROOT}{}/", safe(session))
}

/// Which end of a turn a ref is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    Start,
    End,
}

impl Edge {
    fn name(self) -> &'static str {
        match self {
            Edge::Start => "start",
            Edge::End => "end",
        }
    }
}

/// The ref of turn `n` of `session`, at `edge`.
pub fn turn_ref(session: &str, n: u64, edge: Edge) -> String {
    format!("{}{n}/{}", prefix(session), edge.name())
}

/// What the refs of a session hold of one turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Turn {
    pub n: u64,
    pub start: bool,
    pub end: bool,
}

/// The turns of `session` among `refs`, oldest first.
pub fn turns(refs: &[String], session: &str) -> Vec<Turn> {
    let prefix = prefix(session);
    let mut found: Vec<Turn> = Vec::new();
    for r in refs {
        let Some((n, edge)) = r
            .strip_prefix(&prefix)
            .and_then(|rest| rest.split_once('/'))
        else {
            continue;
        };
        let Ok(n) = n.parse::<u64>() else {
            continue;
        };
        let at = match found.iter().position(|t| t.n == n) {
            Some(i) => i,
            None => {
                found.push(Turn {
                    n,
                    start: false,
                    end: false,
                });
                found.len() - 1
            }
        };
        match edge {
            "start" => found[at].start = true,
            "end" => found[at].end = true,
            _ => {}
        }
    }
    found.sort_by_key(|t| t.n);
    found
}

/// The number the next turn of `session` takes.
pub fn next(refs: &[String], session: &str) -> u64 {
    turns(refs, session).last().map_or(1, |t| t.n + 1)
}

/// The refs to delete so `session` keeps its last `keep` turns.
pub fn prune(refs: &[String], session: &str, keep: usize) -> Vec<String> {
    let all = turns(refs, session);
    let drop = all.len().saturating_sub(keep);
    all[..drop]
        .iter()
        .flat_map(|t| refs_of(session, t))
        .collect()
}

/// Every ref turn `t` of `session` has.
pub fn refs_of(session: &str, t: &Turn) -> Vec<String> {
    let mut out = Vec::new();
    if t.start {
        out.push(turn_ref(session, t.n, Edge::Start));
    }
    if t.end {
        out.push(turn_ref(session, t.n, Edge::End));
    }
    out
}

/// What is known of a turn beyond its two trees, kept in the message of
/// its end commit so it outlives a restart.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Record {
    /// Every file the session's own tools wrote, in [`key`] form.
    pub writes: BTreeSet<String>,
    /// Another session in the same tree was mid turn at some point of this
    /// one, so the diff may hold its work too.
    pub shared: bool,
}

impl Record {
    /// What a turn no one watched is taken to be: shared, with nothing
    /// known to be its own. Undo then touches nothing.
    pub fn unknown() -> Self {
        Record {
            writes: BTreeSet::new(),
            shared: true,
        }
    }

    pub fn to_message(&self, n: u64) -> String {
        let body = json!({ "writes": self.writes, "shared": self.shared });
        format!("Horadric turn {n}\n\n{body}\n")
    }

    /// The record in an end commit's message. A message without one is
    /// [`Record::unknown`].
    pub fn from_message(message: &str) -> Self {
        let parsed = message
            .lines()
            .find(|l| l.trim_start().starts_with('{'))
            .and_then(|l| serde_json::from_str::<Value>(l).ok());
        let Some(v) = parsed else {
            return Record::unknown();
        };
        let writes = v
            .get("writes")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        Record {
            writes,
            shared: v.get("shared").and_then(Value::as_bool).unwrap_or(true),
        }
    }
}

#[derive(Debug, Clone)]
struct Open {
    tree: String,
    n: u64,
    at: SystemTime,
    record: Record,
}

/// The turns under way, by session, as hook events tell of them.
#[derive(Debug, Default)]
pub struct Turns {
    open: HashMap<String, Open>,
}

impl Turns {
    /// Turn `n` of `session` started in the tree at `tree`. Any turn of the
    /// session still open is handed back, to be ended where this one
    /// starts: an interrupted turn sends no stop.
    pub fn begin(
        &mut self,
        session: &str,
        tree: &str,
        n: u64,
        now: SystemTime,
    ) -> Option<(u64, Record)> {
        let left = self.open.remove(session).map(|o| (o.n, o.record));
        self.open
            .retain(|_, o| now.duration_since(o.at).unwrap_or_default() < OPEN_LIFE);
        let mut shared = false;
        for o in self.open.values_mut().filter(|o| o.tree == tree) {
            o.record.shared = true;
            shared = true;
        }
        self.open.insert(
            session.to_string(),
            Open {
                tree: tree.to_string(),
                n,
                at: now,
                record: Record {
                    writes: BTreeSet::new(),
                    shared,
                },
            },
        );
        left
    }

    /// Takes in a tool that finished: a file the session wrote is its own.
    /// Subagents count, their work is the session's.
    pub fn hear(&mut self, session: &str, event: &HookEvent) {
        if event.hook_event_name != "PostToolUse" {
            return;
        }
        let Some(open) = self.open.get_mut(session) else {
            return;
        };
        let tool = event.tool_name.as_deref().unwrap_or("");
        if !WRITERS.contains(&tool) {
            return;
        }
        let file = event.tool_input.as_ref().and_then(|i| {
            i.get("file_path")
                .or_else(|| i.get("notebook_path"))
                .and_then(Value::as_str)
        });
        if let Some(f) = file {
            open.record.writes.insert(key(f, &event.cwd));
        }
    }

    /// The turn of `session` stopped: its tree, number and record.
    pub fn finish(&mut self, session: &str) -> Option<(String, u64, Record)> {
        self.open.remove(session).map(|o| (o.tree, o.n, o.record))
    }

    /// The record of turn `n` of `session`, while it is open.
    pub fn record(&self, session: &str, n: u64) -> Option<Record> {
        self.open
            .get(session)
            .filter(|o| o.n == n)
            .map(|o| o.record.clone())
    }
}

/// One file a turn changed: its blob before and after, None where it did
/// not exist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    /// Inside the tree, forward slashes.
    pub path: String,
    pub before: Option<String>,
    pub after: Option<String>,
}

/// Reads what `git diff-tree -r -z --no-renames` prints between two
/// commits. Submodules are left out: their work is in another repository.
pub fn parse_diff(raw: &[u8]) -> Vec<Change> {
    let text = String::from_utf8_lossy(raw);
    let mut fields = text.split('\0');
    let mut out = Vec::new();
    while let Some(meta) = fields.next() {
        let Some(meta) = meta.strip_prefix(':') else {
            continue;
        };
        let Some(path) = fields.next() else {
            break;
        };
        let parts: Vec<&str> = meta.split(' ').collect();
        let [old_mode, new_mode, old, new, ..] = parts[..] else {
            continue;
        };
        if old_mode == "160000" || new_mode == "160000" {
            continue;
        }
        let blob = |mode: &str, sha: &str| {
            (mode != "000000" && !sha.chars().all(|c| c == '0')).then(|| sha.to_string())
        };
        out.push(Change {
            path: path.to_string(),
            before: blob(old_mode, old),
            after: blob(new_mode, new),
        });
    }
    out
}

/// The changes a turn can be held to, and the paths it cannot. A turn no
/// other session shared owns its whole diff; a shared one only what its
/// own tools wrote.
pub fn attribute(changes: Vec<Change>, record: &Record, top: &str) -> (Vec<Change>, Vec<String>) {
    if !record.shared {
        return (changes, Vec::new());
    }
    let mut mine = Vec::new();
    let mut unsure = Vec::new();
    for c in changes {
        if record.writes.contains(&key(&c.path, top)) {
            mine.push(c);
        } else {
            unsure.push(c.path);
        }
    }
    (mine, unsure)
}

/// What undo does to the tree.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Plan {
    /// Paths to write back with the blob the start had.
    pub restore: Vec<(String, String)>,
    /// Paths the turn made, to delete.
    pub delete: Vec<String>,
    /// Paths already as the start had them.
    pub already: Vec<String>,
    /// Paths changed again since the turn ended. Any one refuses the undo.
    pub refused: Vec<String>,
}

/// Plans undo of `mine`, given each path's blob in the tree now (None
/// where it is gone). A path as the turn left it goes back; one as the
/// start had it is done already; anything else was changed since.
pub fn plan(mine: &[Change], now: &HashMap<String, Option<String>>) -> Plan {
    let mut p = Plan::default();
    for c in mine {
        let current = now.get(&c.path).cloned().flatten();
        if current == c.before {
            p.already.push(c.path.clone());
        } else if current != c.after {
            p.refused.push(c.path.clone());
        } else {
            match &c.before {
                Some(blob) => p.restore.push((c.path.clone(), blob.clone())),
                None => p.delete.push(c.path.clone()),
            }
        }
    }
    p
}

/// How an undo went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Not in a git repository.
    NoRepo,
    /// No turn has a checkpoint.
    Nothing,
    /// Files were changed since the turn ended; nothing was touched.
    Refused(Vec<String>),
    /// Put back, with what was left alone.
    Undone {
        restored: Vec<String>,
        deleted: Vec<String>,
        /// Changed in the turn's time but maybe by another session.
        unsure: Vec<String>,
    },
    /// A git call failed, with what it said.
    Failed(String),
}

/// The title and text of the toast that tells how undoing the last turn
/// of the session called `who` went.
pub fn summary(who: &str, o: &Outcome) -> (String, String) {
    match o {
        Outcome::NoRepo => (
            "Nothing to undo".into(),
            format!("{who} does not work in a git repository, so its turns are not kept."),
        ),
        Outcome::Nothing => (
            "Nothing to undo".into(),
            format!("{who} has no turn kept to undo."),
        ),
        Outcome::Refused(files) => (
            "Undo refused".into(),
            format!(
                "{} changed again since {who}'s last turn, by another session or by hand. \
                 Nothing was touched.",
                list(files)
            ),
        ),
        Outcome::Undone {
            restored,
            deleted,
            unsure,
        } => {
            let mut parts = Vec::new();
            if !restored.is_empty() {
                parts.push(format!("Restored {}.", list(restored)));
            }
            if !deleted.is_empty() {
                parts.push(format!("Deleted {}.", list(deleted)));
            }
            if parts.is_empty() {
                parts.push("The turn changed no files of its own.".into());
            }
            if !unsure.is_empty() {
                parts.push(format!(
                    "Left {} alone: another session was working meanwhile.",
                    list(unsure)
                ));
            }
            parts.push(format!("{who} still remembers the turn."));
            (format!("Undid {who}'s last turn"), parts.join(" "))
        }
        Outcome::Failed(why) => ("Undo failed".into(), why.clone()),
    }
}

/// Up to three file names, then how many more.
fn list(paths: &[String]) -> String {
    let names: Vec<&str> = paths
        .iter()
        .map(|p| p.rsplit('/').next().unwrap_or(p))
        .collect();
    match names.len() {
        0 => "no files".into(),
        1..=3 => names.join(", "),
        n => format!("{} and {} more", names[..3].join(", "), n - 3),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refs(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    fn at(minutes: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000 + minutes * 60)
    }

    fn write(file: &str, cwd: &str) -> HookEvent {
        HookEvent {
            tool_name: Some("Write".into()),
            tool_input: Some(json!({ "file_path": file })),
            cwd: cwd.into(),
            ..HookEvent::synthetic("PostToolUse")
        }
    }

    fn change(path: &str, before: Option<&str>, after: Option<&str>) -> Change {
        Change {
            path: path.into(),
            before: before.map(str::to_string),
            after: after.map(str::to_string),
        }
    }

    #[test]
    fn a_session_id_becomes_a_safe_ref_name() {
        assert_eq!(safe("fix-login-3600"), "fix-login-3600");
        assert_eq!(safe("a b/../c.lock"), "a_b____c_lock");
        assert_eq!(safe(""), "_");
        assert_eq!(
            turn_ref("s-1", 7, Edge::End),
            "refs/horadric/turns/s-1/7/end"
        );
    }

    #[test]
    fn turns_are_read_from_refs_in_number_order() {
        let r = refs(&[
            "refs/horadric/turns/s/10/start",
            "refs/horadric/turns/s/2/end",
            "refs/horadric/turns/s/2/start",
            "refs/horadric/turns/other/40/start",
            "refs/horadric/turns/s/x/start",
        ]);
        let t = turns(&r, "s");
        assert_eq!(
            t,
            vec![
                Turn {
                    n: 2,
                    start: true,
                    end: true
                },
                Turn {
                    n: 10,
                    start: true,
                    end: false
                },
            ]
        );
        assert_eq!(next(&r, "s"), 11);
        assert_eq!(next(&r, "new"), 1);
    }

    #[test]
    fn pruning_keeps_the_newest_turns() {
        let r: Vec<String> = (1..=5)
            .flat_map(|n| [turn_ref("s", n, Edge::Start), turn_ref("s", n, Edge::End)])
            .chain([turn_ref("t", 1, Edge::Start)])
            .collect();
        let gone = prune(&r, "s", 3);
        assert_eq!(
            gone,
            refs(&[
                "refs/horadric/turns/s/1/start",
                "refs/horadric/turns/s/1/end",
                "refs/horadric/turns/s/2/start",
                "refs/horadric/turns/s/2/end",
            ])
        );
        assert!(prune(&r, "s", KEEP).is_empty());
    }

    #[test]
    fn a_record_survives_its_commit_message() {
        let mut r = Record::default();
        r.writes.insert("c:/app/x.rs".into());
        let m = r.to_message(4);
        assert!(m.starts_with("Horadric turn 4\n\n"));
        assert_eq!(Record::from_message(&m), r);
        assert_eq!(Record::from_message("something else"), Record::unknown());
    }

    #[test]
    fn a_turn_alone_in_its_tree_is_not_shared() {
        let mut t = Turns::default();
        assert_eq!(t.begin("a", "c:/app", 1, at(0)), None);
        t.hear("a", &write("x.rs", "C:\\app"));
        let (tree, n, r) = t.finish("a").unwrap();
        assert_eq!((tree.as_str(), n), ("c:/app", 1));
        assert!(!r.shared);
        assert!(r.writes.contains("c:/app/x.rs"));
    }

    #[test]
    fn turns_that_meet_in_one_tree_are_both_shared() {
        let mut t = Turns::default();
        t.begin("a", "c:/app", 1, at(0));
        t.begin("elsewhere", "c:/other", 1, at(1));
        t.begin("b", "c:/app", 3, at(2));
        assert!(t.record("a", 1).unwrap().shared);
        assert!(t.record("b", 3).unwrap().shared);
        assert!(!t.record("elsewhere", 1).unwrap().shared);
        // Once a has stopped, b's next turn is alone again.
        t.finish("a");
        t.finish("b");
        t.begin("b", "c:/app", 4, at(3));
        assert!(!t.record("b", 4).unwrap().shared);
    }

    #[test]
    fn a_turn_left_open_is_handed_back_and_goes_stale() {
        let mut t = Turns::default();
        t.begin("a", "c:/app", 1, at(0));
        t.hear("a", &write("C:/app/y.rs", ""));
        let (n, r) = t.begin("a", "c:/app", 2, at(1)).unwrap();
        assert_eq!(n, 1);
        assert!(r.writes.contains("c:/app/y.rs"));
        // Interrupted and never heard of again, it stops sharing others'.
        t.begin("b", "c:/app", 1, at(5 * 60));
        assert!(!t.record("b", 1).unwrap().shared);
    }

    #[test]
    fn a_tool_outside_a_turn_or_not_a_writer_is_ignored() {
        let mut t = Turns::default();
        t.hear("a", &write("x.rs", "c:/app"));
        t.begin("a", "c:/app", 1, at(0));
        let read = HookEvent {
            tool_name: Some("Read".into()),
            ..write("y.rs", "c:/app")
        };
        t.hear("a", &read);
        assert!(t.record("a", 1).unwrap().writes.is_empty());
    }

    const NO_BLOB: &str = "0000000000000000000000000000000000000000";

    #[test]
    fn a_raw_diff_is_read_with_creations_and_deletions() {
        let a = "a".repeat(40);
        let b = "b".repeat(40);
        let raw = format!(
            ":100644 100644 {a} {b} M\0src/x.rs\0\
             :000000 100644 {NO_BLOB} {b} A\0new.txt\0\
             :100644 000000 {a} {NO_BLOB} D\0old.txt\0\
             :160000 160000 {a} {b} M\0vendor/sub\0"
        );
        let c = parse_diff(raw.as_bytes());
        assert_eq!(
            c,
            vec![
                change("src/x.rs", Some(&a), Some(&b)),
                change("new.txt", None, Some(&b)),
                change("old.txt", Some(&a), None),
            ]
        );
        assert!(parse_diff(b"").is_empty());
    }

    #[test]
    fn a_shared_turn_owns_only_what_its_tools_wrote() {
        let changes = vec![
            change("src/x.rs", Some("1"), Some("2")),
            change("src/theirs.rs", Some("1"), Some("2")),
        ];
        let alone = Record::default();
        let (mine, unsure) = attribute(changes.clone(), &alone, "C:/app");
        assert_eq!(mine.len(), 2);
        assert!(unsure.is_empty());

        let mut shared = Record::unknown();
        shared.writes.insert("c:/app/src/x.rs".into());
        let (mine, unsure) = attribute(changes, &shared, "C:\\App");
        assert_eq!(mine, vec![change("src/x.rs", Some("1"), Some("2"))]);
        assert_eq!(unsure, vec!["src/theirs.rs".to_string()]);
    }

    #[test]
    fn undo_restores_deletes_skips_and_refuses_by_what_is_there_now() {
        let mine = vec![
            change("kept.rs", Some("s"), Some("e")),
            change("made.rs", None, Some("e")),
            change("gone.rs", Some("s"), None),
            change("back.rs", Some("s"), Some("e")),
            change("touched.rs", Some("s"), Some("e")),
        ];
        let now: HashMap<String, Option<String>> = [
            ("kept.rs", Some("e")),
            ("made.rs", Some("e")),
            ("gone.rs", None),
            ("back.rs", Some("s")),
            ("touched.rs", Some("other")),
        ]
        .into_iter()
        .map(|(p, b)| (p.to_string(), b.map(str::to_string)))
        .collect();
        let p = plan(&mine, &now);
        assert_eq!(
            p.restore,
            vec![
                ("kept.rs".to_string(), "s".to_string()),
                ("gone.rs".to_string(), "s".to_string())
            ]
        );
        assert_eq!(p.delete, vec!["made.rs".to_string()]);
        assert_eq!(p.already, vec!["back.rs".to_string()]);
        assert_eq!(p.refused, vec!["touched.rs".to_string()]);
    }

    #[test]
    fn a_file_made_by_the_turn_and_since_deleted_is_already_undone() {
        let mine = vec![change("made.rs", None, Some("e"))];
        let p = plan(&mine, &HashMap::new());
        assert_eq!(p.already, vec!["made.rs".to_string()]);
    }

    #[test]
    fn the_toast_names_files_and_what_was_left() {
        let (title, text) = summary(
            "fix-login",
            &Outcome::Undone {
                restored: vec!["src/a.rs".into()],
                deleted: vec!["b.txt".into()],
                unsure: vec!["c.rs".into()],
            },
        );
        assert_eq!(title, "Undid fix-login's last turn");
        assert_eq!(
            text,
            "Restored a.rs. Deleted b.txt. Left c.rs alone: another session was \
             working meanwhile. fix-login still remembers the turn."
        );
        let many: Vec<String> = ["a", "b", "c", "d", "e"].map(String::from).to_vec();
        let (title, text) = summary("s", &Outcome::Refused(many));
        assert_eq!(title, "Undo refused");
        assert!(text.starts_with("a, b, c and 2 more changed again"));
    }
}
