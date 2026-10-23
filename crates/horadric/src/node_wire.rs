//! What a node says that only a node says, the half with no I/O: its
//! session and project lists, made from its registry with the builders in
//! `horadric_core::portal`. Everything else a viewer hears comes from the
//! `Server` in `horadric_core::serving`, as on the Windows UI.
//!
//! Built on every platform so its tests run on the Windows machine that
//! writes it; only the Linux build uses it.

#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use std::collections::BTreeSet;

use horadric_core::portal::{projects_message, session_entry};
use horadric_core::Session;
use serde_json::{json, Value};

/// The last part of a working folder, which names its project.
pub fn project_name(cwd: &str) -> &str {
    let trimmed = cwd.trim_end_matches(['/', '\\']);
    trimmed.rsplit(['/', '\\']).next().unwrap_or(trimmed)
}

/// The `sessions` message. `hosted` says whether this node holds the
/// session's console, which an attach needs; one posting hooks from a
/// terminal of its own is listed but cannot be attached.
pub fn sessions<'a>(all: impl Iterator<Item = (&'a Session, bool)>) -> Value {
    let list: Vec<Value> = all
        .map(|(s, hosted)| {
            let mut e = session_entry(s, &s.cwd, false);
            e["hosted"] = json!(hosted);
            e
        })
        .collect();
    json!({ "t": "sessions", "list": list })
}

/// The `projects` message: every folder a session here works in, which
/// is where `start` may put another.
pub fn projects<'a>(all: impl Iterator<Item = &'a Session>) -> Value {
    let dirs: BTreeSet<&str> = all
        .map(|s| s.cwd.as_str())
        .filter(|c| !c.is_empty())
        .collect();
    let list: Vec<(String, String)> = dirs
        .into_iter()
        .map(|d| (d.to_string(), project_name(d).to_string()))
        .collect();
    projects_message(&list)
}

/// A connection's `s`. The replay guard it feeds matters on the relay,
/// whose frames anyone may record; over `ssh` it only has to differ from
/// the last one, so the clock and the channel are enough.
pub fn fresh_s(nanos: u128, ch: u32) -> String {
    format!("{:032x}", nanos ^ ((ch as u128) << 96))
}

#[cfg(test)]
mod tests {
    use super::*;
    use horadric_core::Phase;

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
        assert_eq!(m["list"][0]["project"], "/srv/app");
        assert_eq!(m["list"][0]["hosted"], true);
        assert_eq!(m["list"][1]["hosted"], false);
    }

    #[test]
    fn projects_are_the_folders_sessions_work_in_once_each() {
        let a = Session::new("a", "a", "/srv/app");
        let b = Session::new("b", "b", "/srv/app");
        let c = Session::new("c", "c", "");
        let m = projects([&a, &b, &c].into_iter());
        assert_eq!(m["t"], "projects");
        assert_eq!(m["list"].as_array().unwrap().len(), 1);
        assert_eq!(m["list"][0]["path"], "/srv/app");
        assert_eq!(m["list"][0]["name"], "app");
    }

    #[test]
    fn each_connection_gets_its_own_s() {
        assert_ne!(fresh_s(5, 1), fresh_s(5, 2));
        assert_ne!(fresh_s(5, 1), fresh_s(6, 1));
        assert_eq!(fresh_s(0, 0).len(), 32);
    }
}
