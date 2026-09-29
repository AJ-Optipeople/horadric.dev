//! Finding what Ctrl and a click in a pane should open: a web address, a
//! file or a folder written in the text, or a link a program made with
//! OSC 8.
//!
//! Pure. Whether a path is on disk comes in through a function, so it is
//! tested without a disk; the pane does the opening.

use std::path::{Path, PathBuf};

use alacritty_terminal::event::EventListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::Term;

/// What a link opens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Web(String),
    /// A file, and the line to open it at when the text gave one.
    File(PathBuf, Option<u32>),
    Folder(PathBuf),
}

/// A link in a line of text: the characters it takes, end exclusive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub start: usize,
    pub end: usize,
    pub target: Target,
}

/// What is on disk at a path: None when nothing is, otherwise whether it
/// is a folder.
pub trait Probe: Fn(&Path) -> Option<bool> {}
impl<F: Fn(&Path) -> Option<bool>> Probe for F {}

/// Where to look for a relative path, and what `~` means.
pub struct Base<'a> {
    pub cwd: Option<&'a Path>,
    pub home: Option<&'a Path>,
}

/// Characters that end a word however it looks. Box drawing and the
/// symbols agents frame their output with never sit inside a path.
fn stops(c: char) -> bool {
    c.is_whitespace()
        || matches!(c, '"' | '\'' | '`' | '<' | '>' | '|')
        || ('\u{2500}'..='\u{259F}').contains(&c)
        || matches!(c, '⏺' | '⎿' | '•' | '→' | '←' | '│')
}

/// The link at character `at` of `text`, if there is one. Quoted text is
/// tried whole first, so a path with spaces in quotes or backticks works.
pub fn find(text: &[char], at: usize, base: &Base, probe: &impl Probe) -> Option<Found> {
    if at >= text.len() || text[at].is_whitespace() {
        return None;
    }
    if let Some((start, end)) = quoted(text, at) {
        let word: String = text[start..end].iter().collect();
        if let Some(target) = classify(&word, base, probe) {
            return Some(Found { start, end, target });
        }
    }
    let mut start = at;
    while start > 0 && !stops(text[start - 1]) {
        start -= 1;
    }
    let mut end = at;
    while end < text.len() && !stops(text[end]) {
        end += 1;
    }
    let (start, end) = trim(text, start, end);
    if at < start || at >= end {
        return None;
    }
    let word: String = text[start..end].iter().collect();
    // A web address may follow something glued to it, `url=` or a
    // markdown label: it starts at its scheme.
    if let Some(i) = scheme_at(&word) {
        let from = start + word[..i].chars().count();
        if at >= from {
            let (from, end) = trim(text, from, end);
            let url: String = text[from..end].iter().collect();
            return web(&url).map(|target| Found {
                start: from,
                end,
                target,
            });
        }
    }
    classify(&word, base, probe).map(|target| Found { start, end, target })
}

/// The quotes round `at` on the line, as the span between them.
fn quoted(text: &[char], at: usize) -> Option<(usize, usize)> {
    for q in ['"', '\'', '`'] {
        let before = text[..at].iter().rposition(|&c| c == q);
        let after = text[at..].iter().position(|&c| c == q).map(|i| at + i);
        if let (Some(b), Some(a)) = (before, after) {
            // An odd count before means `at` is inside a pair, not between.
            let count = text[..b].iter().filter(|&&c| c == q).count();
            if count % 2 == 0 && a > b + 1 {
                return Some((b + 1, a));
            }
        }
    }
    None
}

/// Drops punctuation that ends a sentence or wraps a word, keeping a
/// closing bracket its word opened, as in a Wikipedia address.
fn trim(text: &[char], mut start: usize, mut end: usize) -> (usize, usize) {
    while start < end && matches!(text[start], '(' | '[' | '{') {
        start += 1;
    }
    loop {
        if start >= end {
            break;
        }
        let last = text[end - 1];
        let opened = |open: char| {
            let w = &text[start..end];
            w.iter().filter(|&&c| c == open).count() >= w.iter().filter(|&&c| c == last).count()
        };
        let drop = match last {
            '.' | ',' | ';' | ':' | '!' => true,
            ')' => !opened('('),
            ']' => !opened('['),
            '}' => !opened('{'),
            _ => false,
        };
        if !drop {
            break;
        }
        end -= 1;
    }
    (start, end)
}

/// The byte where a web address starts in a word.
fn scheme_at(word: &str) -> Option<usize> {
    let lower = word.to_ascii_lowercase();
    ["https://", "http://", "mailto:"]
        .iter()
        .filter_map(|s| lower.find(s))
        .min()
}

fn web(url: &str) -> Option<Target> {
    let lower = url.to_ascii_lowercase();
    let rest = ["https://", "http://", "mailto:"]
        .iter()
        .find_map(|s| lower.strip_prefix(s))?;
    (!rest.is_empty() && !url.contains(char::is_whitespace)).then(|| Target::Web(url.to_string()))
}

/// A word as a web address or a path that exists.
fn classify(word: &str, base: &Base, probe: &impl Probe) -> Option<Target> {
    if let Some(t) = web(word) {
        return Some(t);
    }
    if word.to_ascii_lowercase().starts_with("www.") && word.len() > 4 {
        return Some(Target::Web(format!("https://{word}")));
    }
    if let Some(path) = file_uri(word) {
        return path_target(&path, None, base, probe);
    }
    let (path, line) = split_line(word);
    path_target(path, line, base, probe).or_else(|| path_target(word, None, base, probe))
}

/// `src/main.rs:12:5` or `src/main.rs(12,5)` as the path and the line.
pub fn split_line(word: &str) -> (&str, Option<u32>) {
    if let Some(open) = word.strip_suffix(')').and_then(|w| w.rfind('(')) {
        let inside = &word[open + 1..word.len() - 1];
        let line = inside.split(',').next().and_then(|n| n.parse().ok());
        if line.is_some() && open > 0 {
            return (&word[..open], line);
        }
    }
    let mut path = word;
    let mut numbers = Vec::new();
    // At most line and column. A drive's colon is followed by a slash.
    for _ in 0..2 {
        match path.rsplit_once(':') {
            Some((p, n)) if !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) => {
                numbers.push(n);
                path = p;
            }
            _ => break,
        }
    }
    let line = numbers.last().and_then(|n| n.parse().ok());
    if path.is_empty() {
        (word, None)
    } else {
        (path, line)
    }
}

fn path_target(raw: &str, line: Option<u32>, base: &Base, probe: &impl Probe) -> Option<Target> {
    let path = resolve(raw, base)?;
    match probe(&path)? {
        true => Some(Target::Folder(path)),
        false => Some(Target::File(path, line)),
    }
}

/// A path as written, made absolute. None when it is relative and there is
/// nowhere to look.
pub fn resolve(raw: &str, base: &Base) -> Option<PathBuf> {
    if raw.is_empty() {
        return None;
    }
    let native = raw.replace('/', "\\");
    // `src/` names the folder `src`. A drive's root keeps its slash.
    let native = match native.trim_end_matches('\\') {
        t if t.len() < native.len() && !t.is_empty() && !t.ends_with(':') => t.to_string(),
        _ => native,
    };
    if let Some(rest) = native.strip_prefix("~\\").or((native == "~").then_some("")) {
        return Some(base.home?.join(rest));
    }
    let path = Path::new(&native);
    // `\foo` is relative to the drive, which is not what a program printing
    // it means. Only drive paths and UNC paths count as absolute.
    if path.is_absolute() {
        return Some(path.to_path_buf());
    }
    if native.starts_with('\\') {
        return None;
    }
    Some(base.cwd?.join(path))
}

/// The path of a `file:` URI, percent escapes decoded.
pub fn file_uri(uri: &str) -> Option<String> {
    let lower = uri.to_ascii_lowercase();
    if !lower.starts_with("file:") {
        return None;
    }
    let rest = &uri[5..];
    let rest = rest.strip_prefix("//").map_or(rest, |r| {
        // The host: empty or localhost means this machine, anything else is
        // a share.
        let slash = r.find('/').unwrap_or(r.len());
        let host = &r[..slash];
        if host.is_empty() || host.eq_ignore_ascii_case("localhost") {
            &r[slash..]
        } else {
            r
        }
    });
    let decoded = percent_decode(rest)?;
    let has_drive = |s: &str| {
        let b = s.as_bytes();
        b.len() >= 3 && b[0] == b'/' && b[1].is_ascii_alphabetic() && b[2] == b':'
    };
    Some(if has_drive(&decoded) {
        decoded[1..].to_string()
    } else if uri[5..].starts_with("//") && !decoded.starts_with('/') {
        format!("//{decoded}")
    } else {
        decoded
    })
}

fn percent_decode(s: &str) -> Option<String> {
    let mut out = Vec::with_capacity(s.len());
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hex = std::str::from_utf8(&b[i + 1..i + 3]).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// What an OSC 8 link opens. Its text may say anything, so it opens only
/// web pages and files that are not programs: running something the text
/// hid is not what a click on it should do.
pub fn from_uri(uri: &str, probe: &impl Probe) -> Option<Target> {
    if let Some(t) = web(uri) {
        return Some(t);
    }
    let path = file_uri(uri)?;
    let base = Base {
        cwd: None,
        home: None,
    };
    let target = path_target(&path, None, &base, probe)?;
    match &target {
        Target::File(p, _) if is_program(p) => None,
        _ => Some(target),
    }
}

/// Files Windows runs rather than shows when opened.
pub fn is_program(path: &Path) -> bool {
    const RUN: [&str; 12] = [
        "exe",
        "com",
        "bat",
        "cmd",
        "msi",
        "lnk",
        "ps1",
        "vbs",
        "js",
        "wsf",
        "scr",
        "appref-ms",
    ];
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| RUN.iter().any(|r| r.eq_ignore_ascii_case(e)))
}

/// The line a point is on, joined across the rows it wraps over: its
/// characters, the cell each came from, and which of them `point` is.
pub fn line_at<T: EventListener>(
    term: &Term<T>,
    point: Point,
) -> Option<(Vec<char>, Vec<Point>, usize)> {
    let grid = term.grid();
    let top = -(grid.history_size() as i32);
    let bottom = term.screen_lines() as i32 - 1;
    let last = term.last_column();
    let wraps = |line: i32| grid[Line(line)][last].flags.contains(Flags::WRAPLINE);
    let mut first = point.line.0;
    while first > top && wraps(first - 1) {
        first -= 1;
    }
    let mut end = point.line.0;
    while end < bottom && wraps(end) {
        end += 1;
    }
    let mut chars = Vec::new();
    let mut points = Vec::new();
    let mut at = None;
    for line in first..=end {
        for col in 0..=last.0 {
            let p = Point::new(Line(line), Column(col));
            let cell = &grid[p];
            if cell
                .flags
                .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
            {
                // The second half of a wide character points at its first.
                if p == point {
                    at = chars.len().checked_sub(1);
                }
                continue;
            }
            if p == point {
                at = Some(chars.len());
            }
            chars.push(cell.c);
            points.push(p);
        }
    }
    Some((chars, points, at?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alacritty_terminal::term::test::mock_term;

    fn chars(s: &str) -> Vec<char> {
        s.chars().collect()
    }

    /// A disk with `C:\repo\src\main.rs`, `C:\repo\src` and `C:\Users\me\notes.txt`.
    fn disk(p: &Path) -> Option<bool> {
        match p.to_str()? {
            r"C:\repo\src\main.rs" | r"C:\Users\me\notes.txt" | r"C:\Program Files\x\a.txt" => {
                Some(false)
            }
            r"C:\repo\src" | r"C:\repo" => Some(true),
            r"C:\tools\run.exe" => Some(false),
            _ => None,
        }
    }

    fn base() -> Base<'static> {
        Base {
            cwd: Some(Path::new(r"C:\repo")),
            home: Some(Path::new(r"C:\Users\me")),
        }
    }

    /// The link under the first `^` of `mark`, as the text it covers and
    /// its target.
    fn at(text: &str, mark: &str) -> Option<(String, Target)> {
        let t = chars(text);
        let i = mark.find('^').unwrap();
        let f = find(&t, i, &base(), &disk)?;
        Some((t[f.start..f.end].iter().collect(), f.target))
    }

    fn web_at(text: &str, mark: &str) -> Option<String> {
        match at(text, mark)? {
            (s, Target::Web(u)) => {
                assert_eq!(s, u);
                Some(u)
            }
            _ => None,
        }
    }

    #[test]
    fn web_addresses_lose_the_punctuation_round_them() {
        assert_eq!(
            web_at("see https://example.com/a.", "        ^").as_deref(),
            Some("https://example.com/a")
        );
        assert_eq!(
            web_at("(https://example.com/x)", "  ^").as_deref(),
            Some("https://example.com/x")
        );
        assert_eq!(
            web_at("https://en.wikipedia.org/wiki/Rust_(language), ok", "^").as_deref(),
            Some("https://en.wikipedia.org/wiki/Rust_(language)")
        );
        assert_eq!(
            web_at("[docs](http://x.io/p?q=1&r=2)", "         ^").as_deref(),
            Some("http://x.io/p?q=1&r=2")
        );
        assert_eq!(
            web_at("\"https://x.io/a b\"", "  ^").as_deref(),
            Some("https://x.io/a"),
            "a quoted address with a space ends at the space"
        );
    }

    #[test]
    fn a_label_glued_to_an_address_is_not_part_of_it() {
        assert_eq!(at("[docs](http://x.io)", " ^"), None);
        assert_eq!(
            web_at("url=https://x.io", "     ^").as_deref(),
            Some("https://x.io")
        );
    }

    #[test]
    fn www_gets_a_scheme() {
        assert_eq!(
            at("go to www.rust-lang.org.", "      ^"),
            Some((
                "www.rust-lang.org".into(),
                Target::Web("https://www.rust-lang.org".into())
            ))
        );
    }

    #[test]
    fn relative_paths_resolve_against_the_session_folder() {
        assert_eq!(
            at("edited src/main.rs:12:5 and", "         ^"),
            Some((
                "src/main.rs:12:5".into(),
                Target::File(r"C:\repo\src\main.rs".into(), Some(12))
            ))
        );
        assert_eq!(
            at("in src\\main.rs(40,2).", "   ^"),
            Some((
                r"src\main.rs(40,2)".into(),
                Target::File(r"C:\repo\src\main.rs".into(), Some(40))
            ))
        );
        assert_eq!(
            at("⎿ src/", "  ^"),
            Some(("src/".into(), Target::Folder(r"C:\repo\src".into())))
        );
    }

    #[test]
    fn absolute_home_and_quoted_paths() {
        assert_eq!(
            at("C:/repo/src/main.rs", "^"),
            Some((
                "C:/repo/src/main.rs".into(),
                Target::File(r"C:\repo\src\main.rs".into(), None)
            ))
        );
        assert_eq!(
            at("~/notes.txt", "   ^").map(|a| a.1),
            Some(Target::File(r"C:\Users\me\notes.txt".into(), None))
        );
        assert_eq!(
            at(
                r#"open "C:\Program Files\x\a.txt" now"#,
                "                  ^"
            )
            .map(|a| a.1),
            Some(Target::File(r"C:\Program Files\x\a.txt".into(), None))
        );
        assert_eq!(
            at(r"run C:\tools\run.exe", "      ^").map(|a| a.1),
            Some(Target::File(r"C:\tools\run.exe".into(), None))
        );
    }

    #[test]
    fn words_that_are_nothing_on_disk_are_not_links() {
        assert_eq!(at("the missing.rs file", "    ^"), None);
        assert_eq!(at("a  b", " ^"), None);
        assert_eq!(at(r"\src\main.rs", " ^"), None);
    }

    #[test]
    fn line_numbers_come_off_a_path() {
        assert_eq!(split_line("a.rs:3"), ("a.rs", Some(3)));
        assert_eq!(split_line("a.rs:3:9"), ("a.rs", Some(3)));
        assert_eq!(split_line("a.rs(7)"), ("a.rs", Some(7)));
        assert_eq!(split_line("C:\\a.rs"), ("C:\\a.rs", None));
        assert_eq!(split_line("a.rs:"), ("a.rs:", None));
        assert_eq!(split_line(":12"), (":12", None));
    }

    #[test]
    fn file_uris_become_paths() {
        assert_eq!(
            file_uri("file:///C:/Program%20Files/x").as_deref(),
            Some("C:/Program Files/x")
        );
        assert_eq!(file_uri("file://localhost/C:/a").as_deref(), Some("C:/a"));
        assert_eq!(
            file_uri("file://server/share/a").as_deref(),
            Some("//server/share/a")
        );
        assert_eq!(file_uri("https://x"), None);
        assert_eq!(file_uri("file:///C:/%zz"), None);
    }

    #[test]
    fn osc_8_links_never_run_a_program() {
        assert_eq!(
            from_uri("https://x.io", &disk),
            Some(Target::Web("https://x.io".into()))
        );
        assert_eq!(
            from_uri("file:///C:/repo/src/main.rs", &disk),
            Some(Target::File(r"C:\repo\src\main.rs".into(), None))
        );
        assert_eq!(from_uri("file:///C:/tools/run.exe", &disk), None);
        assert_eq!(from_uri("javascript:alert(1)", &disk), None);
        assert_eq!(from_uri("ms-settings:privacy", &disk), None);
    }

    #[test]
    fn programs_are_known_by_extension() {
        assert!(is_program(Path::new(r"C:\a\b.EXE")));
        assert!(is_program(Path::new("x.cmd")));
        assert!(!is_program(Path::new("x.rs")));
        assert!(!is_program(Path::new("exe")));
    }

    #[test]
    fn a_wrapped_line_is_read_whole() {
        let mut term = mock_term("abcd\r\nefgh");
        let last = term.last_column();
        term.grid_mut()[Line(0)][last].flags.insert(Flags::WRAPLINE);
        let (text, points, at) = line_at(&term, Point::new(Line(1), Column(1))).unwrap();
        assert_eq!(text.iter().collect::<String>(), "abcdefgh");
        assert_eq!(at, 5);
        assert_eq!(points[5], Point::new(Line(1), Column(1)));
        let (text, _, at) = line_at(&term, Point::new(Line(0), Column(2))).unwrap();
        assert_eq!((text.len(), at), (8, 2));
    }
}
