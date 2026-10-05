//! Adding and removing a session's own git worktree. What a project wants
//! and what a worktree is told live in `horadric_core::worktree`; this is
//! the part that runs `git`.

use std::collections::HashMap;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use horadric_core::diff::{self, Diff};
use horadric_core::experience;
use horadric_core::journal::{self, Commit};
use horadric_core::merge;
use horadric_core::worktree::{self, Place, Ports, Worktree};
use horadric_hooks::tasks as file;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// How often ending a session tries to remove its worktree. The agent was
/// just killed, and its files can stay locked for a moment after.
const REMOVE_TRIES: u32 = 3;

/// A worktree just added for a new session.
pub struct Fresh {
    /// Where the session starts: the same folder in the new worktree as it
    /// was asked to start in the main one.
    pub cwd: PathBuf,
    pub worktree: Worktree,
    /// The project's setup commands, for its first start only.
    pub setup: Vec<String>,
}

/// Where `dir` is in its repository's main working tree, None when it is
/// in a linked worktree or in no repository.
///
/// Menus ask this as they open, so an answer is remembered: a folder's
/// place in its repository hardly ever changes. A folder in no repository
/// may get one, so that answer is asked again after [`NO_PLACE_FOR`].
pub fn main_tree(dir: &Path) -> Option<Place> {
    type Known = HashMap<PathBuf, (Option<Place>, Instant)>;
    static KNOWN: OnceLock<Mutex<Known>> = OnceLock::new();
    let known = KNOWN.get_or_init(Default::default);
    let now = Instant::now();
    let kept = known.lock().ok().and_then(|k| {
        k.get(dir)
            .filter(|(place, at)| place.is_some() || now.duration_since(*at) < NO_PLACE_FOR)
            .map(|(place, _)| place.clone())
    });
    if let Some(place) = kept {
        return place;
    }
    let place = ask_main_tree(dir);
    if let Ok(mut k) = known.lock() {
        k.insert(dir.to_path_buf(), (place.clone(), now));
    }
    place
}

/// How long a folder is taken to be in no repository before git is asked
/// again.
const NO_PLACE_FOR: Duration = Duration::from_secs(30);

fn ask_main_tree(dir: &Path) -> Option<Place> {
    let out = git(
        dir,
        &[
            "rev-parse",
            "--path-format=absolute",
            "--git-dir",
            "--git-common-dir",
            "--show-toplevel",
            "--show-prefix",
        ],
    )
    .ok()?;
    worktree::main_tree(&out)
}

/// Adds a worktree for a new session named `name` in `dir`, on a new
/// branch from what the main tree has checked out, with a range of ports
/// that `taken` does not use. None when the project does not want one and
/// it was not `asked` for; an error when git refused, and the session then
/// shares the main tree.
pub fn add(dir: &Path, name: &str, taken: &[Ports], asked: bool) -> Result<Option<Fresh>, String> {
    let settings = file::worktrees(dir);
    if !settings.enabled && !asked {
        return Ok(None);
    }
    let Some(place) = main_tree(dir) else {
        return Ok(None);
    };
    let top = Path::new(&place.top);
    let branch = worktree::branch(name, |b| {
        Path::new(&worktree::folder(&place.top, b)).exists()
            || git(
                top,
                &[
                    "show-ref",
                    "--verify",
                    "--quiet",
                    &format!("refs/heads/{b}"),
                ],
            )
            .is_ok()
    });
    let path = worktree::folder(&place.top, &branch);
    git(top, &["worktree", "add", "-b", &branch, &path])?;
    if horadric_hooks::dev() {
        mark_dev_made(Path::new(&path));
    }
    let cwd = if place.prefix.is_empty() {
        PathBuf::from(&path)
    } else {
        Path::new(&path).join(&place.prefix)
    };
    Ok(Some(Fresh {
        cwd,
        worktree: Worktree {
            path,
            main: place.top.replace('\\', "/"),
            branch,
            ports: worktree::free_ports(settings.ports, taken),
        },
        setup: settings.setup,
    }))
}

/// The worktree's own git folder, where git keeps what is per worktree.
fn admin_dir(tree: &Path) -> Option<PathBuf> {
    git(tree, &["rev-parse", "--path-format=absolute", "--git-dir"])
        .ok()
        .map(|d| PathBuf::from(d.trim()))
}

fn mark_dev_made(tree: &Path) {
    let written = admin_dir(tree).map(|d| std::fs::write(d.join(worktree::DEV_MARK), ""));
    if !matches!(written, Some(Ok(()))) {
        eprintln!(
            "horadric: could not mark {} as a dev worktree",
            tree.display()
        );
    }
}

fn dev_made(tree: &Path) -> bool {
    admin_dir(tree).is_some_and(|d| d.join(worktree::DEV_MARK).is_file())
}

/// Removes a session's worktree once the session has ended, and its branch
/// once nothing is on it that the main tree lacks. Git refuses both while
/// there is work to lose, changed files in the tree or commits on the
/// branch, and then both stay for the human. In the background, since the
/// ended agent may hold its files for a moment.
pub fn remove(w: Worktree) {
    remove_then(w, |_| ());
}

/// Removes as `remove` does, and calls `kept` with the worktree when its
/// tree went but its branch stayed, since it has commits the main tree
/// lacks: a branch worth offering to merge.
pub fn remove_then(w: Worktree, kept: impl FnOnce(Worktree) + Send + 'static) {
    remove_with(w, false, kept);
}

/// Removes a worktree and its branch with whatever is in them. Only for a
/// tomb the human did not pick, whose work is thrown away on purpose.
pub fn discard(w: Worktree) {
    remove_with(w, true, |_| ());
}

fn remove_with(w: Worktree, force: bool, kept: impl FnOnce(Worktree) + Send + 'static) {
    std::thread::spawn(move || {
        let main = Path::new(&w.main);
        let mut result = Ok(String::new());
        let remove: &[&str] = if force {
            &["worktree", "remove", "--force", &w.path]
        } else {
            &["worktree", "remove", &w.path]
        };
        for attempt in 0..REMOVE_TRIES {
            std::thread::sleep(Duration::from_secs(1 + attempt as u64));
            result = git(main, remove);
            if result.is_ok() || !Path::new(&w.path).exists() {
                break;
            }
        }
        if let Err(e) = result {
            eprintln!("horadric: kept the worktree {}: {e}", w.path);
            return;
        }
        let delete = if force { "-D" } else { "-d" };
        if let Err(e) = git(main, &["branch", delete, &w.branch]) {
            eprintln!("horadric: kept the branch {}: {e}", w.branch);
            kept(w);
        }
    });
}

/// The branches with commits that what the main tree has checked out
/// lacks.
pub fn unmerged(main: &Path) -> Vec<String> {
    git(
        main,
        &["branch", "--no-merged", "HEAD", "--format=%(refname:short)"],
    )
    .map(|out| out.lines().map(str::to_string).collect())
    .unwrap_or_default()
}

/// What the main tree has checked out, for saying where a merge goes.
pub fn checked_out(main: &Path) -> Option<String> {
    git(main, &["rev-parse", "--abbrev-ref", "HEAD"])
        .ok()
        .map(|b| b.trim().to_string())
        .filter(|b| !b.is_empty() && b != "HEAD")
}

/// Commits the file `rel` in the main tree `main` by itself, leaving
/// whatever else is staged there to the sessions that staged it. True when
/// there was something to commit. Nothing on a detached head, or where git
/// ignores the file.
pub fn commit_file(main: &Path, rel: &str, message: &str) -> Result<bool, String> {
    if git(main, &["status", "--porcelain", "--", rel])?
        .trim()
        .is_empty()
    {
        return Ok(false);
    }
    if checked_out(main).is_none() {
        return Err("the main tree is not on a branch".to_string());
    }
    git(main, &["add", "--", rel])?;
    git(main, &["commit", "--only", "-m", message, "--", rel])?;
    Ok(true)
}

/// Merges `branch` into what the main tree has checked out, always with a
/// merge commit so the item stays one piece in the history, and then
/// deletes the branch. A merge that stops on a conflict is undone, so the
/// main tree is never left half merged; the branch stays for the human.
pub fn merge(main: &Path, branch: &str) -> Result<(), String> {
    if let Err(e) = git(main, &["merge", "--no-ff", "--no-edit", branch]) {
        let _ = git(main, &["merge", "--abort"]);
        return Err(e);
    }
    if let Err(e) = git(main, &["branch", "-d", branch]) {
        eprintln!("horadric: merged but kept the branch {branch}: {e}");
    }
    Ok(())
}

/// Whether what `dir` has checked out is in what its main tree has
/// checked out, so the work done there landed. None when git cannot say:
/// no repository, a bare one, or the main tree on no branch. The main tree
/// is where the shared `.git` is, which a linked worktree also knows.
pub fn landed(dir: &Path) -> Option<bool> {
    let common = git(
        dir,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )
    .ok()?;
    let main = Path::new(common.trim()).parent()?;
    let branch = checked_out(main)?;
    let head = format!("refs/heads/{branch}");
    Some(git(dir, &["merge-base", "--is-ancestor", "HEAD", &head]).is_ok())
}

/// The commit checked out in `dir`.
pub fn head(dir: &Path) -> Option<String> {
    git(dir, &["rev-parse", "HEAD"])
        .ok()
        .map(|h| h.trim().to_string())
}

/// The commits made on the branch checked out in `dir` since `since`, in
/// Unix seconds, newest first. Only its own line: a merge of main into it
/// would otherwise bring every commit main had meanwhile.
pub fn commits_since(dir: &Path, since: u64) -> Vec<Commit> {
    let since = format!("--since=@{since}");
    let format = format!("--format={}", journal::LOG_FORMAT);
    git(
        dir,
        &[
            "log",
            "--first-parent",
            "--no-merges",
            &since,
            &format,
            "HEAD",
        ],
    )
    .map(|log| journal::commits(&log))
    .unwrap_or_default()
}

/// The hashes of the commits on what the main tree of `dir` has checked
/// out whose author is the one git would name for a commit made there now.
/// Merges are left out: they add nothing a commit of their own did not.
pub fn mine(dir: &Path) -> Vec<String> {
    let Ok(email) = git(dir, &["config", "user.email"]) else {
        return Vec::new();
    };
    let format = format!("--format={}", experience::LOG_FORMAT);
    git(dir, &["log", "--no-merges", &format, "HEAD"])
        .map(|log| {
            experience::mine(&log, email.trim())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Removes the linked worktrees of the repository `dir` is in that are
/// done with: the branch is merged into what the main tree has checked
/// out, nothing in the tree is changed or untracked, and no session in
/// `busy` has its folder there. Hand-made ones too, since an agent that
/// adds a worktree itself rarely removes it after the merge. The branch
/// must have been committed to, or a worktree just added and not yet
/// worked in would count as merged and go. Their branches go with them.
/// A dev instance sweeps only the worktrees it added, see
/// [`worktree::sweepable`]. Returns the folders removed.
pub fn sweep(dir: &Path, busy: &[String]) -> Vec<String> {
    let Some(place) = main_tree(dir) else {
        return Vec::new();
    };
    let main = Path::new(&place.top);
    let Ok(list) = git(main, &["worktree", "list", "--porcelain"]) else {
        return Vec::new();
    };
    let dev = horadric_hooks::dev();
    let mut removed = Vec::new();
    for w in worktree::linked(&list) {
        if busy.iter().any(|d| worktree::inside(d, &w.path)) {
            continue;
        }
        if !worktree::sweepable(dev, dev && dev_made(Path::new(&w.path))) {
            continue;
        }
        let branch = format!("refs/heads/{}", w.branch);
        let merged = git(main, &["merge-base", "--is-ancestor", &branch, "HEAD"]).is_ok();
        let worked_in = git(main, &["reflog", "show", "--format=%H", &branch])
            .is_ok_and(|log| log.lines().count() > 1);
        let clean = git(
            Path::new(&w.path),
            &["--no-optional-locks", "status", "--porcelain"],
        )
        .is_ok_and(|s| s.trim().is_empty());
        if !(merged && worked_in && clean) {
            continue;
        }
        if let Err(e) = git(main, &["worktree", "remove", &w.path]) {
            eprintln!("horadric: kept the worktree {}: {e}", w.path);
            continue;
        }
        if let Err(e) = git(main, &["branch", "-d", &w.branch]) {
            eprintln!("horadric: kept the branch {}: {e}", w.branch);
        }
        removed.push(w.path);
    }
    removed
}

/// Counts what a session's worktree has changed: against its last commit,
/// untracked files included, and on its branch since the main tree's
/// commit. None once the worktree is gone. `--no-optional-locks` keeps
/// git from rewriting the index, which a watcher would take for a change.
pub fn count(w: &Worktree) -> Option<Diff> {
    let top = Path::new(&w.path);
    if !top.is_dir() {
        return None;
    }
    let numstat = |range: &str| {
        git(
            top,
            &[
                "--no-optional-locks",
                "diff",
                "--numstat",
                "-z",
                "--no-renames",
                range,
            ],
        )
        .map(|out| diff::parse_numstat(&out))
    };
    let mut uncommitted = numstat("HEAD").ok()?;
    let others = git(
        top,
        &[
            "--no-optional-locks",
            "ls-files",
            "-z",
            "--others",
            "--exclude-standard",
        ],
    )
    .unwrap_or_default();
    for path in others.split('\0').filter(|p| !p.is_empty()) {
        let content = std::fs::read(top.join(path)).unwrap_or_default();
        uncommitted.push(diff::untracked(path, &content));
    }
    // The main tree may have moved on since; `...` counts from where the
    // branch left it.
    let committed = git(Path::new(&w.main), &["rev-parse", "HEAD"])
        .and_then(|main| numstat(&format!("{}...HEAD", main.trim())))
        .unwrap_or_default();
    Some(Diff {
        uncommitted,
        committed,
    })
}

/// Runs git in `dir`: what it printed, or what it said when it failed.
fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| format!("cannot run git: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        // A merge that stops on a conflict says why on stdout.
        let said =
            [&out.stdout, &out.stderr].map(|s| String::from_utf8_lossy(s).trim().to_string());
        Err(said
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n"))
    }
}

/// How long one check may run before it counts as failed, so a test that
/// hangs does not hold every later merge of its project.
const CHECK_LONGEST: Duration = Duration::from_secs(30 * 60);

/// How a finished quest's merge into the main tree came out.
pub enum Landing {
    /// `main` holds the branch now, or held it already. With the commit
    /// the checks passed on, which `main` was left at, or empty when it
    /// held the branch already and none ran.
    Merged(String),
    /// A step failed with this output. The branch stayed.
    Failed(merge::Failure, String),
}

/// Merges the finished quest in worktree `w` into `into`, what the main
/// tree has checked out: rebased on it in the worktree, `checks` run
/// there, and `into` fast forwarded to it. Starts over when `into` moved
/// meanwhile. Blocks for as long as the checks take, so it runs on a
/// thread of its own.
pub fn land(w: &Worktree, into: &str, checks: &[String]) -> Landing {
    let main = Path::new(&w.main);
    let tree = Path::new(&w.path);
    // The agent was just ended, and may hold its files for a moment.
    std::thread::sleep(Duration::from_secs(1));
    if git(main, &["merge-base", "--is-ancestor", &w.branch, into]).is_ok() {
        return Landing::Merged(String::new());
    }
    let mut last = (merge::Failure::Moved, String::new());
    for _ in 0..merge::TRIES {
        let mut moved = false;
        for step in merge::plan(checks) {
            let done = match &step {
                merge::Step::Rebase => git(tree, &["rebase", into]),
                merge::Step::Check(c) => shell(tree, c),
                merge::Step::FastForward => git(main, &["merge", "--ff-only", &w.branch]),
            };
            let Err(out) = done else { continue };
            let why = merge::failed(&step, &out);
            if step == merge::Step::Rebase {
                let _ = git(tree, &["rebase", "--abort"]);
            }
            if why == merge::Failure::Moved {
                last = (why, out);
                moved = true;
                break;
            }
            return Landing::Failed(why, out);
        }
        if !moved {
            return Landing::Merged(head(tree).unwrap_or_default());
        }
    }
    Landing::Failed(last.0, last.1)
}

/// Runs `command` with `cmd /c` in `dir`, with no window: what it printed
/// when it exits 0, else what it printed, stdout and stderr as they came.
fn shell(dir: &Path, command: &str) -> Result<String, String> {
    let (mut read, write) = std::io::pipe().map_err(|e| format!("cannot run {command}: {e}"))?;
    let err = write
        .try_clone()
        .map_err(|e| format!("cannot run {command}: {e}"))?;
    let mut cmd = Command::new("cmd.exe");
    cmd.arg("/d").arg("/c").raw_arg(command);
    let child = cmd
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(write)
        .stderr(err)
        .creation_flags(CREATE_NO_WINDOW)
        .spawn();
    // The pipe ends only once every writer is gone, ours included.
    drop(cmd);
    let mut child = child.map_err(|e| format!("cannot run {command}: {e}"))?;
    let reader = std::thread::spawn(move || {
        let mut out = Vec::new();
        let _ = std::io::Read::read_to_end(&mut read, &mut out);
        String::from_utf8_lossy(&out).into_owned()
    });
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if start.elapsed() < CHECK_LONGEST => {
                std::thread::sleep(Duration::from_millis(200))
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let Some(status) = status else {
        // What it started may still hold the pipe, so its output is not
        // waited for.
        let minutes = CHECK_LONGEST.as_secs() / 60;
        return Err(format!("{command} ran longer than {minutes} minutes"));
    };
    let out = reader.join().unwrap_or_default();
    if status.success() {
        Ok(out)
    } else {
        Err(out)
    }
}
