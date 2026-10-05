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

use horadric_core::chronicle::{self, Command, Happened, Record};
use horadric_core::tasks::{self, Mark, Wait};
use horadric_core::{aim, runeword, tombs, warriv};
use horadric_hooks::listener::TasksChanged;
use horadric_hooks::{
    client, tasks as file, COMMAND_HEADER, OWNER_ENV, SESSION_ENV, TASKS_ENV, TASKS_PATH,
};

const USAGE: &str = "\
usage: horadric quest done [\"summary\"]  The quest this session works is completed,
                                        with one line on what it achieved
       horadric quest blocked \"why\"     It can not go on without the human
             [--on \"title\"]           or until another quest is done,
             [--on-main <ref>]         a commit or branch is on main,
             [--on-file <path>]        a file exists,
             [--on-cmd \"command\"]     a command (cmd.exe) exits 0,
             [--until <+30m|UTC time>] or a time comes: then it goes on
       horadric quest add \"title\"       Add a quest to the end of the log
             [--notes \"text\"]          with notes for the agent under it,
             [--after \"title\"]         to start once that quest is done,
             [--below \"title\"]         right under that quest
       horadric quest note \"title\" \"text\"  Add a notes line under a quest
       horadric quest tell \"title\" \"text\"  Tell the session on a quest, between turns
       horadric quest pass \"title\"      A quest waiting for review is good: it lands
       horadric quest fix \"title\" \"what\"  Send a quest waiting for review back
       horadric quest blocked \"question\" --quest \"title\"
                                        Hand a quest a session holds to the human
       horadric quest aim \"text\"        Say where the work is going: Warriv files
                                        the next quests toward it when the log runs dry
       horadric quest aim done \"text\"   That aim is reached
       horadric quest list              Show the log";

/// What the errors call the list, which may still be the old file.
const NO_LOG: &str = "no quest log (.horadric/quests.md or .horadric/tasks.md) above here";

pub fn run(args: &[String]) -> Result<(), String> {
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    match args.first().map(String::as_str) {
        Some("done") => report(&cwd, None, &args[1..].join(" ")),
        Some("blocked") => {
            let (quest, rest) = quest_flag(&args[1..])?;
            let (why, wait) = why_and_wait(&rest, unix_now())?;
            match quest {
                Some(title) => hand_on(&cwd, &title, &why),
                None => report(&cwd, Some((&why, wait)), ""),
            }
        }
        Some("add") => {
            let (title, notes, below) = title_and_notes(&args[1..])?;
            if title.is_empty() {
                return Err("say what: horadric quest add \"title\"".into());
            }
            add(&cwd, &title, &notes, below.as_deref())
        }
        Some("note") => {
            let (title, text) = title_and_text(&args[1..], "note")?;
            note(&cwd, &title, &text)
        }
        Some("tell") => {
            let (title, text) = title_and_text(&args[1..], "tell")?;
            tell(&cwd, &title, &text)
        }
        Some("pass") => {
            let title = tasks::one_line(&args[1..].join(" "));
            if title.trim().is_empty() {
                return Err("say which: horadric quest pass \"title\"".into());
            }
            pass(&cwd, &title)
        }
        Some("fix") => {
            let (title, what) = title_and_text(&args[1..], "fix")?;
            fix(&cwd, &title, &what)
        }
        Some("aim") => match args.get(1).map(String::as_str) {
            Some("done") => aim(&cwd, &args[2..], true),
            _ => aim(&cwd, &args[1..], false),
        },
        Some("list") => list(&cwd),
        _ => Err(USAGE.into()),
    }
}

/// `--quest "title"` taken out of `quest blocked`'s words: the quest to
/// hand to the human, rather than the session's own.
fn quest_flag(args: &[String]) -> Result<(Option<String>, Vec<String>), String> {
    let Some(at) = args.iter().position(|a| a == "--quest") else {
        return Ok((None, args.to_vec()));
    };
    let tail = &args[at + 1..];
    let end = tail
        .iter()
        .position(|a| a.starts_with("--"))
        .unwrap_or(tail.len());
    let title = tasks::one_line(&tail[..end].join(" "));
    if title.is_empty() {
        return Err("say which quest: --quest \"title\"".into());
    }
    let mut rest = args[..at].to_vec();
    rest.extend_from_slice(&tail[end..]);
    if rest.iter().any(|a| a.starts_with("--")) {
        return Err(
            "a quest handed to the human waits on the human, so --quest takes no --on".into(),
        );
    }
    Ok((Some(title), rest))
}

/// The quest and the words of `quest note` and `quest tell`.
fn title_and_text(args: &[String], verb: &str) -> Result<(String, String), String> {
    let usage = || format!("usage: horadric quest {verb} \"title\" \"text\"");
    let [title, text @ ..] = args else {
        return Err(usage());
    };
    let text = tasks::one_line(&text.join(" "));
    if text.is_empty() {
        return Err(usage());
    }
    Ok((tasks::one_line(title), text))
}

/// What `quest blocked` waits on: a quest in the log, written as an
/// `After:` line under it, or something else the runner checks.
#[derive(Debug, PartialEq)]
enum On {
    Quest(String),
    Check(Wait),
}

/// Why `quest blocked` is blocked, and what it waits on when a flag says:
/// the words before the flag are the why, the word after it the wait. A
/// wait is reason enough, so the why may be left out then.
fn why_and_wait(args: &[String], now: u64) -> Result<(String, Option<On>), String> {
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
            // `--on-quest` is the flag from before `After:` lines.
            Some(match flag.as_str() {
                "--on" | "--on-quest" => On::Quest(tasks::one_line(&value)),
                "--on-main" => On::Check(Wait::Main(value)),
                "--on-file" => On::Check(Wait::File(value)),
                "--on-cmd" => On::Check(Wait::Cmd(value)),
                "--until" => On::Check(Wait::Until(tasks::parse_when(&value, now).ok_or(
                    format!(
                        "--until takes +30m (or s, h, d), Unix seconds or a UTC time like \
                         2026-10-01T14:05Z, not {value}"
                    ),
                )?)),
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
fn report(cwd: &Path, blocked: Option<(&str, Option<On>)>, summary: &str) -> Result<(), String> {
    let why = blocked.as_ref().map(|(w, _)| *w);
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
    let changed = file::update(&project, |text| match &wait {
        Some(On::Quest(title)) => tasks::block_after(text, &id, why, title),
        Some(On::Check(w)) => tasks::set_held(text, &id, mark, why, Some(w)),
        None => tasks::set_held(text, &id, mark, why, None),
    })
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
        ..TasksChanged::default()
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

/// The title and the notes of `quest add`: the words before the first flag
/// make the title, on one line, the words after `--notes` the notes, and
/// each `--after` an `After:` line under them. Last, the quest it goes
/// right under, from `--below`.
fn title_and_notes(args: &[String]) -> Result<(String, String, Option<String>), String> {
    let is_flag = |a: &String| a == "--notes" || a == "--after" || a == "--below";
    let first = args.iter().position(is_flag).unwrap_or(args.len());
    let title = tasks::one_line(&args[..first].join(" "));
    let mut notes = Vec::new();
    let mut after = Vec::new();
    let mut below = None;
    let mut rest = &args[first..];
    while let [flag, tail @ ..] = rest {
        let end = tail.iter().position(is_flag).unwrap_or(tail.len());
        let value = tail[..end].join(" ");
        if flag == "--after" {
            if value.trim().is_empty() {
                return Err("say which quest: --after \"title\"".into());
            }
            after.push(tasks::after_note(&value));
        } else if flag == "--below" {
            if value.trim().is_empty() {
                return Err("say which quest: --below \"title\"".into());
            }
            below = Some(tasks::one_line(&value));
        } else {
            notes.push(value);
        }
        rest = &tail[end..];
    }
    notes.extend(after);
    Ok((title, notes.join("\n"), below))
}

fn add(cwd: &Path, title: &str, notes: &str, below: Option<&str>) -> Result<(), String> {
    let project = session()
        .and_then(|id| held(cwd, &id))
        .or_else(main_list)
        .or_else(|| file::find_list(cwd))
        .unwrap_or_else(|| cwd.to_path_buf());
    write(&project, |text| {
        // An `After:` line naming either of two quests alike would match
        // both, so Warriv files none that is already in the log.
        if session().is_some_and(|id| warriv::is_warriv(&id)) {
            if let Some(t) = tasks::parse(text)
                .iter()
                .find(|t| aim::same(&t.title, title))
            {
                return Err(format!("\"{}\" is in the log already", t.title));
            }
        }
        match below {
            None => Ok(tasks::append_with_notes(text, title, notes)),
            Some(b) => warriv::add_below(text, b, title, notes),
        }
    })?;
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
    warriv_ran(&project, Command::Add, title, notes);
    tell_app(&project);
    println!("Added to {}", file::file(&project).display());
    Ok(())
}

/// Changes the log with `change`, which says what is wrong when it can
/// not.
fn write(
    project: &Path,
    change: impl FnOnce(&str) -> Result<String, String>,
) -> Result<(), String> {
    let mut wrong = None;
    file::update(project, |text| {
        change(text).map_err(|e| wrong = Some(e)).ok()
    })
    .map_err(|e| format!("{}: {e}", file::file(project).display()))?;
    wrong.map_or(Ok(()), Err)
}

/// The project whose log a command about any quest works on.
fn log_of(cwd: &Path) -> Result<PathBuf, String> {
    main_list()
        .or_else(|| file::find_list(cwd))
        .ok_or_else(|| NO_LOG.to_string())
}

/// A notes line under a quest, marked as Warriv's when Warriv wrote it.
fn note(cwd: &Path, title: &str, text: &str) -> Result<(), String> {
    let project = log_of(cwd)?;
    let line = match session() {
        Some(id) if warriv::is_warriv(&id) => warriv::note(text),
        _ => text.to_string(),
    };
    write(&project, |log| warriv::add_note(log, title, &line))?;
    warriv_ran(&project, Command::Note, title, text);
    // The away card lists what was assumed, for the human to overrule.
    if let Some(assumed) = tasks::assumed(text) {
        let what = Happened::Assumed {
            text: assumed.to_string(),
        };
        record(&project, String::new(), title, what);
    }
    tell_app(&project);
    println!("Noted under the quest.");
    Ok(())
}

/// Hands a quest a session holds to the human, with the question.
fn hand_on(cwd: &Path, title: &str, question: &str) -> Result<(), String> {
    let project = log_of(cwd)?;
    write(&project, |log| warriv::hand_on(log, title, question))?;
    warriv_ran(&project, Command::Blocked, title, question);
    tell_app(&project);
    println!("Handed to the human. The quest waits for their answer.");
    Ok(())
}

/// Has the app type `text` into the session holding a quest, once it is
/// between turns. A quest nobody holds has no session to hear it.
fn tell(cwd: &Path, title: &str, text: &str) -> Result<(), String> {
    let project = log_of(cwd)?;
    let list = tasks::parse(&file::read(&project));
    let t = match tasks::find(&list, title) {
        Ok(i) => &list[i],
        Err(0) => return Err(format!("no quest is called \"{title}\"")),
        Err(_) => return Err(format!("\"{title}\" matches more than one quest")),
    };
    if t.holder.is_none() || !t.mark.held() {
        return Err(format!(
            "no session holds \"{}\"; add a note to it instead",
            t.title
        ));
    }
    let heard = post_app(&TasksChanged {
        dir: project.to_string_lossy().into_owned(),
        quest: Some(t.title.clone()),
        tell: Some(text.to_string()),
        by: session(),
        ..TasksChanged::default()
    });
    if heard != Some(200) {
        return Err("Horadric did not hear it. Add a note to the quest instead.".into());
    }
    warriv_ran(&project, Command::Tell, &t.title, text);
    println!("Horadric types it into the quest's session once that is between turns.");
    Ok(())
}

/// A quest waiting for review is good: completed, so it lands on main as
/// the human's approval would.
fn pass(cwd: &Path, title: &str) -> Result<(), String> {
    let project = log_of(cwd)?;
    write(&project, |log| warriv::pass(log, title))?;
    tell_app(&project);
    println!("Passed. It lands on main as an approval would; stop here.");
    Ok(())
}

/// Sends a quest waiting for review back: the app tells its session what
/// is wrong, or files a fix-up quest when that session is gone, which only
/// the app knows.
fn fix(cwd: &Path, title: &str, what: &str) -> Result<(), String> {
    let project = log_of(cwd)?;
    let t = warriv::reviewed(&file::read(&project), title)?;
    let heard = post_app(&TasksChanged {
        dir: project.to_string_lossy().into_owned(),
        quest: Some(t.title),
        fix: Some(what.to_string()),
        by: session(),
        ..TasksChanged::default()
    });
    if heard != Some(200) {
        return Err("Horadric did not hear it. Add a note to the quest instead.".into());
    }
    println!("Sent back. Horadric tells its session, or files a fix-up quest; stop here.");
    Ok(())
}

/// Adds an aim to the top of the log, or marks one reached.
fn aim(cwd: &Path, words: &[String], done: bool) -> Result<(), String> {
    let text = tasks::one_line(&words.join(" "));
    if text.is_empty() {
        return Err(USAGE.into());
    }
    // The first aim may come before any quest, so it can start the log.
    let project = main_list()
        .or_else(|| file::find_list(cwd))
        .unwrap_or_else(|| cwd.to_path_buf());
    write(&project, |log| match done {
        true => aim::reach(log, &text),
        false => aim::add(log, &text),
    })?;
    tell_app(&project);
    println!(
        "{}",
        match done {
            true => "Marked reached.",
            false => "Aim added. Warriv files quests toward it when the log runs dry in auto mode.",
        }
    );
    Ok(())
}

fn list(cwd: &Path) -> Result<(), String> {
    let project = main_list().or_else(|| file::find_list(cwd)).ok_or(NO_LOG)?;
    let text = file::read(&project);
    for a in aim::read(&text) {
        let head = if a.reached { aim::REACHED } else { aim::OPEN };
        println!("{head}{}", a.text);
    }
    for t in tasks::parse(&text) {
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

/// A command Warriv ran, for the chronicle's story of its wake, or an
/// errand's session, for its cast's. Anyone else's command is no part of
/// one.
fn warriv_ran(project: &Path, command: Command, title: &str, text: &str) {
    let Some(wake) = session().filter(|id| warriv::is_warriv(id) || runeword::is_errand(id)) else {
        return;
    };
    record(
        project,
        String::new(),
        title,
        Happened::WarrivRan {
            wake,
            conversation: std::env::var("CLAUDE_CODE_SESSION_ID").unwrap_or_default(),
            command,
            text: text.to_string(),
        },
    );
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

/// The project whose list has the session's item: for a session in a
/// worktree of its own, in the main working tree, or else above `cwd`. The
/// main tree goes first because a worktree can hold a committed copy of the
/// log with the same holder line, and marking that copy leaves the real
/// quest unfinished.
fn held(cwd: &Path, id: &str) -> Option<PathBuf> {
    main_tree()
        .and_then(|main| file::find_held(&main, id))
        .or_else(|| file::find_held(cwd, id))
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
        by: session(),
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
        let check = |w: Wait| Some(On::Check(w));
        assert_eq!(w(&["needs", "a key"]), Ok(("needs a key".into(), None)));
        assert_eq!(
            w(&["needs B", "--on", "Build", "B"]),
            Ok(("needs B".into(), Some(On::Quest("Build B".into()))))
        );
        // The flag from before After: lines still works, and writes one.
        assert_eq!(
            w(&["needs B", "--on-quest", "Build B"]),
            Ok(("needs B".into(), Some(On::Quest("Build B".into()))))
        );
        assert_eq!(
            w(&["--on-file", "out/x"]),
            Ok(("waits".into(), check(Wait::File("out/x".into()))))
        );
        assert_eq!(
            w(&["later", "--until", "+1m"]),
            Ok(("later".into(), check(Wait::Until(1060))))
        );
        assert_eq!(
            w(&["x", "--on-cmd", "git diff --quiet"]),
            Ok(("x".into(), check(Wait::Cmd("git diff --quiet".into()))))
        );
        assert_eq!(
            w(&["x", "--on-main", "abc"]),
            Ok(("x".into(), check(Wait::Main("abc".into()))))
        );
        assert!(w(&[]).is_err());
        assert!(w(&["x", "--on"]).is_err());
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
            Ok(("Fix the login".into(), "Only after\nexpiry".into(), None))
        );
        assert_eq!(
            title_and_notes(&words(&["Fix", "the login"])),
            Ok(("Fix the login".into(), String::new(), None))
        );
        assert_eq!(
            title_and_notes(&words(&["--notes", "why"])),
            Ok((String::new(), "why".into(), None))
        );
    }

    #[test]
    fn each_after_flag_becomes_an_after_line_under_the_notes() {
        assert_eq!(
            title_and_notes(&words(&[
                "Wire it", "--after", "Build", "it", "--notes", "Why.", "--after", "Test it"
            ])),
            Ok((
                "Wire it".into(),
                "Why.\nAfter: Build it\nAfter: Test it".into(),
                None
            ))
        );
        assert_eq!(
            title_and_notes(&words(&["Wire it", "--after", "Build"])),
            Ok(("Wire it".into(), "After: Build".into(), None))
        );
        assert!(title_and_notes(&words(&["Wire it", "--after"])).is_err());
    }

    #[test]
    fn below_names_the_quest_it_goes_under() {
        assert_eq!(
            title_and_notes(&words(&[
                "Half", "--below", "Big", "one", "--after", "Big one"
            ])),
            Ok((
                "Half".into(),
                "After: Big one".into(),
                Some("Big one".into())
            ))
        );
        assert!(title_and_notes(&words(&["Half", "--below"])).is_err());
    }

    #[test]
    fn blocked_with_quest_hands_that_quest_on() {
        assert_eq!(
            quest_flag(&words(&["Which", "card?", "--quest", "Pay", "for it"])),
            Ok((Some("Pay for it".into()), words(&["Which", "card?"])))
        );
        assert_eq!(
            quest_flag(&words(&["needs", "--on", "B"])),
            Ok((None, words(&["needs", "--on", "B"])))
        );
        assert!(quest_flag(&words(&["x", "--quest"])).is_err());
        assert!(quest_flag(&words(&["x", "--quest", "A", "--on", "B"])).is_err());
    }

    #[test]
    fn note_and_tell_take_a_quest_then_the_words() {
        assert_eq!(
            title_and_text(&words(&["Serve the API", "Use", "port 4100."]), "tell"),
            Ok(("Serve the API".into(), "Use port 4100.".into()))
        );
        assert!(title_and_text(&words(&["Serve the API"]), "tell").is_err());
        assert!(title_and_text(&words(&["Serve", " "]), "note").is_err());
        assert!(title_and_text(&[], "note").is_err());
        assert_eq!(
            title_and_text(&words(&["Serve", "No", "tests."]), "fix"),
            Ok(("Serve".into(), "No tests.".into()))
        );
    }
}
