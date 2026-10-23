//! Town Portal's messages, as the PC side builds them: the session list a
//! phone sees and the text of a terminal it watches. What goes over the
//! wire is in docs/PORTAL.md; the app's side that sends them is
//! `app/portal.rs`.

use alacritty_terminal::event::EventListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::Term;
use alacritty_terminal::vte::ansi::{NamedColor, Rgb};
use horadric_core::diff::FileDiff;
// The frame crypto is Windows CNG, so the Mac build has no portal yet.
#[cfg(windows)]
use horadric_core::portal::{key32, Config};
use horadric_core::tasks::{Mark, Mode, Ready, Task};
use serde_json::{json, Value};

// The messages a headless node builds too live in core.
pub use horadric_core::portal::{
    asked_size, fingerprint, newly_waiting, out_messages, phase_word, projects_message,
    session_entry, session_name, size_message, NAME_MAX, OUT_CHUNK,
};

use crate::board::Board;
use crate::palette;

/// The most scrollback a screen message carries, so a long session does
/// not send megabytes on every change.
pub const HISTORY: usize = 500;

/// One project's quest log as a phone sees it: what Warriv is about, the
/// quests not done in the log's order, and how many are done.
pub struct QuestLog<'a> {
    pub path: &'a str,
    pub name: &'a str,
    pub board: Option<&'a Board>,
    /// Warriv drives the project: it starts and lands quests by itself.
    pub drives: bool,
    /// The words of the quests tile's Warriv line, when it has one.
    pub warriv_line: Option<&'a str>,
}

/// The most notes lines a quest carries to a phone, so one long plan
/// does not swell every quests message.
pub const QUEST_NOTES_MAX: usize = 12;

/// The quests message: every project's log, in the tiles' order.
pub fn quests_message(logs: &[QuestLog], now: u64) -> Value {
    let list: Vec<Value> = logs.iter().map(|l| quest_log(l, now)).collect();
    json!({"t": "quests", "list": list})
}

fn quest_log(l: &QuestLog, now: u64) -> Value {
    let (quests, done, mode, on) = match l.board {
        Some(b) => {
            let ready = horadric_core::tasks::readiness(&b.tasks);
            let quests: Vec<Value> = b
                .tasks
                .iter()
                .zip(&ready)
                .filter(|(t, _)| t.mark != Mark::Done && !t.title.trim().is_empty())
                .map(|(t, r)| quest_entry(t, r, now))
                .collect();
            let done = b.tasks.iter().filter(|t| t.mark == Mark::Done).count();
            (quests, done, b.mode, b.orchestrator)
        }
        None => (Vec::new(), 0, Mode::default(), false),
    };
    json!({
        "project": l.path,
        "name": l.name,
        "mode": mode.name(),
        "warriv": {
            "on": horadric_core::warriv::orchestrates(on, l.drives),
            "drives": l.drives,
            "line": l.warriv_line.unwrap_or_default(),
        },
        "done": done,
        "quests": quests,
    })
}

fn quest_entry(t: &Task, ready: &Ready, now: u64) -> Value {
    let state = match t.mark {
        Mark::Open | Mark::Blocked if ready.tangled() => "tangled",
        Mark::Open if *ready != Ready::Yes => "after",
        Mark::Open => "open",
        Mark::Working => "working",
        Mark::Review => "review",
        Mark::Blocked => "blocked",
        Mark::Done => "done",
    };
    let why = t
        .reason
        .clone()
        .filter(|_| t.mark == Mark::Blocked)
        .or_else(|| crate::board::note(t, ready, now))
        .unwrap_or_default();
    let notes: Vec<&str> = t
        .notes
        .iter()
        .take(QUEST_NOTES_MAX)
        .map(String::as_str)
        .collect();
    json!({
        "title": t.title,
        "state": state,
        "session": t.holder.clone().filter(|_| t.mark.held()),
        "why": why,
        "notes": notes,
    })
}

/// The title of a quest a phone adds, on one line, or why it will not do.
/// The log drops blank notes lines itself.
pub fn new_quest(title: &str, notes: &str) -> Result<String, &'static str> {
    let title = horadric_core::tasks::one_line(title);
    if title.is_empty() {
        return Err("Say what the quest is.");
    }
    if title.chars().count() > 200 {
        return Err("A quest's title must be at most 200 characters.");
    }
    if notes.chars().count() > 8000 {
        return Err("The notes must be at most 8000 characters.");
    }
    Ok(title)
}

/// Whose key opened a phone's first frame on a channel.
#[cfg(windows)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sender {
    /// A paired phone: its id and key.
    Phone(String, [u8; 32]),
    /// A phone pairing with the ticket's key.
    Pairing([u8; 32]),
}

/// Opens a frame from a channel not yet known: with each paired phone's
/// key, then with the ticket's while it lasts. `None` when no key opens
/// it, which is every frame from a phone that was forgotten.
#[cfg(windows)]
pub fn identify(config: &Config, sealed: &[u8], now: u64) -> Option<(Sender, Value)> {
    let read = |key: &[u8; 32]| -> Option<Value> {
        serde_json::from_slice(&crate::portal_crypto::open(key, sealed)?).ok()
    };
    for phone in &config.phones {
        if let Some(key) = key32(&phone.key) {
            if let Some(msg) = read(&key) {
                return Some((Sender::Phone(phone.id.clone(), key), msg));
            }
        }
    }
    let key = config.ticket_key(now)?;
    Some((Sender::Pairing(key), read(&key)?))
}

/// A run of characters drawn alike: how many, foreground and background
/// as 0xRRGGBB (`None` for the terminal's own, so the page uses its own),
/// and [`BOLD`], [`ITALIC`], [`UNDERLINE`] and [`STRIKE`].
pub type Run = (usize, Option<u32>, Option<u32>, u8);

pub const BOLD: u8 = 1;
pub const ITALIC: u8 = 2;
pub const UNDERLINE: u8 = 4;
pub const STRIKE: u8 = 8;

/// What a phone is sent of a terminal.
pub struct Screen {
    pub lines: Vec<String>,
    /// One per line. Characters past a line's runs are plain, so a plain
    /// line has none.
    pub styles: Vec<Vec<Run>>,
    pub cursor: (usize, usize),
}

/// A terminal's text: up to [`HISTORY`] lines of scrollback, then the
/// screen, each without trailing spaces, their colours, and the cursor as
/// (row, col) counted from the first line given.
pub fn screen_text<T: EventListener>(term: &Term<T>) -> Screen {
    let grid = term.grid();
    let colors = term.colors();
    let plain_fg = palette::rgb_at(NamedColor::Foreground as usize, colors);
    let plain_bg = palette::rgb_at(NamedColor::Background as usize, colors);
    let hex = |c: Rgb, plain: Rgb| (c != plain).then_some(u32::from_be_bytes([0, c.r, c.g, c.b]));
    let history = grid.history_size().min(HISTORY);
    let rows = term.screen_lines();
    let cols = term.columns();
    let mut lines = Vec::with_capacity(history + rows);
    let mut styles = Vec::with_capacity(history + rows);
    for line in -(history as i32)..rows as i32 {
        let row = &grid[Line(line)];
        let mut text = String::with_capacity(cols);
        let mut runs: Vec<Run> = Vec::new();
        for col in 0..cols {
            let cell = &row[Column(col)];
            if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                continue;
            }
            let before = text.chars().count();
            text.push(cell.c);
            if let Some(zero) = cell.zerowidth() {
                text.extend(zero);
            }
            let n = text.chars().count() - before;
            let (fg, bg) = palette::cell_colors(cell.fg, cell.bg, cell.flags, colors);
            let mut f = 0;
            if cell.flags.contains(Flags::BOLD) {
                f |= BOLD;
            }
            if cell.flags.contains(Flags::ITALIC) {
                f |= ITALIC;
            }
            if cell.flags.intersects(Flags::ALL_UNDERLINES) {
                f |= UNDERLINE;
            }
            if cell.flags.contains(Flags::STRIKEOUT) {
                f |= STRIKE;
            }
            let style = (hex(fg, plain_fg), hex(bg, plain_bg), f);
            match runs.last_mut() {
                Some(last) if (last.1, last.2, last.3) == style => last.0 += n,
                _ => runs.push((n, style.0, style.1, style.2)),
            }
        }
        let text = text.trim_end().to_string();
        styles.push(trim_runs(runs, text.chars().count()));
        lines.push(text);
    }
    let cursor = grid.cursor.point;
    let row = (cursor.line.0 + history as i32).max(0) as usize;
    Screen {
        lines,
        styles,
        cursor: (row, cursor.column.0),
    }
}

/// Runs cut to a trimmed line's length, with the plain tail dropped.
fn trim_runs(mut runs: Vec<Run>, len: usize) -> Vec<Run> {
    let mut left = len;
    runs.retain_mut(|r| {
        let keep = left > 0;
        r.0 = r.0.min(left);
        left -= r.0;
        keep
    });
    while runs
        .last()
        .is_some_and(|r| r.1.is_none() && r.2.is_none() && r.3 == 0)
    {
        runs.pop();
    }
    runs
}

/// The screen message for a session.
pub fn screen_message(id: &str, cols: usize, rows: usize, screen: &Screen) -> Value {
    let styles: Vec<Value> = screen
        .styles
        .iter()
        .map(|runs| runs.iter().map(|r| json!([r.0, r.1, r.2, r.3])).collect())
        .collect();
    json!({
        "t": "screen",
        "id": id,
        "cols": cols,
        "rows": rows,
        "lines": screen.lines,
        "styles": styles,
        "cursor": [screen.cursor.0, screen.cursor.1],
    })
}

/// The folders the human's projects live in: the parent of each project
/// folder, once each (Windows paths compare without case and either
/// slash), in first seen order. A drive's root is left out, and so is
/// anything in `skip`, such as the temp folder scratch projects sit in.
pub fn roots(projects: &[String], skip: &[String]) -> Vec<String> {
    let norm = |p: &str| p.replace('\\', "/").trim_end_matches('/').to_lowercase();
    let skip: Vec<String> = skip.iter().map(|s| norm(s)).collect();
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for project in projects {
        let trimmed = project.trim_end_matches(['\\', '/']);
        let Some(cut) = trimmed.rfind(['\\', '/']) else {
            continue;
        };
        let parent = &trimmed[..cut];
        // Listing a whole drive is not a folder of projects.
        if parent.is_empty() || parent.ends_with(':') {
            continue;
        }
        let key = norm(parent);
        if skip
            .iter()
            .any(|s| key == *s || key.starts_with(&format!("{s}/")))
        {
            continue;
        }
        if seen.insert(key) {
            out.push(parent.to_string());
        }
    }
    out
}

/// A folder a phone may start a session in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Folder {
    pub path: String,
    pub name: String,
    pub git: bool,
    /// Last changed, in seconds since the epoch, for the order.
    pub changed: u64,
}

/// At most this many folders a root.
pub const FOLDERS_MAX: usize = 300;

/// Puts a root's folders newest first and cuts them to [`FOLDERS_MAX`].
pub fn order_folders(mut list: Vec<Folder>) -> Vec<Folder> {
    list.sort_by(|a, b| b.changed.cmp(&a.changed).then_with(|| a.name.cmp(&b.name)));
    list.truncate(FOLDERS_MAX);
    list
}

/// The folders message, one entry a root.
pub fn folders_message(roots: &[(String, Vec<Folder>)]) -> Value {
    let roots: Vec<Value> = roots
        .iter()
        .map(|(path, list)| {
            let list: Vec<Value> = list
                .iter()
                .map(|f| json!({"path": f.path, "name": f.name, "git": f.git}))
                .collect();
            let name = crate::project::project_name(&path.replace('\\', "/"));
            json!({"path": path, "name": name, "list": list})
        })
        .collect();
    json!({"t": "folders", "roots": roots})
}

/// Why a new project's folder name will not do, or `None` when it will:
/// 1 to 64 letters, digits, spaces, dots, dashes and underscores, not
/// starting with a dot, and not a name Windows keeps for a device.
pub fn bad_project_name(name: &str) -> Option<&'static str> {
    if name.is_empty() || name.chars().count() > 64 {
        return Some("The name must be 1 to 64 characters.");
    }
    if name.starts_with('.') {
        return Some("The name cannot start with a dot.");
    }
    if !name
        .chars()
        .all(|c| c.is_alphanumeric() || matches!(c, ' ' | '.' | '-' | '_'))
    {
        return Some("Use letters, digits, spaces, dots, dashes and underscores.");
    }
    // Windows drops a trailing dot or space, so the folder would not be the name.
    if name.ends_with(['.', ' ']) || name.starts_with(' ') {
        return Some("The name cannot start with a space or end with a dot or a space.");
    }
    let stem = name.split('.').next().unwrap_or(name).to_ascii_uppercase();
    let device = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ((stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.len() == 4
            && stem.as_bytes()[3].is_ascii_digit());
    if device {
        return Some("Windows keeps that name for a device.");
    }
    None
}

/// The most of one file's diff a phone is sent. A generated file or a
/// lock file can be megabytes, and a frame is at most 1 MB.
pub const DIFF_MAX: usize = 200 * 1024;

/// The changes message: each file the session changed, with the lines it
/// added and removed, `null` for both in a binary file.
pub fn changes_message(id: &str, files: &[FileDiff]) -> Value {
    let list: Vec<Value> = files
        .iter()
        .map(|f| {
            let (added, removed) = f.lines.unzip();
            json!({"path": f.path, "added": added, "removed": removed})
        })
        .collect();
    json!({"t": "changes", "id": id, "files": list})
}

/// The diff message for one file: its diff, cut at [`DIFF_MAX`] at the end
/// of a line, and whether it was cut.
pub fn diff_message(id: &str, path: &str, text: &str) -> Value {
    let (text, cut) = cut_diff(text, DIFF_MAX);
    json!({"t": "diff", "id": id, "path": path, "text": text, "cut": cut})
}

/// `text` whole when it fits in `max` bytes, else up to the last line that
/// does, so the phone never draws half a line or half a character.
pub fn cut_diff(text: &str, max: usize) -> (&str, bool) {
    if text.len() <= max {
        return (text, false);
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let end = text[..end].rfind('\n').map_or(end, |i| i + 1);
    (&text[..end], true)
}

/// Whether a phone may see `path`'s diff: only a file the session changed,
/// so a path the phone makes up reads nothing else on the PC.
pub fn listed(files: &[FileDiff], path: &str) -> bool {
    files.iter().any(|f| f.path == path)
}

/// How long a photo sent from the phone is kept: long enough for the
/// agent to read it, and the folder does not grow for ever.
pub const ATTACH_KEEP_SECS: u64 = 7 * 24 * 3600;

/// The folder a session's attachments go in, under `portal-attach`, or
/// `None` for an id that could climb out of it.
pub fn attach_folder(id: &str) -> Option<&str> {
    let ok = !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'));
    ok.then_some(id)
}

/// The file name a phone's attachment is saved as: the time first, so two
/// photos called `image.jpg` do not overwrite each other, then the name
/// the phone gave cut to its last part and to characters any folder takes.
pub fn attach_file_name(name: &str, now: u64) -> String {
    let last = name.rsplit(['/', '\\']).next().unwrap_or_default();
    let clean: String = last
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .take(60)
        .collect();
    // Windows drops a trailing dot, and a leading one hides the file.
    let clean = clean.trim_matches('.');
    let clean = if clean.is_empty() { "photo.jpg" } else { clean };
    format!("{now}-{clean}")
}

/// Whether an attachment last written at `modified` is old enough to go.
pub fn attach_stale(modified: u64, now: u64) -> bool {
    now.saturating_sub(modified) >= ATTACH_KEEP_SECS
}

#[cfg(test)]
mod tests {
    use super::*;
    use alacritty_terminal::event::VoidListener;
    use alacritty_terminal::term::Config;
    use alacritty_terminal::vte::ansi::Processor;

    struct Size(usize, usize);

    impl Dimensions for Size {
        fn total_lines(&self) -> usize {
            self.1
        }
        fn screen_lines(&self) -> usize {
            self.1
        }
        fn columns(&self) -> usize {
            self.0
        }
    }

    fn term(cols: usize, rows: usize, bytes: &[u8]) -> Term<VoidListener> {
        let mut t = Term::new(Config::default(), &Size(cols, rows), VoidListener);
        let mut p: Processor = Processor::new();
        p.advance(&mut t, bytes);
        t
    }

    #[test]
    fn the_screen_is_its_lines_without_trailing_spaces() {
        let t = term(10, 3, b"hi  \r\nthere");
        let s = screen_text(&t);
        assert_eq!(s.lines, vec!["hi", "there", ""]);
        assert_eq!(s.cursor, (1, 5));
    }

    #[test]
    fn scrollback_comes_first_and_the_cursor_counts_it() {
        let t = term(10, 2, b"a\r\nb\r\nc\r\nd");
        let s = screen_text(&t);
        assert_eq!(s.lines, vec!["a", "b", "c", "d"]);
        assert_eq!(s.cursor, (3, 1));
    }

    #[test]
    fn wide_characters_are_one_character() {
        let t = term(10, 1, "日本".as_bytes());
        assert_eq!(screen_text(&t).lines, vec!["日本"]);
    }

    #[test]
    fn plain_text_has_no_runs() {
        let t = term(10, 2, b"hi");
        assert_eq!(screen_text(&t).styles, vec![vec![], vec![]]);
    }

    #[test]
    fn colours_and_bold_are_runs_and_the_plain_tail_is_dropped() {
        let t = term(20, 1, b"ab[1;38;2;255;0;0mred[0m end  ");
        let s = screen_text(&t);
        assert_eq!(s.lines, vec!["abred end"]);
        assert_eq!(
            s.styles[0],
            vec![(2, None, None, 0), (3, Some(0xFF0000), None, BOLD)]
        );
    }

    #[test]
    fn a_coloured_background_on_trailing_spaces_is_cut_with_the_line() {
        let t = term(10, 1, b"x[44m    ");
        let s = screen_text(&t);
        assert_eq!(s.lines, vec!["x"]);
        assert_eq!(s.styles[0], vec![]);
    }

    #[test]
    fn the_message_carries_a_style_list_per_line() {
        let t = term(10, 1, b"[3mi");
        let m = screen_message("a1", 10, 1, &screen_text(&t));
        assert_eq!(m["styles"], json!([[[1, null, null, ITALIC]]]));
        assert_eq!(m["lines"], json!(["i"]));
    }

    fn log_of(text: &str) -> Board {
        Board {
            tasks: horadric_core::tasks::parse(text),
            mode: Mode::Warriv,
            ..Board::default()
        }
    }

    #[test]
    fn a_quest_log_lists_what_is_not_done_with_its_state() {
        let b = log_of(
            "- [x] Old\n\
             - [/] Build it @s1\n  Do it well\n\
             - [ ] Then this\n  After: Build it\n\
             - [!] Stuck @s2: which colour?\n\
             - [ ] Free\n",
        );
        let m = quests_message(
            &[QuestLog {
                path: "C:/p",
                name: "p",
                board: Some(&b),
                drives: true,
                warriv_line: Some("reads Build it"),
            }],
            0,
        );
        assert_eq!(m["t"], "quests");
        let log = &m["list"][0];
        assert_eq!(log["project"], "C:/p");
        assert_eq!(log["mode"], "warriv");
        assert_eq!(log["done"], 1);
        assert_eq!(log["warriv"]["on"], true);
        assert_eq!(log["warriv"]["drives"], true);
        assert_eq!(log["warriv"]["line"], "reads Build it");
        let q = log["quests"].as_array().unwrap();
        let states: Vec<&str> = q.iter().map(|q| q["state"].as_str().unwrap()).collect();
        assert_eq!(states, ["working", "after", "blocked", "open"]);
        assert_eq!(q[0]["session"], "s1");
        assert_eq!(q[0]["notes"][0], "Do it well");
        assert_eq!(q[1]["why"], "after Build it");
        assert_eq!(q[2]["why"], "which colour?");
        assert_eq!(q[3]["session"], Value::Null);
    }

    #[test]
    fn a_project_without_a_log_has_no_quests_and_no_warriv() {
        let m = quests_message(
            &[QuestLog {
                path: "C:/p",
                name: "p",
                board: None,
                drives: false,
                warriv_line: None,
            }],
            0,
        );
        assert_eq!(m["list"][0]["quests"].as_array().unwrap().len(), 0);
        assert_eq!(m["list"][0]["warriv"]["on"], false);
        assert_eq!(m["list"][0]["mode"], "manual");
    }

    #[test]
    fn long_notes_are_cut() {
        let notes: String = (0..30).map(|i| format!("  line {i}\n")).collect();
        let b = log_of(&format!("- [ ] Long\n{notes}"));
        let log = QuestLog {
            path: "",
            name: "",
            board: Some(&b),
            drives: false,
            warriv_line: None,
        };
        let m = quests_message(&[log], 0);
        let n = m["list"][0]["quests"][0]["notes"].as_array().unwrap().len();
        assert_eq!(n, QUEST_NOTES_MAX);
    }

    #[test]
    fn a_new_quest_needs_a_title_on_one_line() {
        assert_eq!(new_quest("  Fix\n the  bug ", ""), Ok("Fix the bug".into()));
        assert!(new_quest("  ", "notes").is_err());
        assert!(new_quest(&"x".repeat(201), "").is_err());
        assert!(new_quest("ok", &"x".repeat(8001)).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn a_frame_is_known_by_the_key_that_opens_it() {
        let mut c = horadric_core::portal::Config::fresh(&[0; 32]);
        c.open_ticket(&[9; 32], 0);
        c.pair("p", &[1; 40], 1);
        let phone = c.phones[0].id.clone();
        c.open_ticket(&[8; 32], 100);
        let hi = br#"{"t":"hello"}"#;
        let sealed = |k: u8| crate::portal_crypto::seal(&[k; 32], hi);
        assert_eq!(
            identify(&c, &sealed(1), 100),
            Some((Sender::Phone(phone, [1; 32]), json!({"t":"hello"})))
        );
        assert_eq!(
            identify(&c, &sealed(8), 100),
            Some((Sender::Pairing([8; 32]), json!({"t":"hello"})))
        );
        // The ticket's key opens nothing once it ran out, nor does the
        // old shared key or any other.
        assert_eq!(identify(&c, &sealed(8), 400), None);
        assert_eq!(identify(&c, &sealed(9), 100), None);
        assert_eq!(identify(&c, b"short", 100), None);
    }

    #[test]
    fn the_same_value_has_the_same_fingerprint() {
        let a = json!({"t": "sessions", "list": [1, 2]});
        assert_eq!(fingerprint(&a), fingerprint(&a.clone()));
        assert_ne!(fingerprint(&a), fingerprint(&json!({"t": "sessions"})));
    }

    #[test]
    fn roots_are_parents_once_each() {
        let projects = vec![
            r"C:\Users\me\Github\app".to_string(),
            "c:/users/me/github/other/".to_string(),
            r"C:\Users\me\Work\thing".to_string(),
            r"C:\top".to_string(),
            r"C:\Users\me\AppData\Local\Temp\x\scratch".to_string(),
            "nofolder".to_string(),
        ];
        let skip = vec![r"C:\Users\me\AppData\Local\Temp".to_string()];
        assert_eq!(
            roots(&projects, &skip),
            vec![
                r"C:\Users\me\Github".to_string(),
                r"C:\Users\me\Work".to_string()
            ]
        );
    }

    #[test]
    fn folders_go_newest_first_and_are_capped() {
        let f = |name: &str, changed| Folder {
            path: format!("C:/r/{name}"),
            name: name.into(),
            git: false,
            changed,
        };
        let list = order_folders(vec![f("old", 1), f("new", 9), f("mid", 5)]);
        let names: Vec<&str> = list.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["new", "mid", "old"]);
        let many = (0..400).map(|i| f(&i.to_string(), i)).collect();
        assert_eq!(order_folders(many).len(), FOLDERS_MAX);
    }

    #[test]
    fn folders_message_has_roots_and_folders() {
        let app = Folder {
            path: "C:/x/app".into(),
            name: "app".into(),
            git: true,
            changed: 0,
        };
        let m = folders_message(&[(r"C:\Users\me\Github".into(), vec![app])]);
        assert_eq!(m["t"], "folders");
        assert_eq!(m["roots"][0]["name"], "Github");
        assert_eq!(m["roots"][0]["list"][0]["git"], true);
    }

    #[test]
    fn project_names_are_checked() {
        assert_eq!(bad_project_name("my-app_2.0 beta"), None);
        assert_eq!(bad_project_name("console"), None);
        for bad in [
            "", ".hidden", "a/b", r"a\b", "..", "x:", "trail.", " lead", "con", "COM1.txt",
        ] {
            assert!(bad_project_name(bad).is_some(), "{bad}");
        }
        assert!(bad_project_name(&"a".repeat(65)).is_some());
    }

    fn changed(path: &str, lines: Option<(u32, u32)>) -> FileDiff {
        FileDiff {
            path: path.into(),
            lines,
        }
    }

    #[test]
    fn changes_message_counts_lines_and_leaves_binaries_blank() {
        let m = changes_message(
            "s1",
            &[
                changed("src/a.rs", Some((12, 3))),
                changed("logo.png", None),
            ],
        );
        assert_eq!(m["t"], "changes");
        assert_eq!(m["id"], "s1");
        assert_eq!(m["files"][0]["path"], "src/a.rs");
        assert_eq!(m["files"][0]["added"], 12);
        assert_eq!(m["files"][0]["removed"], 3);
        assert!(m["files"][1]["added"].is_null());
        assert!(m["files"][1]["removed"].is_null());
    }

    #[test]
    fn a_long_diff_is_cut_at_the_end_of_a_line() {
        assert_eq!(cut_diff("+a\n+b\n", 100), ("+a\n+b\n", false));
        assert_eq!(cut_diff("+a\n+bcd\n", 5), ("+a\n", true));
        // No line ends in reach: cut at a character, never inside one.
        assert_eq!(cut_diff("+\u{e6}\u{e6}", 2), ("+", true));
        let m = diff_message("s1", "a", &"+x\n".repeat(DIFF_MAX));
        assert_eq!(m["cut"], true);
        assert!(m["text"].as_str().unwrap().len() <= DIFF_MAX);
        assert_eq!(m["path"], "a");
    }

    #[test]
    fn only_a_changed_file_may_be_diffed() {
        let files = [changed("src/a.rs", Some((1, 0)))];
        assert!(listed(&files, "src/a.rs"));
        assert!(!listed(&files, "../secret.txt"));
        assert!(!listed(&files, "src/b.rs"));
    }

    #[test]
    fn attach_folders_stay_inside() {
        assert_eq!(attach_folder("3f2a-b_9"), Some("3f2a-b_9"));
        for bad in ["", "..", "a/b", r"a\b", "a.b", "C:"] {
            assert_eq!(attach_folder(bad), None, "{bad}");
        }
        assert_eq!(attach_folder(&"a".repeat(65)), None);
    }

    #[test]
    fn attach_file_names_are_safe() {
        assert_eq!(attach_file_name("IMG_1234.jpg", 7), "7-IMG_1234.jpg");
        assert_eq!(attach_file_name(r"..\..\evil.exe", 7), "7-evil.exe");
        assert_eq!(attach_file_name("../a b:c?.png", 7), "7-a_b_c_.png");
        assert_eq!(attach_file_name("", 7), "7-photo.jpg");
        assert_eq!(attach_file_name("...", 7), "7-photo.jpg");
        assert_eq!(attach_file_name("con.", 7), "7-con");
        assert_eq!(attach_file_name(&"x".repeat(200), 7).len(), 62);
    }

    #[test]
    fn attachments_go_after_a_week() {
        assert!(!attach_stale(100, 100 + ATTACH_KEEP_SECS - 1));
        assert!(attach_stale(100, 100 + ATTACH_KEEP_SECS));
        assert!(!attach_stale(200, 100));
    }
}
