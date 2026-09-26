//! Which project a folder belongs to. A git worktree belongs to the
//! repository it was added to, so every worktree of a repository lands in
//! one cluster instead of a cluster each.

use std::collections::HashMap;
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use horadric_core::Session;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// The project a session belongs to.
pub fn project_key(s: &Session) -> String {
    folder_key(&s.cwd)
}

/// The project key of the project in this folder: the folder itself,
/// normalised, unless it is inside a linked worktree, in which case the
/// same folder in the repository's main working tree.
///
/// Asking git takes a process start, and keys are asked for on every
/// paint, so each folder is asked once and remembered. A folder that does
/// not exist may become a worktree later, so that answer is kept only for
/// [`MISSING_FOR`]: a swept worktree or a deleted project would otherwise
/// start git for each of its sessions in every paint of every cluster.
pub fn folder_key(dir: &str) -> String {
    let own = normalise(dir);
    if own.is_empty() {
        return own;
    }
    static KEYS: OnceLock<Mutex<HashMap<String, Known>>> = OnceLock::new();
    let keys = KEYS.get_or_init(Default::default);
    let now = Instant::now();
    if let Some(key) = keys
        .lock()
        .ok()
        .and_then(|k| k.get(&own).filter(|k| k.fresh(now)).map(|k| k.key.clone()))
    {
        return key;
    }
    let key = ask_git(dir)
        .and_then(|out| main_tree_key(&out))
        .unwrap_or_else(|| own.clone());
    let missing = (!Path::new(dir).is_dir()).then_some(now);
    if let Ok(mut k) = keys.lock() {
        k.insert(
            own,
            Known {
                key: key.clone(),
                missing,
            },
        );
    }
    key
}

/// How long the key of a folder that does not exist is trusted.
const MISSING_FOR: Duration = Duration::from_secs(5);

/// A folder's key as git gave it, and when, if the folder was missing.
struct Known {
    key: String,
    missing: Option<Instant>,
}

impl Known {
    fn fresh(&self, now: Instant) -> bool {
        self.missing
            .is_none_or(|at| now.saturating_duration_since(at) < MISSING_FOR)
    }
}

/// A readable name for a project key.
pub fn project_name(key: &str) -> String {
    key.rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(key)
        .to_string()
}

/// One spelling for a folder, so the same folder always gives the same key.
fn normalise(dir: &str) -> String {
    dir.replace('\\', "/")
        .trim_end_matches('/')
        .to_ascii_lowercase()
}

fn ask_git(dir: &str) -> Option<String> {
    let out = Command::new("git")
        .args([
            "--no-optional-locks",
            "rev-parse",
            "--path-format=absolute",
            "--git-dir",
            "--git-common-dir",
            "--show-prefix",
        ])
        .current_dir(dir)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The key from what `git rev-parse --git-dir --git-common-dir
/// --show-prefix` printed, or None when the folder is not inside a linked
/// worktree and its own path is the key. The main working tree is the
/// folder holding the common `.git`; a bare repository has none, and its
/// worktrees group under the repository folder instead.
fn main_tree_key(out: &str) -> Option<String> {
    let mut lines = out.lines();
    let git_dir = normalise(lines.next()?);
    let common = normalise(lines.next()?);
    let prefix = lines.next().unwrap_or("").trim_end_matches(['/', '\\']);
    if git_dir == common || common.is_empty() {
        return None;
    }
    let root = common.strip_suffix("/.git").unwrap_or(&common);
    Some(if prefix.is_empty() {
        root.to_string()
    } else {
        normalise(&format!("{root}/{prefix}"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_folder_is_asked_about_again_after_a_while() {
        let t0 = Instant::now();
        let known = |missing| Known {
            key: "c:/app".into(),
            missing,
        };
        assert!(known(None).fresh(t0 + Duration::from_secs(3600)));
        assert!(known(Some(t0)).fresh(t0 + Duration::from_secs(1)));
        assert!(!known(Some(t0)).fresh(t0 + MISSING_FOR));
    }

    #[test]
    fn normalise_gives_one_spelling() {
        assert_eq!(normalise(r"C:\Code\App\"), "c:/code/app");
        assert_eq!(normalise("c:/code/app"), "c:/code/app");
    }

    #[test]
    fn the_main_working_tree_keeps_its_own_key() {
        let out = "C:/Code/App/.git\nC:/Code/App/.git\n\n";
        assert_eq!(main_tree_key(out), None);
    }

    #[test]
    fn a_subfolder_of_the_main_tree_keeps_its_own_key() {
        let out = "C:/Code/App/.git\nC:/Code/App/.git\nweb/\n";
        assert_eq!(main_tree_key(out), None);
    }

    #[test]
    fn a_linked_worktree_maps_to_the_main_tree() {
        let out = "C:/Code/App/.git/worktrees/fix\nC:/Code/App/.git\n\n";
        assert_eq!(main_tree_key(out).as_deref(), Some("c:/code/app"));
    }

    #[test]
    fn a_subfolder_of_a_worktree_maps_to_the_same_subfolder() {
        let out = "C:/Code/App/.git/worktrees/fix\nC:/Code/App/.git\nweb/src/\n";
        assert_eq!(main_tree_key(out).as_deref(), Some("c:/code/app/web/src"));
    }

    #[test]
    fn worktrees_of_a_bare_repository_group_under_it() {
        let out = "C:/Code/app.git/worktrees/fix\nC:/Code/app.git\n\n";
        assert_eq!(main_tree_key(out).as_deref(), Some("c:/code/app.git"));
    }

    #[test]
    fn output_that_is_not_git_gives_no_key() {
        assert_eq!(main_tree_key(""), None);
    }

    #[test]
    fn project_name_is_the_last_part() {
        assert_eq!(project_name("c:/code/app"), "app");
        assert_eq!(project_name("app"), "app");
    }
}
