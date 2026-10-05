//! A project's quest log: `.horadric/quests.md`, a Markdown checklist that
//! agents take items from, one at a time or all the way down by themselves.
//! A list from before the rename, `.horadric/tasks.md`, is still read and
//! written until a quest log sits beside it.
//!
//! The file belongs to the human and the agents. Horadric only ever changes
//! one line at a time, the marker and the session that holds the item, and
//! leaves every other byte as it found it, line endings included. A line it
//! cannot read is left alone.
//!
//! ```text
//! - [x] Rename Glance to Horadric
//! - [/] Fix the login redirect @fix-login-redirect-51234
//!   Happens only after a session expires.
//! - [?] Add dark mode to the settings page @add-dark-mode-51300
//! - [!] Migrate to the new API @migrate-api-51400: needs a key I do not have
//! - [!] Wire the new API in @wire-api-51500: needs it
//!   After: Migrate to the new API
//! - [ ] Show the build time in the footer
//!   After: Add dark mode
//! ```
//!
//! A notes line `After: <title>` names a quest this one waits for, see
//! [`readiness`]. The runner passes a quest whose `After:` quests are not
//! all done, and starts it, or tells its session to go on, once they are.
//! A blocked item that says in braces what it waits on is the runner's to
//! wake too, see [`Wait`]. One that says only why waits on the human.

use serde_json::{Map, Value};

use crate::tombs;
use crate::usage::format_until;

/// Where the list lives, from the project folder.
pub const QUESTS_FILE: &str = ".horadric/quests.md";

/// Where the list lived before it was a quest log.
pub const OLD_FILE: &str = ".horadric/tasks.md";

/// Which file holds a project's list, given which of the two exist. The
/// old one only while it is the only one, so a list that is running keeps
/// its file, and a new list is a quest log.
pub fn list_file(quests: bool, old: bool) -> &'static str {
    if old && !quests {
        OLD_FILE
    } else {
        QUESTS_FILE
    }
}

/// Where the mode lives, from the project folder.
pub const CONFIG_FILE: &str = ".horadric/config.json";

/// What state an item is in, from the character between its brackets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    /// `[ ]`: nobody has it.
    Open,
    /// `[/]`: a session is on it.
    Working,
    /// `[?]`: its agent says it is done, and the human has not looked.
    Review,
    /// `[!]`: its agent can not go on without the human.
    Blocked,
    /// `[x]`: finished.
    Done,
}

impl Mark {
    fn from_char(c: char) -> Option<Mark> {
        match c {
            ' ' => Some(Mark::Open),
            '/' => Some(Mark::Working),
            '?' => Some(Mark::Review),
            '!' => Some(Mark::Blocked),
            'x' | 'X' => Some(Mark::Done),
            _ => None,
        }
    }

    pub fn char(self) -> char {
        match self {
            Mark::Open => ' ',
            Mark::Working => '/',
            Mark::Review => '?',
            Mark::Blocked => '!',
            Mark::Done => 'x',
        }
    }

    /// A session holds an item it is on, or has finished and waits on.
    pub fn held(self) -> bool {
        matches!(self, Mark::Working | Mark::Review | Mark::Blocked)
    }
}

/// One item of the list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Task {
    /// Which line of the file it is on, counting from zero.
    pub line: usize,
    pub mark: Mark,
    pub title: String,
    /// The Horadric id of the session that holds it.
    pub holder: Option<String>,
    /// Why it is blocked, as its agent said.
    pub reason: Option<String>,
    /// What a blocked item waits on, in a form the runner can check.
    pub wait: Option<Wait>,
    /// The indented lines under it, indent taken off.
    pub notes: Vec<String>,
}

impl Task {
    /// The quests this one waits for, as they are named: its `After:`
    /// notes lines, and a blocked line's `{on quest: ...}` from before
    /// those lines were the way to say it.
    pub fn after(&self) -> Vec<&str> {
        let mut out: Vec<&str> = self.notes.iter().filter_map(|n| after_line(n)).collect();
        if let Some(Wait::Quest(t)) = &self.wait {
            out.push(t);
        }
        out
    }

    /// The model its `Model:` notes line names. A human's line wins over
    /// Warriv's, since Warriv never changes a human's choice; of Warriv's
    /// own, the last is the one it settled on.
    pub fn model(&self) -> Option<&'static str> {
        let human = self.notes.iter().find_map(|n| model_line(n));
        let warriv = self
            .notes
            .iter()
            .rev()
            .find_map(|n| model_line(n.strip_prefix(crate::warriv::NOTE)?));
        human.or(warriv)
    }

    /// What its session is started with for its `Model:` line, Claude
    /// Code's way. Other agents name their models otherwise, so they get
    /// none.
    pub fn model_args(&self, agent: crate::Agent) -> Vec<String> {
        match (agent, self.model()) {
            (crate::Agent::Claude, Some(m)) => vec!["--model".into(), m.into()],
            _ => Vec::new(),
        }
    }

    /// What it waited on, for its session told to go on and the toast
    /// that says so. Empty for an item that waits on nothing.
    pub fn over(&self) -> String {
        match &self.wait {
            Some(Wait::After) => match self.after().as_slice() {
                [one] => format!("The quest \"{one}\" is done"),
                many => format!(
                    "The quests {} are done",
                    many.iter()
                        .map(|t| format!("\"{t}\""))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            },
            Some(w) => w.over(),
            None => String::new(),
        }
    }
}

/// The title an `After: <title>` notes line names. The word is read in any
/// case, since a human writes these too.
pub fn after_line(note: &str) -> Option<&str> {
    let head = note.get(..6)?;
    let title = note[6..].trim();
    (head.eq_ignore_ascii_case("after:") && !title.is_empty()).then_some(title)
}

/// The models a quest's `Model:` line may name, as Claude Code's
/// `--model` takes them, from the cheapest up.
pub const MODELS: [&str; 3] = ["haiku", "sonnet", "opus"];

/// The model a `Model: <name>` notes line names, read in any case. A name
/// outside [`MODELS`] is not one, so a typo starts the default model
/// rather than a session that fails at once.
pub fn model_line(note: &str) -> Option<&'static str> {
    let head = note.get(..6)?;
    let name = note[6..].trim();
    if !head.eq_ignore_ascii_case("model:") {
        return None;
    }
    MODELS.into_iter().find(|m| m.eq_ignore_ascii_case(name))
}

/// The notes line that says a quest waits for `title`.
pub fn after_note(title: &str) -> String {
    format!("After: {}", one_line(title))
}

/// Whether a quest may start, from the quests it names in `After:` lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ready {
    /// It names none, or every one it names is done.
    Yes,
    /// The first quest it names that is not done, by title.
    After(String),
    /// A name that matches no quest, as written.
    Unknown(String),
    /// A name that matches several quests, as written.
    Several(String),
    /// It waits, by one name or a chain of them, on itself. Only a human
    /// can break that.
    Cycle,
}

impl Ready {
    /// What the quest's row says, in a few words, since the title needs
    /// the room. Nothing for a ready quest.
    pub fn label(&self) -> Option<String> {
        match self {
            Ready::Yes => None,
            Ready::After(t) => Some(format!("after {}", short(t))),
            Ready::Unknown(n) => Some(format!("no quest {}", short(n))),
            Ready::Several(n) => Some(format!("which {}?", short(n))),
            Ready::Cycle => Some("waits on itself".into()),
        }
    }

    /// What is wrong with a tangled quest's `After:` lines, in a sentence
    /// for the human. Empty for any other.
    pub fn why(&self) -> String {
        match self {
            Ready::Unknown(n) => format!("No quest is called \"{n}\"."),
            Ready::Several(n) => format!("\"{n}\" matches more than one quest."),
            Ready::Cycle => "Its After: lines come back round to itself.".into(),
            Ready::Yes | Ready::After(_) => String::new(),
        }
    }

    /// A name that matches nothing or several, or a cycle: no quest
    /// finishing will make it ready, so it needs the human.
    pub fn tangled(&self) -> bool {
        matches!(self, Ready::Unknown(_) | Ready::Several(_) | Ready::Cycle)
    }
}

/// Which quest `name` means: the one whose title it is, or failing that
/// the one whose title starts with it, since titles are long. Case and
/// runs of spaces do not count. Err with how many matched when not one.
pub fn find(tasks: &[Task], name: &str) -> Result<usize, usize> {
    let want = one_line(name).to_lowercase();
    let titles: Vec<String> = tasks
        .iter()
        .map(|t| one_line(&t.title).to_lowercase())
        .collect();
    let exact: Vec<usize> = (0..tasks.len()).filter(|&i| titles[i] == want).collect();
    let found: Vec<usize> = if exact.is_empty() && !want.is_empty() {
        (0..tasks.len())
            .filter(|&i| titles[i].starts_with(&want))
            .collect()
    } else {
        exact
    };
    match found.as_slice() {
        [one] => Ok(*one),
        many => Err(many.len()),
    }
}

/// Whether each quest of the list may start, by index. A quest in a cycle
/// is `Cycle` before anything else, since nothing else will free it.
pub fn readiness(tasks: &[Task]) -> Vec<Ready> {
    let names: Vec<Vec<&str>> = tasks.iter().map(Task::after).collect();
    let edges: Vec<Vec<usize>> = names
        .iter()
        .map(|n| n.iter().filter_map(|n| find(tasks, n).ok()).collect())
        .collect();
    (0..tasks.len())
        .map(|i| {
            if reaches(&edges, i, i) {
                return Ready::Cycle;
            }
            for name in &names[i] {
                match find(tasks, name) {
                    Err(0) => return Ready::Unknown(name.to_string()),
                    Err(_) => return Ready::Several(name.to_string()),
                    Ok(j) if tasks[j].mark != Mark::Done => {
                        return Ready::After(tasks[j].title.clone())
                    }
                    Ok(_) => {}
                }
            }
            Ready::Yes
        })
        .collect()
}

/// Whether following `edges` from `from` comes to `to`, in one step or more.
fn reaches(edges: &[Vec<usize>], from: usize, to: usize) -> bool {
    let mut seen = vec![false; edges.len()];
    let mut stack = edges[from].clone();
    while let Some(i) = stack.pop() {
        if i == to {
            return true;
        }
        if !std::mem::replace(&mut seen[i], true) {
            stack.extend(&edges[i]);
        }
    }
    false
}

/// A name cut to a few words for the right end of a row.
fn short(s: &str) -> String {
    let s = one_line(s);
    if s.chars().count() > 18 {
        format!("{}\u{2026}", s.chars().take(17).collect::<String>())
    } else {
        s
    }
}

/// Every item in the file, top to bottom. Only lines that start a list item
/// at the left edge count; indented lines under one are its notes, and
/// everything else (headings, prose, nested lists) is left out.
pub fn parse(text: &str) -> Vec<Task> {
    let mut tasks: Vec<Task> = Vec::new();
    let mut in_item = false;
    for (i, raw) in text.lines().enumerate() {
        if let Some(t) = parse_item(raw, i) {
            tasks.push(t);
            in_item = true;
            continue;
        }
        let indented = raw.starts_with([' ', '\t']);
        if raw.trim().is_empty() {
            continue;
        }
        match tasks.last_mut() {
            Some(t) if in_item && indented => t.notes.push(raw.trim().to_string()),
            _ => in_item = false,
        }
    }
    tasks
}

/// Reads one line as an item, or None.
fn parse_item(raw: &str, line: usize) -> Option<Task> {
    let rest = raw
        .strip_prefix("- [")
        .or_else(|| raw.strip_prefix("* ["))?;
    let mut chars = rest.chars();
    let mark = Mark::from_char(chars.next()?)?;
    let rest = chars.as_str().strip_prefix(']')?;
    // `- [ ]` alone is an item still to be named.
    let body = match rest.strip_prefix(' ') {
        Some(b) => b,
        None if rest.trim().is_empty() => "",
        None => return None,
    };
    let (body, wait) = split_wait(body.trim_end());
    let (title, holder, reason) = split_holder(body);
    Some(Task {
        line,
        mark,
        title,
        holder,
        reason,
        wait,
        notes: Vec::new(),
    })
}

/// What a blocked item waits on before it can go on by itself. Written at
/// the end of its line in braces, `{on file: out/x.txt}`, so the file
/// stays the state and a human can write one too.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Wait {
    /// The quests its `After:` lines name are done. Written `{after}`, so a
    /// quest blocked until then is told apart from one blocked on the
    /// human that happens to have `After:` lines from before it started.
    After,
    /// Another quest in the same log, by title, is done. Read from lists
    /// written before `After:` lines, never written any more.
    Quest(String),
    /// A commit or branch is in the main tree's checked out branch.
    Main(String),
    /// A file exists, from the project folder.
    File(String),
    /// A command `cmd.exe` runs in the project folder exits 0.
    Cmd(String),
    /// A time has come, in Unix seconds.
    Until(u64),
}

impl Wait {
    /// How it is written on the item's line.
    pub fn spell(&self) -> String {
        match self {
            Wait::After => "{after}".into(),
            Wait::Quest(t) => format!("{{on quest: {}}}", one_line(t)),
            Wait::Main(r) => format!("{{on main: {}}}", one_line(r)),
            Wait::File(f) => format!("{{on file: {}}}", one_line(f)),
            Wait::Cmd(c) => format!("{{on cmd: {}}}", one_line(c)),
            Wait::Until(t) => format!("{{until: {}}}", utc(*t)),
        }
    }

    /// Reads what `spell` wrote, braces included.
    pub fn read(s: &str) -> Option<Wait> {
        let inner = s.strip_prefix('{')?.strip_suffix('}')?;
        if inner.trim() == "after" {
            return Some(Wait::After);
        }
        let (kind, value) = inner.split_once(':')?;
        let value = value.trim();
        if value.is_empty() {
            return None;
        }
        let v = value.to_string();
        match kind.trim() {
            "on quest" => Some(Wait::Quest(v)),
            "on main" => Some(Wait::Main(v)),
            "on file" => Some(Wait::File(v)),
            "on cmd" => Some(Wait::Cmd(v)),
            "until" => parse_utc(value).map(Wait::Until),
            _ => None,
        }
    }

    /// Whether it is over, for the kinds the clock answers. None for those
    /// that need a look at the disk or a command run. A wait on quests is
    /// over here, since [`readiness`] is what holds those back.
    pub fn met(&self, now: u64) -> Option<bool> {
        match self {
            Wait::After | Wait::Quest(_) => Some(true),
            Wait::Until(at) => Some(now >= *at),
            Wait::Main(_) | Wait::File(_) | Wait::Cmd(_) => None,
        }
    }

    /// What the row says it waits on: a few words, since the title needs
    /// the room.
    pub fn label(&self, now: u64) -> String {
        match self {
            Wait::After => "after".into(),
            Wait::Quest(t) => format!("after {}", short(t)),
            Wait::Main(r) => format!("on main {}", short(r)),
            Wait::File(f) => format!("for {}", short(f)),
            Wait::Cmd(_) => "on a command".into(),
            Wait::Until(t) if *t > now => format!("in {}", format_until(t - now)),
            Wait::Until(_) => "due".into(),
        }
    }

    /// What it waited on, for the session told to go on.
    pub fn over(&self) -> String {
        match self {
            Wait::After => "The quests this one waited for are done".into(),
            Wait::Quest(t) => format!("The quest \"{t}\" is done"),
            Wait::Main(r) => format!("{r} is on the main branch"),
            Wait::File(f) => format!("{f} exists"),
            Wait::Cmd(c) => format!("`{c}` exits 0"),
            Wait::Until(t) => format!("It is past {}", utc(*t)),
        }
    }
}

/// Splits a wait in braces off the end of an item's body. The last ` {`
/// that reads as one is it, so braces earlier in the title stay there.
fn split_wait(body: &str) -> (&str, Option<Wait>) {
    let mut search = body.len();
    while let Some(at) = body[..search].rfind(" {") {
        search = at;
        if let Some(w) = Wait::read(&body[at + 1..]) {
            return (body[..at].trim_end(), Some(w));
        }
    }
    (body, None)
}

/// A time as `2026-10-01T14:05Z`, in UTC, with seconds only when it has
/// some.
pub fn utc(secs: u64) -> String {
    let (y, m, d) = civil((secs / 86_400) as i64);
    let rest = secs % 86_400;
    let (h, min, s) = (rest / 3600, rest % 3600 / 60, rest % 60);
    if s == 0 {
        format!("{y:04}-{m:02}-{d:02}T{h:02}:{min:02}Z")
    } else {
        format!("{y:04}-{m:02}-{d:02}T{h:02}:{min:02}:{s:02}Z")
    }
}

/// Reads `2026-10-01T14:05Z`, with seconds or a space for the `T` too. Only
/// UTC, so the file means one moment wherever it is read.
pub fn parse_utc(s: &str) -> Option<u64> {
    let s = s.trim().strip_suffix(['Z', 'z'])?;
    let (date, time) = s.split_once(['T', 't', ' '])?;
    let num = |p: &str, len: usize| {
        (p.len() == len && p.bytes().all(|b| b.is_ascii_digit()))
            .then(|| p.parse::<u64>().ok())
            .flatten()
    };
    let date: Vec<&str> = date.split('-').collect();
    let time: Vec<&str> = time.split(':').collect();
    let ([y, m, d], [h, min, rest @ ..]) = (date.as_slice(), time.as_slice()) else {
        return None;
    };
    let (y, m, d, h, min) = (num(y, 4)?, num(m, 2)?, num(d, 2)?, num(h, 2)?, num(min, 2)?);
    let sec = match rest {
        [] => 0,
        [s] => num(s, 2)?,
        _ => return None,
    };
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) || h > 23 || min > 59 || sec > 59 {
        return None;
    }
    let days = u64::try_from(days_from_civil(y as i64, m, d)).ok()?;
    Some(days * 86_400 + h * 3600 + min * 60 + sec)
}

/// What `quest blocked --until` takes: `+30m` (or s, h, d) from `now`,
/// Unix seconds, or a UTC time as `parse_utc` reads it.
pub fn parse_when(s: &str, now: u64) -> Option<u64> {
    let s = s.trim();
    if let Some(rel) = s.strip_prefix('+') {
        let unit = rel.chars().last()?;
        let n: u64 = rel[..rel.len() - unit.len_utf8()].parse().ok()?;
        let each = match unit {
            's' => 1,
            'm' => 60,
            'h' => 3600,
            'd' => 86_400,
            _ => return None,
        };
        return now.checked_add(n.checked_mul(each)?);
    }
    if !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) {
        return s.parse().ok();
    }
    parse_utc(s)
}

/// Year, month and day of a day counted from 1970-01-01, after Howard
/// Hinnant's `civil_from_days`.
fn civil(days: i64) -> (i64, u64, u64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u64;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u64;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// The day counted from 1970-01-01, after Hinnant's `days_from_civil`.
fn days_from_civil(y: i64, m: u64, d: u64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = if m > 2 { m - 3 } else { m + 9 } as i64;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Splits `Fix it @fix-it-51234: why` into the title, the holder and the
/// reason. The last ` @` followed by an id and then the end or a colon is
/// the holder, so an `@` in the title itself stays in the title.
fn split_holder(body: &str) -> (String, Option<String>, Option<String>) {
    let mut search = body.len();
    while let Some(at) = body[..search].rfind('@') {
        search = at;
        if at > 0 && !body[..at].ends_with(' ') {
            continue;
        }
        let after = &body[at + 1..];
        let id_len = after.find(|c: char| !is_id_char(c)).unwrap_or(after.len());
        if id_len == 0 {
            continue;
        }
        let tail = &after[id_len..];
        let reason = if tail.is_empty() {
            None
        } else if let Some(r) = tail.strip_prefix(':') {
            Some(r.trim().to_string()).filter(|r| !r.is_empty())
        } else {
            continue;
        };
        let title = body[..at].trim_end().to_string();
        return (title, Some(after[..id_len].to_string()), reason);
    }
    (body.to_string(), None, None)
}

fn is_id_char(c: char) -> bool {
    // Any letter, not only ASCII, so a holder written before ids were
    // ASCII, like `@kør-tests`, still reads as one.
    c.is_alphanumeric() || matches!(c, '-' | '_' | '.')
}

/// An item's line as Horadric writes it back.
pub fn item_line(
    mark: Mark,
    title: &str,
    holder: Option<&str>,
    reason: Option<&str>,
    wait: Option<&Wait>,
) -> String {
    let mut out = format!("- [{}] {}", mark.char(), title);
    if let Some(h) = holder {
        out.push_str(" @");
        out.push_str(h);
        if let Some(r) = reason.map(one_line).filter(|r| !r.is_empty()) {
            out.push_str(": ");
            out.push_str(&r);
        }
    }
    if let Some(w) = wait {
        out.push(' ');
        out.push_str(&w.spell());
    }
    out
}

/// `text` with line `line` swapped for `new`, every other byte as it was,
/// the line keeping its own ending. None when there is no such line.
pub fn replace_line(text: &str, line: usize, new: &str) -> Option<String> {
    let mut out = String::with_capacity(text.len() + new.len());
    let mut found = false;
    for (i, part) in text.split_inclusive('\n').enumerate() {
        if i == line {
            let ending = if part.ends_with("\r\n") {
                "\r\n"
            } else if part.ends_with('\n') {
                "\n"
            } else {
                ""
            };
            out.push_str(new);
            out.push_str(ending);
            found = true;
        } else {
            out.push_str(part);
        }
    }
    found.then_some(out)
}

/// `text` with a new open item at the end, in the file's own line endings.
pub fn append(text: &str, title: &str) -> String {
    append_with_notes(text, title, "")
}

/// `text` with a new open item at the end and `notes` indented under it,
/// in the file's own line endings. Blank lines in the notes would end the
/// item, so they are left out.
pub fn append_with_notes(text: &str, title: &str, notes: &str) -> String {
    let ending = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let mut out = text.to_string();
    if !out.is_empty() && !out.ends_with('\n') {
        out.push_str(ending);
    }
    out.push_str(&new_item(title, notes, ending));
    out
}

/// `text` with a new open item and its notes put in front of line
/// `line`, or at the end when there is no such line.
pub fn insert_with_notes(text: &str, line: usize, title: &str, notes: &str) -> String {
    let (mut lines, ending) = cut(text);
    if line >= lines.len() {
        return append_with_notes(text, title, notes);
    }
    lines.insert(line, new_item(title, notes, ending));
    lines.concat()
}

/// `text` with `note` put as a notes line in front of line `line`, which
/// is right under an item or among its notes, or at the end when there is
/// no such line.
pub fn insert_note(text: &str, line: usize, note: &str) -> String {
    let (mut lines, ending) = cut(text);
    if let Some(last) = lines.last_mut().filter(|l| !l.ends_with('\n')) {
        last.push_str(ending);
    }
    let note = format!("  {}{ending}", one_line(note));
    lines.insert(line.min(lines.len()), note);
    lines.concat()
}

/// The line just past the item on `line` and its notes.
pub fn end_of(text: &str, line: usize) -> usize {
    let (lines, _) = cut(text);
    span(&lines, line).end
}

/// A new open item's line and its notes, each line ended.
fn new_item(title: &str, notes: &str, ending: &str) -> String {
    let mut out = item_line(Mark::Open, &one_line(title), None, None, None);
    out.push_str(ending);
    for line in notes.lines().map(str::trim).filter(|l| !l.is_empty()) {
        out.push_str("  ");
        out.push_str(line);
        out.push_str(ending);
    }
    out
}

/// Blocks the item held by `holder` until the quest named `after` is done:
/// marked blocked with why and `{after}`, and an `After:` line under its
/// notes unless it has one naming that quest already. None when no item
/// is held by it.
pub fn block_after(text: &str, holder: &str, reason: Option<&str>, after: &str) -> Option<String> {
    let text = set_held(text, holder, Mark::Blocked, reason, Some(&Wait::After))?;
    let task = parse(&text)
        .into_iter()
        .find(|t| t.holder.as_deref() == Some(holder) && t.mark == Mark::Blocked)?;
    let want = one_line(after).to_lowercase();
    if task
        .after()
        .iter()
        .any(|a| one_line(a).to_lowercase() == want)
    {
        return Some(text);
    }
    let (mut lines, ending) = cut(&text);
    let end = span(&lines, task.line).end;
    // The last line may have had no ending: the note follows it now.
    if let Some(prev) = lines.get_mut(end - 1).filter(|l| !l.ends_with('\n')) {
        prev.push_str(ending);
    }
    lines.insert(end, format!("  {}{ending}", after_note(after)));
    Some(join(lines, ending, text.ends_with('\n')))
}

/// Changes the item held by `holder` to `mark`, keeping the holder, with
/// why it is blocked and what it waits on. None when no item is held by it.
pub fn set_held(
    text: &str,
    holder: &str,
    mark: Mark,
    reason: Option<&str>,
    wait: Option<&Wait>,
) -> Option<String> {
    let task = parse(text)
        .into_iter()
        .find(|t| t.holder.as_deref() == Some(holder) && t.mark != Mark::Done)?;
    let line = item_line(mark, &task.title, Some(holder), reason, wait);
    replace_line(text, task.line, &line)
}

/// Hands the item held by the tombs `batch` to the one tomb that won,
/// `winner`, and marks it done: the human picking is the review. None when
/// no item not done is held by the batch.
pub fn pick(text: &str, batch: &str, winner: &str) -> Option<String> {
    let task = parse(text)
        .into_iter()
        .find(|t| t.holder.as_deref() == Some(batch) && t.mark != Mark::Done)?;
    let line = item_line(Mark::Done, &task.title, Some(winner), None, None);
    replace_line(text, task.line, &line)
}

/// Gives the open item on `line` titled `title` to `holder`. None when that
/// line no longer holds that open item, because the file changed under us.
pub fn take(text: &str, line: usize, title: &str, holder: &str) -> Option<String> {
    let task = parse(text).into_iter().find(|t| t.line == line)?;
    if task.mark != Mark::Open || task.title != title {
        return None;
    }
    replace_line(
        text,
        line,
        &item_line(Mark::Working, title, Some(holder), None, None),
    )
}

/// Sets the item on `line` titled `title` to `mark`. `Open` lets go of the
/// holder; every other mark keeps it. Only a blocked item keeps why and
/// what it waits on. None when the line holds something else now.
pub fn set_mark(text: &str, line: usize, title: &str, mark: Mark) -> Option<String> {
    let task = parse(text).into_iter().find(|t| t.line == line)?;
    if task.title != title {
        return None;
    }
    let holder = task.holder.as_deref().filter(|_| mark != Mark::Open);
    let reason = task.reason.as_deref().filter(|_| mark == Mark::Blocked);
    let wait = task.wait.as_ref().filter(|_| mark == Mark::Blocked);
    replace_line(text, line, &item_line(mark, title, holder, reason, wait))
}

/// The file cut into lines, each keeping its ending, and the ending the
/// file uses, for lines that move to where one is needed.
fn cut(text: &str) -> (Vec<String>, &'static str) {
    let ending = if text.contains("\r\n") { "\r\n" } else { "\n" };
    (
        text.split_inclusive('\n').map(String::from).collect(),
        ending,
    )
}

/// The lines put back together. A moved last line gains an ending, so it
/// is taken off again when the file had none at its end.
fn join(mut lines: Vec<String>, ending: &str, ended: bool) -> String {
    if let Some(last) = lines.last_mut() {
        if !last.ends_with('\n') {
            last.push_str(ending);
        }
    }
    let mut out = lines.concat();
    if !ended {
        let kept = out.trim_end_matches(['\r', '\n']).len();
        out.truncate(kept);
    }
    out
}

/// Which lines the item on `line` covers: itself and its notes, as
/// `parse` reads them, without the blank lines after the last note.
fn span(lines: &[String], line: usize) -> std::ops::Range<usize> {
    let mut end = line + 1;
    for (i, raw) in lines.iter().enumerate().skip(line + 1) {
        let raw = raw.trim_end_matches(['\r', '\n']);
        if raw.trim().is_empty() {
            continue;
        }
        if parse_item(raw, i).is_some() || !raw.starts_with([' ', '\t']) {
            break;
        }
        end = i + 1;
    }
    line..end
}

/// The item on `line` if it is still titled `title`.
fn found(text: &str, line: usize, title: &str) -> Option<Task> {
    parse(text)
        .into_iter()
        .find(|t| t.line == line && t.title == title)
}

/// Gives the item on `line` titled `title` a new title and notes, keeping
/// its mark and holder. None when the line holds something else now or
/// the new title is empty. Blank lines in the notes would end the item,
/// so they are left out.
pub fn edit(text: &str, line: usize, title: &str, new_title: &str, notes: &str) -> Option<String> {
    let task = found(text, line, title)?;
    let new_title = one_line(new_title);
    if new_title.is_empty() {
        return None;
    }
    let (mut lines, ending) = cut(text);
    let covered = span(&lines, line);
    let mut item = vec![
        item_line(
            task.mark,
            &new_title,
            task.holder.as_deref(),
            task.reason.as_deref(),
            task.wait.as_ref(),
        ) + ending,
    ];
    item.extend(
        notes
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(|l| format!("  {l}{ending}")),
    );
    lines.splice(covered, item);
    Some(join(lines, ending, text.ends_with('\n')))
}

/// The file without the item on `line` titled `title` and its notes. None
/// when the line holds something else now.
pub fn remove(text: &str, line: usize, title: &str) -> Option<String> {
    found(text, line, title)?;
    let (mut lines, ending) = cut(text);
    let covered = span(&lines, line);
    lines.drain(covered);
    Some(join(lines, ending, text.ends_with('\n')))
}

/// The item on `line` titled `title` swapped with the item above it, or
/// below, notes and all. Whatever lies between the two, a heading say,
/// stays where it is, so an item can move from one section to the next.
/// None at the top or bottom of the list, or when the line holds
/// something else now.
pub fn shift(text: &str, line: usize, title: &str, up: bool) -> Option<String> {
    found(text, line, title)?;
    let list = parse(text);
    let at = list.iter().position(|t| t.line == line)?;
    let other = if up {
        list.get(at.checked_sub(1)?)?
    } else {
        list.get(at + 1)?
    };
    let (first, second) = if up {
        (other.line, line)
    } else {
        (line, other.line)
    };
    let (lines, ending) = cut(text);
    let a = span(&lines, first);
    let b = span(&lines, second);
    let mut out = lines[..a.start].to_vec();
    out.extend_from_slice(&lines[b.clone()]);
    out.extend_from_slice(&lines[a.end..b.start]);
    out.extend_from_slice(&lines[a.clone()]);
    out.extend_from_slice(&lines[b.end..]);
    // The first item's last line may have been the file's last: it needs
    // an ending now that something follows it.
    let last = out.len() - 1;
    for l in &mut out[..last] {
        if !l.ends_with('\n') {
            l.push_str(ending);
        }
    }
    Some(join(out, ending, text.ends_with('\n')))
}

/// How a project's list gets worked through.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// Nothing starts by itself. A click takes an item.
    #[default]
    Manual,
    /// The next item starts once the human approves the last one.
    Review,
    /// The next item starts as soon as the last one is done.
    Auto,
}

impl Mode {
    pub const ALL: [Mode; 3] = [Mode::Manual, Mode::Review, Mode::Auto];

    pub fn name(self) -> &'static str {
        match self {
            Mode::Manual => "manual",
            Mode::Review => "review",
            Mode::Auto => "auto",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Mode::Manual => "Manual",
            Mode::Review => "Review",
            Mode::Auto => "Auto",
        }
    }

    /// What a menu says about it.
    pub fn explain(self) -> &'static str {
        match self {
            Mode::Manual => "Manual: click an item to start it",
            Mode::Review => "Review: the next item starts when you approve the last",
            Mode::Auto => "Auto: work down the list until it is done",
        }
    }

    fn from_name(s: &str) -> Option<Mode> {
        Mode::ALL.into_iter().find(|m| m.name() == s)
    }

    /// Items start by themselves.
    pub fn runs(self) -> bool {
        self != Mode::Manual
    }

    /// What an agent's "done" makes of its item.
    pub fn finished(self) -> Mark {
        match self {
            Mode::Auto => Mark::Done,
            Mode::Manual | Mode::Review => Mark::Review,
        }
    }
}

/// The mode in a `config.json`. Anything unreadable is manual, the mode
/// that starts nothing.
pub fn mode(config: &str) -> Mode {
    serde_json::from_str::<Value>(config)
        .ok()
        .and_then(|v| {
            let name = v.get("tasks")?.get("mode")?.as_str()?.to_string();
            Mode::from_name(&name)
        })
        .unwrap_or_default()
}

/// `config` with the mode set, everything else in it kept.
pub fn with_mode(config: &str, mode: Mode) -> String {
    with_setting(config, "mode", Value::String(mode.name().into()))
}

/// `config` with how many items the runner holds at once, everything
/// else in it kept. One takes the key out, since one is what no key means.
pub fn with_parallel(config: &str, n: usize) -> String {
    let n = n.clamp(1, MOST_PARALLEL);
    if n == 1 {
        return with_setting(config, "parallel", Value::Null);
    }
    with_setting(config, "parallel", Value::from(n))
}

/// `config` with one key under `"tasks"` set, or taken out for null. A
/// file that is not a JSON object is started afresh rather than lost in
/// part.
fn with_setting(config: &str, key: &str, value: Value) -> String {
    let mut root = match serde_json::from_str::<Value>(config) {
        Ok(Value::Object(m)) => m,
        _ => Map::new(),
    };
    let tasks = root
        .entry("tasks")
        .or_insert_with(|| Value::Object(Map::new()));
    if !tasks.is_object() {
        *tasks = Value::Object(Map::new());
    }
    if let Some(t) = tasks.as_object_mut() {
        if value.is_null() {
            t.remove(key);
        } else {
            t.insert(key.into(), value);
        }
    }
    let mut out = serde_json::to_string_pretty(&Value::Object(root)).unwrap_or_default();
    out.push('\n');
    out
}

/// At most this many items in hand at once, so a typo in the config can
/// not start a crowd of agents.
pub const MOST_PARALLEL: usize = 16;

/// How many items the runner holds at once, from `"tasks": {"parallel":
/// 3}` in a `config.json`. One when it says nothing or nonsense.
pub fn parallel(config: &str) -> usize {
    serde_json::from_str::<Value>(config)
        .ok()
        .and_then(|v| v.get("tasks")?.get("parallel")?.as_u64())
        .map_or(1, |n| (n as usize).clamp(1, MOST_PARALLEL))
}

/// Whether the list still has work in it, which keeps its project's
/// tile up with no session running. A list of done items is history.
pub fn unfinished(tasks: &[Task]) -> bool {
    tasks
        .iter()
        .any(|t| t.mark != Mark::Done && !t.title.trim().is_empty())
}

/// What the runner does next for a project.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Next {
    /// Manual mode: nothing.
    Off,
    /// Start the item at this index.
    Start(usize),
    /// As many items are in hand as the project lets run at once, or one
    /// of them waits for a click to resume.
    Wait,
    /// The item at this index is in the way: blocked on the human, or held
    /// by a session that is gone. The order is the order, so the runner
    /// stops there.
    Stuck(usize),
    /// The blocked item at this index waited on something that is over:
    /// its session goes on, or it starts again when that is gone.
    Resume(usize),
    /// Nothing left to do.
    Finished,
}

/// What became of the session holding an item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Holder {
    Gone,
    /// Brought back after a restart. Only a click resumes it, and until
    /// then the runner starts nothing beside it either.
    Paused,
    Live,
}

/// The runner's decision, given the list, the mode, how many items may be
/// in hand at once, what became of each holder, and whether what a blocked
/// item waits on is over. It takes the first ready open item, passing
/// those whose `After:` quests are not done. A blocked item that waits on
/// something the runner can check is passed by until then; one that waits
/// on the human stops the list, and so does a cycle.
pub fn next(
    tasks: &[Task],
    mode: Mode,
    parallel: usize,
    holder: impl Fn(&str) -> Holder,
    met: impl Fn(&Task) -> bool,
) -> Next {
    if !mode.runs() {
        return Next::Off;
    }
    let of = |t: &Task| t.holder.as_deref().map_or(Holder::Gone, &holder);
    let in_hand: Vec<(Holder, usize)> = tasks
        .iter()
        .filter(|t| matches!(t.mark, Mark::Working | Mark::Review))
        .map(|t| (of(t), t.holder.as_deref().map_or(1, tombs::weight)))
        .filter(|(h, _)| *h != Holder::Gone)
        .collect();
    // An item in tombs takes a place for each of its sessions.
    let places: usize = in_hand.iter().map(|(_, w)| w).sum();
    if in_hand.iter().any(|(h, _)| *h == Holder::Paused) || places >= parallel.max(1) {
        return Next::Wait;
    }
    let ready = readiness(tasks);
    let mut waits = false;
    for (i, t) in tasks.iter().enumerate() {
        match (t.mark, &ready[i]) {
            (Mark::Done, _) => {}
            (Mark::Open, _) if t.title.trim().is_empty() => {}
            (Mark::Working | Mark::Review, _) if of(t) == Holder::Live => {}
            (Mark::Working | Mark::Review, _) => return Next::Stuck(i),
            (Mark::Blocked, _) if t.wait.is_none() => return Next::Stuck(i),
            // Only a human breaks a cycle, so the list stops there.
            (Mark::Open | Mark::Blocked, Ready::Cycle) => return Next::Stuck(i),
            (Mark::Open, Ready::Yes) => return Next::Start(i),
            // A paused session comes back only by a click.
            (Mark::Blocked, Ready::Yes) if met(t) && of(t) != Holder::Paused => {
                return Next::Resume(i)
            }
            (Mark::Open | Mark::Blocked, _) => waits = true,
        }
    }
    if in_hand.is_empty() && !waits {
        Next::Finished
    } else {
        Next::Wait
    }
}

/// A session name from an item's title: lower case ASCII words joined by
/// hyphens, short enough for a tile. ASCII because the name becomes the
/// session's id, which goes into headers, branch names and the list's
/// `@holder`, and `kør` must not lose its quest there.
pub fn slug(title: &str) -> String {
    let mut out = String::new();
    let ascii: String = title
        .chars()
        .flat_map(char::to_lowercase)
        .map(ascii)
        .collect();
    for word in ascii
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
    {
        if out.len() + word.len() > 40 {
            break;
        }
        if !out.is_empty() {
            out.push('-');
        }
        out.push_str(word);
    }
    if out.is_empty() {
        out.push_str("task");
    }
    out
}

/// The ASCII spelling of a lower case letter: the Danish and German ones
/// as they are written without them, accents dropped, anything else a
/// space so it splits words.
fn ascii(c: char) -> &'static str {
    const PLAIN: &str = "abcdefghijklmnopqrstuvwxyz0123456789";
    let plain = |i: usize| &PLAIN[i..i + 1];
    match c {
        'æ' => "ae",
        'ø' => "oe",
        'å' => "aa",
        'ß' => "ss",
        'ä' | 'à' | 'á' | 'â' | 'ã' => "a",
        'ö' | 'ò' | 'ó' | 'ô' | 'õ' => "o",
        'ü' | 'ù' | 'ú' | 'û' => "u",
        'é' | 'è' | 'ê' | 'ë' => "e",
        'í' | 'ì' | 'î' | 'ï' => "i",
        'ç' => "c",
        'ñ' => "n",
        'ý' | 'ÿ' => "y",
        c => match PLAIN.find(c) {
            Some(i) if c.is_ascii() => plain(i),
            _ => " ",
        },
    }
}

/// The first prompt of a session that takes `task`: the item, its notes,
/// and how to report back. The system prompt says so too, but an agent
/// follows its prompt more surely, and the human sees what it was asked.
pub fn prompt(task: &Task, horadric: &str, file: &str) -> String {
    let mut out = task.title.clone();
    if !task.notes.is_empty() {
        out.push_str("\n\n");
        out.push_str(&task.notes.join("\n"));
    }
    out.push_str(&format!(
        "\n\n(A quest from {file}. When it is finished, run \
         `{horadric} quest done \"<one short line on what you achieved>\"`.)"
    ));
    out
}

/// Whether a session runs with permission prompts bypassed: only when it
/// holds a quest in a worktree of its own. In the main tree a wrong
/// command touches what the human and other agents work in, so a quest
/// there, the quest giver and any session holding none keep the
/// project's mode.
pub fn bypasses_prompts(holds_quest: bool, own_tree: bool) -> bool {
    holds_quest && own_tree
}

/// What the agent is told beside its first prompt, every time it starts or
/// resumes: that it works one item of the list, and how to report back.
/// `horadric` is how to run this Horadric from the agent's shell, `file`
/// the list's path from the project folder. `list`
/// is where the list is when the agent works in a worktree of its own,
/// which has no list or an old copy of it.
pub fn system_prompt(horadric: &str, file: &str, list: Option<&str>) -> String {
    let mut out = format!(
        "You are working on one quest of this project's quest log, {file}. \
         Horadric started you on it and does not know you are finished until you \
         tell it, so your last step is always a command in your shell. Do only this \
         item. When it is finished, commit your work if you changed files, then run \
         `{horadric} quest done \"<one short line on what you achieved>\"` with your \
         Bash tool; that line is kept as the quest's record. If you can not go on \
         without the human, run `{horadric} quest blocked \"<why>\"` instead and say \
         what you need. When what you wait on is something Horadric can check, say so and it \
         wakes you itself once that holds: add `--on \"<title>\"` for another \
         quest in the log being done, `--on-main <commit or branch>` for one being on \
         the main branch, `--on-file <path>` for a file existing, `--on-cmd \
         \"<command>\"` for a command cmd.exe runs in the project folder exiting 0, \
         or `--until <+30m or 2026-10-01T14:05Z>` for a time. Use one whenever it \
         fits: the list goes on past a quest that waits on one, and stops at one that \
         waits on the human. If you find other work worth doing, add it to the list \
         with `{horadric} quest add \"<title>\"` instead of doing it now, adding \
         `--after \"<other title>\"` when it can only start once another quest is \
         done. Quests in {file} are lines like `- [ ] Title`, in the order they \
         should be done, with notes indented under them; a notes line `After: \
         <title>` holds a quest back until that one is done. When your item is to \
         plan work, write the items you decide on into the file below your own \
         line, each with an `After:` line for every quest it needs first."
    );
    if let Some(list) = list {
        out.push_str(&format!(
            " Other items run beside yours, each in a worktree of its own. The list \
             lives only in the main working tree, at {list}: read and write it \
             there, the one file in the main tree you may change, and never a \
             copy in your worktree. `{horadric} quest` finds it from anywhere."
        ));
    }
    out
}

/// The first prompt of the quest giver, a session the gold ! on the
/// tile starts to suggest quests. It only suggests: the human picks, since
/// a project in auto mode would start whatever lands in the log at once.
pub fn giver_prompt(horadric: &str, file: &str) -> String {
    format!(
        "You are the quest giver for this project. Look around before you suggest \
         anything: the README, the docs and any plan in them, the quest log at {file} \
         (what is done, what is open), the recent git log, and the code itself. Then \
         suggest three to five quests worth doing next that are not on the log yet, \
         each a short title and a few lines of notes saying what to do and why, \
         numbered. Change nothing while you look.\n\n\
         Ask me which to add. Add only the ones I pick, each with \
         `{horadric} quest add \"<title>\" --notes \"<notes>\"`, in the order they \
         should be done, then stop. When one can only start once another quest is \
         done, one you add or one already on the log, add `--after \"<that title>\"` \
         as well, once for each, so the runner does not start it early."
    )
}

/// What an agent is told, once, when its turn ended without a report.
pub fn nudge(horadric: &str) -> String {
    format!(
        "If you are finished with this quest, commit your work and run \
         `{horadric} quest done \"<one short line on what you achieved>\"`. \
         If not, say what you need from me."
    )
}

/// What a session stopped by the usage limit is told once the limit has
/// reset, so it picks up the item where the refusal left it.
pub fn go_on(horadric: &str) -> String {
    format!(
        "The usage limit has reset. Go on with this item where you left off, \
         and when it is finished, commit your work and run \
         `{horadric} quest done \"<one short line on what you achieved>\"`."
    )
}

/// What a blocked session is told once what it waited on is over.
pub fn waited(horadric: &str, task: &Task) -> String {
    format!(
        "{}, which this quest waited on. Go on with it where you left off, \
         and when it is finished, commit your work and run \
         `{horadric} quest done \"<one short line on what you achieved>\"`.",
        task.over()
    )
}

/// Text on one line: newlines and tabs become spaces.
pub fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_quest_in_its_own_worktree_bypasses_prompts() {
        assert!(bypasses_prompts(true, true));
        assert!(!bypasses_prompts(true, false));
        assert!(!bypasses_prompts(false, true));
        assert!(!bypasses_prompts(false, false));
    }

    const SAMPLE: &str = "# Backlog\n\
        \n\
        - [x] Rename Glance to Horadric\n\
        - [/] Fix the login redirect @fix-login-51234\n\
        \x20 Happens only after a session expires.\n\
        \x20 Repro in #12.\n\
        - [?] Add dark mode @dark-mode-51300\n\
        - [!] Migrate to the new API @migrate-51400: needs a key I do not have\n\
        - [ ] Mail support@example.com about it\n\
        - [-] Something Horadric does not know\n\
        Some prose.\n\
        \x20 Not a note, the item before is over.\n";

    #[test]
    fn the_quest_giver_suggests_and_adds_only_what_the_human_picks() {
        let p = giver_prompt("horadric", ".horadric/quests.md");
        assert!(p.contains("the quest log at .horadric/quests.md"));
        assert!(p.contains("Add only the ones I pick"));
        assert!(p.contains("`horadric quest add \"<title>\" --notes \"<notes>\"`"));
        assert!(p.contains("Change nothing while you look."));
        assert!(p.contains("`--after \"<that title>\"`"));
        let p = system_prompt("hx", QUESTS_FILE, None);
        assert!(p.contains("`--after \"<other title>\"`"));
        assert!(p.contains("an `After:` line for every quest it needs first"));
    }

    #[test]
    fn a_list_is_unfinished_while_any_item_is_not_done() {
        assert!(unfinished(&parse(SAMPLE)));
        assert!(unfinished(&parse(
            "- [!] Stuck @a: why
"
        )));
        assert!(!unfinished(&parse(
            "- [x] One
- [X] Two
"
        )));
        assert!(!unfinished(&parse(
            "- [ ]   
"
        )));
        assert!(!unfinished(&[]));
    }

    #[test]
    fn every_mark_parses_with_its_holder_reason_and_notes() {
        let t = parse(SAMPLE);
        assert_eq!(t.len(), 5);
        assert_eq!((t[0].mark, t[0].line), (Mark::Done, 2));
        assert_eq!(t[0].title, "Rename Glance to Horadric");
        assert_eq!(t[1].mark, Mark::Working);
        assert_eq!(t[1].title, "Fix the login redirect");
        assert_eq!(t[1].holder.as_deref(), Some("fix-login-51234"));
        assert_eq!(
            t[1].notes,
            ["Happens only after a session expires.", "Repro in #12."]
        );
        assert_eq!(t[2].mark, Mark::Review);
        assert_eq!(t[3].mark, Mark::Blocked);
        assert_eq!(t[3].reason.as_deref(), Some("needs a key I do not have"));
        // An @ inside a word is the title's.
        assert_eq!(t[4].title, "Mail support@example.com about it");
        assert_eq!(t[4].holder, None);
        assert!(t[4].notes.is_empty());
    }

    #[test]
    fn crlf_files_parse_the_same() {
        let crlf = SAMPLE.replace('\n', "\r\n");
        assert_eq!(parse(&crlf), parse(SAMPLE));
    }

    #[test]
    fn an_at_in_the_middle_of_a_title_is_not_a_holder() {
        let t = &parse("- [ ] Ask @alice about it\n")[0];
        assert_eq!(t.title, "Ask @alice about it");
        assert_eq!(t.holder, None);
        let t = &parse("- [/] Ask @alice about it @ask-1\n")[0];
        assert_eq!(t.title, "Ask @alice about it");
        assert_eq!(t.holder.as_deref(), Some("ask-1"));
    }

    #[test]
    fn a_line_is_rewritten_and_nothing_else_moves() {
        let crlf = SAMPLE.replace('\n', "\r\n");
        for text in [SAMPLE.to_string(), crlf] {
            let out = replace_line(&text, 8, "- [/] Mail it @mail-1").unwrap();
            let before: Vec<&str> = text.split_inclusive('\n').collect();
            let after: Vec<&str> = out.split_inclusive('\n').collect();
            assert_eq!(before.len(), after.len());
            for (i, (a, b)) in before.iter().zip(&after).enumerate() {
                if i == 8 {
                    assert!(b.starts_with("- [/] Mail it @mail-1"));
                    assert_eq!(a.ends_with("\r\n"), b.ends_with("\r\n"));
                } else {
                    assert_eq!(a, b);
                }
            }
        }
        assert_eq!(replace_line("a\nb", 1, "c").as_deref(), Some("a\nc"));
        assert_eq!(replace_line("a\n", 5, "c"), None);
    }

    #[test]
    fn taking_an_item_marks_it_and_names_its_holder() {
        let out = take(SAMPLE, 8, "Mail support@example.com about it", "mail-9").unwrap();
        let t = &parse(&out)[4];
        assert_eq!(t.mark, Mark::Working);
        assert_eq!(t.holder.as_deref(), Some("mail-9"));
        assert_eq!(t.title, "Mail support@example.com about it");
        // The file changed under us: that line is something else now.
        assert_eq!(take(SAMPLE, 8, "Another title", "x"), None);
        assert_eq!(take(SAMPLE, 3, "Fix the login redirect", "x"), None);
    }

    #[test]
    fn the_holder_reports_done_or_blocked() {
        let out = set_held(SAMPLE, "fix-login-51234", Mark::Review, None, None).unwrap();
        assert_eq!(parse(&out)[1].mark, Mark::Review);
        let out = set_held(
            &out,
            "fix-login-51234",
            Mark::Blocked,
            Some("no\nkey"),
            None,
        )
        .unwrap();
        let t = &parse(&out)[1];
        assert_eq!(t.mark, Mark::Blocked);
        assert_eq!(t.reason.as_deref(), Some("no key"));
        // Notes stay under it.
        assert_eq!(t.notes.len(), 2);
        assert_eq!(set_held(SAMPLE, "nobody", Mark::Done, None, None), None);
    }

    #[test]
    fn picking_a_tomb_hands_it_the_item_as_done() {
        let text = "- [/] Fix it @fix-1.x3
  A note.
- [ ] Next
";
        let out = pick(text, "fix-1.x3", "fix-1.x3.2").unwrap();
        assert_eq!(
            out,
            "- [x] Fix it @fix-1.x3.2
  A note.
- [ ] Next
"
        );
        assert_eq!(pick(&out, "fix-1.x3", "fix-1.x3.1"), None);
    }

    #[test]
    fn an_item_in_tombs_takes_a_place_for_each_tomb() {
        let list = parse(
            "- [/] A @a-1.x3
- [ ] B
",
        );
        assert_eq!(
            next(&list, Mode::Auto, 3, |_| Holder::Live, unmet),
            Next::Wait
        );
        assert_eq!(
            next(&list, Mode::Auto, 4, |_| Holder::Live, unmet),
            Next::Start(1)
        );
        let list = parse(
            "- [/] A @a-1
- [ ] B
",
        );
        assert_eq!(
            next(&list, Mode::Auto, 2, |_| Holder::Live, unmet),
            Next::Start(1)
        );
    }

    #[test]
    fn putting_an_item_back_lets_go_of_its_holder() {
        let out = set_mark(SAMPLE, 7, "Migrate to the new API", Mark::Open).unwrap();
        let t = &parse(&out)[3];
        assert_eq!(
            (t.mark, t.holder.as_deref(), t.reason.as_deref()),
            (Mark::Open, None, None)
        );
        let out = set_mark(SAMPLE, 6, "Add dark mode", Mark::Done).unwrap();
        assert_eq!(parse(&out)[2].holder.as_deref(), Some("dark-mode-51300"));
    }

    #[test]
    fn appending_keeps_the_file_s_line_endings() {
        assert_eq!(append("", "First"), "- [ ] First\n");
        assert_eq!(append("- [ ] A", "B"), "- [ ] A\n- [ ] B\n");
        assert_eq!(append("- [ ] A\r\n", "B\nC"), "- [ ] A\r\n- [ ] B C\r\n");
    }

    #[test]
    fn notes_go_indented_under_the_new_item_and_parse_back() {
        let text = append_with_notes("- [ ] A\n", "B", "First line.\n\n  Second one.\r\n");
        assert_eq!(text, "- [ ] A\n- [ ] B\n  First line.\n  Second one.\n");
        let t = parse(&text);
        assert_eq!(t[1].notes, ["First line.", "Second one."]);
        assert_eq!(append_with_notes("", "C", "  "), "- [ ] C\n");
    }

    #[test]
    fn an_inserted_item_goes_in_front_of_its_line_with_its_notes() {
        let text = "# List\r\n- [x] A\r\n  a note\r\n- [ ] B\r\n";
        let out = insert_with_notes(text, 3, "Fix\nA", "Why.\n\nHow.");
        assert_eq!(
            out,
            "# List\r\n- [x] A\r\n  a note\r\n- [ ] Fix A\r\n  Why.\r\n  How.\r\n- [ ] B\r\n"
        );
        let t = parse(&out);
        assert_eq!(t[1].title, "Fix A");
        assert_eq!(t[1].notes, ["Why.", "How."]);
        assert_eq!(t[0].notes, ["a note"]);
        assert_eq!(
            insert_with_notes("- [ ] A", 9, "B", ""),
            "- [ ] A\n- [ ] B\n"
        );
    }

    #[test]
    fn the_mode_round_trips_and_keeps_the_rest_of_the_config() {
        assert_eq!(mode(""), Mode::Manual);
        assert_eq!(mode("{\"tasks\":{\"mode\":\"auto\"}}"), Mode::Auto);
        assert_eq!(mode("{\"tasks\":{\"mode\":\"wild\"}}"), Mode::Manual);
        let out = with_mode("{\"hosts\":[\"vps\"]}", Mode::Review);
        assert_eq!(mode(&out), Mode::Review);
        assert!(out.contains("\"vps\""));
        assert_eq!(mode(&with_mode("not json", Mode::Auto)), Mode::Auto);
        assert_eq!(mode(&with_mode("{\"tasks\":3}", Mode::Auto)), Mode::Auto);
    }

    fn live(_: &str) -> Holder {
        Holder::Live
    }

    fn unmet(_: &Task) -> bool {
        false
    }

    fn met(_: &Task) -> bool {
        true
    }

    #[test]
    fn a_wait_in_braces_parses_off_the_end_of_the_line() {
        let t = &parse("- [!] Wire it @wire-1: needs the engine {on quest: Build the engine}\n")[0];
        assert_eq!(t.title, "Wire it");
        assert_eq!(t.holder.as_deref(), Some("wire-1"));
        assert_eq!(t.reason.as_deref(), Some("needs the engine"));
        assert_eq!(t.wait, Some(Wait::Quest("Build the engine".into())));
        let t = &parse("- [!] A @a-1 {on file: out/x.txt}\n")[0];
        assert_eq!(
            (t.holder.as_deref(), t.reason.as_deref()),
            (Some("a-1"), None)
        );
        assert_eq!(t.wait, Some(Wait::File("out/x.txt".into())));
        // Written by a human, with no holder.
        let t = &parse("- [!] A {on cmd: exit /b 0}\n")[0];
        assert_eq!(
            (t.title.as_str(), t.wait.clone()),
            ("A", Some(Wait::Cmd("exit /b 0".into())))
        );
        let t = &parse("- [!] A @a-1 {until: 2026-10-01T14:05Z}\n")[0];
        assert_eq!(t.wait, Some(Wait::Until(1_790_863_500)));
        // Braces that are not a wait stay in the title.
        let t = &parse("- [ ] Fix {braces} in {on nothing: x}\n")[0];
        assert_eq!(t.title, "Fix {braces} in {on nothing: x}");
        assert_eq!(t.wait, None);
        let t = &parse("- [!] Use {x} @a-1: why {on main: feature}\n")[0];
        assert_eq!(t.title, "Use {x}");
        assert_eq!(t.wait, Some(Wait::Main("feature".into())));
    }

    #[test]
    fn every_kind_of_wait_round_trips() {
        for w in [
            Wait::After,
            Wait::Quest("Build {it}".into()),
            Wait::Main("abc123".into()),
            Wait::File("C:/x y/z.txt".into()),
            Wait::Cmd("git diff --quiet".into()),
            Wait::Until(1_790_863_500),
            Wait::Until(1_790_863_501),
        ] {
            let line = item_line(Mark::Blocked, "T", Some("t-1"), Some("why"), Some(&w));
            let t = &parse(&line)[0];
            assert_eq!(t.wait.as_ref(), Some(&w), "{line}");
            assert_eq!(t.reason.as_deref(), Some("why"));
        }
        assert_eq!(Wait::read("{until: soon}"), None);
        assert_eq!(Wait::read("{on quest:   }"), None);
    }

    #[test]
    fn blocking_with_a_wait_writes_it_and_going_on_drops_it() {
        let w = Wait::Quest("Add dark mode".into());
        let out = set_held(
            SAMPLE,
            "fix-login-51234",
            Mark::Blocked,
            Some("later"),
            Some(&w),
        )
        .unwrap();
        let t = &parse(&out)[1];
        assert_eq!((t.mark, t.wait.as_ref()), (Mark::Blocked, Some(&w)));
        assert_eq!(t.notes.len(), 2);
        let back = set_mark(&out, t.line, &t.title, Mark::Working).unwrap();
        let t = &parse(&back)[1];
        assert_eq!(t.mark, Mark::Working);
        assert_eq!(t.holder.as_deref(), Some("fix-login-51234"));
        assert_eq!((t.reason.as_deref(), t.wait.as_ref()), (None, None));
        // An edit keeps it.
        let edited = edit(&out, 3, "Fix the login redirect", "Fix login", "").unwrap();
        assert_eq!(parse(&edited)[1].wait.as_ref(), Some(&w));
    }

    #[test]
    fn a_quest_wait_is_over_when_that_quest_is_done() {
        // The old form reads as an After: line, matched the same way.
        let list = parse("- [!] B @b-1 {on quest: the  first one}\n- [ ] The first one\n");
        assert_eq!(list[0].after(), ["the  first one"]);
        assert_eq!(readiness(&list)[0], Ready::After("The first one".into()));
        let list = parse("- [!] B @b-1 {on quest: the first one}\n- [x] The first one\n");
        assert_eq!(readiness(&list)[0], Ready::Yes);
        let list = parse("- [!] B @b-1 {on quest: Missing}\n- [x] The first one\n");
        assert_eq!(readiness(&list)[0], Ready::Unknown("Missing".into()));
        assert_eq!(Wait::Quest("x".into()).met(0), Some(true));
        assert_eq!(Wait::Until(100).met(99), Some(false));
        assert_eq!(Wait::Until(100).met(100), Some(true));
        assert_eq!(Wait::File("x".into()).met(0), None);
    }

    #[test]
    fn after_lines_are_read_from_the_notes() {
        let t = &parse(
            "- [ ] C\n  Some note.\n  After: Build the engine\n  after:   Wire it  \n  After:\n  Afterwards, more.\n",
        )[0];
        assert_eq!(t.after(), ["Build the engine", "Wire it"]);
        assert_eq!(t.notes.len(), 5);
        assert_eq!(after_line("AFTER: x"), Some("x"));
        assert_eq!(after_line("Aft"), None);
        assert_eq!(after_line("æøå: x"), None);
    }

    #[test]
    fn a_model_line_names_one_of_the_three_models() {
        assert_eq!(model_line("Model: haiku"), Some("haiku"));
        assert_eq!(model_line("model:   Opus  "), Some("opus"));
        assert_eq!(model_line("MODEL:sonnet"), Some("sonnet"));
        assert_eq!(model_line("Model: gpt-5"), None);
        assert_eq!(model_line("Model:"), None);
        assert_eq!(model_line("Models: haiku"), None);
        assert_eq!(model_line("Mod"), None);
        assert_eq!(model_line("æøå: haiku"), None);
    }

    #[test]
    fn a_humans_model_line_wins_over_warrivs_and_warrivs_last_counts() {
        let model = |notes: &str| {
            parse(&format!(
                "- [ ] Q
{notes}"
            ))[0]
                .model()
        };
        assert_eq!(model(""), None);
        assert_eq!(
            model(
                "  Model: opus
"
            ),
            Some("opus")
        );
        assert_eq!(
            model(
                "  Warriv: Model: haiku
"
            ),
            Some("haiku")
        );
        assert_eq!(
            model(
                "  Warriv: Model: haiku
  Warriv: Model: sonnet
"
            ),
            Some("sonnet")
        );
        assert_eq!(
            model(
                "  Warriv: Model: haiku
  Model: opus
  Warriv: Model: sonnet
"
            ),
            Some("opus")
        );
        assert_eq!(
            model(
                "  Warriv: model was hard to pick
"
            ),
            None
        );
    }

    #[test]
    fn only_claude_code_is_started_with_the_quests_model() {
        let t = &parse(
            "- [ ] Q
  Model: haiku
",
        )[0];
        assert_eq!(t.model_args(crate::Agent::Claude), ["--model", "haiku"]);
        assert!(t.model_args(crate::Agent::Codex).is_empty());
        assert!(t.model_args(crate::Agent::Grok).is_empty());
        let none = &parse(
            "- [ ] Q
",
        )[0];
        assert!(none.model_args(crate::Agent::Claude).is_empty());
    }

    #[test]
    fn a_name_matches_a_title_exactly_or_by_a_unique_start() {
        let list = parse(
            "- [ ] Build the engine\n- [ ] Build the engine: part two\n- [ ] Wire it\n- [ ] Wire it in\n",
        );
        assert_eq!(find(&list, "build the  engine"), Ok(0));
        assert_eq!(find(&list, "Build the engine: part"), Ok(1));
        assert_eq!(find(&list, "Wire"), Err(2));
        assert_eq!(find(&list, "Wire it"), Ok(2));
        assert_eq!(find(&list, "Nothing"), Err(0));
        assert_eq!(find(&list, ""), Err(0));
        let list = parse("- [ ] Same\n- [x] Same\n");
        assert_eq!(find(&list, "Same"), Err(2));
    }

    #[test]
    fn a_quest_is_ready_when_every_quest_it_names_is_done() {
        let list = parse(
            "- [x] A\n- [?] B\n- [ ] C\n  After: A\n- [ ] D\n  After: A\n  After: B\n- [ ] E\n  After: Nope\n- [ ] F\n  After: \n",
        );
        let r = readiness(&list);
        assert_eq!(r[2], Ready::Yes);
        // In review is not done: the work may not be on main yet.
        assert_eq!(r[3], Ready::After("B".into()));
        assert_eq!(r[4], Ready::Unknown("Nope".into()));
        assert_eq!(r[5], Ready::Yes);
        assert_eq!(r[3].label().as_deref(), Some("after B"));
        assert_eq!(r[4].label().as_deref(), Some("no quest Nope"));
        assert!(r[4].tangled() && !r[3].tangled() && !r[2].tangled());
        assert_eq!(r[4].why(), "No quest is called \"Nope\".");
        assert_eq!(r[3].why(), "");
        let list = parse("- [ ] Fix one\n- [ ] Fix two\n- [ ] C\n  After: Fix\n");
        assert_eq!(readiness(&list)[2], Ready::Several("Fix".into()));
        assert_eq!(readiness(&list)[2].label().as_deref(), Some("which Fix?"));
    }

    #[test]
    fn a_cycle_is_never_ready_and_says_so() {
        let list = parse(
            "- [ ] A\n  After: C\n- [ ] B\n  After: A\n- [ ] C\n  After: B\n- [ ] D\n  After: A\n- [ ] E\n  After: E\n",
        );
        let r = readiness(&list);
        assert_eq!(&r[..3], [Ready::Cycle, Ready::Cycle, Ready::Cycle]);
        // Waiting on a cycle is waiting, not being in one.
        assert_eq!(r[3], Ready::After("A".into()));
        assert_eq!(r[4], Ready::Cycle);
        assert_eq!(r[4].label().as_deref(), Some("waits on itself"));
    }

    #[test]
    fn the_runner_passes_quests_that_wait_and_takes_the_first_ready() {
        let t = parse("- [ ] B\n  After: A\n- [ ] A\n- [ ] C\n");
        assert_eq!(next(&t, Mode::Auto, 1, live, unmet), Next::Start(1));
        // A chain of three on three places: one runs, the others wait.
        let t = parse("- [/] A @a-1\n- [ ] B\n  After: A\n- [ ] C\n  After: B\n");
        assert_eq!(next(&t, Mode::Auto, 3, live, unmet), Next::Wait);
        let t = parse("- [/] A @a-1\n- [ ] B\n  After: A\n- [ ] D\n");
        assert_eq!(next(&t, Mode::Auto, 3, live, unmet), Next::Start(2));
        // Once A is done, B goes.
        let t = parse("- [x] A @a-1\n- [ ] B\n  After: A\n");
        assert_eq!(next(&t, Mode::Auto, 1, live, unmet), Next::Start(1));
        // A typo is passed over, not started early and not the end.
        let t = parse("- [x] A\n- [ ] B\n  After: Typo\n");
        assert_eq!(next(&t, Mode::Auto, 1, live, unmet), Next::Wait);
        // A cycle stops the list there.
        let t = parse("- [ ] A\n  After: B\n- [ ] B\n  After: A\n- [ ] C\n");
        assert_eq!(next(&t, Mode::Auto, 1, live, unmet), Next::Stuck(0));
    }

    #[test]
    fn blocked_on_a_quest_writes_an_after_line_and_resumes_once_it_is_done() {
        let text = "- [/] B @b-1\n  A note.\n- [ ] A\n";
        let out = block_after(text, "b-1", Some("needs A"), "A").unwrap();
        assert_eq!(
            out,
            "- [!] B @b-1: needs A {after}\n  A note.\n  After: A\n- [ ] A\n"
        );
        let t = parse(&out);
        assert_eq!(t[0].wait, Some(Wait::After));
        assert_eq!(next(&t, Mode::Auto, 1, live, met), Next::Start(1));
        let done = set_mark(&out, 3, "A", Mark::Done).unwrap();
        let t = parse(&done);
        assert_eq!(next(&t, Mode::Auto, 1, live, met), Next::Resume(0));
        assert!(waited("hx", &t[0]).starts_with("The quest \"A\" is done"));
        // The same name twice is one line.
        assert_eq!(
            block_after(&out, "b-1", Some("needs A"), "a"),
            Some(out.clone())
        );
        // A file without a last ending keeps none, and CRLF stays CRLF.
        assert_eq!(
            block_after("- [ ] A\r\n- [/] B @b-1", "b-1", None, "A").unwrap(),
            "- [ ] A\r\n- [!] B @b-1 {after}\r\n  After: A"
        );
        assert_eq!(block_after(text, "nobody", None, "A"), None);
    }

    #[test]
    fn a_plain_block_with_after_lines_still_waits_on_the_human() {
        let t = parse("- [x] A\n- [!] B @b-1: why\n  After: A\n- [ ] C\n");
        assert_eq!(next(&t, Mode::Auto, 1, live, met), Next::Stuck(1));
    }

    #[test]
    fn the_runner_passes_a_quest_that_waits_on_something_it_can_check() {
        let t = parse("- [!] A @a-1: later {on file: b.txt}\n- [ ] B\n");
        assert_eq!(next(&t, Mode::Auto, 1, live, unmet), Next::Start(1));
        assert_eq!(next(&t, Mode::Auto, 1, live, met), Next::Resume(0));
        assert_eq!(
            next(&t, Mode::Auto, 1, |_| Holder::Gone, met),
            Next::Resume(0)
        );
        // A paused session waits for a click, and the list goes on.
        assert_eq!(
            next(&t, Mode::Auto, 1, |_| Holder::Paused, met),
            Next::Start(1)
        );
        // A wait that is not over is not the end of the list.
        let t = parse("- [!] A @a-1 {on file: b.txt}\n- [x] C\n");
        assert_eq!(next(&t, Mode::Auto, 1, live, unmet), Next::Wait);
        // A plain why still stops it.
        let t = parse("- [!] A @a-1 {on file: b.txt}\n- [!] C @c-1: why\n- [ ] D\n");
        assert_eq!(next(&t, Mode::Auto, 1, live, unmet), Next::Stuck(1));
        assert_eq!(next(&t, Mode::Manual, 1, live, met), Next::Off);
    }

    #[test]
    fn going_on_takes_a_free_place_like_a_start() {
        let t = parse("- [/] A @a-1\n- [!] B @b-1 {on file: x.txt}\n- [ ] C\n");
        assert_eq!(next(&t, Mode::Auto, 1, live, met), Next::Wait);
        assert_eq!(next(&t, Mode::Auto, 2, live, met), Next::Resume(1));
    }

    #[test]
    fn times_read_and_write_in_utc() {
        assert_eq!(utc(0), "1970-01-01T00:00Z");
        assert_eq!(utc(1_790_863_500), "2026-10-01T14:05Z");
        assert_eq!(utc(951_782_400), "2000-02-29T00:00Z");
        assert_eq!(utc(1_790_863_501), "2026-10-01T14:05:01Z");
        assert_eq!(parse_utc("2026-10-01T14:05Z"), Some(1_790_863_500));
        assert_eq!(parse_utc("2026-10-01 14:05:01z"), Some(1_790_863_501));
        assert_eq!(parse_utc("2000-02-29T00:00Z"), Some(951_782_400));
        assert_eq!(parse_utc("2026-10-01T14:05"), None);
        assert_eq!(parse_utc("2026-13-01T14:05Z"), None);
        assert_eq!(parse_utc("2026-10-01T14:05:01:02Z"), None);
        assert_eq!(parse_utc("1969-12-31T23:59Z"), None);
        for secs in [0, 59, 86_399, 1_000_000_000, 4_102_444_800] {
            assert_eq!(parse_utc(&utc(secs)), Some(secs));
        }
    }

    #[test]
    fn until_takes_a_span_from_now_a_unix_time_or_a_utc_time() {
        assert_eq!(parse_when("+30m", 1000), Some(2800));
        assert_eq!(parse_when("+2h", 0), Some(7200));
        assert_eq!(parse_when("+1d", 0), Some(86_400));
        assert_eq!(parse_when("+45s", 5), Some(50));
        assert_eq!(parse_when("+3w", 0), None);
        assert_eq!(parse_when("+m", 0), None);
        assert_eq!(parse_when("1790863500", 0), Some(1_790_863_500));
        assert_eq!(parse_when("2026-10-01T14:05Z", 0), Some(1_790_863_500));
        assert_eq!(parse_when("tomorrow", 0), None);
    }

    #[test]
    fn a_row_says_in_a_few_words_what_it_waits_on() {
        assert_eq!(Wait::Quest("Build".into()).label(0), "after Build");
        assert_eq!(
            Wait::Quest("Build the Runetome engine".into()).label(0),
            "after Build the Runetom\u{2026}"
        );
        assert_eq!(Wait::Until(4000).label(400), "in 1 h 00 min");
        assert_eq!(Wait::Until(4000).label(5000), "due");
        let t = &parse("- [!] A @a-1 {on quest: B}\n")[0];
        assert!(waited("hx", t).starts_with("The quest \"B\" is done"));
        let t = &parse("- [!] A @a-1 {on file: x}\n")[0];
        assert!(waited("hx", t).contains("`hx quest done \""));
    }

    #[test]
    fn the_system_prompt_offers_the_checkable_waits() {
        let p = system_prompt("hx", QUESTS_FILE, None);
        for flag in ["--on ", "--on-main", "--on-file", "--on-cmd", "--until"] {
            assert!(p.contains(flag), "{flag}");
        }
        assert!(!p.contains("  "));
    }

    #[test]
    fn the_runner_waits_for_the_item_in_hand() {
        let t = parse(SAMPLE);
        assert_eq!(next(&t, Mode::Manual, 1, live, unmet), Next::Off);
        assert_eq!(next(&t, Mode::Auto, 1, live, unmet), Next::Wait);
    }

    #[test]
    fn the_runner_stops_at_a_blocked_item_or_one_whose_session_is_gone() {
        let t = parse(SAMPLE);
        // The login fix's session is gone: that item is in the way.
        assert_eq!(
            next(&t, Mode::Auto, 1, |_| Holder::Gone, unmet),
            Next::Stuck(1)
        );
        let t = parse("- [x] A\n- [!] B @b-1: why\n- [ ] C\n");
        assert_eq!(next(&t, Mode::Review, 1, live, unmet), Next::Stuck(1));
    }

    #[test]
    fn the_runner_starts_the_first_open_item_then_finishes() {
        let t = parse("- [x] A @a-1\n- [ ]\n- [ ] B\n- [ ] C\n");
        assert_eq!(next(&t, Mode::Auto, 1, live, unmet), Next::Start(2));
        let t = parse("- [x] A\n");
        assert_eq!(next(&t, Mode::Auto, 1, live, unmet), Next::Finished);
        assert_eq!(next(&[], Mode::Review, 1, live, unmet), Next::Finished);
    }

    #[test]
    fn a_blocked_item_does_not_count_as_in_hand() {
        // Its session is still there, but it waits on the human: nothing
        // else starts past it either.
        let t = parse("- [!] A @a-1: why\n- [ ] B\n");
        assert_eq!(next(&t, Mode::Auto, 1, live, unmet), Next::Stuck(0));
    }

    #[test]
    fn several_items_run_at_once_up_to_the_config_s_number() {
        let t = parse(
            "- [/] A @a-1
- [ ] B
- [ ] C
",
        );
        assert_eq!(next(&t, Mode::Auto, 1, live, unmet), Next::Wait);
        assert_eq!(next(&t, Mode::Auto, 2, live, unmet), Next::Start(1));
        let t = parse(
            "- [/] A @a-1
- [?] B @b-1
- [ ] C
",
        );
        assert_eq!(next(&t, Mode::Review, 2, live, unmet), Next::Wait);
        assert_eq!(next(&t, Mode::Review, 3, live, unmet), Next::Start(2));
        // Everything started, nothing finished yet.
        assert_eq!(next(&t[..2], Mode::Auto, 3, live, unmet), Next::Wait);
    }

    #[test]
    fn with_several_at_once_a_blocked_item_still_stops_the_list() {
        let t = parse(
            "- [/] A @a-1
- [!] B @b-1: why
- [ ] C
",
        );
        assert_eq!(next(&t, Mode::Auto, 3, live, unmet), Next::Stuck(1));
        let t = parse(
            "- [/] A @a-1
- [/] B @b-1
- [ ] C
",
        );
        let gone = |id: &str| {
            if id == "b-1" {
                Holder::Gone
            } else {
                Holder::Live
            }
        };
        assert_eq!(next(&t, Mode::Auto, 3, gone, unmet), Next::Stuck(1));
    }

    #[test]
    fn a_paused_item_holds_the_runner_however_many_may_run() {
        let t = parse(
            "- [/] A @a-1
- [ ] B
",
        );
        assert_eq!(
            next(&t, Mode::Auto, 4, |_| Holder::Paused, unmet),
            Next::Wait
        );
    }

    #[test]
    fn parallel_is_one_unless_the_config_says_more_and_has_a_ceiling() {
        assert_eq!(parallel(""), 1);
        assert_eq!(parallel("{\"tasks\":{\"mode\":\"auto\"}}"), 1);
        assert_eq!(parallel("{\"tasks\":{\"parallel\":3}}"), 3);
        assert_eq!(parallel("{\"tasks\":{\"parallel\":0}}"), 1);
        assert_eq!(parallel("{\"tasks\":{\"parallel\":\"3\"}}"), 1);
        assert_eq!(parallel("{\"tasks\":{\"parallel\":500}}"), MOST_PARALLEL);
        // Setting the mode keeps it.
        let out = with_mode("{\"tasks\":{\"parallel\":3}}", Mode::Auto);
        assert_eq!(parallel(&out), 3);
    }

    #[test]
    fn parallel_is_written_beside_the_mode_and_one_takes_it_out() {
        let out = with_parallel("{\"tasks\":{\"mode\":\"auto\"},\"hosts\":[\"vps\"]}", 3);
        assert_eq!(parallel(&out), 3);
        assert_eq!(mode(&out), Mode::Auto);
        assert!(out.contains("vps"));
        let back = with_parallel(&out, 1);
        assert_eq!(parallel(&back), 1);
        assert!(!back.contains("parallel"));
        assert_eq!(mode(&back), Mode::Auto);
        assert_eq!(parallel(&with_parallel("not json", 50)), MOST_PARALLEL);
    }

    #[test]
    fn an_agent_in_a_worktree_is_told_where_the_list_is() {
        assert!(!system_prompt("hx", QUESTS_FILE, None).contains("main working tree"));
        assert!(
            system_prompt("hx", QUESTS_FILE, Some("C:/app/.horadric/quests.md"))
                .contains("at C:/app/.horadric/quests.md")
        );
    }

    #[test]
    fn a_list_from_before_the_rename_is_read_until_a_quest_log_is_beside_it() {
        assert_eq!(list_file(false, false), QUESTS_FILE);
        assert_eq!(list_file(false, true), OLD_FILE);
        assert_eq!(list_file(true, true), QUESTS_FILE);
        assert_eq!(list_file(true, false), QUESTS_FILE);
        assert!(system_prompt("hx", OLD_FILE, None).contains("quest log, .horadric/tasks.md."));
    }

    #[test]
    fn a_slug_is_short_lower_case_words() {
        assert_eq!(slug("Fix the login redirect!"), "fix-the-login-redirect");
        assert_eq!(slug("  ...  "), "task");
        assert!(slug(&"word ".repeat(30)).len() <= 40);
    }

    #[test]
    fn a_slug_spells_danish_letters_in_ascii() {
        assert_eq!(
            slug("Danske tegn: Kør på Æblerne"),
            "danske-tegn-koer-paa-aeblerne"
        );
        assert_eq!(slug("Crème brûlée für 日本"), "creme-brulee-fur");
        assert_eq!(slug("日本"), "task");
    }

    #[test]
    fn a_holder_with_danish_letters_still_parses() {
        let t = &parse(
            "- [/] Kør tests @kør-tests
",
        )[0];
        assert_eq!(t.title, "Kør tests");
        assert_eq!(t.holder.as_deref(), Some("kør-tests"));
    }

    #[test]
    fn the_first_prompt_is_the_item_its_notes_and_how_to_report() {
        let t = &parse(SAMPLE)[1];
        assert_eq!(
            prompt(t, "hx", QUESTS_FILE),
            "Fix the login redirect\n\nHappens only after a session expires.\nRepro in #12.\n\n\
             (A quest from .horadric/quests.md. When it is finished, run \
             `hx quest done \"<one short line on what you achieved>\"`.)"
        );
        let report = "`hx quest done \"<one short line on what you achieved>\"`";
        assert!(system_prompt("hx", QUESTS_FILE, None).contains(report));
        // Wrapped lines join with one space, not the source's indent.
        assert!(!system_prompt("hx", QUESTS_FILE, Some("C:/p/q.md")).contains("  "));
        assert!(nudge("hx").contains(report));
        assert!(go_on("hx").contains(report));
    }

    #[test]
    fn done_means_review_unless_the_list_runs_by_itself() {
        assert_eq!(Mode::Auto.finished(), Mark::Done);
        assert_eq!(Mode::Review.finished(), Mark::Review);
        assert_eq!(Mode::Manual.finished(), Mark::Review);
    }

    const LOG: &str = "# Quests\n\
        - [ ] First\n\
        \x20 a note\n\
        \n\
        \x20 after a blank\n\
        \n\
        ## Later\n\
        - [/] Second @second-1\n\
        - [ ] Third\n";

    #[test]
    fn an_edit_keeps_the_mark_and_holder_and_replaces_the_notes() {
        let out = edit(LOG, 7, "Second", "Second,  renamed", "one\n\n two ").unwrap();
        assert_eq!(
            out,
            "# Quests\n- [ ] First\n  a note\n\n  after a blank\n\n## Later\n\
             - [/] Second, renamed @second-1\n  one\n  two\n- [ ] Third\n"
        );
        let out = edit(LOG, 1, "First", "First", "").unwrap();
        assert_eq!(
            out,
            "# Quests\n- [ ] First\n\n## Later\n- [/] Second @second-1\n- [ ] Third\n"
        );
    }

    #[test]
    fn an_edit_refuses_a_moved_line_or_an_empty_title() {
        assert_eq!(edit(LOG, 7, "Other", "x", ""), None);
        assert_eq!(edit(LOG, 7, "Second", "   ", ""), None);
        assert_eq!(edit(LOG, 0, "# Quests", "x", ""), None);
    }

    #[test]
    fn an_edit_keeps_crlf_and_a_file_without_a_last_ending() {
        let text = "- [ ] One\r\n  n\r\n- [ ] Two";
        assert_eq!(
            edit(text, 0, "One", "Uno", "m").unwrap(),
            "- [ ] Uno\r\n  m\r\n- [ ] Two"
        );
        assert_eq!(
            edit(text, 2, "Two", "Dos", "x").unwrap(),
            "- [ ] One\r\n  n\r\n- [ ] Dos\r\n  x"
        );
    }

    #[test]
    fn removing_takes_the_item_and_its_notes() {
        assert_eq!(
            remove(LOG, 1, "First").unwrap(),
            "# Quests\n\n## Later\n- [/] Second @second-1\n- [ ] Third\n"
        );
        assert_eq!(
            remove(LOG, 8, "Third").unwrap(),
            "# Quests\n- [ ] First\n  a note\n\n  after a blank\n\n## Later\n- [/] Second @second-1\n"
        );
        assert_eq!(remove(LOG, 8, "Second"), None);
    }

    #[test]
    fn shifting_swaps_with_the_next_item_and_leaves_what_is_between() {
        assert_eq!(
            shift(LOG, 7, "Second", true).unwrap(),
            "# Quests\n- [/] Second @second-1\n\n## Later\n- [ ] First\n  a note\n\n  after a blank\n- [ ] Third\n"
        );
        assert_eq!(
            shift(LOG, 7, "Second", false).unwrap(),
            "# Quests\n- [ ] First\n  a note\n\n  after a blank\n\n## Later\n- [ ] Third\n- [/] Second @second-1\n"
        );
        assert_eq!(shift(LOG, 1, "First", true), None);
        assert_eq!(shift(LOG, 8, "Third", false), None);
        assert_eq!(shift(LOG, 8, "First", true), None);
    }

    #[test]
    fn shifting_the_last_line_of_a_file_without_an_ending() {
        let text = "- [ ] One\r\n- [ ] Two";
        assert_eq!(
            shift(text, 1, "Two", true).unwrap(),
            "- [ ] Two\r\n- [ ] One"
        );
        assert_eq!(
            shift(text, 0, "One", false).unwrap(),
            "- [ ] Two\r\n- [ ] One"
        );
    }
}
