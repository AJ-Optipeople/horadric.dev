//! `horadric runeword list`: every stone a project's Runetome has, so the
//! Runesmith, or anyone, can check a stone it wrote parses before the
//! human looks for it on the tile.

use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use horadric_core::runeword::{self, Source, Stone};
use horadric_core::tasks::CONFIG_FILE;
use horadric_hooks::TASKS_ENV;

use crate::CREATE_NO_WINDOW;

const USAGE: &str =
    "usage: horadric runeword list    Every stone this project has, and any that do not parse";

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
    let global = horadric_hooks::tasks::runewords_file();
    let read = |p: &Path| std::fs::read_to_string(p).unwrap_or_default();
    let project_text = read(&config);
    let global_text = global.as_deref().map(read).unwrap_or_default();
    println!("Stones of {}", project.display());
    println!("  this project's: {}", config.display());
    if let Some(g) = &global {
        println!("  every project's: {}", g.display());
    }
    let stones = runeword::stones(&project_text, &global_text);
    for s in &stones {
        print!(
            "
{}",
            show(s)
        );
    }
    let broken = [
        (&project_text, Source::Project),
        (&global_text, Source::Global),
    ]
    .into_iter()
    .filter_map(|(text, source)| broken_file(text, source))
    .collect::<Vec<_>>();
    for b in &broken {
        print!(
            "
{}",
            show(b)
        );
    }
    let cracked = stones.iter().filter(|s| s.steps.is_err()).count() + broken.len();
    if cracked > 0 {
        return Err(format!(
            "{cracked} stone{} did not parse",
            if cracked == 1 { "" } else { "s" }
        ));
    }
    Ok(())
}

/// A file the Runetome cannot read at all, as one cracked stone, since
/// the tile shows none of its stones and the list should say why.
fn broken_file(text: &str, source: Source) -> Option<Stone> {
    runeword::parse(text).err().map(|why| Stone {
        label: "(the whole file)".into(),
        steps: Err(why),
        source,
        about: String::new(),
        errand: None,
    })
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

fn show(s: &Stone) -> String {
    let whose = match s.source {
        Source::BuiltIn => "built in",
        Source::Project => "this project",
        Source::Global => "every project",
    };
    let mut out = format!("{}  \"{}\"  ({whose})\n", runeword::name(&s.label), s.label);
    if !s.about.is_empty() {
        out.push_str(&format!("  {}\n", s.about));
    }
    match &s.steps {
        Ok(runes) => {
            for (i, rune) in runes.iter().enumerate() {
                out.push_str(&format!("  {}. {}\n", i + 1, rune.describe()));
            }
        }
        Err(why) => out.push_str(&format!("  CRACKED: {why}\n")),
    }
    if let Some(e) = &s.errand {
        out.push_str(&format!(
            "  an errand: {}, for {} at most{}, once armed in the tome\n",
            runeword::describe(e.every),
            runeword::length(e.most),
            if e.bypass && !s.sessionless() {
                ", skipping permission prompts"
            } else {
                ""
            }
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_list_shows_the_name_label_and_steps() {
        let stones = runeword::stones(
            r#"{ "runewords": { "Open": { "steps": [ { "run": "npm run dev", "show": true } ] } } }"#,
            "",
        );
        let shown = show(&stones[0]);
        assert!(shown.starts_with(&runeword::name("Approve")), "{shown}");
        assert!(
            shown.contains("\"Approve\"  (built in)\n  Presses Enter"),
            "{shown}"
        );
        assert!(shown.contains("  1. keys {Enter}\n"), "{shown}");
        let open = show(stones.last().unwrap());
        assert!(open.contains("\"Open\"  (this project)"), "{open}");
        assert!(open.contains("  1. run npm run dev (shown)\n"), "{open}");
    }

    #[test]
    fn an_errand_says_when_it_runs() {
        let stones = runeword::stones(
            r#"{ "runewords": { "Clean": { "every": "day 03:00", "steps": [ { "run": "cargo clean" } ] } } }"#,
            "",
        );
        let shown = show(stones.last().unwrap());
        assert!(
            shown.ends_with(
                "  an errand: every day at 03:00, for 30m at most, once armed in the tome\n"
            ),
            "{shown}"
        );
    }

    #[test]
    fn a_cracked_stone_says_why() {
        let stones = runeword::stones("", r#"{ "runewords": { "Empty": [] } }"#);
        let shown = show(stones.last().unwrap());
        assert!(shown.contains("(every project)"), "{shown}");
        assert!(shown.contains("CRACKED: it has no steps"), "{shown}");
    }

    #[test]
    fn a_file_that_is_not_json_is_one_cracked_stone() {
        let b = broken_file("{ \"runewords\": ", Source::Project).unwrap();
        assert_eq!(b.label, "(the whole file)");
        assert!(b.steps.unwrap_err().starts_with("not JSON"));
        assert!(broken_file("", Source::Project).is_none());
        assert!(broken_file("{\"worktrees\": {}}", Source::Global).is_none());
    }
}
