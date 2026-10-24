//! The git half of undoing a turn: writing each turn's tree into hidden
//! refs as its hooks arrive, hashing the files its edit tools write, and
//! putting a turn's own files back.
//!
//! A snapshot never touches the working tree, the real index, HEAD or the
//! stash. It stages into a temporary index of its own and keeps the commit
//! under `refs/horadric/turns/`. The temporary index starts as a copy of
//! the last one Horadric staged in that repository, never of the real
//! index: the real one holds blob ids made through line ending filters,
//! and its stat cache would carry them into a snapshot that must hold the
//! bytes on disk. What is decided from what git says is in
//! `horadric_core::checkpoint`.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use horadric_core::checkpoint::{
    self, attribute, parse_diff, plan, prefix, prune, turn_ref, turns, written, Blob, Edge,
    Outcome, Record, Turns, KEEP,
};
use horadric_core::HookEvent;

/// The longest a snapshot, every git call of it, may hold a hook's reply.
/// A prompt's reply waits for the memory's recall after it, within Claude
/// Code's few seconds for a hook. A tree that takes longer goes without a
/// checkpoint for that turn, and its staging goes on in the background so
/// the next one is quick.
const SNAPSHOT_BUDGET: Duration = Duration::from_millis(if cfg!(test) { 30_000 } else { 1500 });

/// The longest hashing a file an edit tool writes may hold its hook.
const HASH_BUDGET: Duration = Duration::from_millis(if cfg!(test) { 30_000 } else { 500 });

/// A temporary index older than this was left by a Horadric that stopped
/// mid snapshot, never one still staging.
const STALE_TEMP: Duration = Duration::from_secs(10 * 60);

thread_local! {
    /// When git calls on this thread give up, while a hook waits on them.
    /// Each request has a thread of its own, and an undo has one with none.
    static UNTIL: std::cell::Cell<Option<Instant>> = const { std::cell::Cell::new(None) };
}

/// Bounds every git call on this thread until it is dropped.
struct Budget;

impl Budget {
    fn start(budget: Duration) -> Budget {
        UNTIL.with(|u| u.set(Some(Instant::now() + budget)));
        Budget
    }
}

impl Drop for Budget {
    fn drop(&mut self) {
        UNTIL.with(|u| u.set(None));
    }
}

fn until() -> Option<Instant> {
    UNTIL.with(std::cell::Cell::get)
}

fn open_turns() -> &'static Mutex<Turns> {
    static TURNS: OnceLock<Mutex<Turns>> = OnceLock::new();
    TURNS.get_or_init(Mutex::default)
}

/// Takes in a hook event of the Horadric session `session`: a prompt
/// snapshots the tree as the turn starts, a stop as it ends, and an edit
/// tool's file is hashed before and after it writes. Runs git, so it is
/// called on the listener's thread for the request, never the UI's. A
/// folder outside git is left alone.
pub fn hear(session: &str, event: &HookEvent) {
    if session.is_empty() || event.cwd.is_empty() {
        return;
    }
    let main_loop = !event.is_subagent();
    match event.hook_event_name.as_str() {
        "PreToolUse" | "PostToolUse" | "PostToolUseFailure" => {
            let Some((path, key)) = written(event) else {
                return;
            };
            let top = open_turns()
                .lock()
                .ok()
                .and_then(|t| t.tree(session).map(PathBuf::from));
            let Some(top) = top else {
                return;
            };
            let blob = {
                let _budget = Budget::start(HASH_BUDGET);
                hash(&top, Path::new(&path))
            };
            if let Ok(mut t) = open_turns().lock() {
                match event.hook_event_name.as_str() {
                    "PreToolUse" => t.before(session, &key, blob),
                    "PostToolUse" => t.after(session, &key, blob),
                    _ => t.failed(session, &key, blob),
                }
            }
        }
        "UserPromptSubmit" if main_loop => {
            let _budget = Budget::start(SNAPSHOT_BUDGET);
            if let Err(e) = begin(session, &event.cwd) {
                eprintln!("horadric: no checkpoint for {session}: {e}");
            }
        }
        "Stop" | "StopFailure" | "SessionEnd" if main_loop => {
            let open = open_turns().lock().ok().and_then(|mut t| t.finish(session));
            if let Some((top, n, record)) = open {
                let _budget = Budget::start(SNAPSHOT_BUDGET);
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
    let started = start_turn(session, &top);
    if started.is_err() {
        // No start, so nothing of this turn or the one before can be
        // known to its end.
        if let Ok(mut t) = open_turns().lock() {
            t.finish(session);
        }
    }
    started
}

fn start_turn(session: &str, top: &Path) -> Result<(), String> {
    let top = top.to_path_buf();
    let refs = list(&top, session)?;
    let n = checkpoint::next(&refs, session);
    let tree = write_tree(&top)?;
    let start = commit(&top, &tree, &format!("Horadric turn {n} start\n"))?;
    let mut updates = vec![format!(
        "update {} {start}",
        turn_ref(session, n, Edge::Start)
    )];
    let left = open_turns()
        .lock()
        .ok()
        .and_then(|mut t| t.begin(session, &top.to_string_lossy(), n, SystemTime::now()));
    let mut gone = prune(&refs, session, KEEP - 1);
    for t in turns(&refs, session).iter().filter(|t| !t.start || !t.end) {
        match &left {
            // An interrupted turn sends no stop, so it ends where the next
            // starts.
            Some((prev, record)) if *prev == t.n && t.start => {
                let end = commit(&top, &tree, &record.to_message(t.n))?;
                updates.push(format!(
                    "update {} {end}",
                    turn_ref(session, t.n, Edge::End)
                ));
            }
            // A turn no one saw end, through a reload or a slow snapshot,
            // can never be undone.
            _ => gone.extend(checkpoint::refs_of(session, t)),
        }
    }
    gone.sort();
    gone.dedup();
    updates.extend(gone.into_iter().map(|r| format!("delete {r}")));
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
/// wrote with its own edit tools, unless any was changed again since. The
/// turn's checkpoints go once something is undone, so the next undo
/// reaches the turn before.
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
        // Interrupted with no stop heard: it ends now. Its record still
        // holds each file as its tools left it, so a file changed by hand
        // since is not taken for the turn's.
        let open = open_turns()
            .lock()
            .ok()
            .and_then(|t| t.record(session, last.n));
        let Some(record) = open else {
            return Ok(Outcome::Unended);
        };
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
    let (mine, left) = attribute(parse_diff(&raw), &record, &top_text);
    if mine.is_empty() {
        return Ok(Outcome::NotOwned(left));
    }
    let now: HashMap<String, Blob> = mine
        .iter()
        .map(|c| (c.path.clone(), hash(top, &top.join(&c.path))))
        .collect();
    let p = plan(&mine, &now);
    if !p.refused.is_empty() {
        return Ok(Outcome::Refused(p.refused));
    }
    let outcome = if p.restore.is_empty() && p.delete.is_empty() {
        Outcome::Already
    } else {
        put_back(top, &p.restore, &p.delete)?;
        Outcome::Undone {
            restored: p.restore.into_iter().map(|(path, _)| path).collect(),
            deleted: p.delete,
            left,
        }
    };
    let gone: Vec<String> = checkpoint::refs_of(session, &last)
        .into_iter()
        .map(|r| format!("delete {r}"))
        .collect();
    // The files are back already; should this fail, the next undo finds
    // them as the start had them and says so.
    let _ = update_refs(top, &gone);
    if !last.end {
        if let Ok(mut t) = open_turns().lock() {
            t.finish(session);
        }
    }
    Ok(outcome)
}

/// Writes each path in `restore` back as its blob, and deletes each in
/// `delete`. Every blob is read before any file is touched, and a write
/// that fails puts back the ones done before it, so an undo happens whole
/// or not at all.
fn put_back(top: &Path, restore: &[(String, String)], delete: &[String]) -> Result<(), String> {
    let mut ops: Vec<(&str, Option<Vec<u8>>)> = Vec::new();
    for (path, blob) in restore {
        ops.push((
            path,
            Some(git(top, &["cat-file", "blob", blob], &[], None)?),
        ));
    }
    ops.extend(delete.iter().map(|p| (p.as_str(), None)));
    let mut done: Vec<(PathBuf, Option<Vec<u8>>)> = Vec::new();
    for (path, bytes) in ops {
        let file = top.join(path);
        let was = fs::read(&file).ok();
        let r = match &bytes {
            Some(b) => file
                .parent()
                .map_or(Ok(()), fs::create_dir_all)
                .and_then(|_| fs::write(&file, b)),
            None => fs::remove_file(&file),
        };
        if let Err(e) = r {
            for (f, was) in done.into_iter().rev() {
                let _ = match was {
                    Some(b) => fs::write(&f, b),
                    None => fs::remove_file(&f),
                };
            }
            return Err(format!("{path}: {e}"));
        }
        done.push((file, was));
    }
    for path in delete {
        remove_empty_parents(top, &top.join(path));
    }
    Ok(())
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
/// put back what was on disk, not what a checkout would make of it.
///
/// Each snapshot stages into a temporary index of its own name, so two at
/// once, in one repository or two, never wait on each other and a lock
/// left behind never sticks. It starts as a copy of the last one that
/// finished, whose stat cache spares hashing unchanged files again.
fn write_tree(top: &Path) -> Result<String, String> {
    static SERIAL: AtomicU64 = AtomicU64::new(0);
    // A repository whose staging ran past its budget stages on in the
    // background; until it is done, its turns go without, rather than
    // start one full `git add` after another.
    static STAGING: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();
    let staging = STAGING.get_or_init(Mutex::default);
    if staging.lock().is_ok_and(|s| s.contains(top)) {
        return Err("the last snapshot is still staging".into());
    }
    let seed = top.join(text(git(
        top,
        &["rev-parse", "--git-path", "horadric-turns.index"],
        &[],
        None,
    )?));
    sweep(&seed);
    let temp = seed.with_file_name(format!(
        "horadric-turn-{}-{}.index",
        std::process::id(),
        SERIAL.fetch_add(1, Ordering::Relaxed)
    ));
    if seed.is_file() {
        let _ = fs::copy(&seed, &temp);
    }
    let raw = raw(top)?;
    let env = [("GIT_INDEX_FILE", temp.as_os_str().to_owned())];
    let mut add = Command::new("git");
    crate::no_window(&mut add)
        .arg("--no-optional-locks")
        .args(&raw)
        .args(["add", "-A"])
        .current_dir(top)
        .env("GIT_INDEX_FILE", &temp)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = add.spawn().map_err(|e| format!("git add: {e}"))?;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if until().is_none_or(|u| Instant::now() < u) => {
                thread::sleep(Duration::from_millis(10))
            }
            Ok(None) => {
                // Left to finish, so the next snapshot starts from it.
                if let Ok(mut s) = staging.lock() {
                    s.insert(top.to_path_buf());
                }
                let top = top.to_path_buf();
                thread::spawn(move || {
                    match child.wait() {
                        Ok(s) if s.success() => keep(&temp, &seed),
                        _ => {
                            let _ = fs::remove_file(&temp);
                        }
                    }
                    if let Ok(mut s) = staging.lock() {
                        s.remove(&top);
                    }
                });
                return Err("staging the tree took too long".into());
            }
            Err(e) => {
                let _ = fs::remove_file(&temp);
                return Err(format!("git add: {e}"));
            }
        }
    };
    let raw: Vec<&str> = raw.iter().map(String::as_str).collect();
    let tree = if status.success() {
        git(top, &[&raw[..], &["write-tree"]].concat(), &env, None).map(text)
    } else {
        Err("git add failed".into())
    };
    if tree.is_ok() {
        keep(&temp, &seed);
    } else {
        let _ = fs::remove_file(&temp);
    }
    tree
}

/// Deletes the temporary indexes beside `seed` that a Horadric which
/// stopped mid snapshot left behind, once per repository a run.
fn sweep(seed: &Path) {
    static SWEPT: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();
    let Some(dir) = seed.parent() else {
        return;
    };
    let first = SWEPT
        .get_or_init(Mutex::default)
        .lock()
        .is_ok_and(|mut s| s.insert(dir.to_path_buf()));
    let Some(entries) = first.then(|| fs::read_dir(dir).ok()).flatten() else {
        return;
    };
    for e in entries.flatten() {
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .is_ok_and(|t| t.elapsed().unwrap_or_default() > STALE_TEMP);
        if old
            && e.file_name()
                .to_string_lossy()
                .starts_with("horadric-turn-")
        {
            let _ = fs::remove_file(e.path());
        }
    }
}

/// Makes `temp` the index the next snapshot starts from.
fn keep(temp: &Path, seed: &Path) {
    if fs::rename(temp, seed).is_err() {
        let _ = fs::remove_file(temp);
    }
}

/// The options that make git read and write files as their bytes: no
/// line ending conversion and no filters, since attributes are read from
/// the empty tree instead of the working tree. The empty tree is written
/// once per repository, which need not have it.
fn raw(top: &Path) -> Result<Vec<String>, String> {
    static EMPTY: OnceLock<Mutex<HashMap<PathBuf, String>>> = OnceLock::new();
    let known = EMPTY.get_or_init(Mutex::default);
    let cached = known.lock().ok().and_then(|k| k.get(top).cloned());
    let empty = match cached {
        Some(e) => e,
        None => {
            let e = text(git(
                top,
                &["hash-object", "-t", "tree", "-w", "--stdin"],
                &[],
                Some(b""),
            )?);
            if let Ok(mut k) = known.lock() {
                k.insert(top.to_path_buf(), e.clone());
            }
            e
        }
    };
    Ok(vec![
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

/// A file's blob as a snapshot stores it, its bytes unfiltered. Gone is
/// None; something that cannot be hashed matches no blob at all.
fn hash(top: &Path, file: &Path) -> Blob {
    if !file.exists() {
        return None;
    }
    let path = file.to_string_lossy();
    let hashed = file
        .is_file()
        .then(|| {
            git(
                top,
                &["hash-object", "--no-filters", "--", &path],
                &[],
                None,
            )
            .ok()
        })
        .flatten();
    Some(hashed.map_or_else(|| "unreadable".to_string(), text))
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
    let what = args.iter().find(|a| !a.starts_with('-')).unwrap_or(&"");
    if until().is_some_and(|u| Instant::now() >= u) {
        return Err(format!("no time left for git {what}"));
    }
    let mut child = cmd.spawn().map_err(|e| format!("git: {e}"))?;
    if let (Some(bytes), Some(mut stdin)) = (input, child.stdin.take()) {
        stdin.write_all(bytes).map_err(|e| format!("git: {e}"))?;
    }
    let out = match until() {
        None => child.wait_with_output().map_err(|e| format!("git: {e}"))?,
        Some(u) => bounded(child, u).ok_or_else(|| format!("git {what} took too long"))?,
    };
    if out.status.success() {
        Ok(out.stdout)
    } else {
        Err(format!(
            "git {what}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

/// What `child` printed once it exits, or None, with it killed, once
/// `until` comes first. Its output is read meanwhile, so a full pipe never
/// holds it up.
fn bounded(mut child: std::process::Child, until: Instant) -> Option<std::process::Output> {
    fn read<R: std::io::Read + Send + 'static>(pipe: Option<R>) -> thread::JoinHandle<Vec<u8>> {
        thread::spawn(move || {
            let mut bytes = Vec::new();
            if let Some(mut p) = pipe {
                let _ = p.read_to_end(&mut bytes);
            }
            bytes
        })
    }
    let stdout = read(child.stdout.take());
    let stderr = read(child.stderr.take());
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < until => thread::sleep(Duration::from_millis(5)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    Some(std::process::Output {
        status,
        stdout: stdout.join().unwrap_or_default(),
        stderr: stderr.join().unwrap_or_default(),
    })
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

    fn tool(hook: &str, cwd: &Path, file: &str) -> HookEvent {
        HookEvent {
            tool_name: Some("Write".into()),
            tool_input: Some(json!({ "file_path": file })),
            ..event(hook, cwd)
        }
    }

    /// The edit tool `Write` writing `text` into `file`, with its hooks.
    fn write(s: &str, dir: &Path, file: &str, text: &[u8]) {
        hear(s, &tool("PreToolUse", dir, file));
        fs::create_dir_all(dir.join(file).parent().unwrap()).unwrap();
        fs::write(dir.join(file), text).unwrap();
        hear(s, &tool("PostToolUse", dir, file));
    }

    /// Runs git for real: what the turn's own tools wrote is put back byte
    /// for byte, what a command changed is named and left, the real index
    /// never moves, and a file changed since refuses the undo.
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
        write(s, &dir, "kept.txt", b"two\n");
        write(s, &dir, "crlf.txt", b"c\r\n");
        write(s, &dir, "sub/made.txt", b"new\n");
        // A command's work, not the turn's tools'.
        fs::remove_file(dir.join("gone.txt")).unwrap();
        fs::write(dir.join("ignored.txt"), "x\n").unwrap();
        hear(s, &event("Stop", &dir));
        assert_eq!(fs::read(dir.join(".git/index")).unwrap(), index);

        let cwd = dir.to_string_lossy().into_owned();
        match undo(s, &cwd) {
            Outcome::Undone {
                restored,
                deleted,
                left,
            } => {
                assert_eq!(restored, ["crlf.txt", "kept.txt"]);
                assert_eq!(deleted, ["sub/made.txt"]);
                assert_eq!(left, ["gone.txt"]);
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(fs::read_to_string(dir.join("kept.txt")).unwrap(), "one\n");
        assert_eq!(fs::read(dir.join("crlf.txt")).unwrap(), b"a\r\nb\r\n");
        assert!(!dir.join("gone.txt").exists());
        assert!(!dir.join("sub").exists());
        assert!(dir.join("ignored.txt").exists());
        assert_eq!(fs::read(dir.join(".git/index")).unwrap(), index);
        assert_eq!(undo(s, &cwd), Outcome::Nothing);

        // A file changed after the turn refuses it, and nothing moves.
        hear(s, &event("UserPromptSubmit", &dir));
        write(s, &dir, "kept.txt", b"three\n");
        write(s, &dir, "gone.txt", b"mine\n");
        hear(s, &event("Stop", &dir));
        fs::write(dir.join("kept.txt"), "someone else\n").unwrap();
        assert_eq!(undo(s, &cwd), Outcome::Refused(vec!["kept.txt".into()]));
        assert_eq!(fs::read_to_string(dir.join("gone.txt")).unwrap(), "mine\n");

        // A file someone else changed between the turn's own writes is not
        // the turn's: nothing is undone and the turn stays.
        fs::write(dir.join("kept.txt"), "three\n").unwrap();
        hear(s, &event("UserPromptSubmit", &dir));
        write(s, &dir, "kept.txt", b"four\n");
        fs::write(dir.join("kept.txt"), "theirs\n").unwrap();
        write(s, &dir, "kept.txt", b"theirs and mine\n");
        hear(s, &event("Stop", &dir));
        assert_eq!(undo(s, &cwd), Outcome::NotOwned(vec!["kept.txt".into()]));
        assert_eq!(list(&dir, s).unwrap().len(), 4);

        forget(s, &cwd);
        assert!(list(&dir, s).unwrap().is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    /// An interrupted turn sends no stop. Undone before the next prompt it
    /// ends at the click, and a file edited by hand since is left alone.
    #[test]
    fn an_interrupted_turn_leaves_a_hand_edit_alone() {
        let dir = repo();
        fs::write(dir.join("a.txt"), "a\n").unwrap();
        fs::write(dir.join("b.txt"), "b\n").unwrap();
        let s = "undo-test-2";
        hear(s, &event("UserPromptSubmit", &dir));
        write(s, &dir, "a.txt", b"a2\n");
        write(s, &dir, "b.txt", b"b2\n");
        fs::write(dir.join("b.txt"), "by hand\n").unwrap();
        let cwd = dir.to_string_lossy().into_owned();
        match undo(s, &cwd) {
            Outcome::Undone { restored, left, .. } => {
                assert_eq!(restored, ["a.txt"]);
                assert_eq!(left, ["b.txt"]);
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(fs::read_to_string(dir.join("a.txt")).unwrap(), "a\n");
        assert_eq!(fs::read_to_string(dir.join("b.txt")).unwrap(), "by hand\n");
        // Its stop, should it come late, finds no turn to end.
        hear(s, &event("Stop", &dir));
        assert!(list(&dir, s).unwrap().is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    /// A's Write of a new file waits at a permission prompt, B makes the
    /// file, A's write fails: undo must not delete B's file.
    #[test]
    fn a_failed_write_leaves_the_file_another_session_made() {
        let dir = repo();
        let s = "undo-test-3";
        hear(s, &event("UserPromptSubmit", &dir));
        hear(s, &tool("PreToolUse", &dir, "x.rs"));
        fs::write(dir.join("x.rs"), "theirs\n").unwrap();
        hear(s, &tool("PostToolUseFailure", &dir, "x.rs"));
        hear(s, &event("Stop", &dir));
        let cwd = dir.to_string_lossy().into_owned();
        assert_eq!(undo(s, &cwd), Outcome::NotOwned(vec!["x.rs".into()]));
        assert_eq!(fs::read_to_string(dir.join("x.rs")).unwrap(), "theirs\n");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn git_gives_up_once_the_budget_is_spent() {
        let dir = repo();
        let budget = Budget::start(Duration::ZERO);
        assert!(git(&dir, &["status"], &[], None).is_err());
        drop(budget);
        assert!(git(&dir, &["status"], &[], None).is_ok());
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
