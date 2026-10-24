//! What a node says that only a node says, the half with no I/O: its
//! session and project lists, made from its registry with the builders in
//! `horadric_core::portal`. Everything else a viewer hears comes from the
//! `Server` in `horadric_core::serving`, as on the Windows UI.
//!
//! Built on every platform so its tests run on the Windows machine that
//! writes it; only the Linux build uses it.

#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use std::collections::BTreeSet;
use std::path::Path;

use horadric_core::portal::{projects_message, session_entry};
use horadric_core::{Agent, Session};
use serde_json::{json, Value};

/// The last part of a working folder, which names its project.
pub fn project_name(cwd: &str) -> &str {
    let trimmed = cwd.trim_end_matches(['/', '\\']);
    trimmed.rsplit(['/', '\\']).next().unwrap_or(trimmed)
}

/// The `sessions` message. `hosted` says whether this node holds the
/// session's console, which an attach needs; one posting hooks from a
/// terminal of its own is listed but cannot be attached. The project is
/// named as in `projects`, as a Windows PC names it, since a client
/// titles its cluster with it and finds the folder to start in by it.
pub fn sessions<'a>(all: impl Iterator<Item = (&'a Session, bool)>) -> Value {
    let list: Vec<Value> = all
        .map(|(s, hosted)| {
            let mut e = session_entry(s, project_name(&s.cwd), false);
            e["hosted"] = json!(hosted);
            e
        })
        .collect();
    json!({ "t": "sessions", "list": list })
}

/// The `projects` message: every folder a session here works in or worked
/// in lately (`recent`), which is where `start` may put another.
pub fn projects<'a>(all: impl Iterator<Item = &'a Session>, recent: &'a [String]) -> Value {
    let dirs: BTreeSet<&str> = all
        .map(|s| s.cwd.as_str())
        .chain(recent.iter().map(String::as_str))
        .filter(|c| !c.is_empty())
        .collect();
    let list: Vec<(String, String)> = dirs
        .into_iter()
        .map(|d| (d.to_string(), project_name(d).to_string()))
        .collect();
    projects_message(&list)
}

/// The folders a new session may be started in or a project made in: the
/// parents of the folders sessions worked in, and the home folder, once
/// each. Never `/` or the temp folder, which hold no projects.
pub fn roots(projects: &[String], home: Option<&str>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let parents = projects.iter().filter_map(|p| {
        let p = p.trim_end_matches('/');
        p.rfind('/').map(|i| &p[..i])
    });
    for root in home.into_iter().chain(parents) {
        let root = root.trim_end_matches('/');
        let skip = root.is_empty() || root == "/tmp" || root.starts_with("/tmp/");
        if !skip && !out.iter().any(|o| o == root) {
            out.push(root.to_string());
        }
    }
    out
}

/// A folder in a root, as `folders` lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Folder {
    pub path: String,
    pub name: String,
    pub git: bool,
    /// Last changed, in seconds since 1970, for the order.
    pub changed: u64,
}

/// At most this many folders a root.
pub const FOLDERS_MAX: usize = 300;

/// The `folders` message: each root with its folders newest first, hidden
/// ones left out.
pub fn folders_message(roots: Vec<(String, Vec<Folder>)>) -> Value {
    let roots: Vec<Value> = roots
        .into_iter()
        .map(|(path, mut list)| {
            list.retain(|f| !f.name.starts_with('.'));
            list.sort_by(|a, b| b.changed.cmp(&a.changed).then_with(|| a.name.cmp(&b.name)));
            list.truncate(FOLDERS_MAX);
            let list: Vec<Value> = list
                .iter()
                .map(|f| json!({"path": f.path, "name": f.name, "git": f.git}))
                .collect();
            json!({"path": path, "name": project_name(&path), "list": list})
        })
        .collect();
    json!({"t": "folders", "roots": roots})
}

/// Why a new project's folder name will not do, or None when it will: 1 to
/// 64 letters, digits, spaces, dots, dashes and underscores, not starting
/// with a dot or a space.
pub fn bad_name(name: &str) -> Option<&'static str> {
    if name.is_empty() || name.chars().count() > 64 {
        return Some("The name must be 1 to 64 characters.");
    }
    if name.starts_with(['.', ' ']) {
        return Some("The name cannot start with a dot or a space.");
    }
    if !name
        .chars()
        .all(|c| c.is_alphanumeric() || matches!(c, ' ' | '.' | '-' | '_'))
    {
        return Some("Use letters, digits, spaces, dots, dashes and underscores.");
    }
    None
}

/// The name a viewer gave in its hello, which a handoff to it is asked by.
pub fn hello_name(hello: &Value) -> Option<String> {
    hello["name"]
        .as_str()
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .map(str::to_string)
}

/// What a viewer's hello says this node is called there (`you`), which is
/// what a session handed between them names it.
pub fn hello_you(hello: &Value) -> Option<String> {
    hello["you"]
        .as_str()
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .map(str::to_string)
}

/// A brother as the side that reads the moved line knows it: the name it
/// was `heard` by there, else the name it gave itself (`own`, its host
/// name for this node).
pub fn known_as(heard: Option<&str>, own: &str) -> String {
    heard
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .unwrap_or(own)
        .to_string()
}

/// `path` with the folders agents install themselves into added after it:
/// Claude Code's native installer and npm's user prefix under `home`, and
/// `/usr/local/bin`. A node runs as a systemd user service, whose PATH
/// has none of them.
pub fn search_path(path: &str, home: Option<&str>) -> String {
    let mut dirs: Vec<String> = path
        .split(':')
        .filter(|d| !d.is_empty())
        .map(str::to_string)
        .collect();
    let home = home.map(|h| h.trim_end_matches('/'));
    let extra = home
        .into_iter()
        .flat_map(|h| {
            [".local/bin", ".claude/local", ".npm-global/bin"].map(|d| format!("{h}/{d}"))
        })
        .chain(["/usr/local/bin".to_string()]);
    for d in extra {
        if !dirs.contains(&d) {
            dirs.push(d);
        }
    }
    dirs.join(":")
}

/// A connection's `s`. The replay guard it feeds matters on the relay,
/// whose frames anyone may record; over `ssh` it only has to differ from
/// the last one, so the clock and the channel are enough.
pub fn fresh_s(nanos: u128, ch: u32) -> String {
    format!("{:032x}", nanos ^ ((ch as u128) << 96))
}

/// Whether `program` is `agent` itself, which takes Horadric's flags,
/// and not a shell put in its place with `HORADRIC_AGENT`.
pub fn is_agent(program: &Path, agent: Agent) -> bool {
    program
        .file_stem()
        .and_then(|s| Agent::from_name(&s.to_string_lossy()))
        == Some(agent)
}

/// This program's path as `current_exe` gave it, less the ` (deleted)`
/// Linux adds once the file was replaced under the running node, as an
/// update or a reinstall does: the new build is at the same path, and
/// what a session starts must be that one.
pub fn live_exe(exe: &str) -> &str {
    exe.strip_suffix(" (deleted)").unwrap_or(exe)
}

#[cfg(test)]
mod tests {
    use super::*;
    use horadric_core::Phase;

    #[test]
    fn a_stand_in_for_the_agent_gets_none_of_its_flags() {
        let claude = Path::new("/home/a/.local/bin/claude");
        assert!(is_agent(claude, Agent::Claude));
        assert!(!is_agent(claude, Agent::Codex));
        assert!(!is_agent(Path::new("/bin/bash"), Agent::Claude));
        assert!(is_agent(Path::new("/usr/bin/codex"), Agent::Codex));
    }

    #[test]
    fn a_replaced_program_is_started_from_its_path() {
        assert_eq!(
            live_exe("/h/.local/bin/horadric (deleted)"),
            "/h/.local/bin/horadric"
        );
        assert_eq!(live_exe("/h/.local/bin/horadric"), "/h/.local/bin/horadric");
    }

    #[test]
    fn a_project_is_named_by_its_folder() {
        assert_eq!(project_name("/home/me/work/horadric/"), "horadric");
        assert_eq!(project_name(r"C:\p\x"), "x");
        assert_eq!(project_name("x"), "x");
    }

    #[test]
    fn the_list_says_which_sessions_are_hosted() {
        let mut a = Session::new("a1", "fix", "/srv/app");
        a.phase = Phase::Working;
        let b = Session::new("b1", "b1", "/tmp");
        let m = sessions([(&a, true), (&b, false)].into_iter());
        assert_eq!(m["t"], "sessions");
        assert_eq!(m["list"][0]["id"], "a1");
        assert_eq!(m["list"][0]["phase"], "working");
        assert_eq!(m["list"][0]["project"], "app");
        assert_eq!(m["list"][0]["hosted"], true);
        assert_eq!(m["list"][1]["hosted"], false);
    }

    #[test]
    fn projects_are_the_folders_sessions_work_in_once_each() {
        let a = Session::new("a", "a", "/srv/app");
        let b = Session::new("b", "b", "/srv/app");
        let c = Session::new("c", "c", "");
        let m = projects([&a, &b, &c].into_iter(), &[]);
        assert_eq!(m["t"], "projects");
        assert_eq!(m["list"].as_array().unwrap().len(), 1);
        let recent = vec!["/srv/old".to_string(), "/srv/app".to_string()];
        let m = projects([&a].into_iter(), &recent);
        assert_eq!(m["list"].as_array().unwrap().len(), 2);
        assert_eq!(m["list"][0]["path"], "/srv/app");
        assert_eq!(m["list"][0]["name"], "app");
    }

    #[test]
    fn roots_are_home_and_the_parents_of_projects_once_each() {
        let projects = vec![
            "/home/a/work/x".to_string(),
            "/home/a/work/y/".to_string(),
            "/srv/z".to_string(),
            "/top".to_string(),
            "/tmp/scratch".to_string(),
        ];
        assert_eq!(
            roots(&projects, Some("/home/a")),
            vec!["/home/a", "/home/a/work", "/srv"]
        );
        assert!(roots(&[], None).is_empty());
    }

    #[test]
    fn folders_go_newest_first_without_hidden_ones() {
        let f = |name: &str, changed| Folder {
            path: format!("/r/{name}"),
            name: name.into(),
            git: name == "b",
            changed,
        };
        let m = folders_message(vec![("/r".into(), vec![f("a", 1), f(".x", 9), f("b", 5)])]);
        assert_eq!(m["t"], "folders");
        assert_eq!(m["roots"][0]["name"], "r");
        let list = m["roots"][0]["list"].as_array().unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0]["name"], "b");
        assert_eq!(list[0]["git"], true);
    }

    #[test]
    fn project_names_are_checked() {
        assert_eq!(bad_name("my app-2_x.y"), None);
        assert!(bad_name("").is_some());
        assert!(bad_name(".hidden").is_some());
        assert!(bad_name("a/b").is_some());
        assert!(bad_name(&"x".repeat(65)).is_some());
    }

    #[test]
    fn a_hello_names_its_viewer_or_not() {
        assert_eq!(
            hello_name(&json!({"name": " Desk "})).as_deref(),
            Some("Desk")
        );
        assert_eq!(hello_name(&json!({"name": ""})), None);
        assert_eq!(hello_name(&json!({})), None);
    }

    #[test]
    fn a_brother_is_named_as_the_other_side_knows_it() {
        assert_eq!(hello_you(&json!({"you": " vps "})).as_deref(), Some("vps"));
        assert_eq!(hello_you(&json!({"you": ""})), None);
        assert_eq!(hello_you(&json!({"name": "Desk"})), None);
        assert_eq!(known_as(Some("vps"), "srv1387547"), "vps");
        assert_eq!(known_as(Some(" "), "srv1387547"), "srv1387547");
        assert_eq!(known_as(None, "srv1387547"), "srv1387547");
    }

    #[test]
    fn the_agents_install_folders_are_searched_after_path() {
        assert_eq!(
            search_path("/usr/bin:/home/a/.local/bin", Some("/home/a/")),
            "/usr/bin:/home/a/.local/bin:/home/a/.claude/local:/home/a/.npm-global/bin:/usr/local/bin"
        );
        assert_eq!(search_path("", None), "/usr/local/bin");
    }

    #[test]
    fn each_connection_gets_its_own_s() {
        assert_ne!(fresh_s(5, 1), fresh_s(5, 2));
        assert_ne!(fresh_s(5, 1), fresh_s(6, 1));
        assert_eq!(fresh_s(0, 0).len(), 32);
    }
}
