//! The git half of undoing a turn: writing each turn's tree into hidden
//! refs as its hooks arrive, and putting a turn's files back.
//!
//! A snapshot never touches the working tree, the real index, HEAD or the
//! stash. It stages into an index of its own, a copy of the real one so
//! unchanged files are not hashed again, and keeps the commit under
//! `refs/horadric/turns/`. What is decided from what git says is in
//! `horadric_core::checkpoint`.

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

use horadric_core::checkpoint::{
    self, attribute, parse_diff, plan, prefix, prune, turn_ref, turns, Edge, Outcome, Record,
    Turns, KEEP,
};
use horadric_core::HookEvent;

fn open_turns() -> &'static Mutex<Turns> {
    static TURNS: OnceLock<Mutex<Turns>> = OnceLock::new();
    TURNS.get_or_init(Mutex::default)
}

/// Takes in a hook event of the Horadric session `session`: a prompt
/// snapshots the tree as the turn starts, a stop as it ends. Runs git, so
/// it is called on the listener's thread for the request, never the UI's.
/// A folder outside git is left alone.
pub fn hear(session: &str, event: &HookEvent) {
    if session.is_empty() || event.cwd.is_empty() {
        return;
    }
    let main_loop = !event.is_subagent();
    match event.hook_event_name.as_str() {
        "PostToolUse" => {
            if let Ok(mut t) = open_turns().lock() {
                t.hear(session, event);
            }
        }
        "UserPromptSubmit" if main_loop => {
            if let Err(e) = begin(session, &event.cwd) {
                eprintln!("horadric: no checkpoint for {session}: {e}");
            }
        }
        "Stop" | "StopFailure" | "SessionEnd" if main_loop => {
            let open = open_turns().lock().ok().and_then(|mut t| t.finish(session));
            if let Some((top, n, record)) = open {
                if let Err(e) = end(session, Path::new(&top), n, &record) {
                    eprintln!("horadric: no checkpoint for {session}: {e}");
                }
            }
        }
        _ => {}
    }
}

fn begin(session: &str, cwd: &str) -> Result<(), String> {
    let Some(top) = toplevel(Path::new(cwd)) else {
        return Ok(());
    };
    let refs = list(&top, session)?;
    let n = checkpoint::next(&refs, session);
    let tree = write_tree(&top)?;
    let start = commit(&top, &tree, &format!("Horadric turn {n} start\n"))?;
    let mut updates = vec![format!(
        "update {} {start}",
        turn_ref(session, n, Edge::Start)
    )];
    let tree_key = top.to_string_lossy().replace('\\', "/").to_lowercase();
    let left = open_turns()
        .lock()
        .ok()
        .and_then(|mut t| t.begin(session, &tree_key, n, SystemTime::now()));
    // A turn interrupted sends no stop, so it ends where the next starts.
    if let Some((prev, record)) = left {
        let has_end = turns(&refs, session)
            .iter()
            .any(|t| t.n == prev && t.start && t.end);
        if !has_end && turns(&refs, session).iter().any(|t| t.n == prev) {
            let end = commit(&top, &tree, &record.to_message(prev))?;
            updates.push(format!(
                "update {} {end}",
                turn_ref(session, prev, Edge::End)
            ));
        }
    }
    for gone in prune(&refs, session, KEEP - 1) {
        updates.push(format!("delete {gone}"));
    }
    update_refs(&top, &updates)
}

fn end(session: &str, top: &Path, n: u64, record: &Record) -> Result<(), String> {
    let tree = write_tree(top)?;
    let end = commit(top, &tree, &record.to_message(n))?;
    update_refs(
        top,
        &[format!("update {} {end}", turn_ref(session, n, Edge::End))],
    )
}

/// Puts back the files the last kept turn of `session`, working in `cwd`,
/// changed, unless any was changed again since. The turn's checkpoints go
/// once it is undone, so the next undo reaches the turn before.
pub fn undo(session: &str, cwd: &str) -> Outcome {
    let Some(top) = toplevel(Path::new(cwd)) else {
        return Outcome::NoRepo;
    };
    match try_undo(session, &top) {
        Ok(o) => o,
        Err(e) => Outcome::Failed(e),
    }
}

fn try_undo(session: &str, top: &Path) -> Result<Outcome, String> {
    let refs = list(top, session)?;
    let Some(last) = turns(&refs, session).into_iter().rev().find(|t| t.start) else {
        return Ok(Outcome::Nothing);
    };
    let start = turn_ref(session, last.n, Edge::Start);
    let end = if last.end {
        turn_ref(session, last.n, Edge::End)
    } else {
        // Interrupted with no stop heard: it ends now.
        let record = open_turns()
            .lock()
            .ok()
            .and_then(|t| t.record(session, last.n))
            .unwrap_or_else(Record::unknown);
        let tree = write_tree(top)?;
        commit(top, &tree, &record.to_message(last.n))?
    };
    let message = text(git(top, &["log", "-1", "--format=%B", &end], &[], None)?);
    let record = Record::from_message(&message);
    let raw = git(
        top,
        &["diff-tree", "-r", "-z", "--no-renames", &start, &end],
        &[],
        None,
    )?;
    let top_text = top.to_string_lossy().replace('\\', "/");
    let (mine, unsure) = attribute(parse_diff(&raw), &record, &top_text);
    let now = hashes(top, mine.iter().map(|c| c.path.as_str()))?;
    let p = plan(&mine, &now);
    if !p.refused.is_empty() {
        return Ok(Outcome::Refused(p.refused));
    }
    for (path, blob) in &p.restore {
        let bytes = git(top, &["cat-file", "blob", blob], &[], None)?;
        let file = top.join(path);
        if let Some(dir) = file.parent() {
            fs::create_dir_all(dir).map_err(|e| format!("{path}: {e}"))?;
        }
        fs::write(&file, bytes).map_err(|e| format!("{path}: {e}"))?;
    }
    for path in &p.delete {
        let file = top.join(path);
        fs::remove_file(&file).map_err(|e| format!("{path}: {e}"))?;
        remove_empty_parents(top, &file);
    }
    let gone: Vec<String> = checkpoint::refs_of(session, &last)
        .into_iter()
        .map(|r| format!("delete {r}"))
        .collect();
    update_refs(top, &gone)?;
    Ok(Outcome::Undone {
        restored: p.restore.into_iter().map(|(path, _)| path).collect(),
        deleted: p.delete,
        unsure,
    })
}

/// Deletes every checkpoint of `session`, which ended for good.
pub fn forget(session: &str, cwd: &str) {
    if let Ok(mut t) = open_turns().lock() {
        t.finish(session);
    }
    let Some(top) = toplevel(Path::new(cwd)) else {
        return;
    };
    let Ok(refs) = list(&top, session) else {
        return;
    };
    let gone: Vec<String> = refs.iter().map(|r| format!("delete {r}")).collect();
    let _ = update_refs(&top, &gone);
}

/// The top of the git tree `cwd` is in, remembered since it is asked on
/// every turn. A folder outside git is asked again, as it may be made one.
fn toplevel(cwd: &Path) -> Option<PathBuf> {
    static TOPS: OnceLock<Mutex<HashMap<PathBuf, PathBuf>>> = OnceLock::new();
    let tops = TOPS.get_or_init(Mutex::default);
    if let Some(known) = tops.lock().ok()?.get(cwd) {
        return Some(known.clone());
    }
    if !cwd.is_dir() {
        return None;
    }
    let top = git(cwd, &["rev-parse", "--show-toplevel"], &[], None)
        .ok()
        .map(text)
        .filter(|t| !t.is_empty())
        .map(PathBuf::from);
    if let Some(t) = &top {
        tops.lock().ok()?.insert(cwd.to_path_buf(), t.clone());
    }
    top
}

fn list(top: &Path, session: &str) -> Result<Vec<String>, String> {
    let out = git(
        top,
        &["for-each-ref", "--format=%(refname)", &prefix(session)],
        &[],
        None,
    )?;
    Ok(text(out).lines().map(str::to_string).collect())
}

/// The tree as `git add -A` would stage it, but byte for byte: undo must
/// put back what was on disk, not what a checkout would make of it. Staged
/// into an index of Horadric's own, so the real one never changes; kept
/// between turns, so unchanged files are known by their stat and not read
/// again, which keeps a big tree's snapshot quick.
fn write_tree(top: &Path) -> Result<String, String> {
    static SERIAL: AtomicU64 = AtomicU64::new(0);
    static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());
    let _held = ONE_AT_A_TIME.lock();
    let kept = text(git(
        top,
        &["rev-parse", "--git-path", "horadric-turns.index"],
        &[],
        None,
    )?);
    let kept = top.join(kept);
    let raw = raw(top)?;
    let raw: Vec<&str> = raw.iter().map(String::as_str).collect();
    let stage = |index: &Path| {
        let env = [("GIT_INDEX_FILE", index.as_os_str().to_owned())];
        let add = [&raw[..], &["add", "-A"]].concat();
        let write = [&raw[..], &["write-tree"]].concat();
        git(top, &add, &env, None)
            .and_then(|_| git(top, &write, &env, None))
            .map(text)
    };
    match stage(&kept) {
        Ok(tree) => Ok(tree),
        // Another Horadric holds it, or it is damaged: stage afresh.
        Err(_) => {
            let temp = kept.with_file_name(format!(
                "horadric-turn-{}-{}.index",
                std::process::id(),
                SERIAL.fetch_add(1, Ordering::Relaxed)
            ));
            let tree = stage(&temp);
            let _ = fs::remove_file(&temp);
            tree
        }
    }
}

/// The options that make git read and write files as their bytes: no
/// line ending conversion and no filters, since attributes are read from
/// the empty tree instead of the working tree. The empty tree is written
/// first, as a repository need not have it.
fn raw(top: &Path) -> Result<[String; 3], String> {
    let empty = text(git(
        top,
        &["hash-object", "-t", "tree", "-w", "--stdin"],
        &[],
        Some(b""),
    )?);
    Ok([
        "-c".into(),
        "core.autocrlf=false".into(),
        format!("--attr-source={empty}"),
    ])
}

fn commit(top: &Path, tree: &str, message: &str) -> Result<String, String> {
    // Named here so a repository with no user set still takes it.
    let who = |k: &str, v: &str| (k.to_string(), std::ffi::OsString::from(v));
    let env = [
        who("GIT_AUTHOR_NAME", "Horadric"),
        who("GIT_AUTHOR_EMAIL", "horadric@localhost"),
        who("GIT_COMMITTER_NAME", "Horadric"),
        who("GIT_COMMITTER_EMAIL", "horadric@localhost"),
    ];
    let env: Vec<(&str, std::ffi::OsString)> =
        env.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();
    git(top, &["commit-tree", tree], &env, Some(message.as_bytes())).map(text)
}

fn update_refs(top: &Path, lines: &[String]) -> Result<(), String> {
    if lines.is_empty() {
        return Ok(());
    }
    let input = lines.join("\n") + "\n";
    git(top, &["update-ref", "--stdin"], &[], Some(input.as_bytes())).map(|_| ())
}

/// Each path's blob as `git add` would make it, None where it is gone.
fn hashes<'a>(
    top: &Path,
    paths: impl Iterator<Item = &'a str>,
) -> Result<HashMap<String, Option<String>>, String> {
    let mut out = HashMap::new();
    let mut there = Vec::new();
    for p in paths {
        if top.join(p).is_file() {
            there.push(p.to_string());
        } else if top.join(p).exists() {
            // A folder where the file was: changed, by someone.
            out.insert(p.to_string(), Some("not a file".to_string()));
        } else {
            out.insert(p.to_string(), None);
        }
    }
    if there.is_empty() {
        return Ok(out);
    }
    let input = there.join("\n") + "\n";
    let hashed = text(git(
        top,
        &["hash-object", "--no-filters", "--stdin-paths"],
        &[],
        Some(input.as_bytes()),
    )?);
    for (p, h) in there.into_iter().zip(hashed.lines()) {
        out.insert(p, Some(h.to_string()));
    }
    Ok(out)
}

/// Deletes the folders a deleted file leaves empty, up to the tree's top.
fn remove_empty_parents(top: &Path, file: &Path) {
    let mut dir = file.parent();
    while let Some(d) = dir.filter(|d| d.starts_with(top) && *d != top) {
        if fs::remove_dir(d).is_err() {
            break;
        }
        dir = d.parent();
    }
}

fn text(bytes: Vec<u8>) -> String {
    String::from_utf8_lossy(&bytes).trim().to_string()
}

fn git(
    dir: &Path,
    args: &[&str],
    env: &[(&str, std::ffi::OsString)],
    input: Option<&[u8]>,
) -> Result<Vec<u8>, String> {
    let mut cmd = Command::new("git");
    crate::no_window(&mut cmd)
        .arg("--no-optional-locks")
        .args(["-c", "core.quotepath=off"])
        .args(args)
        .current_dir(dir)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in env {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().map_err(|e| format!("git: {e}"))?;
    if let (Some(bytes), Some(mut stdin)) = (input, child.stdin.take()) {
        stdin.write_all(bytes).map_err(|e| format!("git: {e}"))?;
    }
    let out = child.wait_with_output().map_err(|e| format!("git: {e}"))?;
    if out.status.success() {
        Ok(out.stdout)
    } else {
        Err(format!(
            "git {}: {}",
            args.first().unwrap_or(&""),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn repo() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "horadric-checkpoint-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "-q"], &[], None).unwrap();
        dir
    }

    fn event(name: &str, cwd: &Path) -> HookEvent {
        HookEvent {
            cwd: cwd.to_string_lossy().into_owned(),
            ..HookEvent::synthetic(name)
        }
    }

    fn wrote(cwd: &Path, file: &str) -> HookEvent {
        HookEvent {
            tool_name: Some("Write".into()),
            tool_input: Some(json!({ "file_path": file })),
            ..event("PostToolUse", cwd)
        }
    }

    /// Runs git for real: a turn that edits, makes and deletes files is put
    /// back, the real index and the stash never move, and a file changed
    /// since refuses the undo.
    #[test]
    fn a_turn_is_snapshotted_and_undone_in_a_real_repository() {
        let dir = repo();
        fs::write(dir.join("kept.txt"), "one\n").unwrap();
        fs::write(dir.join("gone.txt"), "old\n").unwrap();
        fs::write(dir.join(".gitignore"), "ignored.txt\n").unwrap();
        // Line endings a checkout would change must come back as they were.
        fs::write(dir.join(".gitattributes"), "* text=auto eol=lf\n").unwrap();
        fs::write(dir.join("crlf.txt"), "a\r\nb\r\n").unwrap();
        git(&dir, &["add", "kept.txt"], &[], None).unwrap();
        let index = fs::read(dir.join(".git/index")).unwrap();

        let s = "undo-test-1";
        hear(s, &event("UserPromptSubmit", &dir));
        fs::write(dir.join("kept.txt"), "two\n").unwrap();
        fs::write(dir.join("crlf.txt"), "c\r\n").unwrap();
        fs::remove_file(dir.join("gone.txt")).unwrap();
        fs::create_dir_all(dir.join("sub")).unwrap();
        fs::write(dir.join("sub/made.txt"), "new\n").unwrap();
        fs::write(dir.join("ignored.txt"), "x\n").unwrap();
        hear(s, &wrote(&dir, "kept.txt"));
        hear(s, &event("Stop", &dir));
        assert_eq!(fs::read(dir.join(".git/index")).unwrap(), index);

        let cwd = dir.to_string_lossy().into_owned();
        match undo(s, &cwd) {
            Outcome::Undone {
                restored,
                deleted,
                unsure,
            } => {
                assert_eq!(restored.len(), 3, "{restored:?}");
                assert_eq!(deleted, ["sub/made.txt"]);
                assert!(unsure.is_empty());
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(fs::read_to_string(dir.join("kept.txt")).unwrap(), "one\n");
        assert_eq!(fs::read_to_string(dir.join("gone.txt")).unwrap(), "old\n");
        assert_eq!(fs::read(dir.join("crlf.txt")).unwrap(), b"a\r\nb\r\n");
        assert!(!dir.join("sub").exists());
        assert!(dir.join("ignored.txt").exists());
        assert_eq!(fs::read(dir.join(".git/index")).unwrap(), index);
        assert_eq!(undo(s, &cwd), Outcome::Nothing);

        // A file changed after the turn refuses it, and nothing moves.
        hear(s, &event("UserPromptSubmit", &dir));
        fs::write(dir.join("kept.txt"), "three\n").unwrap();
        fs::write(dir.join("gone.txt"), "mine\n").unwrap();
        hear(s, &event("Stop", &dir));
        fs::write(dir.join("kept.txt"), "someone else\n").unwrap();
        assert_eq!(undo(s, &cwd), Outcome::Refused(vec!["kept.txt".into()]));
        assert_eq!(fs::read_to_string(dir.join("gone.txt")).unwrap(), "mine\n");

        forget(s, &cwd);
        assert!(list(&dir, s).unwrap().is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_folder_outside_git_is_left_alone() {
        let dir = std::env::temp_dir().join(format!("horadric-nogit-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        hear("x", &event("UserPromptSubmit", &dir));
        assert_eq!(undo("x", &dir.to_string_lossy()), Outcome::NoRepo);
        let _ = fs::remove_dir_all(&dir);
    }
}
