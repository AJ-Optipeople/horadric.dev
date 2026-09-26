//! A file shown read only in a pane, the way an editor shows it: line
//! numbers in a gutter, colours from [`crate::highlight`], long lines cut
//! at the edge and scrolled sideways rather than wrapped.
//!
//! The pane is a terminal grid with no program behind it. This module turns
//! the file into the escape sequences that paint that grid, and keeps the
//! map from grid rows back to lines, which scrolling across a resize and
//! copying without the line numbers both need.

/// Spaces a tab stands for. What VS Code shows by default.
const TAB: usize = 4;
/// A file longer than this shows its start and says so.
pub const MAX_LINES: usize = 10_000;
/// A file bigger than this is not read at all.
pub const MAX_BYTES: u64 = 4 * 1024 * 1024;
/// Enough room for text beside the gutter in a very narrow pane.
const MIN_TEXT: usize = 8;

/// VS Code's dark theme: its text and its line numbers.
pub const TEXT: Style = Style::plain([0xD4, 0xD4, 0xD4]);
const NUMBER: Style = Style::plain([0x6E, 0x76, 0x81]);
/// VS Code's gutter colours for lines added, changed and removed.
const ADDED: Style = Style::plain([0x2E, 0xA0, 0x43]);
const MODIFIED: Style = Style::plain([0x00, 0x78, 0xD4]);
const DELETED: Style = Style::plain([0xF8, 0x51, 0x49]);

/// How a line differs from the last commit, drawn in the gutter between
/// its number and its text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    Added,
    Modified,
    /// Lines were removed after this one.
    DeletedBelow,
    /// Lines were removed before this one, which is the first.
    DeletedAbove,
}

impl Mark {
    fn glyph(self) -> (char, Style) {
        match self {
            Mark::Added => ('\u{258e}', ADDED),
            Mark::Modified => ('\u{258e}', MODIFIED),
            Mark::DeletedBelow => ('\u{2581}', DELETED),
            Mark::DeletedAbove => ('\u{2594}', DELETED),
        }
    }
}

/// Each line's mark, from the hunk headers of `git diff -U0`. A line both
/// changed and followed by a removal shows the change.
pub fn marks(diff: &str, lines: usize) -> Vec<Option<Mark>> {
    let mut out = vec![None; lines];
    let mut set = |line: usize, mark: Mark, over: bool| {
        if let Some(slot) = out.get_mut(line) {
            if over || slot.is_none() {
                *slot = Some(mark);
            }
        }
    };
    for header in diff.lines().filter_map(|l| l.strip_prefix("@@ -")) {
        let mut parts = header.split(' ');
        let (Some(old), Some(new)) = (parts.next(), parts.next()) else {
            continue;
        };
        let (Some((_, removed)), Some((start, added))) =
            (range(old), new.strip_prefix('+').and_then(range))
        else {
            continue;
        };
        match (removed, added) {
            (_, 0) if start == 0 => set(0, Mark::DeletedAbove, false),
            (_, 0) => set(start - 1, Mark::DeletedBelow, false),
            (0, n) => (start..start + n).for_each(|l| set(l - 1, Mark::Added, true)),
            (_, n) => (start..start + n).for_each(|l| set(l - 1, Mark::Modified, true)),
        }
    }
    out
}

/// `12,3` as start and count, a count of one left out as git does.
fn range(s: &str) -> Option<(usize, usize)> {
    match s.split_once(',') {
        Some((a, n)) => Some((a.parse().ok()?, n.parse().ok()?)),
        None => Some((s.parse().ok()?, 1)),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Style {
    pub fg: [u8; 3],
    pub bold: bool,
    pub italic: bool,
}

impl Style {
    pub const fn plain(fg: [u8; 3]) -> Style {
        Style {
            fg,
            bold: false,
            italic: false,
        }
    }

    fn sgr(self, out: &mut Vec<u8>) {
        let [r, g, b] = self.fg;
        let mut s = format!("\x1b[0;38;2;{r};{g};{b}");
        if self.bold {
            s.push_str(";1");
        }
        if self.italic {
            s.push_str(";3");
        }
        s.push('m');
        out.extend_from_slice(s.as_bytes());
    }
}

/// A run of one style, `len` bytes long. A line's spans follow each other
/// from its start; text past the last one is [`TEXT`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub len: usize,
    pub style: Style,
}

/// One grid row: which line it shows, and which bytes of it are on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Row {
    pub line: usize,
    pub start: usize,
    pub end: usize,
    /// Empty cells before `start`, where a wide character was cut in two.
    pub skip: usize,
}

pub struct Rendered {
    /// What to feed the terminal.
    pub bytes: Vec<u8>,
    pub rows: Vec<Row>,
    /// Columns the line numbers take, spacing included.
    pub gutter: usize,
}

/// Whether the file is better not shown as text.
pub fn is_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(8000).any(|&b| b == 0)
}

/// The file's lines, ready to draw: tabs expanded, control characters made
/// visible so none can reach the terminal as a command, at most
/// [`MAX_LINES`]. Also the number of lines the file really has.
pub fn lines(text: &str) -> (Vec<String>, usize) {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut all: Vec<&str> = text.split('\n').collect();
    if all.last() == Some(&"") {
        all.pop();
    }
    let total = all.len();
    let shown = all
        .into_iter()
        .take(MAX_LINES)
        .map(|l| clean(l.strip_suffix('\r').unwrap_or(l)))
        .collect();
    (shown, total)
}

fn clean(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut col = 0;
    for c in line.chars() {
        match c {
            '\t' => {
                let n = TAB - col % TAB;
                out.extend(std::iter::repeat_n(' ', n));
                col += n;
            }
            // The Unicode pictures of the control characters.
            '\0'..='\x1f' => {
                out.push(char::from_u32(0x2400 + c as u32).unwrap_or('?'));
                col += 1;
            }
            '\x7f' => {
                out.push('\u{2421}');
                col += 1;
            }
            '\u{80}'..='\u{9f}' => {
                out.push('\u{fffd}');
                col += 1;
            }
            _ => {
                out.push(c);
                col += width(c);
            }
        }
    }
    out
}

/// Cells a character takes: two for the wide ranges of East Asian scripts
/// and emoji, otherwise one. Close enough for code; a miss only moves a
/// wrap by a cell.
pub fn width(c: char) -> usize {
    match c as u32 {
        0x1100..=0x115F
        | 0x2E80..=0x303E
        | 0x3041..=0x33FF
        | 0x3400..=0x4DBF
        | 0x4E00..=0x9FFF
        | 0xA000..=0xA4CF
        | 0xAC00..=0xD7A3
        | 0xF900..=0xFAFF
        | 0xFE30..=0xFE4F
        | 0xFF00..=0xFF60
        | 0xFFE0..=0xFFE6
        | 0x1F300..=0x1F64F
        | 0x1F900..=0x1F9FF
        | 0x20000..=0x3FFFD => 2,
        _ => 1,
    }
}

/// Columns for the line numbers of a file this long: the widest number,
/// never under three digits, a space before and two after.
pub fn gutter(lines: usize) -> usize {
    lines.max(1).to_string().len().max(3) + 3
}

/// Columns left for text beside the gutter in a grid `cols` wide.
pub fn text_cols(lines: usize, cols: usize) -> usize {
    cols.saturating_sub(gutter(lines)).max(MIN_TEXT)
}

/// Cells a piece of a line takes.
pub fn cells_of(text: &str) -> usize {
    text.chars().map(width).sum()
}

/// Cells the longest line takes, which bounds scrolling sideways.
pub fn widest(lines: &[String]) -> usize {
    lines.iter().map(|l| cells_of(l)).max().unwrap_or(0)
}

/// The first column to show so that cells `a..b` of a line are on screen,
/// moving from `left` only when they are not, and then putting them in the
/// middle. Never past the point where the longest line, `widest` cells,
/// ends at the right edge.
pub fn reveal(left: usize, avail: usize, widest: usize, a: usize, b: usize) -> usize {
    let left = if a >= left && b <= left + avail {
        left
    } else if b - a >= avail {
        a
    } else {
        a.saturating_sub((avail - (b - a)) / 2)
    };
    left.min(widest.saturating_sub(avail))
}

/// Lays the lines out for a grid `cols` wide, one row each, showing their
/// cells from `left` on. Long lines are cut at the edge, not wrapped: code
/// reads better with its indentation intact. `spans` may cover fewer lines
/// than there are, while highlighting is still running. `marks` may be
/// empty, or shorter than the lines.
pub fn render(
    lines: &[String],
    spans: &[Vec<Span>],
    marks: &[Option<Mark>],
    cols: usize,
    left: usize,
) -> Rendered {
    let gutter = gutter(lines.len());
    let avail = text_cols(lines.len(), cols);
    let digits = gutter - 3;
    // No wrapping by the terminal, and no cursor: this is not a prompt.
    let mut bytes = b"\x1b[?7l\x1b[?25l".to_vec();
    let mut rows = Vec::with_capacity(lines.len());
    for (n, line) in lines.iter().enumerate() {
        let mut styles = Styles::new(spans.get(n).map_or(&[][..], Vec::as_slice));
        let row = clip(line, n, left, avail);
        if n > 0 {
            bytes.extend_from_slice(b"\r\n");
        }
        NUMBER.sgr(&mut bytes);
        bytes.extend_from_slice(format!(" {:>digits$}", n + 1).as_bytes());
        match marks.get(n).copied().flatten() {
            Some(m) => {
                let (c, style) = m.glyph();
                style.sgr(&mut bytes);
                bytes.extend_from_slice(format!(" {c}").as_bytes());
            }
            None => bytes.extend_from_slice(b"  "),
        }
        // Half of a wide character cut by the left edge shows as a space.
        bytes.extend(std::iter::repeat_n(b' ', row.skip));
        let mut current = None;
        for (i, c) in line[row.start..row.end].char_indices() {
            let style = styles.at(row.start + i);
            if current != Some(style) {
                style.sgr(&mut bytes);
                current = Some(style);
            }
            let mut buf = [0u8; 4];
            bytes.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
        }
        rows.push(row);
    }
    Rendered {
        bytes,
        rows,
        gutter,
    }
}

/// The bytes of line `n` that fit in `avail` cells from cell `left`, and
/// the cells before the first of them that a wide character cut in two
/// leaves empty.
fn clip(line: &str, n: usize, left: usize, avail: usize) -> Row {
    let mut cells = 0;
    let mut start = None;
    let mut end = line.len();
    for (i, c) in line.char_indices() {
        let w = width(c);
        if start.is_none() && cells >= left {
            start = Some((i, cells - left));
        }
        if start.is_some() && cells + w > left + avail {
            end = i;
            break;
        }
        cells += w;
    }
    let (start, skip) = start.unwrap_or((line.len(), 0));
    Row {
        line: n,
        start,
        end: end.max(start),
        skip,
    }
}

/// Walks a line's spans in order, answering the style at a byte.
struct Styles<'a> {
    spans: &'a [Span],
    next: usize,
    /// Where the span at `next` ends.
    end: usize,
}

impl<'a> Styles<'a> {
    fn new(spans: &'a [Span]) -> Self {
        Styles {
            spans,
            next: 0,
            end: spans.first().map_or(0, |s| s.len),
        }
    }

    fn at(&mut self, byte: usize) -> Style {
        while byte >= self.end {
            self.next += 1;
            match self.spans.get(self.next) {
                Some(s) => self.end += s.len,
                None => return TEXT,
            }
        }
        self.spans[self.next].style
    }
}

/// The first row showing this line, or the last row when it is past them.
pub fn row_of(rows: &[Row], line: usize) -> usize {
    rows.partition_point(|r| r.line < line)
        .min(rows.len().saturating_sub(1))
}

/// A match of a search: bytes `start..end` of a line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hit {
    pub line: usize,
    pub start: usize,
    pub end: usize,
}

/// The next place `query` is in the file from byte `byte` of `line`:
/// forward, the first one starting there or later; backward, the last one
/// starting before it. Round the end of the file to its start and back.
/// Case is ignored unless the query has a capital, as in the terminal.
pub fn find(lines: &[String], query: &str, line: usize, byte: usize, forward: bool) -> Option<Hit> {
    if query.is_empty() || lines.is_empty() {
        return None;
    }
    let exact = query.chars().any(char::is_uppercase);
    let n = lines.len();
    let line = line.min(n - 1);
    // The starting line comes round again last, for its other side.
    for step in 0..=n {
        let l = if forward {
            (line + step) % n
        } else {
            (line + n - step % n) % n
        };
        let hits = hits_in(&lines[l], query, exact);
        let pick = match (step, forward) {
            (0, true) => hits.into_iter().find(|&(s, _)| s >= byte),
            (0, false) => hits.into_iter().rfind(|&(s, _)| s < byte),
            (s, true) if s == n => hits.into_iter().find(|&(s, _)| s < byte),
            (s, false) if s == n => hits.into_iter().rfind(|&(s, _)| s >= byte),
            (_, true) => hits.into_iter().next(),
            (_, false) => hits.into_iter().next_back(),
        };
        if let Some((start, end)) = pick {
            return Some(Hit {
                line: l,
                start,
                end,
            });
        }
    }
    None
}

/// Where `query` starts and ends in `text`, overlapping matches included.
fn hits_in(text: &str, query: &str, exact: bool) -> Vec<(usize, usize)> {
    text.char_indices()
        .filter_map(|(i, _)| {
            let rest = &text[i..];
            if exact {
                return rest.starts_with(query).then(|| (i, i + query.len()));
            }
            let mut have = rest.char_indices();
            for q in query.chars() {
                let (_, c) = have.next()?;
                if !c.to_lowercase().eq(q.to_lowercase()) {
                    return None;
                }
            }
            let end = have.next().map_or(text.len(), |(j, _)| i + j);
            Some((i, end))
        })
        .collect()
}

/// The first and last cell a match covers, for selecting it, with the
/// line shown from cell `left`. None when it starts off screen.
pub fn cells(
    lines: &[String],
    rows: &[Row],
    gutter: usize,
    left: usize,
    hit: Hit,
) -> Option<(Cell, Cell)> {
    let text = lines.get(hit.line)?;
    let row = rows.iter().position(|r| r.line == hit.line)?;
    let (a, b) = span(text, hit.start, hit.end);
    let col = |cell: usize| Some(gutter + cell.checked_sub(left)?);
    Some((
        Cell { row, col: col(a)? },
        Cell {
            row,
            col: col(b.max(a + 1) - 1)?,
        },
    ))
}

/// The cells bytes `start..end` of a line cover.
pub fn span(text: &str, start: usize, end: usize) -> (usize, usize) {
    let a = cells_of(&text[..start]);
    (a, a + cells_of(&text[start..end]))
}

/// A cell in the grid, as the selection names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    pub row: usize,
    pub col: usize,
}

/// The text between two cells, both included, as it is in the file: no line
/// numbers. A selection that starts in the gutter takes its first line from
/// the start, and one that ends past the text on screen takes its last line
/// to the end, off screen parts included.
pub fn copy(lines: &[String], rows: &[Row], gutter: usize, from: Cell, to: Cell) -> String {
    let last = rows.len().saturating_sub(1);
    let (Some(a), Some(b)) = (rows.get(from.row), rows.get(to.row.min(last))) else {
        return String::new();
    };
    let start = if from.col < gutter {
        0
    } else {
        byte_at(&lines[a.line], a, from.col - gutter, false)
    };
    let end = if to.row >= rows.len() {
        lines[b.line].len()
    } else if to.col < gutter {
        // Only numbers selected on the last row: none of its text.
        0
    } else {
        byte_at(&lines[b.line], b, to.col - gutter, true)
    };
    let mut out = String::new();
    for (n, line) in lines.iter().enumerate().take(b.line + 1).skip(a.line) {
        let s = if n == a.line { start } else { 0 };
        let e = if n == b.line { end } else { line.len() };
        if n > a.line {
            out.push('\n');
        }
        if s < e {
            out.push_str(&line[s..e]);
        }
    }
    out
}

/// The byte where the character covering a cell of the row starts, or with
/// `after`, where it ends. Ending on the last character on screen, or past
/// it, is the line's end, so a selection to the right edge takes what is
/// off screen too.
fn byte_at(line: &str, row: &Row, col: usize, after: bool) -> usize {
    let mut cells = row.skip;
    for (i, c) in line[row.start..row.end].char_indices() {
        cells += width(c);
        if cells > col {
            let at = row.start + i;
            if !after {
                return at;
            }
            let end = at + c.len_utf8();
            return if end == row.end { line.len() } else { end };
        }
    }
    line.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    /// What the grid would show, one string per row, escapes dropped.
    fn screen(r: &Rendered) -> Vec<String> {
        let text = String::from_utf8(r.bytes.clone()).unwrap();
        let mut plain = String::new();
        let mut chars = text.chars();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            } else if c != '\r' {
                plain.push(c);
            }
        }
        plain.split('\n').map(str::to_string).collect()
    }

    #[test]
    fn lines_drop_the_last_newline_and_carriage_returns() {
        assert_eq!(lines("a\r\nb\n"), (s(&["a", "b"]), 2));
        assert_eq!(lines("a\n\n"), (s(&["a", ""]), 2));
        assert_eq!(lines(""), (s(&[]), 0));
        assert_eq!(lines("\u{feff}x"), (s(&["x"]), 1));
    }

    #[test]
    fn tabs_line_up_and_controls_cannot_reach_the_terminal() {
        assert_eq!(lines("\tx\nab\tc").0, s(&["    x", "ab  c"]));
        let (l, _) = lines("a\x1b[2Jb\x7f");
        assert_eq!(l, s(&["a\u{241b}[2Jb\u{2421}"]));
    }

    #[test]
    fn a_long_file_shows_its_start_and_counts_the_rest() {
        let text = "x\n".repeat(MAX_LINES + 5);
        let (l, total) = lines(&text);
        assert_eq!((l.len(), total), (MAX_LINES, MAX_LINES + 5));
    }

    #[test]
    fn binary_is_a_nul_near_the_start() {
        assert!(is_binary(b"PNG\0\x01"));
        assert!(!is_binary("plain ö text".as_bytes()));
    }

    #[test]
    fn the_gutter_fits_the_widest_number() {
        assert_eq!(gutter(9), 6);
        assert_eq!(gutter(1000), 7);
    }

    fn row(line: usize, start: usize, end: usize, skip: usize) -> Row {
        Row {
            line,
            start,
            end,
            skip,
        }
    }

    #[test]
    fn numbers_sit_in_the_gutter_and_long_lines_are_cut() {
        let r = render(&s(&["abcdefghijkl", "", "xy"]), &[], &[], 6 + 8, 0);
        assert_eq!(screen(&r), s(&["   1  abcdefgh", "   2  ", "   3  xy"]));
        assert_eq!(
            r.rows,
            vec![row(0, 0, 8, 0), row(1, 0, 0, 0), row(2, 0, 2, 0)]
        );
    }

    #[test]
    fn scrolled_sideways_the_numbers_stay() {
        let r = render(&s(&["abcdefghijkl", "", "xy"]), &[], &[], 6 + 8, 5);
        assert_eq!(screen(&r), s(&["   1  fghijkl", "   2  ", "   3  "]));
        assert_eq!(r.rows[0], row(0, 5, 12, 0));
        assert_eq!(r.rows[2], row(2, 2, 2, 0));
    }

    #[test]
    fn a_wide_character_cut_by_an_edge_is_left_out() {
        let l = s(&["abcdefg\u{4e00}z"]);
        let r = render(&l, &[], &[], 6 + 8, 0);
        assert_eq!(screen(&r), s(&["   1  abcdefg"]));
        // Its right half at the left edge becomes a space.
        let r = render(&l, &[], &[], 6 + 8, 8);
        assert_eq!(screen(&r), s(&["   1   z"]));
        assert_eq!(r.rows[0], row(0, 10, 11, 1));
    }

    #[test]
    fn reveal_moves_only_to_show_what_is_off_screen() {
        // Already shown.
        assert_eq!(reveal(10, 20, 100, 12, 15), 10);
        // Off to the right, and to the left: put in the middle.
        assert_eq!(reveal(0, 20, 100, 40, 44), 32);
        assert_eq!(reveal(50, 20, 100, 40, 44), 32);
        // Near the start the middle would be before it.
        assert_eq!(reveal(50, 20, 100, 3, 5), 0);
        // Wider than the screen: its start at the edge.
        assert_eq!(reveal(0, 20, 100, 40, 70), 40);
        // Never past where the longest line ends at the right edge.
        assert_eq!(reveal(0, 20, 50, 46, 50), 30);
        assert_eq!(reveal(0, 20, 10, 0, 0), 0);
    }

    #[test]
    fn the_widest_line_counts_cells() {
        assert_eq!(widest(&s(&["ab", "\u{4e00}\u{4e00}x", ""])), 5);
        assert_eq!(widest(&[]), 0);
        assert_eq!(text_cols(9, 40), 34);
        assert_eq!(text_cols(9, 3), MIN_TEXT);
    }

    #[test]
    fn colours_change_only_where_the_style_does() {
        let red = Style::plain([255, 0, 0]);
        let spans = vec![vec![
            Span { len: 2, style: red },
            Span { len: 1, style: red },
            Span {
                len: 1,
                style: TEXT,
            },
        ]];
        let r = render(&s(&["fn x"]), &spans, &[], 40, 0);
        let text = String::from_utf8(r.bytes).unwrap();
        assert!(text.ends_with("\x1b[0;38;2;255;0;0mfn \x1b[0;38;2;212;212;212mx"));
    }

    #[test]
    fn text_past_the_spans_is_plain() {
        let bold = Style { bold: true, ..TEXT };
        let r = render(
            &s(&["ab"]),
            &[vec![Span {
                len: 1,
                style: bold,
            }]],
            &[],
            40,
            0,
        );
        let text = String::from_utf8(r.bytes).unwrap();
        assert!(text.ends_with("\x1b[0;38;2;212;212;212;1ma\x1b[0;38;2;212;212;212mb"));
    }

    #[test]
    fn row_of_finds_a_line_after_a_resize() {
        let r = render(&s(&["abcdefghijkl", "", "xy"]), &[], &[], 6 + 8, 0);
        assert_eq!(row_of(&r.rows, 0), 0);
        assert_eq!(row_of(&r.rows, 2), 2);
        assert_eq!(row_of(&r.rows, 99), 2);
    }

    #[test]
    fn copy_leaves_the_numbers_and_takes_what_is_off_screen_at_the_ends() {
        let l = s(&["abcdefghijkl", "", "xy"]);
        let r = render(&l, &[], &[], 6 + 8, 2);
        let g = r.gutter;
        let c = |row, col| Cell { row, col };
        // From the gutter of the first row to the end of the last.
        assert_eq!(
            copy(&l, &r.rows, g, c(0, 0), c(2, 20)),
            "abcdefghijkl\n\nxy"
        );
        // Inside the part on screen.
        assert_eq!(copy(&l, &r.rows, g, c(0, g + 1), c(0, g + 3)), "def");
        // To the right edge: the rest of the line too.
        assert_eq!(copy(&l, &r.rows, g, c(0, g + 6), c(0, g + 7)), "ijkl");
        // Only numbers on the last row.
        assert_eq!(copy(&l, &r.rows, g, c(0, g + 8), c(2, 1)), "\n\n");
        // Past the last row.
        assert_eq!(copy(&l, &r.rows, g, c(2, 0), c(9, 0)), "xy");
    }

    #[test]
    fn copy_takes_a_wide_character_whole() {
        let l = s(&["a\u{4e00}b"]);
        let r = render(&l, &[], &[], 40, 0);
        let g = r.gutter;
        let c = |col| Cell { row: 0, col };
        assert_eq!(copy(&l, &r.rows, g, c(g + 2), c(g + 2)), "\u{4e00}");
        assert_eq!(copy(&l, &r.rows, g, c(g + 1), c(g + 3)), "\u{4e00}b");
        // Cut by the left edge, its space copies nothing of it.
        let r = render(&l, &[], &[], 40, 2);
        assert_eq!(copy(&l, &r.rows, g, c(g), c(g + 1)), "b");
    }

    #[test]
    fn hunk_headers_mark_added_changed_and_removed_lines() {
        let diff = "diff --git a/x b/x\n--- a/x\n+++ b/x\n\
                    @@ -0,0 +1,2 @@\n+a\n+b\n\
                    @@ -5 +7 @@ fn x\n-c\n+d\n\
                    @@ -9,2 +10,0 @@\n-e\n-f\n\
                    @@ -20,3 +20,2 @@\n";
        let m = marks(diff, 22);
        assert_eq!(m[0..3], [Some(Mark::Added), Some(Mark::Added), None]);
        assert_eq!(m[6], Some(Mark::Modified));
        assert_eq!(m[9], Some(Mark::DeletedBelow));
        assert_eq!(m[19..21], [Some(Mark::Modified), Some(Mark::Modified)]);
        assert_eq!(m.iter().flatten().count(), 6);
    }

    #[test]
    fn a_removal_before_the_first_line_and_past_the_end_are_safe() {
        assert_eq!(marks("@@ -1,2 +0,0 @@", 3)[0], Some(Mark::DeletedAbove));
        assert!(marks("@@ -1 +50,4 @@", 3).iter().all(Option::is_none));
        // A change wins over a removal after the line.
        let m = marks("@@ -2 +2 @@\n@@ -4 +2,0 @@", 3);
        assert_eq!(m[1], Some(Mark::Modified));
    }

    #[test]
    fn marks_sit_between_the_number_and_the_text_however_far_it_scrolls() {
        let l = s(&["abcdefghijkl", "x"]);
        let m = [Some(Mark::Modified), Some(Mark::DeletedBelow)];
        let r = render(&l, &[], &m, 6 + 8, 0);
        assert_eq!(screen(&r), s(&["   1 \u{258e}abcdefgh", "   2 \u{2581}x"]));
        let r = render(&l, &[], &m, 6 + 8, 4);
        assert_eq!(screen(&r), s(&["   1 \u{258e}efghijkl", "   2 \u{2581}"]));
    }

    #[test]
    fn find_goes_on_from_where_it_is_and_wraps_round() {
        let l = s(&["a foo", "bar", "foo foo"]);
        let hit = |line, start| {
            Some(Hit {
                line,
                start,
                end: start + 3,
            })
        };
        assert_eq!(find(&l, "foo", 0, 0, true), hit(0, 2));
        assert_eq!(find(&l, "foo", 0, 3, true), hit(2, 0));
        assert_eq!(find(&l, "foo", 2, 1, true), hit(2, 4));
        assert_eq!(find(&l, "foo", 2, 5, true), hit(0, 2));
        assert_eq!(find(&l, "foo", 2, 4, false), hit(2, 0));
        assert_eq!(find(&l, "foo", 2, 0, false), hit(0, 2));
        assert_eq!(find(&l, "foo", 0, 2, false), hit(2, 4));
        // The only match, found again from just past itself.
        assert_eq!(find(&l, "bar", 1, 1, true), hit(1, 0));
        assert_eq!(find(&l, "baz", 0, 0, true), None);
        assert_eq!(find(&l, "", 0, 0, true), None);
    }

    #[test]
    fn find_ignores_case_unless_the_query_has_a_capital() {
        let l = s(&["Foo foo \u{d6}l"]);
        assert_eq!(find(&l, "foo", 0, 0, true).map(|h| h.start), Some(0));
        assert_eq!(find(&l, "Foo", 0, 1, true).map(|h| h.start), Some(0));
        let h = find(&l, "\u{f6}l", 0, 0, true).unwrap();
        assert_eq!(&l[0][h.start..h.end], "\u{d6}l");
    }

    #[test]
    fn a_match_maps_to_the_cells_it_covers_on_screen() {
        let l = s(&["abcdefghijkl", "a\u{4e00}b"]);
        let r = render(&l, &[], &[], 6 + 8, 0);
        let g = r.gutter;
        let c = |row, col| Cell { row, col };
        let hit = |line, start, end| Hit { line, start, end };
        assert_eq!(
            cells(&l, &r.rows, g, 0, hit(0, 6, 10)),
            Some((c(0, g + 6), c(0, g + 9)))
        );
        assert_eq!(
            cells(&l, &r.rows, g, 0, hit(1, 1, 4)),
            Some((c(1, g + 1), c(1, g + 2)))
        );
        // Scrolled, it moves left with the text; off screen it has none.
        assert_eq!(
            cells(&l, &r.rows, g, 4, hit(0, 6, 10)),
            Some((c(0, g + 2), c(0, g + 5)))
        );
        assert_eq!(cells(&l, &r.rows, g, 7, hit(0, 6, 10)), None);
    }
}
