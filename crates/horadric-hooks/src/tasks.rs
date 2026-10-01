//! Reading and writing a project's task list and its mode on disk, for the
//! app and for `horadric quest`. What the file means is
//! `horadric_core::tasks`; this is only where it lives and how a change
//! lands in it.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use horadric_core::fleet::{self, Device};
use horadric_core::runeword::{self, Stone};
use horadric_core::tasks::{self, Mode, CONFIG_FILE, OLD_FILE, QUESTS_FILE};
use horadric_core::{ssh, worktree};

/// The list's path in a project.
pub fn file(project: &Path) -> PathBuf {
    project.join(rel(project))
}

/// The list's path from the project folder: the quest log, or a list from
/// before the rename while there is no quest log beside it.
pub fn rel(project: &Path) -> &'static str {
    tasks::list_file(
        project.join(QUESTS_FILE).is_file(),
        project.join(OLD_FILE).is_file(),
    )
}

/// The config's path in a project.
pub fn config_file(project: &Path) -> PathBuf {
    project.join(CONFIG_FILE)
}

/// The list's text, empty when there is none yet.
pub fn read(project: &Path) -> String {
    read_text(&file(project))
}

/// The project's mode. No config, or one that says nothing, is manual.
pub fn mode(project: &Path) -> Mode {
    tasks::mode(&read_text(&config_file(project)))
}

/// How many items the project's runner holds at once.
pub fn parallel(project: &Path) -> usize {
    tasks::parallel(&read_text(&config_file(project)))
}

/// The project's SSH hosts, none without a config.
pub fn hosts(project: &Path) -> Vec<String> {
    ssh::hosts(&read_text(&config_file(project)))
}

/// The project's inventory and the devices in it, None when its config
/// names none. A path in the config is from the project's folder. A file
/// that cannot be read is an empty fleet rather than none, so the agent is
/// still told where it should be.
pub fn fleet(project: &Path) -> Option<(PathBuf, Vec<Device>)> {
    let path = project.join(fleet::inventory(&read_text(&config_file(project)))?);
    let devices = fleet::parse(&read_text(&path));
    Some((path, devices))
}

/// Adds `host` to the project's hosts. False when it was there already or
/// is not something `ssh` takes as one destination.
pub fn add_host(project: &Path, host: &str) -> io::Result<bool> {
    let path = config_file(project);
    let old = read_text(&path);
    match ssh::with_host(&old, host) {
        Some(new) => write(&path, &new).map(|_| true),
        None => Ok(false),
    }
}

/// The plain host names in the user's `~/.ssh/config`, none without one.
pub fn ssh_config_hosts() -> Vec<String> {
    let Some(home) = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")) else {
        return Vec::new();
    };
    let path = Path::new(&home).join(".ssh").join("config");
    ssh::config_hosts(&read_text(&path))
}

/// Every stone the project's Runetome holds, those that do not parse too.
pub fn stones(project: &Path) -> Vec<Stone> {
    runeword::stones(&read_text(&config_file(project)), &global_runewords())
}

/// The file of stones every project shares, `runewords.json` beside the
/// app's state: `%APPDATA%\Horadric`, or `Horadric-dev` for a dev instance.
pub fn runewords_file() -> Option<PathBuf> {
    let name = if crate::dev() {
        "Horadric-dev"
    } else {
        "Horadric"
    };
    std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join(name).join("runewords.json"))
}

fn global_runewords() -> String {
    runewords_file().map(|f| read_text(&f)).unwrap_or_default()
}

/// What the project's config says about worktrees.
pub fn worktrees(project: &Path) -> worktree::Settings {
    worktree::settings(&read_text(&config_file(project)))
}

/// Puts the project in trunk mode, or with `enabled` gives each new session
/// a worktree of its own.
pub fn set_worktrees(project: &Path, enabled: bool) -> io::Result<()> {
    let path = config_file(project);
    let old = read_text(&path);
    write(&path, &worktree::with_enabled(&old, enabled))
}

pub fn set_mode(project: &Path, mode: Mode) -> io::Result<()> {
    let path = config_file(project);
    let old = read_text(&path);
    write(&path, &tasks::with_mode(&old, mode))
}

/// How many items the runner holds at once, each in a worktree of its own.
pub fn set_parallel(project: &Path, n: usize) -> io::Result<()> {
    let path = config_file(project);
    let old = read_text(&path);
    write(&path, &tasks::with_parallel(&old, n))
}

/// Reads the list, lets `change` rewrite it and writes the result back.
/// Read and written in one go, so an edit made in an editor a moment
/// before is not lost. False when `change` found nothing to change.
pub fn update(project: &Path, change: impl FnOnce(&str) -> Option<String>) -> io::Result<bool> {
    let path = file(project);
    let old = read_text(&path);
    match change(&old) {
        Some(new) if new != old => write(&path, &new).map(|_| true),
        Some(_) => Ok(true),
        None => Ok(false),
    }
}

/// Writes through a file beside it and a rename, so a reader never sees
/// half a list.
fn write(path: &Path, text: &str) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("horadric-tmp");
    fs::write(&tmp, text)?;
    fs::rename(&tmp, path)
}

/// A file's text, empty when there is none. Without a UTF-8 byte order
/// mark: PowerShell 5 writes one, and left in it hides the first item of a
/// list and makes a config unreadable JSON.
fn read_text(path: &Path) -> String {
    let text = fs::read_to_string(path).unwrap_or_default();
    match text.strip_prefix('\u{feff}') {
        Some(rest) => rest.to_string(),
        None => text,
    }
}

/// The project folder above `from`, or `from` itself, whose list has an
/// item held by `holder`. An agent's shell can be anywhere in its project.
pub fn find_held(from: &Path, holder: &str) -> Option<PathBuf> {
    from.ancestors()
        .find(|dir| {
            tasks::parse(&read(dir))
                .iter()
                .any(|t| t.holder.as_deref() == Some(holder))
        })
        .map(Path::to_path_buf)
}

/// The project folder above `from`, or `from` itself, that has a list.
pub fn find_list(from: &Path) -> Option<PathBuf> {
    from.ancestors()
        .find(|dir| file(dir).is_file())
        .map(Path::to_path_buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("horadric-tasks-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn an_update_writes_only_when_something_changed() {
        let dir = scratch("update");
        assert!(update(&dir, |t| Some(tasks::append(t, "First"))).unwrap());
        assert_eq!(read(&dir), "- [ ] First\n");
        assert!(!update(&dir, |_| None).unwrap());
        assert!(!dir.join(".horadric/quests.horadric-tmp").exists());
        assert!(dir.join(QUESTS_FILE).is_file());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_list_from_before_the_rename_keeps_its_file() {
        let dir = scratch("old");
        write(
            &dir.join(OLD_FILE),
            "- [ ] First
",
        )
        .unwrap();
        assert!(update(&dir, |t| Some(tasks::append(t, "Second"))).unwrap());
        assert_eq!(
            read(&dir),
            "- [ ] First
- [ ] Second
"
        );
        assert!(!dir.join(QUESTS_FILE).exists());
        assert_eq!(find_list(&dir.join("src")), Some(dir.clone()));
        write(
            &dir.join(QUESTS_FILE),
            "- [ ] Quest
",
        )
        .unwrap();
        assert_eq!(
            read(&dir),
            "- [ ] Quest
"
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn parallel_is_read_from_the_config() {
        let dir = scratch("parallel");
        assert_eq!(parallel(&dir), 1);
        write(&config_file(&dir), "{\"tasks\":{\"parallel\":3}}").unwrap();
        assert_eq!(parallel(&dir), 3);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_host_is_added_to_the_config_once() {
        let dir = scratch("host");
        set_mode(&dir, Mode::Auto).unwrap();
        assert!(add_host(&dir, "myvps").unwrap());
        assert!(!add_host(&dir, "myvps").unwrap());
        assert_eq!(hosts(&dir), vec!["myvps"]);
        assert_eq!(mode(&dir), Mode::Auto);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_fleet_is_read_from_the_inventory_the_config_names() {
        let dir = scratch("fleet");
        assert_eq!(fleet(&dir), None);
        write(&config_file(&dir), "{\"inventory\":\"ops/devices.ini\"}").unwrap();
        let (path, devices) = fleet(&dir).unwrap();
        assert_eq!(path, dir.join("ops/devices.ini"));
        assert!(devices.is_empty());
        write(
            &path,
            "[gw]
gw-1 ansible_host=10.0.0.1
",
        )
        .unwrap();
        let (_, devices) = fleet(&dir).unwrap();
        assert_eq!(devices[0].destination, "10.0.0.1");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_mode_is_kept_in_the_config() {
        let dir = scratch("mode");
        assert_eq!(mode(&dir), Mode::Manual);
        set_mode(&dir, Mode::Auto).unwrap();
        assert_eq!(mode(&dir), Mode::Auto);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_byte_order_mark_hides_neither_the_first_item_nor_the_mode() {
        let dir = scratch("bom");
        write(&file(&dir), "\u{feff}- [ ] First\n- [ ] Second\n").unwrap();
        write(
            &config_file(&dir),
            "\u{feff}{\"tasks\":{\"mode\":\"auto\"}}",
        )
        .unwrap();
        assert_eq!(tasks::parse(&read(&dir))[0].title, "First");
        assert_eq!(mode(&dir), Mode::Auto);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_list_is_found_from_a_folder_inside_the_project() {
        let dir = scratch("find");
        let deep = dir.join("src").join("ui");
        fs::create_dir_all(&deep).unwrap();
        assert_eq!(find_list(&deep).filter(|d| d.starts_with(&dir)), None);
        update(&dir, |_| Some("- [/] Fix it @fix-1\n".into())).unwrap();
        assert_eq!(find_list(&deep), Some(dir.clone()));
        assert_eq!(find_held(&deep, "fix-1"), Some(dir.clone()));
        assert_eq!(find_held(&deep, "other-2"), None);
        fs::remove_dir_all(&dir).unwrap();
    }
}
