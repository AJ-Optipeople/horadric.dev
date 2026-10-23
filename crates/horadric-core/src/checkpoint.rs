//! Undoing a session's last turn.
//!
//! When a turn starts, the session's working tree is written into a commit
//! no branch points at, under `refs/horadric/turns/<session>/<n>/start`,
//! and again under `.../end` when it stops. Undo puts back the files the
//! turn changed, as the start had them.
//!
//! Sessions share one tree in trunk mode, and anything else may change it
//! meanwhile: a shell, an editor, a pull, a merge from a worktree. So a
//! turn owns only the files its own edit tools wrote, and only while
//! nothing else touched them: each file is hashed before and after each
//! of its writes, and owned when the chain runs unbroken from the start's
//! blob to the end's. Everything else the diff shows is named and left
//! alone. A file changed again since the turn ended refuses the whole
//! undo rather than be clobbered.
//!
//! This module is the reasoning only: the git calls are in the hooks
//! crate's `checkpoint`.

use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, SystemTime};

use serde_json::{json, Map, Value};

use crate::overlap::key;
use crate::rarity::WRITERS;
use crate::HookEvent;

/// How many turns of a session keep their checkpoints. Older ones are
/// deleted as new ones come, so the refs never pile up.
pub const KEEP: usize = 20;

/// Where every checkpoint lives.
pub const ROOT: &str = "refs/horadric/turns/";

/// A turn with no stop heard for this long is forgotten.
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

/// A file's blob as `git hash-object --no-filters` gives it, None where
/// the file does not exist.
pub type Blob = Option<String>;

/// What a turn's own tools did to one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Written {
    /// Before its first write.
    pub before: Blob,
    /// After its last write.
    pub after: Blob,
    /// Something else changed it between two of its writes, or a write
    /// was heard without the hash before it.
    pub broken: bool,
}

/// What is known of a turn beyond its two trees, kept in the message of
/// its end commit so it outlives a restart.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Record {
    /// Every file the session's own edit tools wrote, by [`key`].
    pub files: BTreeMap<String, Written>,
}

impl Record {
    pub fn to_message(&self, n: u64) -> String {
        let files: Map<String, Value> = self
            .files
            .iter()
            .map(|(k, w)| {
                let v = json!({ "before": w.before, "after": w.after, "broken": w.broken });
                (k.clone(), v)
            })
            .collect();
        format!("Horadric turn {n}\n\n{}\n", json!({ "files": files }))
    }

    /// The record in an end commit's message. A message without one owns
    /// nothing.
    pub fn from_message(message: &str) -> Self {
        let parsed = message
            .lines()
            .find(|l| l.trim_start().starts_with('{'))
            .and_then(|l| serde_json::from_str::<Value>(l).ok());
        let Some(files) = parsed.as_ref().and_then(|v| v.get("files")?.as_object()) else {
            return Record::default();
        };
        let blob = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
        let files = files
            .iter()
            .map(|(k, v)| {
                let w = Written {
                    before: blob(v, "before"),
                    after: blob(v, "after"),
                    broken: v.get("broken").and_then(Value::as_bool).unwrap_or(true),
                };
                (k.clone(), w)
            })
            .collect();
        Record { files }
    }
}

/// The file an edit tool of Claude Code is about to write or wrote, as
/// named (resolved against the event's folder) and by [`key`]. None for
/// any other event or tool.
pub fn written(event: &HookEvent) -> Option<(String, String)> {
    if !matches!(
        event.hook_event_name.as_str(),
        "PreToolUse" | "PostToolUse" | "PostToolUseFailure"
    ) {
        return None;
    }
    let tool = event.tool_name.as_deref()?;
    if !WRITERS.contains(&tool) {
        return None;
    }
    let input = event.tool_input.as_ref()?;
    let file = input
        .get("file_path")
        .or_else(|| input.get("notebook_path"))
        .and_then(Value::as_str)?;
    let absolute = file.starts_with(['/', '\\']) || file.get(1..2) == Some(":");
    let path = if absolute || event.cwd.is_empty() {
        file.to_string()
    } else {
        format!("{}/{file}", event.cwd.trim_end_matches(['/', '\\']))
    };
    Some((path, key(file, &event.cwd)))
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
        self.open.insert(
            session.to_string(),
            Open {
                tree: tree.to_string(),
                n,
                at: now,
                record: Record::default(),
            },
        );
        left
    }

    /// The tree of the turn `session` has open.
    pub fn tree(&self, session: &str) -> Option<&str> {
        self.open.get(session).map(|o| o.tree.as_str())
    }

    /// The file `file` (by [`key`]) was `blob` just before an edit tool of
    /// the open turn wrote it. Any change since its last write was not
    /// the turn's.
    pub fn before(&mut self, session: &str, file: &str, blob: Blob) {
        let Some(open) = self.open.get_mut(session) else {
            return;
        };
        match open.record.files.get_mut(file) {
            Some(w) => w.broken |= w.after != blob,
            None => {
                open.record.files.insert(
                    file.to_string(),
                    Written {
                        before: blob.clone(),
                        after: blob,
                        broken: false,
                    },
                );
            }
        }
    }

    /// The file was `blob` just after the tool wrote it.
    pub fn after(&mut self, session: &str, file: &str, blob: Blob) {
        let Some(open) = self.open.get_mut(session) else {
            return;
        };
        let w = open
            .record
            .files
            .entry(file.to_string())
            .or_insert(Written {
                before: None,
                after: None,
                broken: true,
            });
        w.after = blob;
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

/// The changes a turn owns, and the paths it does not: a file is the
/// turn's only when its own tools took it from the start's blob to the
/// end's with nothing else touching it in between.
pub fn attribute(changes: Vec<Change>, record: &Record, top: &str) -> (Vec<Change>, Vec<String>) {
    let mut mine = Vec::new();
    let mut others = Vec::new();
    for c in changes {
        let owned = record
            .files
            .get(&key(&c.path, top))
            .is_some_and(|w| !w.broken && w.before == c.before && w.after == c.after);
        if owned {
            mine.push(c);
        } else {
            others.push(c.path);
        }
    }
    (mine, others)
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
    /// The last turn was not seen to its end, so what it owns is unknown.
    Unended,
    /// Files were changed since the turn ended; nothing was touched.
    Refused(Vec<String>),
    /// The turn's own tools changed nothing that could go back; these
    /// changed some other way. Nothing was touched and the turn is kept.
    NotOwned(Vec<String>),
    /// Every file the turn owns is back as the start had it already.
    Already,
    /// Put back, with what was left alone.
    Undone {
        restored: Vec<String>,
        deleted: Vec<String>,
        /// Changed in the turn's time, but not by its edit tools alone.
        left: Vec<String>,
    },
    /// A git call or a write failed, with what it said. Whatever was
    /// written was put back.
    Failed(String),
}

/// The title and text of the toast that tells how undoing the last turn
/// of the session called `who` went.
pub fn summary(who: &str, o: &Outcome) -> (String, String) {
    let not_undone = |files: &[String]| {
        format!(
            "Not undone: {}, changed by a command, by hand or by another session, \
             not by its edit tools alone.",
            list(files)
        )
    };
    match o {
        Outcome::NoRepo => (
            "Nothing undone".into(),
            format!("{who} does not work in a git repository, so its turns are not kept."),
        ),
        Outcome::Nothing => (
            "Nothing undone".into(),
            format!("{who} has no turn kept to undo."),
        ),
        Outcome::Unended => (
            "Nothing undone".into(),
            format!(
                "{who}'s last turn was not seen to its end, so what it changed is not \
                 known. Nothing was touched."
            ),
        ),
        Outcome::Refused(files) => (
            "Undo refused".into(),
            format!(
                "{} changed again since {who}'s last turn, by another session or by hand. \
                 Nothing was touched.",
                list(files)
            ),
        ),
        Outcome::Already => (
            "Nothing undone".into(),
            format!("The files {who}'s last turn changed are back as they were already."),
        ),
        Outcome::NotOwned(files) if files.is_empty() => (
            "Nothing undone".into(),
            format!("{who}'s last turn changed no files."),
        ),
        Outcome::NotOwned(files) => ("Nothing undone".into(), not_undone(files)),
        Outcome::Undone {
            restored,
            deleted,
            left,
        } => {
            let mut parts = Vec::new();
            if !restored.is_empty() {
                parts.push(format!("Restored {}.", list(restored)));
            }
            if !deleted.is_empty() {
                parts.push(format!("Deleted {}.", list(deleted)));
            }
            if !left.is_empty() {
                parts.push(not_undone(left));
            }
            parts.push(format!("{who} still remembers the turn."));
            (format!("Undid {who}'s last turn"), parts.join(" "))
        }
        Outcome::Failed(why) => ("Undo failed".into(), format!("{why}. Nothing was changed.")),
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

    const NO_BLOB: &str = "0000000000000000000000000000000000000000";

    fn refs(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    fn at(minutes: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000 + minutes * 60)
    }

    fn b(s: &str) -> Blob {
        Some(s.to_string())
    }

    fn change(path: &str, before: Option<&str>, after: Option<&str>) -> Change {
        Change {
            path: path.into(),
            before: before.map(str::to_string),
            after: after.map(str::to_string),
        }
    }

    fn tool(name: &str, hook: &str, file: &str, cwd: &str) -> HookEvent {
        HookEvent {
            tool_name: Some(name.into()),
            tool_input: Some(json!({ "file_path": file })),
            cwd: cwd.into(),
            ..HookEvent::synthetic(hook)
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
        r.files.insert(
            "c:/app/x.rs".into(),
            Written {
                before: None,
                after: b("e"),
                broken: false,
            },
        );
        let m = r.to_message(4);
        assert!(m.starts_with("Horadric turn 4\n\n"));
        assert_eq!(Record::from_message(&m), r);
        assert_eq!(Record::from_message("something else"), Record::default());
    }

    #[test]
    fn only_claude_code_edit_tools_name_a_written_file() {
        let e = tool("Edit", "PreToolUse", "src/x.rs", "C:\\App");
        assert_eq!(
            written(&e),
            Some((
                "C:\\App/src/x.rs".to_string(),
                "c:/app/src/x.rs".to_string()
            ))
        );
        let abs = tool("Write", "PostToolUse", "D:/y.rs", "C:/App");
        assert_eq!(written(&abs).unwrap().0, "D:/y.rs");
        assert_eq!(written(&tool("Bash", "PostToolUse", "x", "C:/App")), None);
        assert_eq!(written(&tool("Edit", "Stop", "x", "C:/App")), None);
    }

    #[test]
    fn an_unbroken_chain_of_own_writes_is_recorded() {
        let mut t = Turns::default();
        t.before("a", "c:/app/x.rs", b("s"));
        assert_eq!(t.begin("a", "C:/app", 1, at(0)), None);
        assert_eq!(t.tree("a"), Some("C:/app"));
        t.before("a", "c:/app/x.rs", b("s"));
        t.after("a", "c:/app/x.rs", b("m"));
        t.before("a", "c:/app/x.rs", b("m"));
        t.after("a", "c:/app/x.rs", b("e"));
        let (tree, n, r) = t.finish("a").unwrap();
        assert_eq!((tree.as_str(), n), ("C:/app", 1));
        assert_eq!(
            r.files["c:/app/x.rs"],
            Written {
                before: b("s"),
                after: b("e"),
                broken: false
            }
        );
    }

    #[test]
    fn a_change_between_own_writes_or_a_write_unseen_before_breaks_the_chain() {
        let mut t = Turns::default();
        t.begin("a", "C:/app", 1, at(0));
        t.before("a", "x", b("s"));
        t.after("a", "x", b("m"));
        // Someone else changed it to "o" before the next write.
        t.before("a", "x", b("o"));
        t.after("a", "x", b("e"));
        t.after("a", "y", b("e"));
        let r = t.record("a", 1).unwrap();
        assert!(r.files["x"].broken);
        assert!(r.files["y"].broken);
    }

    #[test]
    fn a_turn_left_open_is_handed_back_and_goes_stale() {
        let mut t = Turns::default();
        t.begin("a", "C:/app", 1, at(0));
        t.before("a", "y", None);
        let (n, r) = t.begin("a", "C:/app", 2, at(1)).unwrap();
        assert_eq!(n, 1);
        assert!(r.files.contains_key("y"));
        t.begin("b", "C:/app", 1, at(5 * 60));
        assert_eq!(t.record("a", 2), None);
        assert!(t.record("b", 1).is_some());
    }

    #[test]
    fn a_raw_diff_is_read_with_creations_and_deletions() {
        let a = "a".repeat(40);
        let bb = "b".repeat(40);
        let raw = format!(
            ":100644 100644 {a} {bb} M\0src/x.rs\0\
             :000000 100644 {NO_BLOB} {bb} A\0new.txt\0\
             :100644 000000 {a} {NO_BLOB} D\0old.txt\0\
             :160000 160000 {a} {bb} M\0vendor/sub\0"
        );
        let c = parse_diff(raw.as_bytes());
        assert_eq!(
            c,
            vec![
                change("src/x.rs", Some(&a), Some(&bb)),
                change("new.txt", None, Some(&bb)),
                change("old.txt", Some(&a), None),
            ]
        );
        assert!(parse_diff(b"").is_empty());
    }

    #[test]
    fn a_turn_owns_only_files_its_tools_took_from_start_to_end() {
        let changes = vec![
            change("mine.rs", Some("s"), Some("e")),
            change("made.rs", None, Some("e")),
            change("bash.rs", Some("s"), Some("e")),
            change("touched_before.rs", Some("s"), Some("e")),
            change("touched_after.rs", Some("s"), Some("e")),
            change("broken.rs", Some("s"), Some("e")),
        ];
        let w = |before: Blob, after: Blob, broken| Written {
            before,
            after,
            broken,
        };
        let mut r = Record::default();
        r.files
            .insert("c:/app/mine.rs".into(), w(b("s"), b("e"), false));
        r.files
            .insert("c:/app/made.rs".into(), w(None, b("e"), false));
        r.files
            .insert("c:/app/touched_before.rs".into(), w(b("o"), b("e"), false));
        r.files
            .insert("c:/app/touched_after.rs".into(), w(b("s"), b("m"), false));
        r.files
            .insert("c:/app/broken.rs".into(), w(b("s"), b("e"), true));
        let (mine, others) = attribute(changes, &r, "C:\\App");
        let mine: Vec<&str> = mine.iter().map(|c| c.path.as_str()).collect();
        assert_eq!(mine, ["mine.rs", "made.rs"]);
        assert_eq!(
            others,
            [
                "bash.rs",
                "touched_before.rs",
                "touched_after.rs",
                "broken.rs"
            ]
        );
        let (mine, others) = attribute(
            vec![change("x", Some("s"), Some("e"))],
            &Record::default(),
            "C:/app",
        );
        assert!(mine.is_empty());
        assert_eq!(others, ["x"]);
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
                left: vec!["c.rs".into()],
            },
        );
        assert_eq!(title, "Undid fix-login's last turn");
        assert_eq!(
            text,
            "Restored a.rs. Deleted b.txt. Not undone: c.rs, changed by a command, by \
             hand or by another session, not by its edit tools alone. fix-login still \
             remembers the turn."
        );
        let many: Vec<String> = ["a", "b", "c", "d", "e"].map(String::from).to_vec();
        let (title, text) = summary("s", &Outcome::Refused(many));
        assert_eq!(title, "Undo refused");
        assert!(text.starts_with("a, b, c and 2 more changed again"));
        let (title, _) = summary("s", &Outcome::NotOwned(vec!["x".into()]));
        assert_eq!(title, "Nothing undone");
    }
}
