//! Handing a session to another brother: the bundle that carries it (see
//! docs/plan/the-brotherhood.md, "Handing off").
//!
//! The work travels as a commit on a private ref, cut into a `git bundle`
//! against a commit both brothers have, so a handoff never pushes and needs
//! no remote. The transcript travels as its lines, filed on the receiver
//! under the folder key Claude Code derives from the new path. The rest is
//! the session's record and the line its next prompt starts with.
//!
//! Unlike most of this crate the git half runs `git`. It is here rather
//! than in the UI because a headless Linux node hands off too.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

use serde::{Deserialize, Serialize};

use crate::release::{base64_decode, base64_encode};

/// Where a session's handed off work is kept on both brothers. Outside
/// `refs/heads`, so no branch list, no `git push` and no GUI ever shows it.
pub const HANDOFF_REFS: &str = "refs/horadric/handoff/";

/// The relay drops a message over 1 MB. Base64 makes these 4/3 as long,
/// which leaves room for the rest of the message.
pub const PART_BYTES: usize = 512 * 1024;

/// The most parts a bundle may claim to have, a gigabyte of them, so a
/// brother's `of` cannot make a joiner set aside more than that.
pub const MAX_PARTS: usize = 2048;

/// The private ref for a session's handoff, None when the session id
/// could not be a ref name or could climb out of the namespace.
pub fn handoff_ref(session: &str) -> Option<String> {
    private_ref(HANDOFF_REFS, session)
}

/// `prefix` and `session` as one ref. Session ids are words, dots and
/// hyphens; anything else is refused rather than escaped, so two ids never
/// share a ref.
pub(crate) fn private_ref(prefix: &str, session: &str) -> Option<String> {
    let fine = !session.is_empty()
        && !session.starts_with('.')
        && !session.ends_with('.')
        && !session.ends_with(".lock")
        && !session.contains("..")
        && session
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.');
    fine.then(|| format!("{prefix}{session}"))
}

/// Runs git in `dir` with `env` set: what it printed, or what it said
/// when it failed.
pub(crate) fn git(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Result<String, String> {
    let mut cmd = Command::new("git");
    cmd.args(args)
        .current_dir(dir)
        .envs(env.iter().copied())
        .stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // Run from a windowed app, every git call would flash a console.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let out = cmd.output().map_err(|e| format!("cannot run git: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        let said = String::from_utf8_lossy(&out.stderr).trim().to_string();
        Err(if said.is_empty() {
            format!("git {} failed", args.join(" "))
        } else {
            said
        })
    }
}

/// The commit a ref or name points at, None when there is none (an unborn
/// HEAD, a missing ref).
pub(crate) fn rev(dir: &Path, name: &str) -> Option<String> {
    let spec = format!("{name}^{{commit}}");
    git(dir, &["rev-parse", "--verify", "-q", &spec], &[])
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// A temporary index beside nothing the human uses. Named per process and
/// call, since two sessions of one brother hand off at once.
fn scratch_index() -> PathBuf {
    static COUNT: AtomicU32 = AtomicU32::new(0);
    let n = COUNT.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("horadric-index-{}-{n}", std::process::id()))
}

/// Commits every change in `dir`'s working tree, untracked files included
/// (ignored ones not), onto `refname`, and returns the commit. The index,
/// HEAD and every branch are left as they were: the tree is built in a
/// scratch index, and the commit's parent is HEAD, or none on an unborn
/// branch. The author is fixed so a brother without `user.name` can still
/// hand off.
pub fn commit_work(dir: &Path, refname: &str, message: &str) -> Result<String, String> {
    let index = scratch_index();
    let index_str = index.to_string_lossy().into_owned();
    let result = commit_in(dir, refname, message, &index_str);
    let _ = std::fs::remove_file(&index);
    result
}

fn commit_in(dir: &Path, refname: &str, message: &str, index: &str) -> Result<String, String> {
    let head = rev(dir, "HEAD");
    // Starting from a copy of the real index keeps its stat cache, so
    // `add -A` hashes only what changed instead of the whole tree.
    let real = git(
        dir,
        &["rev-parse", "--path-format=absolute", "--git-path", "index"],
        &[],
    )?;
    let real = PathBuf::from(real.trim());
    if real.is_file() {
        std::fs::copy(&real, index).map_err(|e| format!("cannot copy the index: {e}"))?;
    }
    let env = [
        ("GIT_INDEX_FILE", index),
        ("GIT_AUTHOR_NAME", "Horadric"),
        ("GIT_AUTHOR_EMAIL", "horadric@localhost"),
        ("GIT_COMMITTER_NAME", "Horadric"),
        ("GIT_COMMITTER_EMAIL", "horadric@localhost"),
    ];
    git(dir, &["add", "-A", "."], &env)?;
    let tree = git(dir, &["write-tree"], &env)?;
    let tree = tree.trim();
    let mut args = vec!["commit-tree", tree, "-m", message];
    if let Some(head) = &head {
        args.extend(["-p", head.as_str()]);
    }
    let commit = git(dir, &args, &env)?.trim().to_string();
    git(dir, &["update-ref", refname, &commit], &[])?;
    Ok(commit)
}

/// The commits this brother has that a sender can cut a bundle against:
/// HEAD and every ref's commit, newest knowledge first, no repeats. What
/// the receiver answers when a sender asks.
pub fn known_commits(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    out.extend(rev(dir, "HEAD"));
    if let Ok(refs) = git(
        dir,
        &["for-each-ref", "--format=%(objectname) %(objecttype)"],
        &[],
    ) {
        out.extend(
            refs.lines()
                .filter_map(|l| l.split_once(' '))
                .filter(|(_, kind)| *kind == "commit")
                .map(|(oid, _)| oid.to_string()),
        );
    }
    let mut seen = HashSet::new();
    out.retain(|c| seen.insert(c.clone()));
    out
}

/// The best base for a bundle: the first commit of `history` (what
/// `git rev-list <tip>` printed, newest first) that the receiver knows.
/// The newest common commit leaves the least to send. None means nothing
/// in common, so the whole history goes.
pub fn best_base(history: &str, known: &[String]) -> Option<String> {
    let known: HashSet<&str> = known.iter().map(|s| s.trim()).collect();
    history
        .lines()
        .map(str::trim)
        .find(|c| known.contains(c))
        .map(str::to_string)
}

/// Writes a bundle of `refname` to `out`, against the newest commit of the
/// receiver's `known` that the ref's history holds, and returns that base.
pub fn cut_bundle(
    dir: &Path,
    refname: &str,
    known: &[String],
    out: &Path,
) -> Result<Option<String>, String> {
    let history = git(dir, &["rev-list", refname], &[])?;
    let base = best_base(&history, known);
    bundle_against(dir, refname, base.as_deref(), out)?;
    Ok(base)
}

/// Writes a bundle of `refname` to `out` holding what `base` lacks, or
/// everything without one. `base` need not be an ancestor: git leaves out
/// whatever it reaches.
pub fn bundle_against(
    dir: &Path,
    refname: &str,
    base: Option<&str>,
    out: &Path,
) -> Result<(), String> {
    let out_str = out.to_string_lossy();
    let not_base = base.map(|b| format!("^{b}"));
    let mut args = vec!["bundle", "create", "-q", &out_str, refname];
    if let Some(n) = &not_base {
        args.push(n);
    }
    git(dir, &args, &[]).map(drop)
}

/// Fetches `refname` out of the bundle at `bundle` into `dir`'s
/// repository, under the same private ref, and returns its commit.
pub fn fetch_bundle(dir: &Path, bundle: &Path, refname: &str) -> Result<String, String> {
    let bundle = bundle.to_string_lossy();
    git(dir, &["bundle", "verify", "-q", &bundle], &[])?;
    let spec = format!("+{refname}:{refname}");
    // Fetch otherwise starts a detached gc or maintenance run, which on
    // Linux was still writing into a keeper's repository as it was pruned.
    let quiet = [
        "-c",
        "gc.auto=0",
        "-c",
        "maintenance.auto=false",
        "fetch",
        "-q",
        "--no-tags",
    ];
    let args: Vec<&str> = quiet.iter().copied().chain([&*bundle, &spec]).collect();
    git(dir, &args, &[])?;
    rev(dir, refname).ok_or_else(|| format!("the bundle held no {refname}"))
}

/// Applies a received bundle in `repo` (the receiver's checkout of the
/// project): fetches the work onto the session's private ref, then makes a
/// worktree at `folder` on a new `branch` from the sender's HEAD, with the
/// sender's changes laid on top as changes, untracked files untracked and
/// nothing staged. So the session finds its tree as it left it, and the
/// receiver's own checkout is never touched. Returns the handoff commit.
pub fn apply_bundle(
    repo: &Path,
    bundle: &Path,
    session: &str,
    folder: &Path,
    branch: &str,
) -> Result<String, String> {
    let refname = handoff_ref(session).ok_or("not a session id")?;
    let commit = fetch_bundle(repo, bundle, &refname)?;
    let parent = rev(repo, &format!("{commit}^"))
        .ok_or("the sender had no commit yet, so there is nothing to branch from")?;
    let folder_str = folder.to_string_lossy();
    git(
        repo,
        &["worktree", "add", "-q", "-b", branch, &folder_str, &parent],
        &[],
    )?;
    // read-tree moves the tree to the handoff's, deleting what the sender
    // deleted, then a mixed reset puts the index back on HEAD so all of it
    // is changes again rather than staged.
    git(folder, &["read-tree", "-u", "--reset", &commit], &[])?;
    git(folder, &["reset", "-q"], &[])?;
    Ok(commit)
}

/// What identifies a repository across brothers: its remotes' URLs,
/// normalised, and its first commits.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoId {
    #[serde(default)]
    pub remotes: Vec<String>,
    #[serde(default)]
    pub roots: Vec<String>,
}

/// `dir`'s repository's identity, None when it is not in one.
pub fn repo_id(dir: &Path) -> Option<RepoId> {
    git(dir, &["rev-parse", "--git-dir"], &[]).ok()?;
    // The URLs as written, like remote_urls: an insteadOf rewrite is this
    // machine's alone, so the same repository must match on a brother
    // without it.
    let remotes = remote_urls(dir)
        .into_iter()
        .map(|(_, u)| normalize_remote(&u))
        .filter(|u| !u.is_empty())
        .collect();
    let mut roots: Vec<String> = git(dir, &["rev-list", "--max-parents=0", "HEAD"], &[])
        .unwrap_or_default()
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    roots.sort();
    Some(RepoId { remotes, roots })
}

/// A remote URL in one spelling for every way of writing it:
/// `git@github.com:Mopra/x.git`, `https://user@GitHub.com/Mopra/x/` and
/// `ssh://git@github.com/Mopra/x` all give `github.com/Mopra/x`. The host
/// is lowercased; the path is not, since some hosts tell case apart.
pub fn normalize_remote(url: &str) -> String {
    let url = url.trim();
    let (rest, scp) = match url.split_once("://") {
        Some((_, rest)) => (rest, false),
        None => (url, true),
    };
    // scp style is host:path, unless the colon is a drive letter's.
    let (host, path) = if scp && !looks_like_local(rest) {
        match rest.split_once(':') {
            Some((h, p)) => (h, p),
            None => ("", rest),
        }
    } else if scp {
        ("", rest)
    } else {
        rest.split_once('/').unwrap_or((rest, ""))
    };
    let host = host.rsplit_once('@').map_or(host, |(_, h)| h);
    let host = host
        .strip_suffix(":22")
        .unwrap_or(host)
        .to_ascii_lowercase();
    let path = path.replace('\\', "/");
    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let path = path.trim_end_matches('/');
    if host.is_empty() {
        path.to_string()
    } else {
        format!("{host}/{path}")
    }
}

fn looks_like_local(s: &str) -> bool {
    let b = s.as_bytes();
    s.starts_with('/')
        || s.starts_with('.')
        || (b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' && b.len() == 2)
        || (b.len() >= 3
            && b[0].is_ascii_alphabetic()
            && b[1] == b':'
            && matches!(b[2], b'\\' | b'/'))
}

/// How sure a match is: a shared remote names the project, a shared first
/// commit only says the histories began alike (a fork does too).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Match {
    Root,
    Remote,
}

/// Whether `a` and `b` are one project, and by what. A shared remote
/// wins; first commits count only when one side has no remote to compare.
pub fn same_repo(a: &RepoId, b: &RepoId) -> Option<Match> {
    if a.remotes.iter().any(|r| b.remotes.contains(r)) {
        return Some(Match::Remote);
    }
    let either_bare = a.remotes.is_empty() || b.remotes.is_empty();
    (either_bare && a.roots.iter().any(|r| b.roots.contains(r))).then_some(Match::Root)
}

/// The receiver's project for the sender's `wanted`, from its own
/// `(folder, identity)` list: a remote match before a first commit match,
/// and the first listed among equals. None means ask the human.
pub fn find_project<'a>(wanted: &RepoId, mine: &'a [(String, RepoId)]) -> Option<&'a str> {
    mine.iter()
        .filter_map(|(folder, id)| same_repo(wanted, id).map(|m| (m, folder)))
        .fold(None::<(Match, &String)>, |best, (m, f)| match best {
            Some((b, _)) if b >= m => best,
            _ => Some((m, f)),
        })
        .map(|(_, f)| f.as_str())
}

/// Claude Code names the folder holding a working directory's transcripts
/// after the path, every character that is not an ASCII letter or digit a
/// hyphen: `C:\Users\morte\x.y` is `C--Users-morte-x-y`, `/home/m/x` is
/// `-home-m-x`. A trailing separator is not part of the name.
///
/// Claude Code cuts a name past 200 characters and adds a hash of the
/// path, which is not reproduced here: for such a path this gives the
/// first 200 characters, and [`key_matches`] finds the folder by them.
pub fn transcript_key(cwd: &str) -> String {
    let trimmed = cwd.trim_end_matches(['\\', '/']);
    let trimmed = if trimmed.is_empty() { cwd } else { trimmed };
    let key: String = trimmed
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    key.chars().take(KEY_LONGEST).collect()
}

/// The longest folder name Claude Code writes before it hashes the rest.
const KEY_LONGEST: usize = 200;

/// Whether the transcripts folder named `folder` holds `cwd`'s sessions.
pub fn key_matches(folder: &str, cwd: &str) -> bool {
    let key = transcript_key(cwd);
    if key.len() < KEY_LONGEST {
        return folder == key;
    }
    folder
        .strip_prefix(&key)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('-'))
}

/// Where Claude Code files the transcript of session `id` held in `cwd`,
/// under its projects folder `root` (`~/.claude/projects`).
pub fn transcript_path(root: &Path, cwd: &str, id: &str) -> PathBuf {
    root.join(transcript_key(cwd)).join(format!("{id}.jsonl"))
}

/// Whether a path is written the Windows way, a drive or a UNC share.
fn windows_path(p: &str) -> bool {
    let b = p.as_bytes();
    (b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':') || p.starts_with(r"\\")
}

/// `path` moved from under `from` to under `to`, its separators turned to
/// `to`'s kind. None when it is not under `from`. Windows paths compare
/// without case, as Windows does.
pub fn move_path(path: &str, from: &str, to: &str) -> Option<String> {
    let from = from.trim_end_matches(['\\', '/']);
    let to = to.trim_end_matches(['\\', '/']);
    let rest = if windows_path(from) {
        let head = path.get(..from.len())?;
        let head_norm = head.replace('/', "\\");
        if !head_norm.eq_ignore_ascii_case(&from.replace('/', "\\")) {
            return None;
        }
        &path[from.len()..]
    } else {
        path.strip_prefix(from)?
    };
    if !(rest.is_empty() || rest.starts_with(['\\', '/'])) {
        return None;
    }
    let (sep, other) = if windows_path(to) {
        ('\\', '/')
    } else {
        ('/', '\\')
    };
    Some(format!("{to}{}", rest.replace(other, &sep.to_string())))
}

/// One transcript line with its `cwd` moved from under `from` to under
/// `to`, everything else byte for byte as it was. A line that is not
/// JSON, has no `cwd`, or a `cwd` elsewhere, is returned unchanged.
pub fn rewrite_cwd_line(line: &str, from: &str, to: &str) -> String {
    let Ok(serde_json::Value::Object(map)) = serde_json::from_str::<serde_json::Value>(line) else {
        return line.to_string();
    };
    let Some(old) = map.get("cwd").and_then(|v| v.as_str()) else {
        return line.to_string();
    };
    let Some(new) = move_path(old, from, to) else {
        return line.to_string();
    };
    // Replaced as text, since reserialising would reorder the keys and
    // turn every other field into serde's spelling of it.
    let old_field = format!("\"cwd\":{}", serde_json::Value::from(old));
    let new_field = format!("\"cwd\":{}", serde_json::Value::from(new));
    line.replacen(&old_field, &new_field, 1)
}

/// A whole transcript's `cwd` fields moved, see [`rewrite_cwd_line`].
/// Line endings are kept as they were.
pub fn rewrite_transcript(text: &str, from: &str, to: &str) -> String {
    text.split_inclusive('\n')
        .map(|l| {
            let body = l.trim_end_matches(['\r', '\n']);
            let end = &l[body.len()..];
            format!("{}{end}", rewrite_cwd_line(body, from, to))
        })
        .collect()
}

/// Every absolute path under `from` in a transcript's JSON text moved to
/// under `to`, separators and all, wherever it is: a large tool output is
/// kept in a file under the transcripts folder, and the transcript names
/// it by its whole path, in a field or inside a message. Windows paths
/// match without case, as Claude Code writes both spellings of a drive.
pub fn rewrite_paths(text: &str, from: &str, to: &str) -> String {
    let from = from.trim_end_matches(['\\', '/']);
    let to = to.trim_end_matches(['\\', '/']);
    if from.is_empty() {
        return text.to_string();
    }
    let escape = |s: &str| {
        let quoted = serde_json::Value::from(s).to_string();
        quoted[1..quoted.len() - 1].to_string()
    };
    let to_esc = escape(to);
    let to_windows = windows_path(to);
    // Lowercasing ASCII keeps every byte where it was, so offsets found
    // in the folded text hold in the original.
    let (hay, needle) = if windows_path(from) {
        (text.to_ascii_lowercase(), escape(from).to_ascii_lowercase())
    } else {
        (text.to_string(), escape(from))
    };
    let mut out = String::with_capacity(text.len());
    let mut done = 0;
    let mut look = 0;
    while let Some(at) = hay[look..].find(&needle) {
        let start = look + at;
        let end = start + needle.len();
        match path_tail(&text[end..], to_windows) {
            Some((tail, used)) => {
                out.push_str(&text[done..start]);
                out.push_str(&to_esc);
                out.push_str(&tail);
                done = end + used;
                look = done;
            }
            None => look = end,
        }
    }
    out.push_str(&text[done..]);
    out
}

/// The rest of a JSON escaped path after its folder, its separators
/// turned to the target's kind, and how many bytes it took. None when the
/// folder's name goes on (`x` matched inside `xy`), so it is not this
/// folder at all.
fn path_tail(rest: &str, to_windows: bool) -> Option<(String, usize)> {
    let sep = if to_windows { "\\\\" } else { "/" };
    // A quote, a space or an escape other than a backslash (`\n`, `\"`)
    // ends a path inside JSON text.
    let ends = |s: &str| {
        s.is_empty()
            || s.starts_with(['"', ' ', '\t'])
            || (s.starts_with('\\') && !s.starts_with("\\\\"))
    };
    if !(ends(rest) || rest.starts_with("\\\\") || rest.starts_with('/')) {
        return None;
    }
    let mut tail = String::new();
    let mut i = 0;
    while !ends(&rest[i..]) {
        if rest[i..].starts_with("\\\\") {
            tail.push_str(sep);
            i += 2;
        } else if rest[i..].starts_with('/') {
            tail.push_str(sep);
            i += 1;
        } else {
            let c = rest[i..].chars().next()?;
            tail.push(c);
            i += c.len_utf8();
        }
    }
    Some((tail, i))
}

/// One file of a session's transcript, by its path inside the transcripts
/// folder with `/` between parts: `<id>.jsonl`, and everything in `<id>/`
/// beside it (the subagents' transcripts and their `.meta.json`, large tool
/// outputs in `tool-results`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptFile {
    pub path: String,
    pub bytes: Vec<u8>,
}

/// A conversation id is a file name, never a way out of the folder.
fn plain_id(id: &str) -> bool {
    !id.is_empty() && !id.contains(['/', '\\', '.', ':'])
}

/// Whether a path from the wire stays inside the folder it is written to.
pub fn safe_relative(path: &str) -> bool {
    !path.is_empty()
        && !path.contains(['\\', ':', '\0'])
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

/// The folder under `root` holding session `id`, held in `cwd`: the one its
/// key names, or failing that the same key in other case, since Claude
/// Code keeps `C:` and `c:` apart.
pub fn transcript_folder(root: &Path, cwd: &str, id: &str) -> Option<PathBuf> {
    if !plain_id(id) {
        return None;
    }
    let file = format!("{id}.jsonl");
    let key = transcript_key(cwd);
    let exact = root.join(&key);
    if exact.join(&file).is_file() {
        return Some(exact);
    }
    let lower = cwd.to_ascii_lowercase();
    std::fs::read_dir(root)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .find(|p| {
            let named = p
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| key_matches(&n.to_ascii_lowercase(), &lower));
            named && p.join(&file).is_file()
        })
}

/// Every file of session `id`'s transcript in `folder`, the main one first.
pub fn gather_transcript(folder: &Path, id: &str) -> Result<Vec<TranscriptFile>, String> {
    if !plain_id(id) {
        return Err("not a session id".into());
    }
    let main = format!("{id}.jsonl");
    let bytes = std::fs::read(folder.join(&main)).map_err(|e| format!("{main}: {e}"))?;
    let mut out = vec![TranscriptFile { path: main, bytes }];
    let side = folder.join(id);
    if side.is_dir() {
        walk(&side, id, &mut out)?;
    }
    Ok(out)
}

fn walk(dir: &Path, rel: &str, out: &mut Vec<TranscriptFile>) -> Result<(), String> {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .map_err(|e| format!("{rel}: {e}"))?
        .flatten()
        .collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let Some(name) = e.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let path = format!("{rel}/{name}");
        let kind = e.file_type().map_err(|err| format!("{path}: {err}"))?;
        if kind.is_dir() {
            walk(&e.path(), &path, out)?;
        } else if kind.is_file() {
            let bytes = std::fs::read(e.path()).map_err(|err| format!("{path}: {err}"))?;
            out.push(TranscriptFile { path, bytes });
        }
    }
    Ok(())
}

/// A transcript file's text as it is filed on the receiver: each line's
/// `cwd` moved from `from_cwd` to `to_cwd`, and every path under the
/// sender's transcripts folder `from_folder` moved to `to_folder`.
pub fn rewrite_moved(
    text: &str,
    from_folder: &str,
    to_folder: &str,
    from_cwd: &str,
    to_cwd: &str,
) -> String {
    rewrite_paths(
        &rewrite_transcript(text, from_cwd, to_cwd),
        from_folder,
        to_folder,
    )
}

/// Writes a handed off session's transcript files into `to_folder`, the
/// receiver's transcripts folder for the session's new working directory,
/// JSONL rewritten by [`rewrite_moved`] and every other file as it came.
pub fn file_transcript(
    files: &[TranscriptFile],
    to_folder: &Path,
    from_folder: &str,
    from_cwd: &str,
    to_cwd: &str,
) -> Result<(), String> {
    let to_text = to_folder.to_string_lossy();
    for f in files {
        if !safe_relative(&f.path) {
            return Err(format!("{} would leave the folder", f.path));
        }
        let dest = f
            .path
            .split('/')
            .fold(to_folder.to_path_buf(), |p, part| p.join(part));
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let bytes = match std::str::from_utf8(&f.bytes) {
            Ok(text) if f.path.ends_with(".jsonl") => {
                rewrite_moved(text, from_folder, &to_text, from_cwd, to_cwd).into_bytes()
            }
            _ => f.bytes.clone(),
        };
        std::fs::write(&dest, bytes).map_err(|e| format!("{}: {e}", f.path))?;
    }
    Ok(())
}

/// Everything a handoff sends, before it is split into parts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Package {
    pub record: Record,
    pub files: Vec<TranscriptFile>,
    /// The work as a `git bundle`, empty for a folder without git.
    pub work: Vec<u8>,
}

#[derive(Serialize, Deserialize)]
struct Header {
    record: Record,
    files: Vec<(String, usize)>,
    work: usize,
}

/// A package as bytes: a JSON line naming the record and each file's
/// path and length, then the files' bytes in order, then the work.
pub fn pack(p: &Package) -> Vec<u8> {
    let header = Header {
        record: p.record.clone(),
        files: p
            .files
            .iter()
            .map(|f| (f.path.clone(), f.bytes.len()))
            .collect(),
        work: p.work.len(),
    };
    let mut out = serde_json::to_vec(&header).unwrap_or_default();
    out.push(b'\n');
    for f in &p.files {
        out.extend(&f.bytes);
    }
    out.extend(&p.work);
    out
}

/// Bytes made by [`pack`], None when they are not, or when a file's path
/// would leave the folder it is filed in.
pub fn unpack(bytes: &[u8]) -> Option<Package> {
    let end = bytes.iter().position(|&b| b == b'\n')?;
    let header: Header = serde_json::from_slice(&bytes[..end]).ok()?;
    let mut rest = &bytes[end + 1..];
    let mut files = Vec::with_capacity(header.files.len());
    for (path, len) in header.files {
        if !safe_relative(&path) || len > rest.len() {
            return None;
        }
        let (body, after) = rest.split_at(len);
        files.push(TranscriptFile {
            path,
            bytes: body.to_vec(),
        });
        rest = after;
    }
    (rest.len() == header.work).then(|| Package {
        record: header.record,
        files,
        work: rest.to_vec(),
    })
}

/// The session's record, sent beside its transcript and work.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    /// Claude Code's conversation id, what `claude --resume` takes.
    pub session: String,
    /// Horadric's id for the session, kept on the receiver so its tile on
    /// every brother is the same tile.
    #[serde(default)]
    pub id: String,
    pub name: String,
    /// The project folder on the sender.
    pub project: String,
    /// The folder the session ran in on the sender, its worktree if it had
    /// one. The transcript's `cwd` fields are moved from under it.
    pub cwd: String,
    /// Where `cwd` is inside its repository, `src/ui/` or empty at the
    /// top, so the session lands in the same folder of its new worktree.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub prefix: String,
    /// So the receiver finds its own checkout of the project.
    #[serde(default)]
    pub repo: RepoId,
    pub agent: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quest: Option<String>,
    /// The branch it was on, so the receiver's worktree can carry its name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// The sender's transcripts folder for `cwd`, the folder whose paths
    /// the transcript names its large tool outputs by.
    #[serde(default)]
    pub transcripts: String,
    /// The handoff commit on the private ref.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    /// The brother it comes from.
    pub from: String,
    /// The roster's number for the session, one higher once it runs here.
    #[serde(default)]
    pub n: u64,
    /// Its last turn was interrupted for the move rather than ending by
    /// itself, which its first line tells it.
    #[serde(default)]
    pub interrupted: bool,
    /// The work is a plain copy of a folder without git (see
    /// [`crate::placing`]) rather than a bundle.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub plain: bool,
    /// The files a plain copy left behind as too large, which its first
    /// line names.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub left: Vec<String>,
}

/// The line a handed off session's next prompt starts with.
pub fn moved_line(from: &str, to: &str, worktree: &str, clean: bool) -> String {
    let ended = if clean {
        "the last turn ended cleanly"
    } else {
        "the last turn was interrupted"
    };
    format!(
        "Moved from {from} to {to}, the work is in this worktree ({worktree}), {ended}. \
         Paths you remember may be wrong now."
    )
}

/// The line a session handed off from a folder without git starts with:
/// the folder is a copy, and what it left behind as too large.
pub fn moved_plain_line(
    from: &str,
    to: &str,
    folder: &str,
    clean: bool,
    left: &[String],
) -> String {
    let ended = if clean {
        "the last turn ended cleanly"
    } else {
        "the last turn was interrupted"
    };
    let left = match left {
        [] => String::new(),
        _ if left.len() > 10 => format!(
            " {} files were too large to copy and stayed on {from}, {} among them.",
            left.len(),
            left[..10].join(", ")
        ),
        _ => format!(
            " Too large to copy, these stayed on {from}: {}.",
            left.join(", ")
        ),
    };
    format!(
        "Moved from {from} to {to}, the work is a copy of the folder in this one ({folder}), \
         {ended}.{left} Paths you remember may be wrong now."
    )
}

/// The remotes of `dir`'s repository as `(name, url)`, as written, for a
/// receiver that has to clone it. Read from the config rather than with
/// `git remote get-url`, which applies this machine's `insteadOf`
/// rewrites: the receiver has its own, and may not reach what ours point
/// at.
pub fn remote_urls(dir: &Path) -> Vec<(String, String)> {
    let listed =
        git(dir, &["config", "--get-regexp", r"^remote\..*\.url$"], &[]).unwrap_or_default();
    configured_urls(&listed)
}

/// `git config --get-regexp` lines (`remote.<name>.url <url>`) as
/// `(name, url)`, the first URL of each remote, as `get-url` gives.
fn configured_urls(listed: &str) -> Vec<(String, String)> {
    let mut urls: Vec<(String, String)> = Vec::new();
    for line in listed.lines() {
        let Some((key, url)) = line.trim().split_once(' ') else {
            continue;
        };
        let Some(name) = key
            .strip_prefix("remote.")
            .and_then(|k| k.strip_suffix(".url"))
        else {
            continue;
        };
        let url = url.trim();
        if name.is_empty() || url.is_empty() || urls.iter().any(|(n, _)| n == name) {
            continue;
        }
        urls.push((name.to_string(), url.to_string()));
    }
    urls
}

/// Whether `dir`'s working tree has changes, untracked files included.
pub fn has_changes(dir: &Path) -> bool {
    git(dir, &["status", "--porcelain"], &[]).is_ok_and(|s| !s.trim().is_empty())
}

/// The line a taken over session's first prompt starts with: the turn
/// that was running on the silent brother may have done what the
/// transcript does not show.
pub fn takeover_line(from: &str, to: &str, worktree: &str, since: &str) -> String {
    format!(
        "Taken over on {to} from {from}, which fell silent at {since}. The work is in this \
         worktree ({worktree}) as of the last turn that ended. A turn that was running on \
         {from} may have done things not in this conversation: a push, a deploy, a file \
         written. Check before doing any of them again. Paths you remember may be wrong now."
    )
}

/// One part of a bundle on the wire,
/// `{"t":"bundle","id","part","of","b64"}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Part {
    pub t: String,
    pub id: String,
    pub part: usize,
    pub of: usize,
    pub b64: String,
}

/// `bytes` as bundle parts for session `id`, each under the relay's
/// limit. An empty bundle is one empty part, so the receiver still hears
/// that it is complete.
pub fn split(id: &str, bytes: &[u8]) -> Vec<Part> {
    let chunks: Vec<&[u8]> = if bytes.is_empty() {
        vec![&[]]
    } else {
        bytes.chunks(PART_BYTES).collect()
    };
    let of = chunks.len();
    chunks
        .into_iter()
        .enumerate()
        .map(|(part, c)| Part {
            t: "bundle".into(),
            id: id.into(),
            part,
            of,
            b64: base64_encode(c),
        })
        .collect()
}

/// Why a part was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PartError {
    /// Another session's, or not a bundle part at all.
    Stranger,
    /// Its `of` disagrees with the parts before it, or `part` is past it.
    OutOfRange,
    /// The same part again, with other bytes.
    Conflict,
    BadBase64,
}

/// Parts of one bundle as they arrive, in any order.
#[derive(Debug, Clone, Default)]
pub struct Joiner {
    id: String,
    parts: Vec<Option<Vec<u8>>>,
}

impl Joiner {
    pub fn new(id: &str) -> Self {
        Self {
            id: id.into(),
            parts: Vec::new(),
        }
    }

    /// Takes one part. The whole bundle once every part is in, else None.
    /// A part that comes twice with the same bytes is ignored, since a
    /// link that reconnects may send it again.
    pub fn add(&mut self, p: &Part) -> Result<Option<Vec<u8>>, PartError> {
        if p.t != "bundle" || p.id != self.id {
            return Err(PartError::Stranger);
        }
        if p.of == 0
            || p.of > MAX_PARTS
            || p.part >= p.of
            || (!self.parts.is_empty() && self.parts.len() != p.of)
        {
            return Err(PartError::OutOfRange);
        }
        if self.parts.is_empty() {
            self.parts = vec![None; p.of];
        }
        let bytes = base64_decode(&p.b64)
            .or_else(|| p.b64.is_empty().then(Vec::new))
            .ok_or(PartError::BadBase64)?;
        match &self.parts[p.part] {
            Some(had) if *had != bytes => return Err(PartError::Conflict),
            Some(_) => {}
            None => self.parts[p.part] = Some(bytes),
        }
        if self.parts.iter().all(Option::is_some) {
            Ok(Some(
                self.parts.iter().flatten().flatten().copied().collect(),
            ))
        } else {
            Ok(None)
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A fresh repository in the temp folder, with one commit, removed
    /// when dropped.
    pub(crate) struct Repo(pub PathBuf);

    impl Repo {
        pub(crate) fn new(name: &str) -> Self {
            static N: AtomicU32 = AtomicU32::new(0);
            let dir = std::env::temp_dir().join(format!(
                "horadric-test-{name}-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let r = Repo(dir);
            r.git(&["init", "-q", "-b", "main"]);
            r.git(&["config", "user.name", "T"]);
            r.git(&["config", "user.email", "t@t"]);
            r.git(&["config", "core.autocrlf", "false"]);
            r
        }

        pub(crate) fn git(&self, args: &[&str]) -> String {
            git(&self.0, args, &[]).unwrap_or_else(|e| panic!("git {args:?}: {e}"))
        }

        pub(crate) fn write(&self, file: &str, text: &str) {
            let p = self.0.join(file);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        }

        pub(crate) fn read(&self, file: &str) -> Option<String> {
            std::fs::read_to_string(self.0.join(file)).ok()
        }

        pub(crate) fn commit(&self, file: &str, text: &str) -> String {
            self.write(file, text);
            self.git(&["add", "-A"]);
            self.git(&["commit", "-q", "-m", file]);
            self.git(&["rev-parse", "HEAD"]).trim().to_string()
        }
    }

    impl Drop for Repo {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn refs_take_only_plain_session_ids() {
        assert_eq!(
            handoff_ref("ab-12.x3").as_deref(),
            Some("refs/horadric/handoff/ab-12.x3")
        );
        for bad in [
            "",
            "../heads/main",
            "a/b",
            "a..b",
            ".a",
            "a.",
            "a.lock",
            "a b",
            "a~1",
        ] {
            assert_eq!(handoff_ref(bad), None, "{bad}");
        }
    }

    #[test]
    fn work_is_committed_without_touching_index_head_or_branches() {
        let r = Repo::new("commit");
        let head = r.commit("a.txt", "one\n");
        r.commit("gone.txt", "bye\n");
        let head2 = r.git(&["rev-parse", "HEAD"]).trim().to_string();
        r.write("a.txt", "two\n");
        r.write("new/b.txt", "untracked\n");
        r.write(".gitignore", "*.log\n");
        r.write("x.log", "ignored\n");
        r.write("staged.txt", "s\n");
        r.git(&["add", "staged.txt"]);
        std::fs::remove_file(r.0.join("gone.txt")).unwrap();
        let status = r.git(&["status", "--porcelain"]);
        let branches = r.git(&["branch", "-a"]);

        let refname = handoff_ref("s1").unwrap();
        let c = commit_work(&r.0, &refname, "handoff").unwrap();

        assert_eq!(r.git(&["rev-parse", "HEAD"]).trim(), head2);
        assert_eq!(r.git(&["status", "--porcelain"]), status);
        assert_eq!(r.git(&["branch", "-a"]), branches);
        assert_eq!(r.git(&["rev-parse", &refname]).trim(), c);
        assert_eq!(r.git(&["rev-parse", &format!("{c}^")]).trim(), head2);
        assert_eq!(r.git(&["show", &format!("{c}:a.txt")]), "two\n");
        assert_eq!(r.git(&["show", &format!("{c}:new/b.txt")]), "untracked\n");
        assert_eq!(r.git(&["show", &format!("{c}:staged.txt")]), "s\n");
        let files = r.git(&["ls-tree", "-r", "--name-only", &c]);
        assert!(!files.contains("x.log"));
        assert!(!files.contains("gone.txt"));
        assert_ne!(head, c);
    }

    #[test]
    fn an_unborn_branch_gives_a_commit_with_no_parent() {
        let r = Repo::new("unborn");
        r.write("a.txt", "x");
        let c = commit_work(&r.0, &handoff_ref("s").unwrap(), "m").unwrap();
        assert_eq!(rev(&r.0, "HEAD"), None);
        assert_eq!(r.git(&["rev-list", "--count", &c]).trim(), "1");
    }

    #[test]
    fn the_newest_known_commit_is_the_base() {
        let history = "c3\nc2\nc1\n";
        assert_eq!(
            best_base(history, &["c1".into(), "c2".into(), "zz".into()]).as_deref(),
            Some("c2")
        );
        assert_eq!(best_base(history, &["zz".into()]), None);
        assert_eq!(best_base("", &["c1".into()]), None);
    }

    #[test]
    fn a_bundle_lands_in_a_new_worktree_with_the_changes_unstaged() {
        let sender = Repo::new("send");
        sender.commit("a.txt", "one\n");
        sender.commit("b.txt", "keep\n");
        sender.commit("gone.txt", "bye\n");
        let receiver = Repo::new("recv");
        receiver.git(&["fetch", "-q", &sender.0.to_string_lossy(), "main"]);
        receiver.git(&["reset", "-q", "--hard", "FETCH_HEAD"]);
        // The sender moves on past what the receiver has.
        sender.commit("c.txt", "later\n");
        sender.write("a.txt", "two\n");
        sender.write("u.txt", "untracked\n");
        std::fs::remove_file(sender.0.join("gone.txt")).unwrap();

        let refname = handoff_ref("s9").unwrap();
        let c = commit_work(&sender.0, &refname, "handoff").unwrap();
        let known = known_commits(&receiver.0);
        let file = sender
            .0
            .join("..")
            .join(format!("horadric-test-bundle-{}", std::process::id()));
        let base = cut_bundle(&sender.0, &refname, &known, &file).unwrap();
        assert_eq!(base, rev(&receiver.0, "main"));

        let folder = receiver.0.with_extension("s9");
        let got = apply_bundle(&receiver.0, &file, "s9", &folder, "s9").unwrap();
        let _ = std::fs::remove_file(&file);
        assert_eq!(got, c);
        let wt = Repo(folder);
        assert_eq!(wt.read("a.txt").as_deref(), Some("two\n"));
        assert_eq!(wt.read("c.txt").as_deref(), Some("later\n"));
        assert_eq!(wt.read("u.txt").as_deref(), Some("untracked\n"));
        assert_eq!(wt.read("gone.txt"), None);
        let status = wt.git(&["status", "--porcelain"]);
        let mut lines: Vec<&str> = status.lines().collect();
        lines.sort();
        assert_eq!(lines, [" D gone.txt", " M a.txt", "?? u.txt"]);
        assert_eq!(wt.git(&["branch", "--show-current"]).trim(), "s9");
        // The receiver's own checkout stayed where it was.
        assert_eq!(receiver.read("c.txt"), None);
        assert_eq!(receiver.git(&["status", "--porcelain"]), "");
        receiver.git(&["worktree", "remove", "--force", &wt.0.to_string_lossy()]);
    }

    #[test]
    fn with_nothing_in_common_the_whole_history_goes() {
        let sender = Repo::new("whole");
        sender.commit("a.txt", "1");
        sender.commit("b.txt", "2");
        let refname = handoff_ref("w").unwrap();
        commit_work(&sender.0, &refname, "h").unwrap();
        let receiver = Repo::new("empty");
        let file = sender.0.with_extension("bundle");
        let base = cut_bundle(&sender.0, &refname, &known_commits(&receiver.0), &file).unwrap();
        assert_eq!(base, None);
        let c = fetch_bundle(&receiver.0, &file, &refname).unwrap();
        let _ = std::fs::remove_file(&file);
        assert_eq!(receiver.git(&["rev-list", "--count", &c]).trim(), "3");
    }

    #[test]
    fn remotes_are_spelled_one_way() {
        for url in [
            "git@github.com:Mopra/horadric.dev.git",
            "https://github.com/Mopra/horadric.dev.git",
            "https://user@GitHub.com/Mopra/horadric.dev/",
            "ssh://git@github.com:22/Mopra/horadric.dev",
            "  https://github.com/Mopra/horadric.dev\n",
        ] {
            assert_eq!(
                normalize_remote(url),
                "github.com/Mopra/horadric.dev",
                "{url}"
            );
        }
        assert_eq!(normalize_remote("/srv/git/x.git"), "srv/git/x");
        assert_eq!(normalize_remote(r"C:\repos\x"), "C:/repos/x");
    }

    #[test]
    fn a_project_is_found_by_remote_before_first_commit() {
        let id = |r: &[&str], roots: &[&str]| RepoId {
            remotes: r.iter().map(|s| s.to_string()).collect(),
            roots: roots.iter().map(|s| s.to_string()).collect(),
        };
        let wanted = id(&["github.com/a/x"], &["r1"]);
        let mine = vec![
            ("C:/fork".to_string(), id(&["github.com/b/x"], &["r1"])),
            ("C:/bare".to_string(), id(&[], &["r1"])),
            ("C:/x".to_string(), id(&["github.com/a/x"], &["r1"])),
        ];
        assert_eq!(find_project(&wanted, &mine), Some("C:/x"));
        assert_eq!(find_project(&wanted, &mine[..2]), Some("C:/bare"));
        assert_eq!(find_project(&wanted, &mine[..1]), None);
        assert_eq!(find_project(&id(&[], &["r2"]), &mine), None);
    }

    #[test]
    fn a_repositorys_identity_is_read() {
        let r = Repo::new("id");
        let root = r.commit("a", "a");
        r.commit("b", "b");
        r.git(&["remote", "add", "origin", "git@github.com:A/b.git"]);
        let id = repo_id(&r.0).unwrap();
        assert_eq!(id.remotes, ["github.com/A/b"]);
        assert_eq!(id.roots, [root]);
    }

    #[test]
    fn config_lines_give_each_remotes_first_url() {
        let listed = "remote.origin.url git@github.com:A/b.git\n\
                      remote.my.fork.url https://x.org/c\n\
                      remote.origin.url https://second\n\
                      remote.bare.url\n\
                      branch.main.remote origin\n";
        assert_eq!(
            configured_urls(listed),
            [
                ("origin".to_string(), "git@github.com:A/b.git".to_string()),
                ("my.fork".to_string(), "https://x.org/c".to_string()),
            ]
        );
        assert!(configured_urls("").is_empty());
    }

    #[test]
    fn remote_urls_are_sent_as_written_not_as_rewritten_here() {
        let r = Repo::new("insteadof");
        r.git(&["remote", "add", "origin", "https://github.com/A/b.git"]);
        r.git(&[
            "config",
            "url.git@github.com:.insteadOf",
            "https://github.com/",
        ]);
        // The rewrite is in force here, which is what must not travel.
        assert_eq!(
            r.git(&["remote", "get-url", "origin"]).trim(),
            "git@github.com:A/b.git"
        );
        assert_eq!(
            remote_urls(&r.0),
            [(
                "origin".to_string(),
                "https://github.com/A/b.git".to_string()
            )]
        );
    }

    #[test]
    fn a_repository_is_known_by_its_url_as_written() {
        let r = Repo::new("insteadof-id");
        r.git(&["remote", "add", "origin", "https://github.com/A/b.git"]);
        r.git(&[
            "config",
            "url.https://mirror.example/.insteadOf",
            "https://github.com/",
        ]);
        let id = repo_id(&r.0).unwrap();
        assert_eq!(id.remotes, [normalize_remote("https://github.com/A/b.git")]);
    }

    #[test]
    fn transcript_keys_match_claude_codes_folders() {
        // Spellings seen in C:/Users/morte/.claude/projects.
        assert_eq!(
            transcript_key(r"C:\Users\morte\Documents\Github\horadric.dev"),
            "C--Users-morte-Documents-Github-horadric-dev"
        );
        assert_eq!(
            transcript_key(r"C:\Users\morte\.claude\jobs\8e613b8c\tmp\probe"),
            "C--Users-morte--claude-jobs-8e613b8c-tmp-probe"
        );
        assert_eq!(transcript_key("/home/m/my_app/"), "-home-m-my-app");
        assert_eq!(transcript_key("/"), "-");
        assert_eq!(
            transcript_path(Path::new("root"), "/a", "id"),
            Path::new("root").join("-a").join("id.jsonl")
        );
    }

    #[test]
    fn long_keys_are_cut_and_found_by_their_start() {
        let long = format!("/{}", "a".repeat(300));
        let key = transcript_key(&long);
        assert_eq!(key.len(), 200);
        assert!(key_matches(&format!("{key}-1x2y3z"), &long));
        assert!(!key_matches(&format!("{key}x"), &long));
        assert!(key_matches("-short", "/short"));
        assert!(!key_matches("-short-1x", "/short"));
    }

    #[test]
    fn paths_move_between_windows_and_linux() {
        let w = r"C:\Users\m\x";
        let l = "/home/m/x";
        assert_eq!(
            move_path(r"C:\Users\m\x\src\a", w, l).as_deref(),
            Some("/home/m/x/src/a")
        );
        assert_eq!(
            move_path(r"c:/users/M/x", w, l).as_deref(),
            Some("/home/m/x")
        );
        assert_eq!(
            move_path("/home/m/x/src/a", l, w).as_deref(),
            Some(r"C:\Users\m\x\src\a")
        );
        assert_eq!(
            move_path("/home/m/x", "/home/m/x/", "/srv/x").as_deref(),
            Some("/srv/x")
        );
        assert_eq!(move_path("/home/m/xy", l, w), None);
        assert_eq!(move_path("/home/M/x", l, w), None);
        assert_eq!(move_path(r"D:\x", w, l), None);
    }

    #[test]
    fn only_the_cwd_field_is_rewritten() {
        let line = r#"{"type":"user","message":{"content":"C:\\Users\\m\\x"},"cwd":"C:\\Users\\m\\x\\sub","z":1}"#;
        assert_eq!(
            rewrite_cwd_line(line, r"C:\Users\m\x", "/home/m/x"),
            r#"{"type":"user","message":{"content":"C:\\Users\\m\\x"},"cwd":"/home/m/x/sub","z":1}"#
        );
        assert_eq!(rewrite_cwd_line("not json", "a", "b"), "not json");
        let elsewhere = r#"{"cwd":"D:\\y"}"#;
        assert_eq!(rewrite_cwd_line(elsewhere, r"C:\x", "/x"), elsewhere);
        let text = "{\"cwd\":\"/a\"}\r\n{\"t\":1}\n{\"cwd\":\"/a/b\"}";
        assert_eq!(
            rewrite_transcript(text, "/a", r"C:\a"),
            "{\"cwd\":\"C:\\\\a\"}\r\n{\"t\":1}\n{\"cwd\":\"C:\\\\a\\\\b\"}"
        );
    }

    #[test]
    fn the_record_round_trips_and_reads_old_fields_missing() {
        let r = Record {
            session: "abc".into(),
            name: "fix login".into(),
            project: r"C:\p".into(),
            cwd: r"C:\p.fix".into(),
            agent: "claude".into(),
            model: Some("opus".into()),
            quest: Some("q1".into()),
            from: "Laptop".into(),
            n: 4,
            ..Default::default()
        };
        let text = serde_json::to_string(&r).unwrap();
        assert!(!text.contains("account"));
        assert_eq!(serde_json::from_str::<Record>(&text).unwrap(), r);
        let bare: Record = serde_json::from_str(
            r#"{"session":"a","name":"n","project":"p","cwd":"p","agent":"claude","from":"L"}"#,
        )
        .unwrap();
        assert_eq!(bare.n, 0);
        assert_eq!(bare.repo, RepoId::default());
    }

    #[test]
    fn the_first_lines_say_what_happened() {
        assert_eq!(
            moved_line("Laptop", "Desktop", r"C:\p.fix", true),
            "Moved from Laptop to Desktop, the work is in this worktree (C:\\p.fix), the last \
             turn ended cleanly. Paths you remember may be wrong now."
        );
        assert!(moved_line("a", "b", "w", false).contains("interrupted"));
        assert_eq!(
            moved_plain_line("Laptop", "Desktop", r"C:\p", true, &[]),
            "Moved from Laptop to Desktop, the work is a copy of the folder in this one (C:\\p), \
             the last turn ended cleanly. Paths you remember may be wrong now."
        );
        let big = moved_plain_line("a", "b", "w", false, &["v.mp4".into()]);
        assert!(big.contains("interrupted. Too large to copy, these stayed on a: v.mp4."));
        let many: Vec<String> = (0..12).map(|i| format!("f{i}")).collect();
        assert!(moved_plain_line("a", "b", "w", true, &many).contains("12 files were too large"));
        let t = takeover_line("Laptop", "Desktop", "/w", "14:02");
        assert!(t.starts_with("Taken over on Desktop from Laptop, which fell silent at 14:02."));
        assert!(t.contains("a push, a deploy, a file written"));
        assert!(t.contains("Check before"));
    }

    #[test]
    fn parts_stay_under_the_relay_limit_and_join_in_any_order() {
        let bytes: Vec<u8> = (0..PART_BYTES * 2 + 17).map(|i| (i * 7) as u8).collect();
        let parts = split("s", &bytes);
        assert_eq!(parts.len(), 3);
        for p in &parts {
            assert!(serde_json::to_string(p).unwrap().len() < 1_000_000);
            assert_eq!(p.of, 3);
        }
        let mut j = Joiner::new("s");
        assert_eq!(j.add(&parts[2]), Ok(None));
        assert_eq!(j.add(&parts[0]), Ok(None));
        assert_eq!(j.add(&parts[0]), Ok(None));
        assert_eq!(j.add(&parts[1]), Ok(Some(bytes)));
    }

    #[test]
    fn strange_parts_are_refused() {
        let parts = split("s", b"hello world");
        assert_eq!(parts.len(), 1);
        let mut j = Joiner::new("other");
        assert_eq!(j.add(&parts[0]), Err(PartError::Stranger));
        let mut j = Joiner::new("s");
        let mut p = parts[0].clone();
        p.part = 1;
        assert_eq!(j.add(&p), Err(PartError::OutOfRange));
        p.part = 0;
        p.b64 = "!!".into();
        assert_eq!(j.add(&p), Err(PartError::BadBase64));
        let two = split("s", &vec![1u8; PART_BYTES + 1]);
        let mut j = Joiner::new("s");
        j.add(&two[0]).unwrap();
        let mut changed = two[0].clone();
        changed.b64 = base64_encode(b"other");
        assert_eq!(j.add(&changed), Err(PartError::Conflict));
        assert_eq!(j.add(&parts[0]), Err(PartError::OutOfRange));
        let empty = split("s", b"");
        assert_eq!(Joiner::new("s").add(&empty[0]), Ok(Some(Vec::new())));
        let mut huge = parts[0].clone();
        huge.of = MAX_PARTS + 1;
        assert_eq!(
            Joiner::new("s").add(&huge),
            Err(PartError::OutOfRange),
            "a claimed size past the cap sets nothing aside"
        );
    }

    #[test]
    fn paths_under_the_transcripts_folder_move_anywhere_in_a_line() {
        let from = r"C:\Users\m\.claude\projects\C--p";
        let to = "/home/m/.claude/projects/-home-m-p";
        let line = r#"{"content":"Output too large. Full output saved to: C:\\Users\\m\\.claude\\projects\\C--p\\abc\\tool-results\\b1.txt\n\nPreview","persisted":"c:\\users\\m\\.claude\\projects\\c--p\\abc\\tool-results\\b1.txt","other":"C:\\Users\\m\\.claude\\projects\\C--px\\a"}"#;
        assert_eq!(
            rewrite_paths(line, from, to),
            r#"{"content":"Output too large. Full output saved to: /home/m/.claude/projects/-home-m-p/abc/tool-results/b1.txt\n\nPreview","persisted":"/home/m/.claude/projects/-home-m-p/abc/tool-results/b1.txt","other":"C:\\Users\\m\\.claude\\projects\\C--px\\a"}"#
        );
        let back = r#"{"p":"/home/m/.claude/projects/-home-m-p/abc/tool-results/b1.txt"}"#;
        assert_eq!(
            rewrite_paths(back, to, from),
            r#"{"p":"C:\\Users\\m\\.claude\\projects\\C--p\\abc\\tool-results\\b1.txt"}"#
        );
        assert_eq!(rewrite_paths("ünï \"x\"", "/x", "/y"), "ünï \"x\"");
        assert_eq!(rewrite_paths(r#"{"a":"/x"}"#, "/x", "/y"), r#"{"a":"/y"}"#);
    }

    #[test]
    fn only_paths_that_stay_inside_are_safe() {
        for good in [
            "a.jsonl",
            "a/subagents/agent-1.meta.json",
            "a/tool-results/x.txt",
        ] {
            assert!(safe_relative(good), "{good}");
        }
        for bad in ["", "/a", "a/../b", "..", "a//b", r"a\b", "C:a", "./a"] {
            assert!(!safe_relative(bad), "{bad}");
        }
    }

    #[test]
    fn a_transcript_travels_with_its_subagents_and_tool_results() {
        let tmp = Repo::new("transcripts");
        let root = tmp.0.join("projects");
        let from_cwd = r"C:\Users\m\p";
        let from_folder = root.join(transcript_key(from_cwd));
        let folder_text = from_folder.to_string_lossy().into_owned();
        let escaped = serde_json::Value::from(format!(
            "{folder_text}{}id1{}tool-results{}b1.txt",
            std::path::MAIN_SEPARATOR,
            std::path::MAIN_SEPARATOR,
            std::path::MAIN_SEPARATOR
        ))
        .to_string();
        let main = format!(
            "{{\"cwd\":\"C:\\\\Users\\\\m\\\\p\",\"r\":{escaped}}}\n{{\"type\":\"ai-title\"}}\n"
        );
        let put = |rel: &str, text: &str| {
            let p = from_folder.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        };
        put("id1.jsonl", &main);
        put(
            "id1/subagents/agent-a.jsonl",
            "{\"cwd\":\"C:\\\\Users\\\\m\\\\p\"}\n",
        );
        put(
            "id1/subagents/agent-a.meta.json",
            "{\"agentType\":\"Explore\"}",
        );
        put("id1/tool-results/b1.txt", "big output");
        put("id2.jsonl", "another session\n");

        assert_eq!(
            transcript_folder(&root, from_cwd, "id1"),
            Some(from_folder.clone())
        );
        // A filesystem that ignores case finds it by the key itself, one
        // that does not by looking through the folders.
        let other_case = transcript_folder(&root, r"c:\users\M\p", "id1").unwrap();
        assert!(other_case
            .to_string_lossy()
            .eq_ignore_ascii_case(&from_folder.to_string_lossy()));
        assert_eq!(transcript_folder(&root, from_cwd, "../id1"), None);
        let files = gather_transcript(&from_folder, "id1").unwrap();
        let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(
            paths,
            [
                "id1.jsonl",
                "id1/subagents/agent-a.jsonl",
                "id1/subagents/agent-a.meta.json",
                "id1/tool-results/b1.txt"
            ]
        );

        let package = Package {
            record: Record {
                session: "id1".into(),
                cwd: from_cwd.into(),
                transcripts: folder_text.clone(),
                ..Default::default()
            },
            files,
            work: b"bundle bytes".to_vec(),
        };
        let bytes = pack(&package);
        assert_eq!(unpack(&bytes).as_ref(), Some(&package));
        assert_eq!(unpack(&bytes[..bytes.len() - 1]), None);

        let to_cwd = "/home/m/p";
        let to_folder = tmp.0.join("elsewhere").join(transcript_key(to_cwd));
        file_transcript(&package.files, &to_folder, &folder_text, from_cwd, to_cwd).unwrap();
        let read = |rel: &str| std::fs::read_to_string(to_folder.join(rel)).unwrap();
        let filed = read("id1.jsonl");
        assert!(filed.starts_with("{\"cwd\":\"/home/m/p\","), "{filed}");
        let old = serde_json::Value::from(folder_text.as_str()).to_string();
        assert!(!filed.contains(old.trim_matches('"')), "{filed}");
        assert!(filed.contains("ai-title"));
        assert_eq!(
            read("id1/subagents/agent-a.jsonl"),
            "{\"cwd\":\"/home/m/p\"}\n"
        );
        assert_eq!(
            read("id1/subagents/agent-a.meta.json"),
            "{\"agentType\":\"Explore\"}"
        );
        assert_eq!(read("id1/tool-results/b1.txt"), "big output");

        let evil = [TranscriptFile {
            path: "../x.jsonl".into(),
            bytes: Vec::new(),
        }];
        assert!(file_transcript(&evil, &to_folder, "", "", "").is_err());
    }
}
