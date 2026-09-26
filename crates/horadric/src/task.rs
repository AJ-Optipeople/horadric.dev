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

use horadric_core::tasks::{self, Mark};
use horadric_core::tombs;
use horadric_hooks::listener::TasksChanged;
use horadric_hooks::{
    client, tasks as file, COMMAND_HEADER, OWNER_ENV, SESSION_ENV, TASKS_ENV, TASKS_PATH,
};

const USAGE: &str = "\
usage: horadric quest done              The quest this session works is completed
       horadric quest blocked \"why\"     It can not go on without the human
       horadric quest add \"title\"       Add a quest to the end of the log
       horadric quest list              Show the log";

/// What the errors call the list, which may still be the old file.
const NO_LOG: &str = "no quest log (.horadric/quests.md or .horadric/tasks.md) above here";

pub fn run(args: &[String]) -> Result<(), String> {
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    let rest = || args[1..].join(" ");
    match args.first().map(String::as_str) {
        Some("done") => report(&cwd, None),
        Some("blocked") => {
            let why = rest();
            if why.trim().is_empty() {
                return Err("say why: horadric quest blocked \"what you need\"".into());
            }
            report(&cwd, Some(&why))
        }
        Some("add") => {
            let title = tasks::one_line(&rest());
            if title.is_empty() {
                return Err("say what: horadric quest add \"title\"".into());
            }
            add(&cwd, &title)
        }
        Some("list") => list(&cwd),
        _ => Err(USAGE.into()),
    }
}

/// The agent's item is done, or blocked with `why`.
fn report(cwd: &Path, why: Option<&str>) -> Result<(), String> {
    let id = session().ok_or("this is not a Horadric session, so there is no item to report on")?;
    if let Some((batch, _)) = tombs::of(&id) {
        return report_tomb(cwd, &id, batch, why);
    }
    let project = held(cwd, &id).ok_or(format!("{NO_LOG} has a quest held by {id}"))?;
    let mode = file::mode(&project);
    let mark = match why {
        Some(_) => Mark::Blocked,
        None => mode.finished(),
    };
    let changed = file::update(&project, |text| tasks::set_held(text, &id, mark, why))
        .map_err(|e| format!("{}: {e}", file::file(&project).display()))?;
    if !changed {
        return Err(format!("the quest held by {id} is already completed"));
    }
    tell_app(&project);
    match (why, mark) {
        (Some(_), _) => println!("Marked blocked. Say what you need, then wait for the human."),
        (None, Mark::Done) => println!("Quest completed. The next one starts once this turn ends."),
        _ => println!("Marked for review. The human looks next; stop here."),
    }
    Ok(())
}

/// A tomb's report leaves the list alone, since the item is the batch's
/// until the human picks. Only the app keeps it, so it has to hear.
fn report_tomb(cwd: &Path, id: &str, batch: &str, why: Option<&str>) -> Result<(), String> {
    let project = held(cwd, batch).ok_or(format!("{NO_LOG} has a quest held by {batch}"))?;
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

fn add(cwd: &Path, title: &str) -> Result<(), String> {
    let project = session()
        .and_then(|id| held(cwd, &id))
        .or_else(main_list)
        .or_else(|| file::find_list(cwd))
        .unwrap_or_else(|| cwd.to_path_buf());
    file::update(&project, |text| Some(tasks::append(text, title)))
        .map_err(|e| format!("{}: {e}", file::file(&project).display()))?;
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
