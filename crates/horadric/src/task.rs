//! `horadric quest`, or `horadric task` from before the rename: how an
//! agent working a quest reports back, and how anyone adds to the quest log
//! from a shell.
//!
//! The command changes the file itself rather than asking the app to, so an
//! agent hears at once whether it worked. It finds its item by the session
//! it runs in, `HORADRIC_SESSION`, which is written beside the item when a
//! session takes it. Then it tells the app, which reads the list again and
//! lets the runner move on.

use std::path::{Path, PathBuf};

use horadric_core::chronicle::{self, Happened, Record};
use horadric_core::tasks::{self, Mark, Wait};
use horadric_core::tombs;
use horadric_hooks::listener::TasksChanged;
use horadric_hooks::{
    client, tasks as file, COMMAND_HEADER, OWNER_ENV, SESSION_ENV, TASKS_ENV, TASKS_PATH,
};

const USAGE: &str = "\
usage: horadric quest done [\"summary\"]  The quest this session works is completed,
                                        with one line on what it achieved
       horadric quest blocked \"why\"     It can not go on without the human
             [--on-quest \"title\"]     or until another quest is done,
             [--on-main <ref>]         a commit or branch is on main,
             [--on-file <path>]        a file exists,
             [--on-cmd \"command\"]     a command (cmd.exe) exits 0,
             [--until <+30m|UTC time>] or a time comes: then it goes on
       horadric quest add \"title\"       Add a quest to the end of the log
             [--notes \"text\"]          with notes for the agent under it
       horadric quest list              Show the log";

/// What the errors call the list, which may still be the old file.
const NO_LOG: &str = "no quest log (.horadric/quests.md or .horadric/tasks.md) above here";

pub fn run(args: &[String]) -> Result<(), String> {
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    match args.first().map(String::as_str) {
        Some("done") => report(&cwd, None, &args[1..].join(" ")),
        Some("blocked") => {
            let (why, wait) = why_and_wait(&args[1..], unix_now())?;
            report(&cwd, Some((&why, wait.as_ref())), "")
        }
        Some("add") => {
            let (title, notes) = title_and_notes(&args[1..]);
            if title.is_empty() {
                return Err("say what: horadric quest add \"title\"".into());
            }
            add(&cwd, &title, &notes)
        }
        Some("list") => list(&cwd),
        _ => Err(USAGE.into()),
    }
}

/// Why `quest blocked` is blocked, and what it waits on when a flag says:
/// the words before the flag are the why, the word after it the wait. A
/// wait is reason enough, so the why may be left out then.
fn why_and_wait(args: &[String], now: u64) -> Result<(String, Option<Wait>), String> {
    let flag = args.iter().position(|a| a.starts_with("--"));
    let (words, rest) = args.split_at(flag.unwrap_or(args.len()));
    let why = tasks::one_line(&words.join(" "));
    let wait = match rest {
        [] => None,
        [flag, value @ ..] => {
            let value = value.join(" ");
            if value.trim().is_empty() {
                return Err(format!("say what {flag} waits on"));
            }
            Some(match flag.as_str() {
                "--on-quest" => Wait::Quest(value),
                "--on-main" => Wait::Main(value),
                "--on-file" => Wait::File(value),
                "--on-cmd" => Wait::Cmd(value),
                "--until" => Wait::Until(tasks::parse_when(&value, now).ok_or(format!(
                    "--until takes +30m (or s, h, d), Unix seconds or a UTC time like \
                     2026-10-01T14:05Z, not {value}"
                ))?),
                _ => return Err(USAGE.into()),
            })
        }
    };
    match (why.is_empty(), &wait) {
        (true, None) => Err("say why: horadric quest blocked \"what you need\"".into()),
        (true, Some(_)) => Ok(("waits".into(), wait)),
        (false, _) => Ok((why, wait)),
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The agent's item is done, or blocked with why and what it waits on,
/// and what the agent says it achieved, for the chronicle.
fn report(cwd: &Path, blocked: Option<(&str, Option<&Wait>)>, summary: &str) -> Result<(), String> {
    let why = blocked.map(|(w, _)| w);
    let wait = blocked.and_then(|(_, w)| w);
    let id = session().ok_or("this is not a Horadric session, so there is no item to report on")?;
    if let Some((batch, _)) = tombs::of(&id) {
        let project = held(cwd, batch).ok_or(format!("{NO_LOG} has a quest held by {batch}"))?;
        record_summary(&project, &id, summary);
        return report_tomb(&project, &id, why);
    }
    let project = held(cwd, &id).ok_or(format!("{NO_LOG} has a quest held by {id}"))?;
    record_summary(&project, &id, summary);
    let mode = file::mode(&project);
    let mark = match why {
        Some(_) => Mark::Blocked,
        None => mode.finished(),
    };
    let changed = file::update(&project, |text| tasks::set_held(text, &id, mark, why, wait))
        .map_err(|e| format!("{}: {e}", file::file(&project).display()))?;
    if !changed {
        return Err(format!("the quest held by {id} is already completed"));
    }
    tell_app(&project);
    match (why, wait, mark) {
        (Some(_), Some(_), _) => println!(
            "Marked blocked until then. Horadric tells this session to go on once it \
             holds, so stop here."
        ),
        (Some(_), None, _) => {
            println!("Marked blocked. Say what you need, then wait for the human.")
        }
        (None, _, Mark::Done) => {
            println!("Quest completed. The next one starts once this turn ends.")
        }
        _ => println!("Marked for review. The human looks next; stop here."),
    }
    Ok(())
}

/// A tomb's report leaves the list alone, since the item is the batch's
/// until the human picks. Only the app keeps it, so it has to hear.
fn report_tomb(project: &Path, id: &str, why: Option<&str>) -> Result<(), String> {
    let heard = post_app(&TasksChanged {
        dir: project.to_string_lossy().into_owned(),
        tomb: Some(id.to_string()),
        why: why.map(str::to_string),
    });
    if heard != Some(200) {
        return Err("Horadric did not hear the report. Tell the human you are finished.".into());
    }
    match why {
        Some(_) => println!("Told the human you are blocked. Say what you need, then wait."),
        None => println!("Marked done. The human compares the tombs and picks one; stop here."),
    }
    Ok(())
}

/// The title and the notes of `quest add`: the words before `--notes`
/// make the title, on one line, and the words after it the notes.
fn title_and_notes(args: &[String]) -> (String, String) {
    let (title, notes) = match args.iter().position(|a| a == "--notes") {
        Some(i) => (&args[..i], &args[i + 1..]),
        None => (args, &[][..]),
    };
    (tasks::one_line(&title.join(" ")), notes.join(" "))
}

fn add(cwd: &Path, title: &str, notes: &str) -> Result<(), String> {
    let project = session()
        .and_then(|id| held(cwd, &id))
        .or_else(main_list)
        .or_else(|| file::find_list(cwd))
        .unwrap_or_else(|| cwd.to_path_buf());
    file::update(&project, |text| {
        Some(tasks::append_with_notes(text, title, notes))
    })
    .map_err(|e| format!("{}: {e}", file::file(&project).display()))?;
    // Inside a session the new quest grows out of whatever that session
    // works, which the quest log draws as a branch: its quest, or else its
    // conversation, which Claude Code names to the commands it runs.
    if let Some(by) = session() {
        let conversation = std::env::var("CLAUDE_CODE_SESSION_ID").unwrap_or_default();
        record(
            &project,
            String::new(),
            title,
            Happened::Added { by, conversation },
        );
    }
    tell_app(&project);
    println!("Added to {}", file::file(&project).display());
    Ok(())
}

fn list(cwd: &Path) -> Result<(), String> {
    let project = main_list().or_else(|| file::find_list(cwd)).ok_or(NO_LOG)?;
    for t in tasks::parse(&file::read(&project)) {
        let holder = t.holder.map(|h| format!(" @{h}")).unwrap_or_default();
        println!("[{}] {}{holder}", t.mark.char(), t.title);
    }
    Ok(())
}

/// Keeps the agent's word on its quest, before the mark that ends it, so
/// the quest log has it however the quest ends up. Said nothing, nothing
/// is kept.
fn record_summary(project: &Path, id: &str, summary: &str) {
    let text = tasks::one_line(summary).trim().to_string();
    if text.is_empty() {
        return;
    }
    let list = tasks::parse(&file::read(project));
    let title = chronicle::worked_by(&list, id)
        .map(|t| t.title.clone())
        .unwrap_or_default();
    record(project, id.to_string(), &title, Happened::Summary { text });
}

/// Appends to the same chronicle the app writes. It is a record, not the
/// report, so it can not fail the command.
fn record(project: &Path, quest: String, title: &str, what: Happened) {
    horadric_ui::chronicle(&Record {
        at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs()),
        project: horadric_ui::folder_key(&project.to_string_lossy()),
        quest,
        title: title.to_string(),
        what,
    });
}

/// The project whose list has the session's item: above `cwd`, or, for a
/// session in a worktree of its own, in the main working tree.
fn held(cwd: &Path, id: &str) -> Option<PathBuf> {
    file::find_held(cwd, id).or_else(|| file::find_held(&main_tree()?, id))
}

/// The list in the main working tree, for a session in a worktree, which
/// has none of its own or an old copy.
fn main_list() -> Option<PathBuf> {
    file::find_list(&main_tree()?)
}

fn main_tree() -> Option<PathBuf> {
    std::env::var_os(TASKS_ENV)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
}

fn session() -> Option<String> {
    std::env::var(SESSION_ENV).ok().filter(|s| !s.is_empty())
}

/// Asks the Horadric that owns this session, or the one on the usual port,
/// to read the list again. It would notice by itself within a second, so a
/// Horadric that is not listening is no error.
fn tell_app(project: &Path) {
    post_app(&TasksChanged {
        dir: PathBuf::from(project).to_string_lossy().into_owned(),
        ..TasksChanged::default()
    });
}

/// Posts to the Horadric that owns this session, and says what it answered.
fn post_app(body: &TasksChanged) -> Option<u16> {
    let port = std::env::var(OWNER_ENV)
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or_else(horadric_hooks::port);
    client::post(
        port,
        TASKS_PATH,
        &[(COMMAND_HEADER, "tasks")],
        &body.to_json(),
    )
    .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(s: &[&str]) -> Vec<String> {
        s.iter().map(|w| w.to_string()).collect()
    }

    #[test]
    fn blocked_takes_why_then_what_it_waits_on() {
        let w = |a: &[&str]| why_and_wait(&words(a), 1000);
        assert_eq!(w(&["needs", "a key"]), Ok(("needs a key".into(), None)));
        assert_eq!(
            w(&["needs B", "--on-quest", "Build", "B"]),
            Ok(("needs B".into(), Some(Wait::Quest("Build B".into()))))
        );
        assert_eq!(
            w(&["--on-file", "out/x"]),
            Ok(("waits".into(), Some(Wait::File("out/x".into()))))
        );
        assert_eq!(
            w(&["later", "--until", "+1m"]),
            Ok(("later".into(), Some(Wait::Until(1060))))
        );
        assert_eq!(
            w(&["x", "--on-cmd", "git diff --quiet"]),
            Ok(("x".into(), Some(Wait::Cmd("git diff --quiet".into()))))
        );
        assert_eq!(
            w(&["x", "--on-main", "abc"]),
            Ok(("x".into(), Some(Wait::Main("abc".into()))))
        );
        assert!(w(&[]).is_err());
        assert!(w(&["x", "--on-quest"]).is_err());
        assert!(w(&["x", "--until", "soon"]).is_err());
        assert!(w(&["x", "--on-moon", "y"]).is_err());
    }

    #[test]
    fn a_quest_takes_its_notes_after_the_flag() {
        assert_eq!(
            title_and_notes(&words(&[
                "Fix the",
                "login",
                "--notes",
                "Only after\nexpiry"
            ])),
            ("Fix the login".into(), "Only after\nexpiry".into())
        );
        assert_eq!(
            title_and_notes(&words(&["Fix", "the login"])),
            ("Fix the login".into(), String::new())
        );
        assert_eq!(
            title_and_notes(&words(&["--notes", "why"])),
            (String::new(), "why".into())
        );
    }
}
