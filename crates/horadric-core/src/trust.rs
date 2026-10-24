//! Claude Code's folder trust, kept in `.claude.json` under `projects`.
//!
//! A session started from the phone has no one at the PC to answer
//! "Do you trust the files in this folder?", and the phone's human already
//! chose the folder, so Horadric answers yes for them beforehand.

use serde_json::{Map, Value};

/// The key `.claude.json` files a folder under: forward slashes, no
/// trailing one, as Claude Code writes it on Windows.
pub fn project_key(dir: &str) -> String {
    let key = dir.replace('\\', "/");
    match key.trim_end_matches('/') {
        "" => key,
        k if k.ends_with(':') => format!("{k}/"),
        k => k.to_string(),
    }
}

/// Whether Claude Code already trusts `key`: it or a folder above it was
/// accepted, the way Claude Code itself looks.
pub fn trusted(config: &Value, key: &str) -> bool {
    let Some(projects) = config.get("projects").and_then(Value::as_object) else {
        return false;
    };
    let accepted = |k: &str| {
        projects
            .iter()
            .any(|(p, v)| p.eq_ignore_ascii_case(k) && v["hasTrustDialogAccepted"] == true)
    };
    let mut k = key;
    loop {
        if accepted(k) {
            return true;
        }
        match k.trim_end_matches('/').rfind('/') {
            Some(i) if i > 0 => {
                k = if k[..i].ends_with(':') {
                    &k[..=i]
                } else {
                    &k[..i]
                }
            }
            _ => return false,
        }
    }
}

/// Marks `key` trusted in `.claude.json`'s contents, the rest left as it
/// was. False when it was trusted already, so nothing needs writing. Only
/// the folder itself counts here: Claude Code 2.1.3 asked again in a
/// worktree whose parent (or home folder) was trusted, on Windows and on
/// Linux alike.
pub fn trust(config: &mut Value, key: &str) -> bool {
    let accepted = config
        .get("projects")
        .and_then(Value::as_object)
        .is_some_and(|p| {
            p.iter()
                .any(|(k, v)| k.eq_ignore_ascii_case(key) && v["hasTrustDialogAccepted"] == true)
        });
    if accepted {
        return false;
    }
    if !config.is_object() {
        *config = Value::Object(Map::new());
    }
    let projects = config
        .as_object_mut()
        .expect("made an object above")
        .entry("projects")
        .or_insert_with(|| Value::Object(Map::new()));
    if !projects.is_object() {
        *projects = Value::Object(Map::new());
    }
    let entry = projects
        .as_object_mut()
        .expect("made an object above")
        .entry(key)
        .or_insert_with(|| Value::Object(Map::new()));
    match entry.as_object_mut() {
        Some(e) => {
            e.insert("hasTrustDialogAccepted".into(), Value::Bool(true));
        }
        None => *entry = serde_json::json!({"hasTrustDialogAccepted": true}),
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn keys_use_forward_slashes() {
        assert_eq!(project_key(r"C:\Users\me\code\x"), "C:/Users/me/code/x");
        assert_eq!(project_key(r"C:\Users\me\code\x\"), "C:/Users/me/code/x");
        assert_eq!(project_key(r"C:\"), "C:/");
    }

    #[test]
    fn a_folder_above_counts() {
        let c = json!({"projects": {"C:/Users/me/code": {"hasTrustDialogAccepted": true}}});
        assert!(trusted(&c, "C:/Users/me/code/x"));
        assert!(trusted(&c, "c:/users/me/code"));
        assert!(!trusted(&c, "C:/Users/me"));
        assert!(!trusted(&c, "C:/Users/me/codex"));
    }

    #[test]
    fn a_trusted_drive_counts() {
        let c = json!({"projects": {"C:/": {"hasTrustDialogAccepted": true}}});
        assert!(trusted(&c, "C:/Users/me"));
    }

    #[test]
    fn an_unaccepted_entry_does_not() {
        let c = json!({"projects": {"C:/a": {"hasTrustDialogAccepted": false}}});
        assert!(!trusted(&c, "C:/a"));
        assert!(!trusted(&json!({}), "C:/a"));
    }

    #[test]
    fn trusting_keeps_the_rest() {
        let mut c = json!({
            "userID": "u",
            "projects": {"C:/a": {"hasTrustDialogAccepted": false, "allowedTools": ["x"]}}
        });
        assert!(trust(&mut c, "C:/a"));
        assert_eq!(c["userID"], "u");
        assert_eq!(c["projects"]["C:/a"]["allowedTools"], json!(["x"]));
        assert_eq!(c["projects"]["C:/a"]["hasTrustDialogAccepted"], true);
        assert!(!trust(&mut c, "C:/a"));
        assert!(!trust(&mut c, "c:/A"));
    }

    #[test]
    fn a_folder_below_a_trusted_one_is_trusted_itself() {
        let mut c = json!({"projects": {"/home/agent": {"hasTrustDialogAccepted": true}}});
        assert!(trust(&mut c, "/home/agent/repo.main-2"));
        assert_eq!(
            c["projects"]["/home/agent/repo.main-2"]["hasTrustDialogAccepted"],
            true
        );
    }

    #[test]
    fn trusting_makes_what_is_missing() {
        let mut c = Value::Null;
        assert!(trust(&mut c, "C:/a"));
        assert!(trusted(&c, "C:/a"));
    }
}
