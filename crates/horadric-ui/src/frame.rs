//! Turns the visible part of a terminal grid into things to draw.
//!
//! Pure, so the run building can be tested against alacritty's mock terminal
//! without a window. The renderer only turns cells into DIPs and issues the
//! Direct2D calls. Building happens with the screen locked, drawing after the
//! lock is released, so a frame is plain owned data.

use alacritty_terminal::event::EventListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::Point;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::Term;
use alacritty_terminal::vte::ansi::{CursorShape, NamedColor, Rgb};

use crate::palette;

/// Bold and italic, as bits. Indexes the renderer's font faces.
pub const BOLD: u8 = 1;
pub const ITALIC: u8 = 2;

/// A rectangle of cells with one background colour.
#[derive(Debug, Clone, PartialEq)]
pub struct Fill {
    pub row: usize,
    pub col: usize,
    pub cells: usize,
    pub color: Rgb,
}

/// Glyphs from the primary font, one per cell, in one colour and style.
#[derive(Debug, Clone, PartialEq)]
pub struct Run {
    pub row: usize,
    pub col: usize,
    pub style: u8,
    pub color: Rgb,
    pub glyphs: Vec<u16>,
}

/// Text the primary font can not draw in one cell: wide characters, emoji,
/// combining sequences, symbols missing from the font. Drawn one by one with
/// font fallback, placed at its cell so it can not push the rest of the row.
#[derive(Debug, Clone, PartialEq)]
pub struct Loose {
    pub row: usize,
    pub col: usize,
    pub cells: usize,
    pub style: u8,
    pub color: Rgb,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decoration {
    Underline,
    Strike,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Stroke {
    pub row: usize,
    pub col: usize,
    pub cells: usize,
    pub kind: Decoration,
    pub color: Rgb,
}

/// A cursor drawn on top of the text. A focused block cursor is not here: it
/// is folded into the cell colours instead, so the glyph under it inverts.
#[derive(Debug, Clone, PartialEq)]
pub struct Caret {
    pub row: usize,
    pub col: usize,
    pub cells: usize,
    pub shape: CursorShape,
    pub color: Rgb,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    pub background: Rgb,
    pub fills: Vec<Fill>,
    pub runs: Vec<Run>,
    pub loose: Vec<Loose>,
    pub strokes: Vec<Stroke>,
    pub caret: Option<Caret>,
}

/// Builds a frame. `glyph` maps a character and style to a glyph index in
/// the primary font, zero when the font lacks it. `lit` is false while a
/// blinking cursor is off, which only a focused pane's cursor ever is.
pub fn build<T: EventListener>(
    term: &Term<T>,
    focused: bool,
    lit: bool,
    mut glyph: impl FnMut(char, u8) -> u16,
) -> Frame {
    let content = term.renderable_content();
    let colors = content.colors;
    let background = palette::rgb_at(NamedColor::Background as usize, colors);
    let cursor_color = palette::rgb_at(NamedColor::Cursor as usize, colors);
    let offset = content.display_offset as i32;
    let rows = term.screen_lines();

    let cursor = content.cursor;
    let cursor_wide = term.grid()[cursor.point].flags.contains(Flags::WIDE_CHAR);
    let lit = lit || !focused;
    let block = lit && focused && cursor.shape == CursorShape::Block;
    let is_cursor = |p: Point| {
        p.line == cursor.point.line
            && (p.column == cursor.point.column
                || (cursor_wide && p.column.0 == cursor.point.column.0 + 1))
    };

    let mut frame = Frame {
        background,
        fills: Vec::new(),
        runs: Vec::new(),
        loose: Vec::new(),
        strokes: Vec::new(),
        caret: None,
    };
    let mut run: Option<Run> = None;
    // Spaces seen since the run's last glyph. They join the run only when
    // more ink follows, so rows padded with blanks cost nothing.
    let mut spaces = 0usize;

    for indexed in content.display_iter {
        let point = indexed.point;
        let cell = indexed.cell;
        let row = (point.line.0 + offset) as usize;
        let col = point.column.0;
        if row >= rows {
            continue;
        }

        let (mut fg, mut bg) = palette::cell_colors(cell.fg, cell.bg, cell.flags, colors);
        if content.selection.is_some_and(|s| s.contains(point)) {
            bg = palette::selection();
        }
        if block && is_cursor(point) {
            fg = bg;
            bg = cursor_color;
        }

        if bg != background {
            match frame.fills.last_mut() {
                Some(f) if f.row == row && f.col + f.cells == col && f.color == bg => f.cells += 1,
                _ => frame.fills.push(Fill {
                    row,
                    col,
                    cells: 1,
                    color: bg,
                }),
            }
        }

        let flags = cell.flags;
        if flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
            continue;
        }
        let cells = if flags.contains(Flags::WIDE_CHAR) {
            2
        } else {
            1
        };

        for (on, kind) in [
            (
                // A link a program made shows as one, as in Windows Terminal.
                flags.intersects(Flags::ALL_UNDERLINES) || cell.hyperlink().is_some(),
                Decoration::Underline,
            ),
            (flags.contains(Flags::STRIKEOUT), Decoration::Strike),
        ] {
            if !on {
                continue;
            }
            match frame.strokes.iter_mut().rev().find(|s| s.kind == kind) {
                Some(s) if s.row == row && s.col + s.cells == col && s.color == fg => {
                    s.cells += cells
                }
                _ => frame.strokes.push(Stroke {
                    row,
                    col,
                    cells,
                    kind,
                    color: fg,
                }),
            }
        }

        let style = (flags.contains(Flags::BOLD) as u8 * BOLD)
            | (flags.contains(Flags::ITALIC) as u8 * ITALIC);
        let c = cell.c;
        let blank = c == ' ' || c == '\t' || flags.contains(Flags::HIDDEN);
        let open_here = run
            .as_ref()
            .is_some_and(|r| r.row == row && r.col + r.glyphs.len() + spaces == col);

        if blank {
            if open_here {
                spaces += 1;
            } else {
                close(&mut run, &mut spaces, &mut frame);
            }
            continue;
        }

        let g = if cells == 2 || cell.zerowidth().is_some() {
            0
        } else {
            glyph(c, style)
        };
        if g == 0 {
            close(&mut run, &mut spaces, &mut frame);
            let mut text = String::from(c);
            if let Some(zw) = cell.zerowidth() {
                text.extend(zw);
            }
            frame.loose.push(Loose {
                row,
                col,
                cells,
                style,
                color: fg,
                text,
            });
            continue;
        }

        match run.as_mut() {
            Some(r) if open_here && r.style == style && r.color == fg => {
                let space = glyph(' ', style);
                r.glyphs.extend(std::iter::repeat_n(space, spaces));
                r.glyphs.push(g);
                spaces = 0;
            }
            _ => {
                close(&mut run, &mut spaces, &mut frame);
                run = Some(Run {
                    row,
                    col,
                    style,
                    color: fg,
                    glyphs: vec![g],
                });
            }
        }
    }
    close(&mut run, &mut spaces, &mut frame);

    let row = cursor.point.line.0 + offset;
    let visible = row >= 0 && (row as usize) < rows;
    if lit && visible && cursor.shape != CursorShape::Hidden && !block {
        let shape = if focused {
            cursor.shape
        } else {
            CursorShape::HollowBlock
        };
        frame.caret = Some(Caret {
            row: row as usize,
            col: cursor.point.column.0,
            cells: if cursor_wide { 2 } else { 1 },
            shape,
            color: cursor_color,
        });
    }
    frame
}

/// The screen row and column of the terminal's cursor, if the view shows
/// it. A hidden cursor still counts: agents hide it and park it where the
/// user types, which is where an input method's window belongs.
pub fn cursor_cell<T: EventListener>(term: &Term<T>) -> Option<(usize, usize)> {
    let content = term.renderable_content();
    let row = content.cursor.point.line.0 + content.display_offset as i32;
    (row >= 0 && (row as usize) < term.screen_lines())
        .then_some((row as usize, content.cursor.point.column.0))
}

fn close(run: &mut Option<Run>, spaces: &mut usize, frame: &mut Frame) {
    if let Some(r) = run.take() {
        frame.runs.push(r);
    }
    *spaces = 0;
}

#[cfg(test)]
mod tests {
    use super::*;
    use alacritty_terminal::term::test::mock_term;
    use alacritty_terminal::vte::ansi::{Color, NamedColor};

    /// Every printable ASCII character exists; anything else is missing.
    fn ascii(c: char, _style: u8) -> u16 {
        if c.is_ascii() {
            c as u16
        } else {
            0
        }
    }

    fn glyphs(s: &str) -> Vec<u16> {
        s.chars().map(|c| c as u16).collect()
    }

    #[test]
    fn cursor_cell_follows_the_cursor_even_when_hidden() {
        use alacritty_terminal::vte::ansi::{Handler, NamedPrivateMode};
        let mut term = mock_term("ab\r\ncde");
        assert_eq!(cursor_cell(&term), Some((0, 0)));
        term.goto(1, 2);
        assert_eq!(cursor_cell(&term), Some((1, 2)));
        term.unset_private_mode(NamedPrivateMode::ShowCursor.into());
        assert_eq!(cursor_cell(&term), Some((1, 2)));
    }

    #[test]
    fn inner_spaces_join_a_run_and_trailing_ones_do_not() {
        let term = mock_term("ab  c    \r\nxy");
        let f = build(&term, false, true, ascii);
        assert_eq!(f.runs.len(), 2);
        assert_eq!((f.runs[0].row, f.runs[0].col), (0, 0));
        assert_eq!(f.runs[0].glyphs, glyphs("ab  c"));
        assert_eq!((f.runs[1].row, f.runs[1].glyphs.clone()), (1, glyphs("xy")));
        assert!(f.fills.is_empty());
    }

    #[test]
    fn wide_and_missing_characters_are_loose() {
        let term = mock_term("a\u{4e2d}b\u{23fa}c");
        let f = build(&term, false, true, ascii);
        let loose: Vec<(usize, usize, &str)> = f
            .loose
            .iter()
            .map(|l| (l.col, l.cells, l.text.as_str()))
            .collect();
        assert_eq!(loose, vec![(1, 2, "\u{4e2d}"), (4, 1, "\u{23fa}")]);
        let runs: Vec<(usize, Vec<u16>)> =
            f.runs.iter().map(|r| (r.col, r.glyphs.clone())).collect();
        assert_eq!(
            runs,
            vec![(0, glyphs("a")), (3, glyphs("b")), (5, glyphs("c"))]
        );
    }

    #[test]
    fn colour_and_style_changes_split_runs_and_backgrounds_merge() {
        let mut term = mock_term("abcd");
        let red = Color::Named(NamedColor::Red);
        term.grid_mut()[Point::new(
            alacritty_terminal::index::Line(0),
            alacritty_terminal::index::Column(2),
        )]
        .fg = red;
        for col in 1..3 {
            term.grid_mut()[Point::new(
                alacritty_terminal::index::Line(0),
                alacritty_terminal::index::Column(col),
            )]
            .bg = Color::Named(NamedColor::Blue);
        }
        let f = build(&term, false, true, ascii);
        assert_eq!(f.runs.len(), 3);
        assert_eq!(f.runs[1].col, 2);
        assert_eq!(f.fills.len(), 1);
        assert_eq!((f.fills[0].col, f.fills[0].cells), (1, 2));
    }

    #[test]
    fn focused_block_cursor_inverts_its_cell() {
        let term = mock_term("ab");
        let f = build(&term, true, true, ascii);
        assert!(f.caret.is_none());
        assert_eq!(f.fills.len(), 1);
        assert_eq!(f.fills[0].col, 0);
        assert_eq!(f.fills[0].color, palette::cursor());
        assert_eq!(f.runs[0].color, palette::background());

        let unfocused = build(&term, false, true, ascii);
        let caret = unfocused.caret.unwrap();
        assert_eq!(caret.shape, CursorShape::HollowBlock);
        assert!(unfocused.fills.is_empty());
    }

    #[test]
    fn a_blinking_cursor_that_is_off_draws_nothing_unless_unfocused() {
        let term = mock_term("ab");
        let off = build(&term, true, false, ascii);
        assert!(off.caret.is_none());
        assert!(off.fills.is_empty());
        assert_eq!(
            off.runs[0].color,
            build(&term, false, true, ascii).runs[0].color
        );

        let unfocused = build(&term, false, false, ascii);
        assert_eq!(unfocused.caret.unwrap().shape, CursorShape::HollowBlock);
    }
}
