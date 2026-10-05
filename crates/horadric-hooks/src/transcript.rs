//! Reads a conversation's title out of its transcript file, and lists the
//! conversations Claude Code keeps for a folder.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

use horadric_core::{title, Agent, Limits, Title};

/// Claude Code repeats the title every few turns, so the end of the file
/// nearly always has it. A transcript is megabytes by the end of a long day.
const TAIL: u64 = 256 * 1024;

/// A file bigger than this is not read whole when its tail has no title.
const WHOLE: u64 = 64 * 1024 * 1024;

/// The conversation's title, or `None` when it has none yet or the file
/// can not be read.
pub fn title(path: &str) -> Option<Title> {
    let mut file = File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let found = read_from(&mut file, len.saturating_sub(TAIL)).and_then(|b| title::latest(&b));
    if found.is_some() || len <= TAIL || len > WHOLE {
        return found;
    }
    read_from(&mut file, 0).and_then(|b| title::latest(&b))
}

/// Whether the conversation `id`, held in `cwd`, is in the middle of a
/// turn, see [`title::mid_turn`]. None when its transcript can not be read.
pub fn mid_turn(cwd: &str, id: &str) -> Option<bool> {
    let root = projects_root(
        std::env::var("CLAUDE_CONFIG_DIR").ok().as_deref(),
        std::env::var("USERPROFILE").ok().as_deref(),
    )?;
    folders(&root, cwd).iter().find_map(|dir| {
        let mut file = File::open(dir.join(format!("{id}.jsonl"))).ok()?;
        let len = file.metadata().ok()?.len();
        title::mid_turn(&read_from(&mut file, len.saturating_sub(TAIL))?)
    })
}

/// The transcript of the conversation `id` held in `cwd`, if it is on
/// disk. A quest's worktree may be long gone, and with it the spelling of
/// its folder, so failing that every folder is looked in.
pub fn path(cwd: &str, id: &str) -> Option<PathBuf> {
    let root = projects_root(
        std::env::var("CLAUDE_CONFIG_DIR").ok().as_deref(),
        std::env::var("USERPROFILE").ok().as_deref(),
    )?;
    path_in(&root, cwd, id)
}

fn path_in(root: &Path, cwd: &str, id: &str) -> Option<PathBuf> {
    // An id is a file name, never a way out of the folder.
    if id.is_empty() || id.contains(['/', '\\', '.']) {
        return None;
    }
    let file = format!("{id}.jsonl");
    let found = |dir: &Path| Some(dir.join(&file)).filter(|p| p.is_file());
    folders(root, cwd)
        .iter()
        .find_map(|d| found(d))
        .or_else(|| {
            std::fs::read_dir(root)
                .ok()?
                .flatten()
                .find_map(|e| found(&e.path()))
        })
}

/// A conversation an agent kept, which its resume carries on.
#[derive(Debug, Clone, PartialEq)]
pub struct Past {
    pub id: String,
    pub title: Title,
    pub modified: SystemTime,
    /// When it started, as near as the file system says.
    pub started: SystemTime,
    /// Whose conversation it is, which says how to carry it on.
    pub agent: Agent,
}

/// A menu is not the place for every conversation ever held in a folder,
/// and each one costs a read.
const LOOKED_AT: usize = 40;

/// The newest `limit` conversations held in `cwd`, newest first, leaving
/// out the ids in `skip`. Only ones with a title count: Claude Code writes
/// one after the first prompt, so one without never got going.
pub fn history(cwd: &str, skip: &[String], limit: usize) -> Vec<Past> {
    let Some(root) = projects_root(
        std::env::var("CLAUDE_CONFIG_DIR").ok().as_deref(),
        std::env::var("USERPROFILE").ok().as_deref(),
    ) else {
        return Vec::new();
    };
    list_in(&root, cwd, skip, limit)
}

fn list_in(root: &Path, cwd: &str, skip: &[String], limit: usize) -> Vec<Past> {
    let mut files: Vec<(PathBuf, SystemTime, u64, SystemTime)> = folders(root, cwd)
        .iter()
        .filter_map(|dir| std::fs::read_dir(dir).ok())
        .flatten()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "jsonl"))
        .filter_map(|e| {
            let meta = e.metadata().ok()?;
            let modified = meta.modified().ok()?;
            let created = meta.created().unwrap_or(modified).min(modified);
            Some((e.path(), modified, meta.len(), created))
        })
        .collect();
    files.sort_by_key(|f| std::cmp::Reverse(f.1));
    files
        .into_iter()
        .filter_map(|(path, modified, len, started)| {
            let id = path.file_stem()?.to_str()?.to_string();
            Some((path, id, modified, len, started))
        })
        .filter(|(_, id, ..)| !skip.contains(id))
        .take(LOOKED_AT)
        .filter_map(|(path, id, modified, len, started)| {
            let title = kept_title(&path, modified, len)?;
            Some(Past {
                id,
                title,
                modified,
                started,
                agent: Agent::Claude,
            })
        })
        .take(limit)
        .collect()
}

/// Where Claude Code keeps its transcripts, one folder per working
/// directory.
fn projects_root(config_dir: Option<&str>, home: Option<&str>) -> Option<PathBuf> {
    let base = match (config_dir.filter(|d| !d.is_empty()), home) {
        (Some(dir), _) => PathBuf::from(dir),
        (None, Some(home)) if !home.is_empty() => Path::new(home).join(".claude"),
        _ => return None,
    };
    Some(base.join("projects"))
}

/// The folders under `root` holding `cwd`'s transcripts. More than one when
/// the drive letter was written in both cases, which Claude Code keeps
/// apart.
fn folders(root: &Path, cwd: &str) -> Vec<PathBuf> {
    let want = folder_name(cwd);
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().eq_ignore_ascii_case(&want))
        .map(|e| e.path())
        .collect()
}

/// The folder name Claude Code gives a working directory: every character
/// that is not an ASCII letter or digit becomes a hyphen.
fn folder_name(cwd: &str) -> String {
    cwd.trim_end_matches(['\\', '/'])
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// [`tail_title`], remembered while the file stays as it was. A menu
/// opening reads the tails of dozens of transcripts, megabytes, and a
/// conversation that is over never changes.
fn kept_title(path: &Path, modified: SystemTime, len: u64) -> Option<Title> {
    type Kept = HashMap<PathBuf, (SystemTime, u64, Option<Title>)>;
    static KEPT: OnceLock<Mutex<Kept>> = OnceLock::new();
    let kept = KEPT.get_or_init(Default::default);
    let same = |k: &Kept| {
        k.get(path)
            .filter(|(m, l, _)| (*m, *l) == (modified, len))
            .map(|(.., t)| t.clone())
    };
    if let Some(title) = kept.lock().ok().and_then(|k| same(&k)) {
        return title;
    }
    let title = tail_title(path);
    if let Ok(mut k) = kept.lock() {
        k.insert(path.to_path_buf(), (modified, len, title.clone()));
    }
    title
}

/// The title from the end of the file only, for a menu that is opening
/// or a tool that just finished: no reading a long transcript whole on
/// the off chance.
pub fn tail_title(path: &Path) -> Option<Title> {
    let mut file = File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    read_from(&mut file, len.saturating_sub(TAIL)).and_then(|b| title::latest(&b))
}

/// The limits in the newest `token_count` near the end of a Codex rollout.
pub fn codex_limits(path: &Path) -> Option<Limits> {
    let mut file = File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let tail = read_from(&mut file, len.saturating_sub(TAIL))?;
    Limits::from_codex(&String::from_utf8_lossy(&tail))
}

fn read_from(file: &mut File, at: u64) -> Option<Vec<u8>> {
    file.seek(SeekFrom::Start(at)).ok()?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf).ok()?;
    Some(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn scratch(name: &str, lines: &[String]) -> String {
        let path =
            std::env::temp_dir().join(format!("horadric-test-{name}-{}.jsonl", std::process::id()));
        let mut f = File::create(&path).unwrap();
        for l in lines {
            writeln!(f, "{l}").unwrap();
        }
        path.to_string_lossy().to_string()
    }

    #[test]
    fn a_transcript_is_found_by_its_folder_or_else_anywhere() {
        let root = std::env::temp_dir().join(format!("horadric-test-paths-{}", std::process::id()));
        let own = root.join("C--code-app");
        let other = root.join("C--code-app-tree");
        std::fs::create_dir_all(&own).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        File::create(own.join("a1.jsonl")).unwrap();
        File::create(other.join("b2.jsonl")).unwrap();
        assert_eq!(
            path_in(&root, r"C:\code\app", "a1"),
            Some(own.join("a1.jsonl"))
        );
        assert_eq!(
            path_in(&root, r"C:\gone\tree", "b2"),
            Some(other.join("b2.jsonl"))
        );
        assert_eq!(path_in(&root, r"C:\code\app", "c3"), None);
        assert_eq!(path_in(&root, r"C:\code\app", "../a1"), None);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn finds_a_title_far_behind_a_huge_tool_result() {
        let big = format!(
            r#"{{"type":"user","content":"{}"}}"#,
            "x".repeat(TAIL as usize * 2)
        );
        let path = scratch(
            "far",
            &[r#"{"type":"ai-title","aiTitle":"Early title"}"#.into(), big],
        );
        assert_eq!(title(&path).map(|t| t.text).as_deref(), Some("Early title"));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn codex_limits_are_read_from_the_rollouts_end() {
        let count = r#"{"type":"event_msg","payload":{"type":"token_count","rate_limits":{"primary":{"used_percent":7.0,"window_minutes":300,"resets_at":50}}}}"#;
        let path = scratch("codex", &[count.into(), r#"{"type":"event_msg"}"#.into()]);
        let l = codex_limits(Path::new(&path)).unwrap();
        assert_eq!(l.five_hour.map(|l| l.used), Some(7.0));
        let _ = std::fs::remove_file(path);
        assert_eq!(codex_limits(Path::new("C:/no/such/rollout.jsonl")), None);
    }

    #[test]
    fn folder_names_match_claude_codes() {
        assert_eq!(
            folder_name(r"C:\Users\morte\Documents\Github\horadric.dev"),
            "C--Users-morte-Documents-Github-horadric-dev"
        );
        assert_eq!(folder_name("c:/work/a b_c/"), "c--work-a-b-c");
    }

    #[test]
    fn the_config_dir_beats_home() {
        assert_eq!(
            projects_root(Some("D:/cc"), Some("C:/Users/x")),
            Some(PathBuf::from("D:/cc").join("projects"))
        );
        assert_eq!(
            projects_root(Some(""), Some("C:/Users/x")),
            Some(Path::new("C:/Users/x").join(".claude").join("projects"))
        );
        assert_eq!(projects_root(None, None), None);
    }

    #[test]
    fn history_is_newest_first_titled_and_skips_the_open_ones() {
        let root = std::env::temp_dir().join(format!("horadric-test-root-{}", std::process::id()));
        let upper = root.join("C--work-app");
        let lower = root.join("c--work-app");
        let other = root.join("C--work-app-2");
        for d in [&upper, &lower, &other] {
            std::fs::create_dir_all(d).unwrap();
        }
        let titled = |t: &str| format!(r#"{{"type":"ai-title","aiTitle":"{t}"}}"#);
        let write = |dir: &Path, id: &str, body: &str, minute: u64| {
            let path = dir.join(format!("{id}.jsonl"));
            std::fs::write(&path, body).unwrap();
            let at = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(minute * 60);
            File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_modified(at)
                .unwrap();
        };
        write(&upper, "old", &titled("Old work"), 1);
        write(&lower, "untitled", r#"{"type":"mode"}"#, 2);
        write(&other, "elsewhere", &titled("Another folder"), 3);
        write(&lower, "open", &titled("Open now"), 4);
        write(&upper, "new", &titled("New work"), 5);

        let found = list_in(&root, r"C:\work\app", &["open".to_string()], 10);
        let ids: Vec<&str> = found.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, vec!["new", "old"]);
        assert_eq!(found[0].title.text, "New work");
        assert_eq!(list_in(&root, r"C:\work\app", &[], 1).len(), 1);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_missing_file_has_no_title() {
        assert_eq!(title("C:/nowhere/at/all.jsonl"), None);
        assert_eq!(title(""), None);
    }
}
