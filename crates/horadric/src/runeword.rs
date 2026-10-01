//! `horadric runeword list`: every stone a project's Runetome has, so the
//! Runesmith, or anyone, can check a stone it wrote parses before the
//! human looks for it on the tile.

use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use horadric_core::runeword::{self, Rune};
use horadric_core::tasks::{one_line, CONFIG_FILE};
use horadric_hooks::TASKS_ENV;
use serde_json::Value;

use crate::CREATE_NO_WINDOW;

const USAGE: &str =
    "usage: horadric runeword list    Every stone this project has, and any that do not parse";

/// The file of stones every project has, beside `state.json`.
pub const GLOBAL_FILE: &str = "runewords.json";

pub fn run(args: &[String]) -> Result<(), String> {
    match args.first().map(String::as_str) {
        Some("list") => list(),
        _ => Err(USAGE.into()),
    }
}

fn list() -> Result<(), String> {
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    let project = project(&cwd);
    let config = project.join(CONFIG_FILE);
    let global = global_file();
    let read = |p: &Path| std::fs::read_to_string(p).unwrap_or_default();
    let mut stones = built_in();
    stones.extend(listed(&read(&config), Kind::Project));
    if let Some(g) = &global {
        stones.extend(listed(&read(g), Kind::Global));
    }
    println!("Stones of {}", project.display());
    println!("  this project's: {}", config.display());
    if let Some(g) = &global {
        println!("  every project's: {}", g.display());
    }
    for s in &stones {
        print!("\n{}", show(s));
    }
    let cracked = stones.iter().filter(|s| s.steps.is_err()).count();
    if cracked > 0 {
        return Err(format!(
            "{cracked} stone{} did not parse",
            if cracked == 1 { "" } else { "s" }
        ));
    }
    Ok(())
}

/// The folder whose config the app reads for this one: the main working
/// tree's, since a worktree's copy may be missing or old.
fn project(cwd: &Path) -> PathBuf {
    if let Some(dir) = std::env::var_os(TASKS_ENV).filter(|d| !d.is_empty()) {
        return PathBuf::from(dir);
    }
    let start = main_tree_dir(cwd).unwrap_or_else(|| cwd.to_path_buf());
    start
        .ancestors()
        .find(|d| d.join(CONFIG_FILE).is_file())
        .map(Path::to_path_buf)
        .unwrap_or(start)
}

/// The same folder in the main working tree when `cwd` is in a linked one.
fn main_tree_dir(cwd: &Path) -> Option<PathBuf> {
    let out = Command::new("git")
        .args([
            "--no-optional-locks",
            "rev-parse",
            "--path-format=absolute",
            "--git-common-dir",
            "--show-prefix",
        ])
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    let out = String::from_utf8_lossy(&out.stdout).into_owned();
    let mut lines = out.lines();
    let common = lines.next()?.trim().replace('\\', "/");
    let top = common.strip_suffix("/.git")?;
    let prefix = lines.next().unwrap_or("").trim();
    Some(Path::new(top).join(prefix))
}

fn global_file() -> Option<PathBuf> {
    let name = if horadric_hooks::dev() {
        "Horadric-dev"
    } else {
        "Horadric"
    };
    std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join(name).join(GLOBAL_FILE))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    BuiltIn,
    Project,
    Global,
}

/// One stone as the list shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Listed {
    label: String,
    kind: Kind,
    /// Each step in words, or why the stone does not parse.
    steps: Result<Vec<String>, String>,
}

fn built_in() -> Vec<Listed> {
    runeword::offered(runeword::stones("", ""))
        .into_iter()
        .map(|(label, runes)| Listed {
            label,
            kind: Kind::BuiltIn,
            steps: Ok(runes.iter().map(rune_step).collect()),
        })
        .collect()
}

fn rune_step(r: &Rune) -> String {
    match r {
        Rune::Say(text) => format!("say \"{text}\""),
        other => other.word(),
    }
}

/// The stones in a config's `runewords`. A file that is not JSON is one
/// cracked stone, so a broken file is seen rather than looking empty.
fn listed(text: &str, kind: Kind) -> Vec<Listed> {
    if text.trim().is_empty() {
        return Vec::new();
    }
    let v = match serde_json::from_str::<Value>(text) {
        Ok(v) => v,
        Err(e) => {
            return vec![Listed {
                label: "(the whole file)".into(),
                kind,
                steps: Err(format!("the file is not JSON: {e}")),
            }]
        }
    };
    let Some(words) = v.get("runewords") else {
        return Vec::new();
    };
    let Some(words) = words.as_object() else {
        return vec![Listed {
            label: "runewords".into(),
            kind,
            steps: Err("\"runewords\" is not an object of stones by label".into()),
        }];
    };
    words
        .iter()
        .map(|(label, stone)| Listed {
            label: one_line(label),
            kind,
            steps: steps(stone),
        })
        .collect()
}

/// A stone's steps in words: the list form, `["test", "Tidy up"]`, or the
/// object form, `{"steps": [{"say": "..."}, {"keys": "..."}, {"run": "..."}]}`.
fn steps(stone: &Value) -> Result<Vec<String>, String> {
    let list = match stone {
        Value::Array(a) => a,
        Value::Object(o) => o
            .get("steps")
            .and_then(Value::as_array)
            .ok_or("it has no \"steps\" list")?,
        _ => return Err("it is neither a list of steps nor {\"steps\": [...]}".into()),
    };
    if list.is_empty() {
        return Err("it has no steps".into());
    }
    list.iter()
        .enumerate()
        .map(|(i, s)| step(s).map_err(|e| format!("step {} {e}", i + 1)))
        .collect()
}

fn step(s: &Value) -> Result<String, String> {
    if let Some(word) = s.as_str() {
        return Rune::parse(word)
            .map(|r| rune_step(&r))
            .ok_or_else(|| "is empty".into());
    }
    let o = s
        .as_object()
        .ok_or("is neither a word nor an object such as {\"say\": \"...\"}")?;
    let kinds = ["say", "keys", "run"];
    let found: Vec<&str> = kinds.into_iter().filter(|k| o.contains_key(*k)).collect();
    let kind = match found.as_slice() {
        [one] => *one,
        [] => return Err("names no step: say, keys or run".into()),
        _ => return Err(format!("names more than one step: {}", found.join(", "))),
    };
    let text = o[kind]
        .as_str()
        .filter(|t| !t.trim().is_empty())
        .ok_or_else(|| format!("has a \"{kind}\" that is not text"))?;
    let shown = o.get("show").and_then(Value::as_bool) == Some(true);
    Ok(match (kind, shown) {
        ("run", true) => format!("run \"{text}\" in a pane"),
        _ => format!("{kind} \"{text}\""),
    })
}

fn show(s: &Listed) -> String {
    let whose = match s.kind {
        Kind::BuiltIn => "built in",
        Kind::Project => "this project",
        Kind::Global => "every project",
    };
    let mut out = format!("{}  \"{}\"  ({whose})\n", runeword::name(&s.label), s.label);
    match &s.steps {
        Ok(steps) => {
            for (i, step) in steps.iter().enumerate() {
                out.push_str(&format!("  {}. {step}\n", i + 1));
            }
        }
        Err(why) => out.push_str(&format!("  CRACKED: {why}\n")),
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_forms_list_their_steps_in_words() {
        let text = r#"{ "runewords": {
            "Fresh start": { "steps": [ { "keys": "/clear{Enter}" },
                                        { "say": "Take the next quest" } ] },
            "Open the site": { "steps": [ { "run": "start http://localhost:3000" },
                                          { "run": "npm run dev", "show": true } ] },
            "Ship": ["test", "Update the changelog", "merge"]
        } }"#;
        let l = listed(text, Kind::Project);
        assert_eq!(l.len(), 3);
        assert_eq!(l[0].label, "Fresh start");
        assert_eq!(
            l[0].steps,
            Ok(vec![
                "keys \"/clear{Enter}\"".to_string(),
                "say \"Take the next quest\"".to_string()
            ])
        );
        assert_eq!(
            l[1].steps.as_ref().unwrap()[1],
            "run \"npm run dev\" in a pane"
        );
        assert_eq!(
            l[2].steps,
            Ok(vec![
                "test".to_string(),
                "say \"Update the changelog\"".to_string(),
                "merge".to_string()
            ])
        );
    }

    #[test]
    fn a_stone_that_does_not_parse_says_why() {
        let text = r#"{ "runewords": {
            "Empty": [],
            "Odd": 3,
            "Twice": { "steps": [ "test", { "say": "a", "run": "b" } ] },
            "Nothing": { "steps": [ { "wait": 5 } ] },
            "Bare": { "steps": [ { "run": "" } ] }
        } }"#;
        let why: Vec<String> = listed(text, Kind::Global)
            .into_iter()
            .map(|s| s.steps.unwrap_err())
            .collect();
        // In label order, as serde_json keeps an object.
        assert_eq!(
            why,
            [
                "step 1 has a \"run\" that is not text",
                "it has no steps",
                "step 1 names no step: say, keys or run",
                "it is neither a list of steps nor {\"steps\": [...]}",
                "step 2 names more than one step: say, run",
            ]
        );
    }

    #[test]
    fn a_broken_file_is_one_cracked_stone_and_no_file_is_none() {
        let l = listed("{ \"runewords\": ", Kind::Project);
        assert_eq!(l.len(), 1);
        assert!(l[0]
            .steps
            .as_ref()
            .unwrap_err()
            .starts_with("the file is not JSON"));
        assert!(listed("", Kind::Project).is_empty());
        assert!(listed("{\"worktrees\": {}}", Kind::Project).is_empty());
    }

    #[test]
    fn the_list_shows_the_name_label_and_steps() {
        let built = built_in();
        assert_eq!(built.len(), 3);
        let shown = show(&built[0]);
        assert!(shown.starts_with(&runeword::name("Test, merge")), "{shown}");
        assert!(shown.contains("\"Test, merge\"  (built in)"));
        assert!(shown.contains("  1. test\n  2. merge\n"));
        let cracked = Listed {
            label: "Odd".into(),
            kind: Kind::Project,
            steps: Err("it has no steps".into()),
        };
        assert!(show(&cracked).contains("CRACKED: it has no steps"));
    }
}
