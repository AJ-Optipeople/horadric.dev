//! Where things go inside a cluster window, in device independent pixels.
//!
//! Pure functions so the geometry can be tested without a window. The
//! renderer scales by DPI, this module never sees a physical pixel.

use horadric_core::cube;
use horadric_core::saved::Side;

/// A rectangle in DIPs.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Rect { x, y, w, h }
    }

    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && py >= self.y && px < self.x + self.w && py < self.y + self.h
    }

    pub fn right(&self) -> f32 {
        self.x + self.w
    }

    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }

    /// Shrinks on every side.
    pub fn inset(&self, d: f32) -> Rect {
        Rect::new(self.x + d, self.y + d, self.w - 2.0 * d, self.h - 2.0 * d)
    }
}

/// Fixed sizes for a cluster. One place to tune the look.
#[derive(Debug, Clone, Copy)]
pub struct Metrics {
    pub width: f32,
    pub pad: f32,
    pub header_h: f32,
    pub tile_h: f32,
    pub add_h: f32,
    /// The button beside the bottom plus that opens a plain terminal.
    pub shell_w: f32,
    pub gap: f32,
    pub radius: f32,
    /// The corner Windows 11 rounds a window to.
    pub window_radius: f32,
    pub tile_radius: f32,
    pub files_header_h: f32,
    pub file_row_h: f32,
    /// The fewest rows a files tile shows before its column folds it.
    pub file_rows_min: usize,
    /// Room under the last row.
    pub file_foot: f32,
    /// How far each folder level is indented.
    pub file_indent: f32,
    /// A row of the tasks tile.
    pub task_row_h: f32,
    /// The most rows the tasks tile shows before it scrolls.
    pub task_rows: usize,
    /// Room under the last task, off the rounded corner.
    pub task_foot: f32,
    /// The button in the tasks tile's header that shows the mode.
    pub mode_w: f32,
    /// A rune stone in the Runetome, its label under it, and how many
    /// stand in a row.
    pub stone: f32,
    pub stone_label_h: f32,
    pub stones_per_row: usize,
    /// The button on a tile whose session has a browser open.
    pub mark_w: f32,
    pub mark_h: f32,
    /// A usage limit in the usage window: its name and numbers, and a bar.
    pub limit_row_h: f32,
    /// A setting in the usage window, and how far in from its section's
    /// edge its name sits.
    pub setting_row_h: f32,
    pub setting_pad: f32,
    /// A setting's list: its edge, the note on top, a value, and the gap
    /// that keeps a risky value apart.
    pub menu_pad: f32,
    pub menu_note_h: f32,
    pub menu_row_h: f32,
    pub menu_apart: f32,
}

impl Default for Metrics {
    fn default() -> Self {
        Metrics {
            // Hardware needs room: a key casts its shadow into the gap
            // below it, and controls crowded together read as cheap.
            width: 304.0,
            pad: 18.0,
            header_h: 36.0,
            tile_h: 64.0,
            add_h: 32.0,
            shell_w: 52.0,
            gap: 12.0,
            radius: 12.0,
            window_radius: 8.0,
            tile_radius: 12.0,
            files_header_h: 30.0,
            file_row_h: 22.0,
            file_rows_min: 3,
            file_foot: 10.0,
            file_indent: 12.0,
            task_row_h: 26.0,
            task_rows: 8,
            task_foot: 6.0,
            mode_w: 72.0,
            stone: 44.0,
            stone_label_h: 18.0,
            stones_per_row: 4,
            mark_w: 24.0,
            mark_h: 20.0,
            limit_row_h: 44.0,
            setting_row_h: 32.0,
            setting_pad: 14.0,
            menu_pad: 8.0,
            menu_note_h: 28.0,
            menu_row_h: 30.0,
            menu_apart: 9.0,
        }
    }
}

/// The computed geometry of one cluster window.
#[derive(Debug, Clone, PartialEq)]
pub struct ClusterLayout {
    /// Full window size.
    pub size: (f32, f32),
    pub header: Rect,
    /// The button at the right end of the header that picks a folder for a
    /// new project. Inside `header`, so it is hit tested first.
    pub new: Rect,
    /// One rect per tile, in the order given. Empty when collapsed.
    pub tiles: Vec<Rect>,
    /// The browser button on each tile, in the same order, where its
    /// session has a browser open. See [`mark`].
    pub marks: Vec<Option<Rect>>,
    /// The button on each tile whose session has a worktree of its own,
    /// which opens it in VS Code. Left of the browser button when both.
    pub codes: Vec<Option<Rect>>,
    /// The wide button below the last tile that starts another session in
    /// this project at once. None when collapsed.
    pub add: Option<Rect>,
    /// The small button at its right that opens a plain terminal in the
    /// project. None when collapsed.
    pub shell: Option<Rect>,
    /// The tasks tile, between the plus and the files. None when the
    /// cluster is collapsed or its project has no folder.
    pub tasks: Option<TasksLayout>,
    /// The Runetome, below the tasks tile. None where the tasks tile is.
    pub tome: Option<TomeLayout>,
    /// The files tile, below everything else. None when the project is not
    /// in git or the cluster is collapsed.
    pub files: Option<FilesLayout>,
}

/// Where the tasks tile, its buttons and its rows go.
#[derive(Debug, Clone, PartialEq)]
pub struct TasksLayout {
    /// The whole tile.
    pub rect: Rect,
    /// Its title row, which folds it.
    pub header: Rect,
    /// The button in the header that picks the mode. Inside `header`.
    pub mode: Rect,
    /// The plus at the header's right end, which adds an item.
    pub add: Rect,
    /// The quest giver left of the plus, which asks an agent for quests.
    pub give: Rect,
    /// One rect per visible row, top to bottom.
    pub rows: Vec<Rect>,
    /// The approve button at the right end of each row waiting for review.
    pub approve: Vec<Option<Rect>>,
}

impl TasksLayout {
    /// Where the rows are, header excluded: the part the wheel scrolls.
    pub fn body(&self) -> Rect {
        Rect::new(
            self.rect.x,
            self.header.bottom(),
            self.rect.w,
            self.rect.bottom() - self.header.bottom(),
        )
    }
}

/// Where the Runetome's header and its stones go.
#[derive(Debug, Clone, PartialEq)]
pub struct TomeLayout {
    pub rect: Rect,
    /// Its title row, which folds it.
    pub header: Rect,
    /// Each stone's slab, in the tome's order, the empty stone last.
    pub stones: Vec<Rect>,
    /// The label under each.
    pub labels: Vec<Rect>,
}

/// Where the files tile and its rows go.
#[derive(Debug, Clone, PartialEq)]
pub struct FilesLayout {
    /// The whole tile.
    pub rect: Rect,
    /// Its title row, which collapses and expands it.
    pub header: Rect,
    /// One rect per visible row, top to bottom.
    pub rows: Vec<Rect>,
}

impl FilesLayout {
    /// Where the rows are, header excluded: the part the wheel scrolls.
    pub fn body(&self) -> Rect {
        Rect::new(
            self.rect.x,
            self.header.bottom(),
            self.rect.w,
            self.rect.bottom() - self.header.bottom(),
        )
    }
}

/// Lays out a cluster with `n` tiles. `tasks` is one entry per task row
/// shown, true where the row has an approve button, none for no tasks
/// tile; empty is its header alone. `tome` is how many stones the
/// Runetome shows, none for no tome and zero for its header alone. `files` is how tall the files tile is
/// below its header, in DIPs, none for no files tile. Zero is the tile
/// folded to its header. It holds as many whole rows as fit and the rest is
/// room under the last one. Sizing it is the column's job, see
/// [`crate::columns::fill`].
pub fn cluster(
    m: &Metrics,
    n: usize,
    collapsed: bool,
    tasks: Option<&[bool]>,
    tome: Option<usize>,
    files: Option<f32>,
) -> ClusterLayout {
    let header = Rect::new(m.pad, m.pad, m.width - 2.0 * m.pad, m.header_h);
    let new = Rect::new(
        header.right() - m.header_h,
        header.y,
        m.header_h,
        m.header_h,
    );
    let full = m.width - 2.0 * m.pad;
    let mut tiles = Vec::new();
    let mut add = None;
    let mut shell = None;
    let mut tasks_layout = None;
    let mut tome_layout = None;
    let mut files_layout = None;
    let mut y = header.bottom() + m.gap;
    if !collapsed {
        for _ in 0..n {
            tiles.push(Rect::new(m.pad, y, full, m.tile_h));
            y += m.tile_h + m.gap;
        }
        let wide = full - m.shell_w - m.gap;
        add = Some(Rect::new(m.pad, y, wide, m.add_h));
        shell = Some(Rect::new(m.pad + wide + m.gap, y, m.shell_w, m.add_h));
        y += m.add_h + m.gap;
        if let Some(approve) = tasks {
            let l = tasks_tile(m, y, approve);
            y = l.rect.bottom() + m.gap;
            tasks_layout = Some(l);
        }
        if let Some(stones) = tome {
            let l = tome_tile(m, y, stones);
            y = l.rect.bottom() + m.gap;
            tome_layout = Some(l);
        }
        if let Some(body) = files {
            let body = body.max(0.0);
            // A height that came back from physical pixels is a hair off.
            let rows = ((body - m.file_foot) / m.file_row_h + 0.01)
                .floor()
                .max(0.0) as usize;
            let header = Rect::new(m.pad, y, full, m.files_header_h);
            let mut row_y = header.bottom();
            let row_rects = (0..rows)
                .map(|_| {
                    let r = Rect::new(m.pad, row_y, full, m.file_row_h);
                    row_y += m.file_row_h;
                    r
                })
                .collect();
            // The foot under the last row keeps it off the rounded corner.
            let bottom = header.bottom() + body;
            files_layout = Some(FilesLayout {
                rect: Rect::new(m.pad, y, full, bottom - y),
                header,
                rows: row_rects,
            });
            y = bottom + m.gap;
        }
    }
    // Trailing gap becomes bottom padding.
    let height = if collapsed {
        header.bottom() + m.pad
    } else {
        y - m.gap + m.pad
    };
    ClusterLayout {
        size: (m.width, height),
        header,
        new,
        marks: vec![None; tiles.len()],
        codes: vec![None; tiles.len()],
        tiles,
        add,
        shell,
        tasks: tasks_layout,
        tome: tome_layout,
        files: files_layout,
    }
}

/// The Runetome with its top at `y` and `n` stones in rows under its
/// header, each centred in its share of the row.
fn tome_tile(m: &Metrics, y: f32, n: usize) -> TomeLayout {
    let full = m.width - 2.0 * m.pad;
    let header = Rect::new(m.pad, y, full, m.files_header_h);
    let cell_w = full / m.stones_per_row as f32;
    let row_h = m.stone + m.stone_label_h + 4.0;
    let mut stones = Vec::with_capacity(n);
    let mut labels = Vec::with_capacity(n);
    for i in 0..n {
        let (row, col) = (i / m.stones_per_row, i % m.stones_per_row);
        let cell_x = m.pad + col as f32 * cell_w;
        let top = header.bottom() + 2.0 + row as f32 * row_h;
        stones.push(Rect::new(
            cell_x + (cell_w - m.stone) / 2.0,
            top,
            m.stone,
            m.stone,
        ));
        labels.push(Rect::new(
            cell_x + 2.0,
            top + m.stone,
            cell_w - 4.0,
            m.stone_label_h,
        ));
    }
    let rows = n.div_ceil(m.stones_per_row);
    let bottom = if n == 0 {
        header.bottom()
    } else {
        header.bottom() + 2.0 + rows as f32 * row_h + m.task_foot
    };
    TomeLayout {
        rect: Rect::new(m.pad, y, full, bottom - y),
        header,
        stones,
        labels,
    }
}

/// How far the tasks tile's plus stands in from the tile's right edge.
const TASKS_ADD_IN: f32 = 6.0;

/// The tasks tile with its top at `y`, a row for each of `approve`.
fn tasks_tile(m: &Metrics, y: f32, approve: &[bool]) -> TasksLayout {
    let full = m.width - 2.0 * m.pad;
    let header = Rect::new(m.pad, y, full, m.files_header_h);
    // The plus is a glyph in a square, so the square stands in from the
    // edge for the glyph to line up with the padded text of the rows.
    let add = Rect::new(
        header.right() - TASKS_ADD_IN - header.h,
        y,
        header.h,
        header.h,
    );
    let give = Rect::new(add.x - header.h, y, header.h, header.h);
    let mode = Rect::new(give.x - 4.0 - m.mode_w, y + 4.0, m.mode_w, header.h - 8.0);
    let mut rows = Vec::new();
    let mut buttons = Vec::new();
    let mut row_y = header.bottom();
    for &a in approve {
        let r = Rect::new(m.pad, row_y, full, m.task_row_h);
        let side = m.task_row_h - 6.0;
        buttons.push(a.then(|| Rect::new(r.right() - 8.0 - side, r.y + 3.0, side, side)));
        rows.push(r);
        row_y += m.task_row_h;
    }
    let bottom = if approve.is_empty() {
        header.bottom()
    } else {
        row_y + m.task_foot
    };
    TasksLayout {
        rect: Rect::new(m.pad, y, full, bottom - y),
        header,
        mode,
        add,
        give,
        rows,
        approve: buttons,
    }
}

/// Puts the browser button on the tiles whose session has a browser open,
/// at the right end of the tile's second line, and the VS Code button on
/// those with a worktree, left of it.
pub fn mark(layout: &mut ClusterLayout, m: &Metrics, marked: &[bool], coded: &[bool]) {
    let on = |flags: &[bool], i: usize| flags.get(i).copied().unwrap_or(false);
    // Centred on the second line of text, which sits a little above the
    // middle of the tile's lower half.
    let button = |t: &Rect, from_right: usize| {
        let line = t.y + t.h * 0.75 - 4.0;
        Rect::new(
            t.right() - 4.0 - m.mark_w - from_right as f32 * (m.mark_w + 2.0),
            line - m.mark_h / 2.0,
            m.mark_w,
            m.mark_h,
        )
    };
    layout.marks = (layout.tiles.iter().enumerate())
        .map(|(i, t)| on(marked, i).then(|| button(t, 0)))
        .collect();
    layout.codes = (layout.tiles.iter().enumerate())
        .map(|(i, t)| on(coded, i).then(|| button(t, usize::from(on(marked, i)))))
        .collect();
}

/// The least of a tile's second line its text keeps. Less than this and
/// the line reads as an ellipsis, which says nothing.
pub const LINE_TEXT_MIN: f32 = 84.0;

/// What a tile's second line shows beside its text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LineParts {
    pub diff: bool,
    pub context: bool,
    pub trace: bool,
}

/// Which of the parts a tile's second line wants fit in `width` beside
/// its text, each width with its gap. The worktree's diff goes first, as
/// it is work to look at; then the context warning, which the meter under
/// the icon also gives; the trace last, as the icon already says busy.
/// Whatever would squeeze the text under [`LINE_TEXT_MIN`] is left out.
pub fn tile_line(
    width: f32,
    diff: Option<f32>,
    context: Option<f32>,
    trace: Option<f32>,
) -> LineParts {
    let mut room = width - LINE_TEXT_MIN;
    let mut take = |w: Option<f32>| match w {
        Some(w) if w <= room => {
            room -= w;
            true
        }
        _ => false,
    };
    LineParts {
        diff: take(diff),
        context: take(context),
        trace: take(trace),
    }
}

/// The shortest a files tile is before its column folds it to its header.
pub fn min_files_body(m: &Metrics) -> f32 {
    m.file_rows_min as f32 * m.file_row_h + m.file_foot
}

/// Which part of the cluster a point is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hit {
    New,
    Header,
    Tile(usize),
    /// The browser button on a tile.
    Browser(usize),
    /// The VS Code button on a tile.
    Code(usize),
    Add,
    /// The terminal button beside the bottom plus.
    Shell,
    FilesHeader,
    /// A row of the files tile, counted from the top one showing.
    File(usize),
    TasksHeader,
    /// The mode button in the tasks tile's header.
    TasksMode,
    /// The plus in the tasks tile's header.
    TasksAdd,
    /// The quest giver beside it.
    TasksGive,
    /// A row of the tasks tile, counted from the top one showing.
    Task(usize),
    /// The approve button on that row.
    TaskApprove(usize),
    TomeHeader,
    /// A stone of the Runetome, or its label, in the tome's order.
    Stone(usize),
    Nothing,
}

impl Hit {
    /// Whether this part lights up under the cursor. The files tile does
    /// not yet.
    pub fn lights(self) -> bool {
        matches!(
            self,
            Hit::New
                | Hit::Header
                | Hit::Tile(_)
                | Hit::Browser(_)
                | Hit::Code(_)
                | Hit::Add
                | Hit::Shell
                | Hit::TasksMode
                | Hit::TasksAdd
                | Hit::TasksGive
                | Hit::Task(_)
                | Hit::TaskApprove(_)
                | Hit::Stone(_)
        )
    }
}

/// How a button draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    Idle,
    Hover,
    Pressed,
}

/// How the button at `which` draws, with the cursor over `hot` and the left
/// button held since it went down on `pressed`. Slid off while held, it
/// looks idle, because letting go there does nothing. While something else
/// is held, nothing lights up under the cursor, as in Windows.
pub fn button<T: PartialEq>(which: T, hot: T, pressed: Option<T>) -> Button {
    match pressed {
        Some(p) if p == which && hot == which => Button::Pressed,
        Some(_) => Button::Idle,
        None if hot == which => Button::Hover,
        None => Button::Idle,
    }
}

pub fn hit(layout: &ClusterLayout, x: f32, y: f32) -> Hit {
    if layout.new.contains(x, y) {
        return Hit::New;
    }
    if layout.header.contains(x, y) {
        return Hit::Header;
    }
    for (i, r) in layout.marks.iter().enumerate() {
        if r.is_some_and(|r| r.contains(x, y)) {
            return Hit::Browser(i);
        }
    }
    for (i, r) in layout.codes.iter().enumerate() {
        if r.is_some_and(|r| r.contains(x, y)) {
            return Hit::Code(i);
        }
    }
    for (i, t) in layout.tiles.iter().enumerate() {
        if t.contains(x, y) {
            return Hit::Tile(i);
        }
    }
    if layout.add.is_some_and(|a| a.contains(x, y)) {
        return Hit::Add;
    }
    if layout.shell.is_some_and(|a| a.contains(x, y)) {
        return Hit::Shell;
    }
    if let Some(t) = &layout.tasks {
        if t.mode.contains(x, y) {
            return Hit::TasksMode;
        }
        if t.add.contains(x, y) {
            return Hit::TasksAdd;
        }
        if t.give.contains(x, y) {
            return Hit::TasksGive;
        }
        if t.header.contains(x, y) {
            return Hit::TasksHeader;
        }
        for (i, r) in t.approve.iter().enumerate() {
            if r.is_some_and(|r| r.contains(x, y)) {
                return Hit::TaskApprove(i);
            }
        }
        if let Some(i) = t.rows.iter().position(|r| r.contains(x, y)) {
            return Hit::Task(i);
        }
    }
    if let Some(t) = &layout.tome {
        if t.header.contains(x, y) {
            return Hit::TomeHeader;
        }
        let on = |r: &Rect| r.contains(x, y);
        if let Some(i) = (t.stones.iter().zip(&t.labels)).position(|(s, l)| on(s) || on(l)) {
            return Hit::Stone(i);
        }
    }
    if let Some(f) = &layout.files {
        if f.header.contains(x, y) {
            return Hit::FilesHeader;
        }
        if let Some(i) = f.rows.iter().position(|r| r.contains(x, y)) {
            return Hit::File(i);
        }
    }
    Hit::Nothing
}

/// The geometry of the usage window: the account's limits on a screen,
/// the settings for sessions in a section below. Folded, only the first
/// limit is left, the session's budget, which is the one that runs out
/// first. With one agent in use it has no header: it belongs to no
/// project, so there is no name to show, and the screen itself is what
/// folds it. With more, a line on top names whose limits and settings
/// these are, and a click on it goes on to the next agent's.
#[derive(Debug, Clone, PartialEq)]
pub struct UsageLayout {
    pub size: (f32, f32),
    /// The line naming the provider, when there is more than one.
    pub header: Option<Rect>,
    /// The screen the limits are on.
    pub limits_box: Rect,
    /// One row per limit, or one for the line that says none is known yet.
    pub limits: Vec<Rect>,
    /// The section round the settings. None when folded.
    pub settings_box: Option<Rect>,
    pub settings: Vec<SettingRow>,
}

/// One setting: a list opens from its row, a scale has a slider under its
/// name.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SettingRow {
    pub rect: Rect,
    /// The line with the setting's name and value.
    pub line: Rect,
    /// A scale's slider: the stops run from its left edge to its right.
    pub track: Option<Rect>,
}

/// Room at each end of a slider for half its knob.
pub const KNOB_R: f32 = 7.0;

/// Lays out the usage window with `limits` limits known and a setting per
/// entry of `scales`, each true for a slider, under a header line when
/// `header`. With no limit known it keeps one row, for saying so.
pub fn usage(
    m: &Metrics,
    header: bool,
    limits: usize,
    scales: &[bool],
    collapsed: bool,
) -> UsageLayout {
    let full = m.width - 2.0 * m.pad;
    // Inside a tile, rows keep off its rounded corners.
    let inner = 4.0;
    let mut y = m.pad;
    let header = header.then(|| {
        let r = Rect::new(m.pad, y, full, STASH_HEADER_H);
        y = r.bottom() + 6.0;
        r
    });
    let top = y;
    y += inner;
    let rows = if collapsed { 1 } else { limits.max(1) };
    let mut l = UsageLayout {
        size: (m.width, 0.0),
        header,
        limits_box: Rect::default(),
        limits: Vec::new(),
        settings_box: None,
        settings: Vec::new(),
    };
    for _ in 0..rows {
        l.limits.push(Rect::new(m.pad, y, full, m.limit_row_h));
        y += m.limit_row_h;
    }
    y += inner;
    l.limits_box = Rect::new(m.pad, top, full, y - top);
    if !collapsed {
        y += m.gap;
        let top = y;
        y += inner;
        for &scale in scales {
            let line = Rect::new(m.pad, y, full, m.setting_row_h);
            let (h, track) = if scale {
                let x = m.pad + m.setting_pad + KNOB_R;
                let track = Rect::new(x, line.bottom() - 4.0, full - 2.0 * (x - m.pad), 16.0);
                (track.bottom() + 8.0 - y, Some(track))
            } else {
                (m.setting_row_h, None)
            };
            l.settings.push(SettingRow {
                rect: Rect::new(m.pad, y, full, h),
                line,
                track,
            });
            y += h;
        }
        y += inner;
        l.settings_box = Some(Rect::new(m.pad, top, full, y - top));
    }
    l.size.1 = y + m.pad;
    l
}

/// Where stop `i` of `n` sits along a slider's track.
pub fn slider_x(track: &Rect, n: usize, i: usize) -> f32 {
    if n < 2 {
        return track.x;
    }
    track.x + track.w * i.min(n - 1) as f32 / (n - 1) as f32
}

/// The stop of `n` nearest to `x`, for a click or a drag anywhere along
/// the slider, past its ends too.
pub fn slider_stop(track: &Rect, n: usize, x: f32) -> usize {
    if n < 2 || track.w <= 0.0 {
        return 0;
    }
    let t = ((x - track.x) / track.w).clamp(0.0, 1.0);
    (t * (n - 1) as f32).round() as usize
}

/// The geometry of the stash: a line naming it, then a three by three
/// grid of slots sunk into the plate, the first filled first.
#[derive(Debug, Clone, PartialEq)]
pub struct StashLayout {
    pub size: (f32, f32),
    pub header: Rect,
    /// The well the slots sit in.
    pub well: Rect,
    pub slots: Vec<Rect>,
}

pub const STASH_SIDE: usize = 3;
const STASH_HEADER_H: f32 = 22.0;
const STASH_SLOT_H: f32 = 50.0;
const STASH_SLOT_GAP: f32 = 8.0;
const STASH_INNER: f32 = 8.0;

pub fn stash(m: &Metrics) -> StashLayout {
    let full = m.width - 2.0 * m.pad;
    let header = Rect::new(m.pad, m.pad, full, STASH_HEADER_H);
    let top = header.bottom() + 6.0;
    let side = STASH_SIDE as f32;
    let slot_w = (full - 2.0 * STASH_INNER - (side - 1.0) * STASH_SLOT_GAP) / side;
    let mut slots = Vec::with_capacity(STASH_SIDE * STASH_SIDE);
    for row in 0..STASH_SIDE {
        for col in 0..STASH_SIDE {
            slots.push(Rect::new(
                m.pad + STASH_INNER + col as f32 * (slot_w + STASH_SLOT_GAP),
                top + STASH_INNER + row as f32 * (STASH_SLOT_H + STASH_SLOT_GAP),
                slot_w,
                STASH_SLOT_H,
            ));
        }
    }
    let h = 2.0 * STASH_INNER + side * STASH_SLOT_H + (side - 1.0) * STASH_SLOT_GAP;
    let well = Rect::new(m.pad, top, full, h);
    StashLayout {
        size: (m.width, well.bottom() + m.pad),
        header,
        well,
        slots,
    }
}

/// The slot under a point, if any.
pub fn stash_hit(l: &StashLayout, x: f32, y: f32) -> Option<usize> {
    l.slots.iter().position(|r| r.contains(x, y))
}

/// The geometry of the cube: the cube itself beside the button that runs
/// the recipe and the rune that puts `main` in, then a well of the three
/// slots tiles are dropped into.
#[derive(Debug, Clone, PartialEq)]
pub struct CubeLayout {
    pub size: (f32, f32),
    /// Where the cube is drawn.
    pub cube: Rect,
    pub transmute: Rect,
    pub main: Rect,
    pub well: Rect,
    pub slots: Vec<Rect>,
}

const CUBE_TOP_H: f32 = 44.0;
const CUBE_MAIN_W: f32 = 52.0;
const CUBE_SLOT_H: f32 = 44.0;

pub fn cube(m: &Metrics) -> CubeLayout {
    let full = m.width - 2.0 * m.pad;
    let top = m.pad + 2.0;
    let cube = Rect::new(m.pad + 2.0, top, CUBE_TOP_H, CUBE_TOP_H);
    let main = Rect::new(m.pad + full - CUBE_MAIN_W, top, CUBE_MAIN_W, CUBE_TOP_H);
    let gap = STASH_SLOT_GAP;
    let transmute = Rect::new(
        cube.right() + gap,
        top,
        main.x - gap - cube.right() - gap,
        CUBE_TOP_H,
    );
    let well_top = cube.bottom() + gap;
    let side = cube::SLOTS as f32;
    let slot_w = (full - 2.0 * STASH_INNER - (side - 1.0) * gap) / side;
    let slots = (0..cube::SLOTS)
        .map(|i| {
            Rect::new(
                m.pad + STASH_INNER + i as f32 * (slot_w + gap),
                well_top + STASH_INNER,
                slot_w,
                CUBE_SLOT_H,
            )
        })
        .collect();
    let well = Rect::new(m.pad, well_top, full, 2.0 * STASH_INNER + CUBE_SLOT_H);
    CubeLayout {
        size: (m.width, well.bottom() + m.pad),
        cube,
        transmute,
        main,
        well,
        slots,
    }
}

/// Which part of the cube a point is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CubeHit {
    Slot(usize),
    Main,
    Transmute,
    Nothing,
}

pub fn cube_hit(l: &CubeLayout, x: f32, y: f32) -> CubeHit {
    if let Some(i) = l.slots.iter().position(|r| r.contains(x, y)) {
        return CubeHit::Slot(i);
    }
    if l.main.contains(x, y) {
        return CubeHit::Main;
    }
    if l.transmute.contains(x, y) {
        return CubeHit::Transmute;
    }
    CubeHit::Nothing
}

/// Which part of the usage window a point is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageHit {
    /// The line naming the provider, which goes on to the next one's.
    Header,
    /// The limits' screen, which folds and unfolds the window.
    Limits,
    Setting(usize),
    Nothing,
}

pub fn usage_hit(l: &UsageLayout, x: f32, y: f32) -> UsageHit {
    if l.header.is_some_and(|h| h.contains(x, y)) {
        return UsageHit::Header;
    }
    if l.limits_box.contains(x, y) {
        return UsageHit::Limits;
    }
    match l.settings.iter().position(|r| r.rect.contains(x, y)) {
        Some(i) => UsageHit::Setting(i),
        None => UsageHit::Nothing,
    }
}

/// The geometry of a setting's list, dropped under its row: a note on
/// when a pick takes hold, then one row per value.
#[derive(Debug, Clone, PartialEq)]
pub struct DropdownLayout {
    pub size: (f32, f32),
    pub note: Rect,
    pub items: Vec<Rect>,
}

/// Lays out a list `width` wide of `items` values. The one at `apart`, if
/// any, sits a gap below the rest, out of reach of a slip.
pub fn dropdown(m: &Metrics, width: f32, items: usize, apart: Option<usize>) -> DropdownLayout {
    let pad = m.menu_pad;
    let full = width - 2.0 * pad;
    let note = Rect::new(pad, pad, full, m.menu_note_h);
    let mut y = note.bottom();
    let mut rows = Vec::with_capacity(items);
    for i in 0..items {
        if apart == Some(i) {
            y += m.menu_apart;
        }
        rows.push(Rect::new(pad, y, full, m.menu_row_h));
        y += m.menu_row_h;
    }
    DropdownLayout {
        size: (width, y + pad),
        note,
        items: rows,
    }
}

pub fn dropdown_hit(l: &DropdownLayout, x: f32, y: f32) -> Option<usize> {
    l.items.iter().position(|r| r.contains(x, y))
}

/// One line of a menu, as far as laying it out goes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MenuLine {
    /// A row whose label is `label` wide and whose right aligned detail
    /// (a shortcut, a place, an age) is `detail` wide, 0 for none. `sub`
    /// when it opens more lines beside it.
    Row {
        label: f32,
        detail: f32,
        sub: bool,
    },
    Separator,
}

/// The geometry of a menu: one rect per line, in the order given, over
/// content `content_h` tall. When that is taller than the window, the
/// lines scroll inside [`MenuLayout::view`].
#[derive(Debug, Clone, PartialEq)]
pub struct MenuLayout {
    pub size: (f32, f32),
    pub lines: Vec<Rect>,
    /// Where the lines show, inside the plate's edge.
    pub view: Rect,
    pub content_h: f32,
}

/// From a row's left edge to its label: room for the lamp of a checked
/// line.
pub const MENU_TEXT_X: f32 = 26.0;
/// From a row's right edge to where its detail ends, room for the
/// chevron of a line that opens more.
pub const MENU_ARROW_W: f32 = 26.0;
const MENU_DETAIL_GAP: f32 = 32.0;
const MENU_MIN_W: f32 = 200.0;
const MENU_MAX_W: f32 = 520.0;

/// Lays out a menu of `lines`, no taller than `max_h` DIPs.
pub fn menu(m: &Metrics, lines: &[MenuLine], max_h: f32) -> MenuLayout {
    let pad = m.menu_pad;
    let (mut label, mut detail) = (0.0f32, 0.0f32);
    for l in lines {
        if let MenuLine::Row {
            label: a,
            detail: b,
            ..
        } = *l
        {
            label = label.max(a);
            detail = detail.max(b);
        }
    }
    let detail = if detail > 0.0 {
        MENU_DETAIL_GAP + detail
    } else {
        0.0
    };
    let row_w = (MENU_TEXT_X + label.ceil() + detail.ceil() + MENU_ARROW_W)
        .clamp(MENU_MIN_W - 2.0 * pad, MENU_MAX_W - 2.0 * pad);
    let mut y = 0.0;
    let mut rects = Vec::with_capacity(lines.len());
    for l in lines {
        let h = match l {
            MenuLine::Row { .. } => m.menu_row_h,
            MenuLine::Separator => m.menu_apart,
        };
        rects.push(Rect::new(pad, pad + y, row_w, h));
        y += h;
    }
    let view_h = y.min((max_h - 2.0 * pad).max(m.menu_row_h));
    MenuLayout {
        size: (row_w + 2.0 * pad, view_h + 2.0 * pad),
        lines: rects,
        view: Rect::new(pad, pad, row_w, view_h),
        content_h: y,
    }
}

/// The line under a point, with the lines scrolled `scroll` up. None off
/// the lines, in the plate's edge or past the view.
pub fn menu_hit(l: &MenuLayout, scroll: f32, x: f32, y: f32) -> Option<usize> {
    if !l.view.contains(x, y) {
        return None;
    }
    l.lines.iter().position(|r| r.contains(x, y + scroll))
}

/// The next line the keyboard can land on from `from`, down or up, round
/// the ends. From nowhere, down is the first and up the last. None when no
/// line can be picked.
pub fn menu_step(pickable: &[bool], from: Option<usize>, down: bool) -> Option<usize> {
    let n = pickable.len();
    if n == 0 {
        return None;
    }
    let start = match (from, down) {
        (Some(i), _) => i,
        (None, true) => n - 1,
        (None, false) => 0,
    };
    (1..=n)
        .map(|k| {
            if down {
                (start + k) % n
            } else {
                (start + n - k % n) % n
            }
        })
        .find(|&i| pickable[i])
}

/// Where a menu `size` goes when opened at `at`, all in screen pixels: its
/// top left corner there, or flipped to the left or above when there is no
/// room, as a right click menu opens, and never off the work area.
pub fn menu_place(at: (i32, i32), size: (i32, i32), work: [i32; 4]) -> (i32, i32) {
    let [wl, wt, wr, wb] = work;
    let (w, h) = size;
    let x = if at.0 + w <= wr { at.0 } else { at.0 - w };
    let y = if at.1 + h <= wb { at.1 } else { at.1 - h };
    (x.min(wr - w).max(wl), y.min(wb - h).max(wt))
}

/// Where a submenu `size` goes beside `parent`, the menu it opens from,
/// with its first line level with `row_top`: to the right, overlapping the
/// parent's edge by `overlap`, or to the left when there is no room.
pub fn submenu_place(
    parent: [i32; 4],
    row_top: i32,
    size: (i32, i32),
    overlap: i32,
    work: [i32; 4],
) -> (i32, i32) {
    let [left, _, right, _] = parent;
    let [wl, wt, wr, wb] = work;
    let (w, h) = size;
    let x = if right - overlap + w <= wr {
        right - overlap
    } else {
        left + overlap - w
    };
    (x.min(wr - w).max(wl), row_top.min(wb - h).max(wt))
}

/// The geometry of the input the app asks with: its title, what it asks,
/// the field, the notes when it takes them, and a line saying which keys
/// do what.
#[derive(Debug, Clone, PartialEq)]
pub struct AskLayout {
    pub size: (f32, f32),
    pub title: Rect,
    pub prompt: Rect,
    pub field: Rect,
    /// The notes' label and their field, for a new task.
    pub notes_label: Option<Rect>,
    pub notes: Option<Rect>,
    /// What the field could be, one row each, when it offers some.
    pub list: Vec<Rect>,
    pub hint: Rect,
    /// A key at the end of the hint's line that browses instead.
    pub browse: Option<Rect>,
}

/// How wide the input is, in DIPs: a task's title fits without scrolling.
pub const ASK_W: f32 = 400.0;
/// Wider for picking from a list, as a path runs long.
const ASK_LIST_W: f32 = 540.0;
pub const ASK_ROW_H: f32 = 28.0;
const ASK_BROWSE_W: f32 = 92.0;
/// Between the window's edge and what is in it.
const ASK_PAD: f32 = 20.0;
const ASK_FIELD_H: f32 = 34.0;
/// Five lines of notes.
const ASK_NOTES_H: f32 = 108.0;

/// How wide an input is, wider with a list to pick from.
fn ask_w(list: usize) -> f32 {
    if list > 0 {
        ASK_LIST_W
    } else {
        ASK_W
    }
}

/// Lays out an input whose prompt wraps to `prompt_h` DIPs at
/// [`ask_text_w`] wide, with room for `list` rows under the field to pick
/// from and a Browse key when `browse`.
pub fn ask(prompt_h: f32, notes: bool, list: usize, browse: bool) -> AskLayout {
    let full = ask_w(list);
    let w = full - 2.0 * ASK_PAD;
    let title = Rect::new(ASK_PAD, 16.0, w, 24.0);
    let prompt = Rect::new(ASK_PAD, title.bottom() + 2.0, w, prompt_h);
    let field = Rect::new(ASK_PAD, prompt.bottom() + 10.0, w, ASK_FIELD_H);
    let (notes_label, notes, below) = if notes {
        let label = Rect::new(ASK_PAD, field.bottom() + 12.0, w, 18.0);
        let notes = Rect::new(ASK_PAD, label.bottom() + 4.0, w, ASK_NOTES_H);
        (Some(label), Some(notes), notes.bottom())
    } else {
        (None, None, field.bottom())
    };
    let list: Vec<Rect> = (0..list)
        .map(|i| Rect::new(ASK_PAD, below + 6.0 + i as f32 * ASK_ROW_H, w, ASK_ROW_H))
        .collect();
    let below = list.last().map_or(below, |r| r.bottom());
    let (hint_w, browse) = if browse {
        let b = Rect::new(
            full - ASK_PAD - ASK_BROWSE_W,
            below + 10.0,
            ASK_BROWSE_W,
            28.0,
        );
        (w - ASK_BROWSE_W - 10.0, Some(b))
    } else {
        (w, None)
    };
    let hint_y = browse.map_or(below + 8.0, |b| b.y + 4.0);
    let hint = Rect::new(ASK_PAD, hint_y, hint_w, 20.0);
    let bottom = browse.map_or(hint.bottom(), |b| b.bottom().max(hint.bottom()));
    AskLayout {
        size: (full, bottom + 12.0),
        title,
        prompt,
        field,
        notes_label,
        notes,
        list,
        hint,
        browse,
    }
}

/// The row of the list under a point.
pub fn ask_list_hit(l: &AskLayout, x: f32, y: f32) -> Option<usize> {
    l.list.iter().position(|r| r.contains(x, y))
}

/// Where a field's text goes inside its well.
pub fn ask_inner(field: &Rect) -> Rect {
    Rect::new(
        field.x + 11.0,
        field.y + 7.0,
        field.w - 22.0,
        field.h - 14.0,
    )
}

/// How wide the prompt wraps, with `list` rows to pick from.
pub fn ask_text_w(list: usize) -> f32 {
    ask_w(list) - 2.0 * ASK_PAD
}

/// Which field a point is in: 0 the first, 1 the notes.
pub fn ask_hit(l: &AskLayout, x: f32, y: f32) -> Option<usize> {
    if l.field.contains(x, y) {
        Some(0)
    } else if l.notes.is_some_and(|n| n.contains(x, y)) {
        Some(1)
    } else {
        None
    }
}

/// The geometry of a question with buttons: a lamp and a title, the text,
/// and the buttons in a row along the bottom, right aligned.
#[derive(Debug, Clone, PartialEq)]
pub struct DialogLayout {
    pub size: (f32, f32),
    pub lamp: (f32, f32),
    pub title: Rect,
    pub text: Rect,
    pub buttons: Vec<Rect>,
}

pub const DIALOG_W: f32 = 440.0;
const DIALOG_PAD: f32 = 24.0;
const DIALOG_BUTTON_H: f32 = 32.0;
const DIALOG_BUTTON_MIN: f32 = 92.0;
/// Room either side of a button's label.
const DIALOG_BUTTON_PAD: f32 = 20.0;
const DIALOG_BUTTON_GAP: f32 = 10.0;

/// How wide a dialog's text wraps.
pub fn dialog_text_w() -> f32 {
    DIALOG_W - 2.0 * DIALOG_PAD
}

/// Lays out a dialog whose text wraps to `text_h` DIPs at
/// [`dialog_text_w`], with a button for each of `labels`, their widths.
pub fn dialog(text_h: f32, labels: &[f32]) -> DialogLayout {
    let w = dialog_text_w();
    let lamp = (DIALOG_PAD + 4.0, 20.0 + 12.0);
    let title = Rect::new(DIALOG_PAD + 16.0, 20.0, w - 16.0, 24.0);
    let text = Rect::new(DIALOG_PAD, title.bottom() + 8.0, w, text_h);
    let y = text.bottom() + 22.0;
    let widths: Vec<f32> = labels
        .iter()
        .map(|l| (l.ceil() + 2.0 * DIALOG_BUTTON_PAD).max(DIALOG_BUTTON_MIN))
        .collect();
    let mut x = DIALOG_W - DIALOG_PAD;
    let mut buttons: Vec<Rect> = widths
        .iter()
        .rev()
        .map(|&bw| {
            x -= bw;
            let r = Rect::new(x, y, bw, DIALOG_BUTTON_H);
            x -= DIALOG_BUTTON_GAP;
            r
        })
        .collect();
    buttons.reverse();
    DialogLayout {
        size: (DIALOG_W, y + DIALOG_BUTTON_H + DIALOG_PAD - 4.0),
        lamp,
        title,
        text,
        buttons,
    }
}

pub fn dialog_hit(l: &DialogLayout, x: f32, y: f32) -> Option<usize> {
    l.buttons.iter().position(|r| r.contains(x, y))
}

/// Where a dialog `size` goes on the work area `work`, both in screen
/// pixels: centred across and a little above the middle, where the eye
/// goes first.
pub fn dialog_place(size: (i32, i32), work: [i32; 4]) -> (i32, i32) {
    let [wl, wt, wr, wb] = work;
    let x = wl + (wr - wl - size.0) / 2;
    let y = wt + (wb - wt - size.1) * 2 / 5;
    (x.max(wl), y.max(wt))
}

/// How tall the stage's caption is, in DIPs.
pub const CAPTION_H: f32 = 38.0;
const CAPTION_KEY_W: f32 = 42.0;
const CAPTION_KEY_H: f32 = 28.0;

/// The geometry of the stage's caption: the project's lamp and title on
/// the left, the window's three keys on the right.
#[derive(Debug, Clone, PartialEq)]
pub struct CaptionLayout {
    pub lamp: (f32, f32),
    pub title: Rect,
    pub min: Rect,
    pub max: Rect,
    pub close: Rect,
}

/// What a point on the caption is over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptionHit {
    Min,
    Max,
    Close,
    /// The rest of it, which drags the window.
    Bar,
}

/// Lays out the caption of a stage `width` DIPs wide.
pub fn caption(width: f32) -> CaptionLayout {
    let top = (CAPTION_H - CAPTION_KEY_H) / 2.0 + 1.0;
    let close = Rect::new(
        width - 9.0 - CAPTION_KEY_W,
        top,
        CAPTION_KEY_W,
        CAPTION_KEY_H,
    );
    let max = Rect::new(
        close.x - 2.0 - CAPTION_KEY_W,
        top,
        CAPTION_KEY_W,
        CAPTION_KEY_H,
    );
    let min = Rect::new(
        max.x - 2.0 - CAPTION_KEY_W,
        top,
        CAPTION_KEY_W,
        CAPTION_KEY_H,
    );
    let lamp = (20.0, CAPTION_H / 2.0 + 1.0);
    let x = 32.0;
    CaptionLayout {
        lamp,
        title: Rect::new(x, 1.0, (min.x - 16.0 - x).max(0.0), CAPTION_H),
        min,
        max,
        close,
    }
}

/// What a point is over, None below the caption.
pub fn caption_hit(l: &CaptionLayout, x: f32, y: f32) -> Option<CaptionHit> {
    if !(0.0..CAPTION_H).contains(&y) {
        return None;
    }
    Some(if l.close.contains(x, y) {
        CaptionHit::Close
    } else if l.max.contains(x, y) {
        CaptionHit::Max
    } else if l.min.contains(x, y) {
        CaptionHit::Min
    } else {
        CaptionHit::Bar
    })
}

/// The geometry of a notification: a lamp and a title, the text under
/// them, and a cross to dismiss it.
#[derive(Debug, Clone, PartialEq)]
pub struct ToastLayout {
    pub size: (f32, f32),
    pub lamp: (f32, f32),
    pub title: Rect,
    pub text: Rect,
    pub close: Rect,
}

pub const TOAST_W: f32 = 360.0;
const TOAST_PAD: f32 = 18.0;
/// Past this the text is cut, as a toast is a glance, not a letter.
const TOAST_TEXT_MAX: f32 = 90.0;

/// How wide a notification's text wraps.
pub fn toast_text_w() -> f32 {
    TOAST_W - 2.0 * TOAST_PAD
}

/// Lays out a notification whose text wraps to `text_h` DIPs, 0 for none.
pub fn toast(text_h: f32) -> ToastLayout {
    let w = toast_text_w();
    let title = Rect::new(TOAST_PAD + 16.0, 16.0, w - 16.0 - 24.0, 22.0);
    let text_h = text_h.min(TOAST_TEXT_MAX);
    let text = Rect::new(TOAST_PAD, title.bottom() + 4.0, w, text_h);
    let bottom = if text_h > 0.0 {
        text.bottom()
    } else {
        title.bottom()
    };
    ToastLayout {
        size: (TOAST_W, bottom + 16.0),
        lamp: (TOAST_PAD + 4.0, title.y + title.h / 2.0),
        title,
        text,
        close: Rect::new(TOAST_W - 10.0 - 26.0, 10.0, 26.0, 26.0),
    }
}

/// Where a notification `size` goes: in the bottom right corner of the
/// work area `work`, `margin` in from its edges, over the tray it comes
/// from. All in screen pixels.
pub fn toast_place(size: (i32, i32), work: [i32; 4], margin: i32) -> (i32, i32) {
    let [wl, wt, wr, wb] = work;
    (
        (wr - margin - size.0).max(wl),
        (wb - margin - size.1).max(wt),
    )
}

/// A row of the catch-up, as far as laying it out goes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CatchupKind {
    /// A project's name over its lines.
    Heading,
    /// A line, with a fainter detail under it or without.
    Line { detail: bool },
}

/// Where a row of the catch-up is.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CatchupRow {
    Heading(Rect),
    Line {
        /// The whole row, lit under the mouse.
        rect: Rect,
        lamp: (f32, f32),
        text: Rect,
        detail: Option<Rect>,
        /// How long ago, right aligned on the text's row.
        age: Rect,
    },
}

/// The geometry of the catch-up: a title and what it covers, a cross, then
/// the rows from `first` on, as many as fit in the height it may have, and
/// a last line saying how many more there are.
#[derive(Debug, Clone, PartialEq)]
pub struct CatchupLayout {
    pub size: (f32, f32),
    pub title: Rect,
    pub sub: Rect,
    pub close: Rect,
    /// The rows placed, the first of them the row `first`.
    pub rows: Vec<CatchupRow>,
    pub first: usize,
    /// Where "and so many more" goes, when not every row shows.
    pub more: Option<Rect>,
}

impl CatchupLayout {
    /// The rows after the last one placed.
    pub fn below(&self, total: usize) -> std::ops::Range<usize> {
        (self.first + self.rows.len()).min(total)..total
    }
}

pub const CATCHUP_W: f32 = 440.0;
const CATCHUP_PAD: f32 = 18.0;
const CATCHUP_HEADING_H: f32 = 26.0;
const CATCHUP_LINE_H: f32 = 30.0;
const CATCHUP_DETAIL_H: f32 = 18.0;
const CATCHUP_AGE_W: f32 = 64.0;

/// Lays out the catch-up with `rows` from `first` on, at most `max_h` DIPs
/// tall. A heading never stands last without a line under it. When every
/// row fits it is as tall as they need; when not, it takes all of `max_h`,
/// whatever `first` is, so scrolling never changes its size.
pub fn catchup(rows: &[CatchupKind], max_h: f32, first: usize) -> CatchupLayout {
    let pad = CATCHUP_PAD;
    let full = CATCHUP_W - 2.0 * pad;
    let title = Rect::new(pad, 14.0, full - 30.0, 24.0);
    let sub = Rect::new(pad, title.bottom(), full - 30.0, 18.0);
    let close = Rect::new(CATCHUP_W - 10.0 - 26.0, 10.0, 26.0, 26.0);
    let height = |k: &CatchupKind| match k {
        CatchupKind::Heading => CATCHUP_HEADING_H,
        CatchupKind::Line { detail: false } => CATCHUP_LINE_H,
        CatchupKind::Line { detail: true } => CATCHUP_LINE_H + CATCHUP_DETAIL_H,
    };
    let bottom = pad - 6.0;
    let top = sub.bottom() + 6.0;
    let room = max_h - bottom;
    let first = first.min(rows.len());
    let mut y = top;
    let mut placed = Vec::new();
    for (i, k) in rows.iter().enumerate().skip(first) {
        let mut need = height(k);
        if *k == CatchupKind::Heading {
            need += rows.get(i + 1).map_or(0.0, height);
        }
        let last = i + 1 == rows.len();
        let more = if last && first == 0 {
            0.0
        } else {
            CATCHUP_LINE_H
        };
        if y + need + more > room {
            break;
        }
        let h = height(k);
        placed.push(match k {
            CatchupKind::Heading => CatchupRow::Heading(Rect::new(pad, y + 6.0, full, h - 6.0)),
            CatchupKind::Line { detail } => {
                let text_x = pad + 22.0;
                let text_w = full - 22.0 - CATCHUP_AGE_W - 8.0;
                CatchupRow::Line {
                    rect: Rect::new(pad - 8.0, y, full + 16.0, h),
                    lamp: (pad + 5.0, y + CATCHUP_LINE_H / 2.0),
                    text: Rect::new(text_x, y, text_w, CATCHUP_LINE_H),
                    detail: detail.then(|| {
                        Rect::new(
                            text_x,
                            y + CATCHUP_LINE_H - 6.0,
                            full - 22.0,
                            CATCHUP_DETAIL_H,
                        )
                    }),
                    age: Rect::new(pad + full - CATCHUP_AGE_W, y, CATCHUP_AGE_W, CATCHUP_LINE_H),
                }
            }
        });
        y += h;
    }
    let all = first == 0 && placed.len() == rows.len();
    let (h, more) = if all {
        (y + bottom, None)
    } else {
        let h = max_h.max(y + CATCHUP_LINE_H + bottom);
        let r = Rect::new(
            pad + 22.0,
            h - bottom - CATCHUP_LINE_H,
            full - 22.0,
            CATCHUP_LINE_H,
        );
        (h, Some(r))
    };
    CatchupLayout {
        size: (CATCHUP_W, h),
        title,
        sub,
        close,
        rows: placed,
        first,
        more,
    }
}

/// Where `notches` of the wheel take the catch-up's first row from
/// `first`, a row a notch as the tasks tile scrolls, down while rows are
/// left under the last one showing and up to the top.
pub fn catchup_scroll(rows: &[CatchupKind], max_h: f32, first: usize, notches: i32) -> usize {
    let mut first = first.min(rows.len());
    for _ in 0..notches.unsigned_abs() {
        if notches > 0 {
            first = first.saturating_sub(1);
        } else if catchup(rows, max_h, first).below(rows.len()).is_empty() {
            break;
        } else {
            first += 1;
        }
    }
    first
}

/// The row under a point, if it is a line.
pub fn catchup_hit(l: &CatchupLayout, x: f32, y: f32) -> Option<usize> {
    l.rows.iter().position(|r| match r {
        CatchupRow::Line { rect, .. } => rect.contains(x, y),
        CatchupRow::Heading(_) => false,
    })
}

/// Where the catch-up `size` goes: across the middle of the work area
/// `work`, a little above the centre. All in screen pixels.
pub fn catchup_place(size: (i32, i32), work: [i32; 4]) -> (i32, i32) {
    let [wl, wt, wr, wb] = work;
    let x = wl + (wr - wl - size.0) / 2;
    let y = wt + (wb - wt - size.1) / 3;
    (x.max(wl), y.max(wt))
}

/// Where a window `size` goes beside `owner`, both in screen pixels, with
/// its top a little above `y`, the height it was asked from. Clusters
/// stand at the right edge of the screen, so it goes to the left when there
/// is room and to the right when not, and it stays on the work area.
pub fn ask_place(
    owner: [i32; 4],
    y: i32,
    size: (i32, i32),
    gap: i32,
    work: [i32; 4],
) -> (i32, i32) {
    let [left, _, right, _] = owner;
    let [wl, wt, wr, wb] = work;
    let (w, h) = size;
    let x = if left - gap - w >= wl {
        left - gap - w
    } else if right + gap + w <= wr {
        right + gap
    } else {
        wl.max(wr - w)
    };
    let y = (y - h / 3).min(wb - h).max(wt);
    (x, y)
}

/// The geometry of the start window, the ghost cluster that stands where
/// the first project will go while none is open: a tile that picks a
/// folder, then the recent projects.
#[derive(Debug, Clone, PartialEq)]
pub struct StartLayout {
    pub size: (f32, f32),
    pub header: Rect,
    /// The ghost tile, which opens the folder picker.
    pub open: Rect,
    /// The tile around the recent projects, its label and rows. None when
    /// there is none.
    pub recent_box: Option<Rect>,
    pub recent_label: Option<Rect>,
    /// One row per recent project, each starting a session there.
    pub recent: Vec<Rect>,
}

/// Lays out the start window with `recent` recent projects.
pub fn start(m: &Metrics, recent: usize) -> StartLayout {
    let header = Rect::new(m.pad, m.pad, m.width - 2.0 * m.pad, m.header_h);
    let full = m.width - 2.0 * m.pad;
    let open = Rect::new(m.pad, header.bottom() + m.gap, full, m.tile_h);
    let mut l = StartLayout {
        size: (m.width, open.bottom() + m.pad),
        header,
        open,
        recent_box: None,
        recent_label: None,
        recent: Vec::new(),
    };
    if recent == 0 {
        return l;
    }
    let top = open.bottom() + m.gap;
    let label = Rect::new(m.pad, top, full, m.files_header_h);
    let mut y = label.bottom();
    for _ in 0..recent {
        l.recent.push(Rect::new(m.pad, y, full, m.setting_row_h));
        y += m.setting_row_h;
    }
    // Keeps the last row off the tile's rounded corner.
    y += 4.0;
    l.recent_box = Some(Rect::new(m.pad, top, full, y - top));
    l.recent_label = Some(label);
    l.size.1 = y + m.pad;
    l
}

/// Which part of the start window a point is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartHit {
    Open,
    Recent(usize),
    Nothing,
}

pub fn start_hit(l: &StartLayout, x: f32, y: f32) -> StartHit {
    if l.open.contains(x, y) {
        return StartHit::Open;
    }
    match l.recent.iter().position(|r| r.contains(x, y)) {
        Some(i) => StartHit::Recent(i),
        None => StartHit::Nothing,
    }
}

/// How far apart things sit when snapped, in physical pixels.
#[derive(Debug, Clone, Copy)]
pub struct Spacing {
    /// From the edge of the work area, the same as `stack` uses.
    pub margin: i32,
    /// Between two windows, the same as `stack` uses.
    pub gap: i32,
    /// How close an edge has to come before it snaps.
    pub reach: i32,
}

/// Where a dragged window lands once its edges are pulled onto nearby lines.
///
/// `pos` is where the drag alone would put its top left corner, `size` its
/// size, `work` the work area of the monitor it is on and `others` the other
/// windows, all as (left, top, right, bottom) in physical pixels. Each axis
/// snaps on its own to the nearest line within reach: the work area inset by
/// the margin, beside another window with the gap between, or lined up with
/// another window's edge.
pub fn snap(
    pos: (i32, i32),
    size: (i32, i32),
    work: (i32, i32, i32, i32),
    others: &[(i32, i32, i32, i32)],
    s: Spacing,
) -> (i32, i32) {
    let (w, h) = size;
    let (x, y) = pos;
    let lines = Lines::new([x, y, x + w, y + h], work, others, s);
    // A left or top line takes the corner there, a right or bottom line
    // takes it one window size before.
    let xs: Vec<i32> = lines
        .left
        .iter()
        .copied()
        .chain(lines.right.iter().map(|r| r - w))
        .collect();
    let ys: Vec<i32> = lines
        .top
        .iter()
        .copied()
        .chain(lines.bottom.iter().map(|b| b - h))
        .collect();
    (nearest(x, &xs, s.reach), nearest(y, &ys, s.reach))
}

/// Where a window being resized lands once the edges it is dragging are
/// pulled onto nearby lines, the same lines [`snap`] uses. `rect` is where
/// the drag alone would put it and `edges` says which edges move, as left,
/// top, right, bottom. The other edges stay put.
pub fn snap_edges(
    rect: [i32; 4],
    edges: [bool; 4],
    work: (i32, i32, i32, i32),
    others: &[(i32, i32, i32, i32)],
    s: Spacing,
) -> [i32; 4] {
    let lines = Lines::new(rect, work, others, s);
    let sets = [&lines.left, &lines.top, &lines.right, &lines.bottom];
    let mut out = rect;
    for i in 0..4 {
        if edges[i] {
            out[i] = nearest(rect[i], sets[i], s.reach);
        }
    }
    out
}

/// The lines each edge of a window at `rect` can snap to.
struct Lines {
    left: Vec<i32>,
    top: Vec<i32>,
    right: Vec<i32>,
    bottom: Vec<i32>,
}

impl Lines {
    fn new(
        [x, y, r, b]: [i32; 4],
        work: (i32, i32, i32, i32),
        others: &[(i32, i32, i32, i32)],
        s: Spacing,
    ) -> Self {
        let mut lines = Lines {
            left: vec![work.0 + s.margin],
            top: vec![work.1 + s.margin],
            right: vec![work.2 - s.margin],
            bottom: vec![work.3 - s.margin],
        };
        // A window only pulls on an axis when it is near on the other one, or
        // lining up with a window across the screen would snap too.
        let near = s.gap + s.reach;
        for &(ol, ot, or, ob) in others {
            if y - near < ob && b + near > ot {
                lines.left.extend([or + s.gap, ol]);
                lines.right.extend([ol - s.gap, or]);
            }
            if x - near < or && r + near > ol {
                lines.top.extend([ob + s.gap, ot]);
                lines.bottom.extend([ot - s.gap, ob]);
            }
        }
        lines
    }
}

/// Tiles `n` windows over `area` (left, top, right, bottom) with `gap`
/// between them: as many columns as the square root rounded up, filled row
/// by row, the last row stretched across when it has fewer windows. Four
/// windows are two by two, three are two above one wide.
pub fn grid(n: usize, area: (i32, i32, i32, i32), gap: i32) -> Vec<[i32; 4]> {
    if n == 0 {
        return Vec::new();
    }
    let (left, top, right, bottom) = area;
    let cols = (1..=n).find(|c| c * c >= n).unwrap_or(1);
    let rows = n.div_ceil(cols);
    let span = |from: i32, to: i32, count: usize, i: usize| {
        let count = count as i32;
        let each = (to - from - gap * (count - 1)) / count;
        let start = from + i as i32 * (each + gap);
        (start, start + each)
    };
    (0..n)
        .map(|i| {
            let (row, col) = (i / cols, i % cols);
            let in_row = if row == rows - 1 {
                n - row * cols
            } else {
                cols
            };
            let (l, r) = span(left, right, in_row, col);
            let (t, b) = span(top, bottom, rows, row);
            [l, t, r, b]
        })
        .collect()
}

/// The grid with its last pane docked on `side`: that pane `size` wide,
/// or tall on top, or half the area without one, across the whole of that
/// side, the rest in a grid of their own beside it. `size` is kept to
/// leave each side at least `min`.
/// None where the area is too small for both, so the plain grid is used.
pub fn docked_grid(
    n: usize,
    area: (i32, i32, i32, i32),
    gap: i32,
    side: Side,
    size: Option<i32>,
    min: i32,
) -> Option<Vec<[i32; 4]>> {
    let (left, top, right, bottom) = area;
    let span = match side {
        Side::Left | Side::Right => right - left,
        Side::Top => bottom - top,
    };
    if n < 2 || span < 2 * min + gap {
        return None;
    }
    let size = size
        .unwrap_or((span - gap) / 2)
        .clamp(min, span - gap - min);
    let (dock, rest) = match side {
        Side::Left => (
            [left, top, left + size, bottom],
            (left + size + gap, top, right, bottom),
        ),
        Side::Right => (
            [right - size, top, right, bottom],
            (left, top, right - size - gap, bottom),
        ),
        Side::Top => (
            [left, top, right, top + size],
            (left, top + size + gap, right, bottom),
        ),
    };
    let mut cells = grid(n - 1, rest, gap);
    cells.push(dock);
    Some(cells)
}

/// The gap between a docked pane and the grid, as its start and end
/// across the seam: x for a side, y on top.
pub fn seam(side: Side, dock: [i32; 4], gap: i32) -> (i32, i32) {
    match side {
        Side::Left => (dock[2], dock[2] + gap),
        Side::Right => (dock[0] - gap, dock[0]),
        Side::Top => (dock[3], dock[3] + gap),
    }
}

/// How big the docked pane is with its seam dragged to start at `at`.
pub fn seam_size(side: Side, area: (i32, i32, i32, i32), gap: i32, at: i32) -> i32 {
    match side {
        Side::Left => at - area.0,
        Side::Right => area.2 - (at + gap),
        Side::Top => at - area.1,
    }
}

/// The order a project's sessions sit in on the stage. `order` is the one
/// remembered, which keeps sessions that are paused right now so they come
/// back to their place; new `live` ones join its end. Returns the live ones
/// in that order.
pub fn grid_order(order: &mut Vec<String>, live: &[String]) -> Vec<String> {
    for s in live {
        if !order.contains(s) {
            order.push(s.clone());
        }
    }
    order.iter().filter(|s| live.contains(s)).cloned().collect()
}

/// Where a session's tile goes in its cluster: its place in the project's
/// order, the same one the stage's grid follows. One not in it yet goes
/// last.
pub fn rank(order: &[String], id: &str) -> usize {
    order.iter().position(|s| s == id).unwrap_or(usize::MAX)
}

/// The order after a tile was dragged: `shown`, the tiles as they now
/// stand, then whatever `order` remembers that has no tile right now.
pub fn reordered(order: &[String], shown: &[String]) -> Vec<String> {
    let mut v = shown.to_vec();
    v.extend(order.iter().filter(|s| !shown.contains(s)).cloned());
    v
}

/// The place a dragged tile takes with its top at `top`: that of the tile
/// whose top is nearest.
pub fn tile_slot(tiles: &[Rect], top: f32) -> usize {
    tiles
        .iter()
        .enumerate()
        .min_by(|(_, a), (_, b)| (a.y - top).abs().total_cmp(&(b.y - top).abs()))
        .map_or(0, |(i, _)| i)
}

/// The space beside the tiles for the stage: the work area inset by the
/// margin, less the box around `tiles` and the gap after it, on whichever
/// side of them has more room. Tiles outside the work area do not count.
/// All as (left, top, right, bottom). Returns the space and whether the
/// tiles are on its left.
pub fn beside(
    work: (i32, i32, i32, i32),
    tiles: &[[i32; 4]],
    margin: i32,
    gap: i32,
) -> ([i32; 4], bool) {
    let (wl, wt, wr, wb) = work;
    let inner = [wl + margin, wt + margin, wr - margin, wb - margin];
    let seen = tiles
        .iter()
        .filter(|&&[l, t, r, b]| l < wr && r > wl && t < wb && b > wt);
    let Some((left, right)) = seen.fold(None, |span: Option<(i32, i32)>, &[l, _, r, _]| {
        Some(span.map_or((l, r), |(a, b)| (a.min(l), b.max(r))))
    }) else {
        return (inner, true);
    };
    let on_right = [right + gap, inner[1], inner[2], inner[3]];
    let on_left = [inner[0], inner[1], left - gap, inner[3]];
    let width = |a: [i32; 4]| a[2] - a[0];
    if width(on_right) >= width(on_left) {
        (on_right, true)
    } else {
        (on_left, false)
    }
}

/// A window of `size` moved into `area` against the tiles: its left edge
/// on the area's left when the tiles are on the left, otherwise its right
/// edge on the area's right, and its top on the area's top. It keeps its
/// size where the area has room and shrinks to fit where it has not.
pub fn dock(size: (i32, i32), area: [i32; 4], tiles_left: bool) -> [i32; 4] {
    let [l, t, r, b] = area;
    let w = size.0.clamp(1, (r - l).max(1));
    let h = size.1.clamp(1, (b - t).max(1));
    if tiles_left {
        [l, t, l + w, t + h]
    } else {
        [r - w, t, r, t + h]
    }
}

/// Where the stage opens the first time, and where it docks when a screen
/// change leaves it lost: a square as tall as `area`, against
/// the tiles, narrower only where the area is. Square because a grid of
/// panes splits it evenly both ways.
pub fn square(area: [i32; 4], tiles_left: bool) -> [i32; 4] {
    let side = area[3] - area[1];
    dock((side, side), area, tiles_left)
}

/// Where the stage goes when the columns of tiles change width: its left
/// edge on `edge`, the first pixel the tiles leave free, when the tiles now
/// reach under it or when it stood against them at `was`, so a sized stage
/// gives way to a new column and takes the room back when one goes. Its
/// right edge, top and bottom stay where you put them, unless giving way
/// would leave it narrower than `min_w`, when it keeps that much inside
/// `right`, the work area's edge. Nothing when it can stay, or when it is
/// on another screen. All as (left, top, right, bottom).
pub fn follow_tiles(
    stage: [i32; 4],
    was: Option<i32>,
    edge: i32,
    (left, right): (i32, i32),
    min_w: i32,
) -> Option<[i32; 4]> {
    let [l, t, r, b] = stage;
    if l >= right || r <= left || l == edge {
        return None;
    }
    let against = was.is_some_and(|w| (l - w).abs() <= 2);
    if l > edge && !against {
        return None;
    }
    let r = r.max(edge + min_w).min(right);
    Some([edge, t, r, b])
}

/// Where a browser window a session opened goes when it first appears: in
/// the space beside the tiles and the stage together, against the stage,
/// its own size where that fits and shrunk where it does not. Nothing when
/// that space is narrower than `min_w`, which leaves the window where it
/// opened. All as (left, top, right, bottom).
pub fn beside_stage(
    work: (i32, i32, i32, i32),
    tiles: &[[i32; 4]],
    stage: [i32; 4],
    size: (i32, i32),
    margin: i32,
    gap: i32,
    min_w: i32,
) -> Option<[i32; 4]> {
    let mut taken = tiles.to_vec();
    taken.push(stage);
    let (area, stage_left) = beside(work, &taken, margin, gap);
    (area[2] - area[0] >= min_w).then(|| dock(size, area, stage_left))
}

/// The grid cell of the pane at `i` while `swap` says the pane at the
/// first index is dragged over the second's cell: the two trade places and
/// the rest stay where they are.
pub fn swapped_cell(i: usize, swap: Option<(usize, usize)>) -> usize {
    match swap {
        Some((a, b)) if i == a => b,
        Some((a, b)) if i == b => a,
        _ => i,
    }
}

/// Where a box of `size` with its top left at `at` has to go to lie inside
/// `area` (left, top, right, bottom), moved as little as it can be. One
/// too big for the area keeps to its left or top edge.
pub fn clamp_into(at: (i32, i32), size: (i32, i32), area: [i32; 4]) -> (i32, i32) {
    let [l, t, r, b] = area;
    (at.0.min(r - size.0).max(l), at.1.min(b - size.1).max(t))
}

/// Which of `rects` (left, top, right, bottom) holds the point, if any.
pub fn slot_at(rects: &[[i32; 4]], x: i32, y: i32) -> Option<usize> {
    rects
        .iter()
        .position(|&[l, t, r, b]| x >= l && x < r && y >= t && y < b)
}

/// A way to move the keyboard from one pane to the next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    Left,
    Right,
    Up,
    Down,
}

/// The pane beside `rects[from]` in a direction: the nearest one wholly
/// past its edge, and of those the one most in line with it. None at the
/// edge of the grid.
pub fn neighbour(rects: &[[i32; 4]], from: usize, dir: Dir) -> Option<usize> {
    let [fl, ft, fr, fb] = *rects.get(from)?;
    let centre = |a: i32, b: i32| (a + b) / 2;
    rects
        .iter()
        .enumerate()
        .filter(|&(i, _)| i != from)
        .filter_map(|(i, &[l, t, r, b])| {
            let (ahead, off) = match dir {
                Dir::Left => (fl - r, centre(t, b) - centre(ft, fb)),
                Dir::Right => (l - fr, centre(t, b) - centre(ft, fb)),
                Dir::Up => (ft - b, centre(l, r) - centre(fl, fr)),
                Dir::Down => (t - fb, centre(l, r) - centre(fl, fr)),
            };
            (ahead >= 0).then_some((ahead, off.abs(), i))
        })
        .min()
        .map(|(_, _, i)| i)
}

/// The line closest to `v` if it is within `reach`, else `v` itself.
fn nearest(v: i32, lines: &[i32], reach: i32) -> i32 {
    lines
        .iter()
        .copied()
        .filter(|l| (l - v).abs() <= reach)
        .min_by_key(|l| (l - v).abs())
        .unwrap_or(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCALES: [bool; 3] = [false, true, false];

    fn row(label: f32, detail: f32) -> MenuLine {
        MenuLine::Row {
            label,
            detail,
            sub: false,
        }
    }

    #[test]
    fn a_menu_stacks_rows_and_thinner_separators() {
        let m = Metrics::default();
        let l = menu(
            &m,
            &[row(80.0, 0.0), MenuLine::Separator, row(60.0, 0.0)],
            1000.0,
        );
        assert_eq!(l.lines[0].y, m.menu_pad);
        assert_eq!(l.lines[1].h, m.menu_apart);
        assert_eq!(l.lines[2].y, l.lines[1].bottom());
        assert_eq!(l.size.1, l.lines[2].bottom() + m.menu_pad);
        assert_eq!(l.content_h, l.view.h);
        assert_eq!(
            menu_hit(&l, 0.0, l.lines[2].x + 1.0, l.lines[2].y + 1.0),
            Some(2)
        );
        assert_eq!(menu_hit(&l, 0.0, 1.0, 1.0), None);
    }

    #[test]
    fn a_menu_is_as_wide_as_its_longest_label_and_detail_within_bounds() {
        let m = Metrics::default();
        let narrow = menu(&m, &[row(10.0, 0.0)], 1000.0);
        assert_eq!(narrow.size.0, MENU_MIN_W);
        let wide = menu(&m, &[row(250.0, 0.0), row(100.0, 60.0)], 1000.0);
        assert_eq!(
            wide.size.0,
            2.0 * m.menu_pad + MENU_TEXT_X + 250.0 + MENU_DETAIL_GAP + 60.0 + MENU_ARROW_W
        );
        let huge = menu(&m, &[row(2000.0, 0.0)], 1000.0);
        assert_eq!(huge.size.0, MENU_MAX_W);
    }

    #[test]
    fn a_menu_taller_than_the_screen_scrolls() {
        let m = Metrics::default();
        let lines = vec![row(50.0, 0.0); 40];
        let l = menu(&m, &lines, 300.0);
        assert_eq!(l.size.1, 300.0);
        assert!(l.content_h > l.view.h);
        let last = l.lines[39];
        let scroll = l.content_h - l.view.h;
        assert_eq!(
            menu_hit(&l, scroll, last.x + 1.0, last.y - scroll + 1.0),
            Some(39)
        );
        assert_eq!(menu_hit(&l, 0.0, last.x + 1.0, l.view.bottom() + 1.0), None);
    }

    #[test]
    fn the_keyboard_skips_what_cannot_be_picked() {
        let p = [false, true, false, true];
        assert_eq!(menu_step(&p, None, true), Some(1));
        assert_eq!(menu_step(&p, None, false), Some(3));
        assert_eq!(menu_step(&p, Some(1), true), Some(3));
        assert_eq!(menu_step(&p, Some(3), true), Some(1));
        assert_eq!(menu_step(&p, Some(1), false), Some(3));
        assert_eq!(menu_step(&[false, false], None, true), None);
        assert_eq!(menu_step(&[true], Some(0), true), Some(0));
    }

    #[test]
    fn a_menu_opens_at_the_mouse_and_flips_at_the_edges() {
        let work = [0, 0, 1000, 800];
        assert_eq!(menu_place((100, 100), (200, 300), work), (100, 100));
        assert_eq!(menu_place((900, 100), (200, 300), work), (700, 100));
        assert_eq!(menu_place((100, 790), (200, 300), work), (100, 490));
        assert_eq!(menu_place((100, 100), (200, 900), work), (100, 0));
    }

    #[test]
    fn a_dialog_puts_its_buttons_right_aligned_under_the_text() {
        let l = dialog(60.0, &[40.0, 120.0]);
        assert_eq!(l.text.h, 60.0);
        let [a, b] = [l.buttons[0], l.buttons[1]];
        assert_eq!(a.w, DIALOG_BUTTON_MIN);
        assert_eq!(b.w, 120.0 + 2.0 * DIALOG_BUTTON_PAD);
        assert_eq!(b.right(), DIALOG_W - DIALOG_PAD);
        assert_eq!(a.right() + DIALOG_BUTTON_GAP, b.x);
        assert!(a.y > l.text.bottom());
        assert!(l.size.1 > b.bottom());
        assert_eq!(dialog_hit(&l, b.x + 1.0, b.y + 1.0), Some(1));
        assert_eq!(dialog_hit(&l, l.text.x + 1.0, l.text.y + 1.0), None);
    }

    #[test]
    fn a_dialog_stands_centred_a_little_high() {
        assert_eq!(dialog_place((400, 200), [0, 0, 1000, 700]), (300, 200));
        assert_eq!(
            dialog_place((400, 200), [1000, 100, 2000, 800]),
            (1300, 300)
        );
    }

    #[test]
    fn the_caption_keeps_its_keys_on_the_right_and_its_title_clear_of_them() {
        let l = caption(800.0);
        assert!(l.close.right() < 800.0);
        assert!(l.max.right() < l.close.x && l.min.right() < l.max.x);
        assert!(l.title.right() < l.min.x);
        assert!(l.close.bottom() <= CAPTION_H);
        let hit = |r: Rect| caption_hit(&l, r.x + 1.0, r.y + 1.0);
        assert_eq!(hit(l.close), Some(CaptionHit::Close));
        assert_eq!(hit(l.max), Some(CaptionHit::Max));
        assert_eq!(hit(l.min), Some(CaptionHit::Min));
        assert_eq!(caption_hit(&l, 100.0, 10.0), Some(CaptionHit::Bar));
        assert_eq!(caption_hit(&l, 100.0, CAPTION_H + 1.0), None);
        assert_eq!(caption(100.0).title.w, 0.0);
    }

    #[test]
    fn a_toast_grows_with_its_text_up_to_a_point() {
        let bare = toast(0.0);
        assert_eq!(bare.size.1, bare.title.bottom() + 16.0);
        let some = toast(40.0);
        assert_eq!(some.text.h, 40.0);
        assert_eq!(some.size.1, some.text.bottom() + 16.0);
        let long = toast(1000.0);
        assert_eq!(long.text.h, TOAST_TEXT_MAX);
        assert!(some.close.right() < some.size.0);
        assert!(some.title.right() < some.close.x);
    }

    #[test]
    fn a_toast_stands_in_the_bottom_right_corner() {
        assert_eq!(toast_place((360, 100), [0, 0, 1920, 1040], 12), (1548, 928));
    }

    #[test]
    fn a_submenu_opens_beside_its_parent() {
        let work = [0, 0, 1000, 800];
        let parent = [100, 50, 300, 400];
        assert_eq!(submenu_place(parent, 80, (200, 100), 4, work), (296, 80));
        let right = [700, 50, 900, 400];
        assert_eq!(submenu_place(right, 80, (200, 100), 4, work), (504, 80));
        assert_eq!(submenu_place(parent, 750, (200, 100), 4, work), (296, 700));
    }

    #[test]
    fn usage_window_holds_limits_then_settings() {
        let m = Metrics::default();
        let l = usage(&m, false, 2, &SCALES, false);
        assert_eq!(l.limits.len(), 2);
        assert_eq!(l.settings.len(), 3);
        let limits = l.limits_box;
        let settings = l.settings_box.unwrap();
        assert_eq!(limits.y, m.pad);
        assert_eq!(settings.y, limits.bottom() + m.gap);
        assert!(l
            .limits
            .iter()
            .all(|r| r.y >= limits.y && r.bottom() <= limits.bottom()));
        assert_eq!(l.size.1, settings.bottom() + m.pad);
        for pair in l.settings.windows(2) {
            assert_eq!(pair[0].rect.bottom(), pair[1].rect.y);
        }
        assert!(l
            .settings
            .iter()
            .all(|r| r.rect.bottom() <= settings.bottom()));
        let s = l.settings[1].rect;
        assert_eq!(usage_hit(&l, s.x + 5.0, s.y + 5.0), UsageHit::Setting(1));
        let r = l.limits[1];
        assert_eq!(usage_hit(&l, r.x + 5.0, r.y + 5.0), UsageHit::Limits);
        assert_eq!(usage_hit(&l, 1.0, 1.0), UsageHit::Nothing);
    }

    #[test]
    fn only_a_scale_gets_a_slider_under_its_name() {
        let m = Metrics::default();
        let l = usage(&m, false, 2, &SCALES, false);
        assert!(l.settings[0].track.is_none() && l.settings[2].track.is_none());
        let row = l.settings[1];
        let t = row.track.unwrap();
        assert!(t.y >= row.line.y + row.line.h / 2.0 && t.bottom() <= row.rect.bottom());
        assert!(t.x - KNOB_R >= row.rect.x && t.right() + KNOB_R <= row.rect.right());
        assert!(row.rect.h > l.settings[0].rect.h);
    }

    #[test]
    fn a_header_names_the_provider_above_the_limits() {
        let m = Metrics::default();
        let plain = usage(&m, false, 2, &SCALES, false);
        assert!(plain.header.is_none());
        let l = usage(&m, true, 2, &SCALES, false);
        let h = l.header.unwrap();
        assert_eq!(h.y, m.pad);
        assert!(l.limits_box.y > h.bottom());
        assert_eq!(l.size.1 - plain.size.1, l.limits_box.y - plain.limits_box.y);
        assert_eq!(usage_hit(&l, h.x + 5.0, h.y + 5.0), UsageHit::Header);
        // Folded, it still says whose.
        assert!(usage(&m, true, 2, &SCALES, true).header.is_some());
    }

    #[test]
    fn folded_it_keeps_the_first_limit_alone() {
        let m = Metrics::default();
        assert_eq!(usage(&m, false, 0, &SCALES, false).limits.len(), 1);
        let folded = usage(&m, false, 3, &SCALES, true);
        assert_eq!(folded.limits.len(), 1);
        assert!(folded.settings.is_empty() && folded.settings_box.is_none());
        assert_eq!(folded.size.1, folded.limits_box.bottom() + m.pad);
        assert!(folded.size.1 < usage(&m, false, 3, &SCALES, false).size.1);
    }

    #[test]
    fn a_slider_snaps_to_the_nearest_stop() {
        let t = Rect::new(10.0, 0.0, 100.0, 16.0);
        assert_eq!(slider_x(&t, 6, 0), 10.0);
        assert_eq!(slider_x(&t, 6, 5), 110.0);
        assert_eq!(slider_x(&t, 6, 9), 110.0);
        assert_eq!(slider_stop(&t, 6, 10.0), 0);
        assert_eq!(slider_stop(&t, 6, 39.0), 1);
        assert_eq!(slider_stop(&t, 6, 41.0), 2);
        assert_eq!(slider_stop(&t, 6, -50.0), 0);
        assert_eq!(slider_stop(&t, 6, 500.0), 5);
        for i in 0..6 {
            assert_eq!(slider_stop(&t, 6, slider_x(&t, 6, i)), i);
        }
        assert_eq!(slider_stop(&t, 1, 80.0), 0);
    }

    #[test]
    fn an_input_stacks_its_parts_and_its_notes_only_when_asked() {
        let l = ask(18.0, false, 0, false);
        assert!(l.notes.is_none() && l.notes_label.is_none());
        assert!(l.list.is_empty() && l.browse.is_none());
        assert!(l.prompt.y >= l.title.bottom());
        assert!(l.field.y >= l.prompt.bottom());
        assert!(l.hint.y >= l.field.bottom());
        assert!(l.hint.bottom() <= l.size.1);
        let t = ask(36.0, true, 0, false);
        let notes = t.notes.unwrap();
        assert!(t.notes_label.unwrap().bottom() <= notes.y);
        assert!(notes.y >= t.field.bottom() && t.hint.y >= notes.bottom());
        assert!(t.size.1 > l.size.1 + 18.0 + notes.h);
        assert_eq!(ask_hit(&t, t.field.x + 1.0, t.field.y + 1.0), Some(0));
        assert_eq!(ask_hit(&t, notes.x + 1.0, notes.bottom() - 1.0), Some(1));
        assert_eq!(ask_hit(&t, t.hint.x + 1.0, t.hint.y + 1.0), None);
        assert_eq!(ask_hit(&l, 1.0, 1.0), None);
    }

    #[test]
    fn a_picker_lists_rows_under_its_field_and_browses_beside_the_hint() {
        let l = ask(18.0, false, 3, true);
        assert_eq!(l.list.len(), 3);
        assert!(l.list[0].y >= l.field.bottom());
        assert_eq!(l.list[1].y, l.list[0].bottom());
        let b = l.browse.unwrap();
        assert!(b.y >= l.list[2].bottom());
        assert!(l.hint.right() < b.x);
        assert_eq!(b.right(), l.size.0 - ASK_PAD);
        assert!(l.size.0 > ASK_W && l.size.1 > b.bottom());
        assert_eq!(
            ask_list_hit(&l, l.list[2].x + 1.0, l.list[2].y + 1.0),
            Some(2)
        );
        assert_eq!(ask_list_hit(&l, b.x + 1.0, b.y + 1.0), None);
        assert_eq!(ask_text_w(3), l.field.w);
    }

    #[test]
    fn an_input_goes_left_of_its_cluster_and_stays_on_screen() {
        let work = [0, 0, 1920, 1040];
        // A cluster at the right edge: to its left, a third of it above
        // the click.
        assert_eq!(
            ask_place([1500, 0, 1920, 600], 300, (400, 150), 8, work),
            (1092, 250)
        );
        // No room on the left: to the right.
        assert_eq!(
            ask_place([100, 0, 500, 600], 300, (400, 150), 8, work),
            (508, 250)
        );
        // Room on neither side: inside the screen.
        assert_eq!(
            ask_place([0, 0, 1900, 600], 300, (400, 150), 8, work),
            (1520, 250)
        );
        // Near the bottom or the top: kept on the work area.
        assert_eq!(
            ask_place([1500, 0, 1920, 1040], 1030, (400, 150), 8, work).1,
            890
        );
        assert_eq!(
            ask_place([1500, 0, 1920, 1040], 10, (400, 150), 8, work).1,
            0
        );
    }

    #[test]
    fn a_dropdown_lists_its_values_under_a_note() {
        let m = Metrics::default();
        let l = dropdown(&m, 200.0, 4, Some(3));
        assert_eq!(l.items.len(), 4);
        assert_eq!(l.items[0].y, l.note.bottom());
        assert_eq!(l.items[2].y, l.items[1].bottom());
        assert_eq!(l.items[3].y, l.items[2].bottom() + m.menu_apart);
        assert_eq!(l.size, (200.0, l.items[3].bottom() + m.menu_pad));
        let r = l.items[1];
        assert_eq!(dropdown_hit(&l, r.x + 1.0, r.y + 1.0), Some(1));
        assert_eq!(dropdown_hit(&l, l.note.x + 1.0, l.note.y + 1.0), None);
        let gap = l.items[2].bottom() + 1.0;
        assert_eq!(dropdown_hit(&l, r.x + 1.0, gap), None);
    }

    #[test]
    fn start_window_has_the_ghost_tile_then_the_recent_projects() {
        let m = Metrics::default();
        let l = start(&m, 3);
        assert_eq!(l.open.y, l.header.bottom() + m.gap);
        assert_eq!(l.open.h, m.tile_h);
        let b = l.recent_box.unwrap();
        assert_eq!(b.y, l.open.bottom() + m.gap);
        assert_eq!(l.recent.len(), 3);
        assert!(l.recent[0].y >= l.recent_label.unwrap().bottom());
        assert!(l.recent.iter().all(|r| r.bottom() <= b.bottom()));
        assert_eq!(l.size.1, b.bottom() + m.pad);
        let o = l.open;
        assert_eq!(start_hit(&l, o.x + 5.0, o.y + 5.0), StartHit::Open);
        let r = l.recent[2];
        assert_eq!(start_hit(&l, r.x + 5.0, r.y + 5.0), StartHit::Recent(2));
        let h = l.header;
        assert_eq!(start_hit(&l, h.x + 5.0, h.y + 5.0), StartHit::Nothing);
    }

    #[test]
    fn start_window_without_recent_projects_is_the_ghost_tile() {
        let m = Metrics::default();
        let l = start(&m, 0);
        assert!(l.recent.is_empty() && l.recent_box.is_none());
        assert_eq!(l.size.1, l.open.bottom() + m.pad);
    }

    #[test]
    fn collapsed_is_header_only() {
        let m = Metrics::default();
        let l = cluster(&m, 5, true, None, None, Some(100.0));
        assert!(l.tiles.is_empty());
        assert!(l.add.is_none());
        assert!(l.shell.is_none());
        assert_eq!(l.size.1, m.pad + m.header_h + m.pad);
        assert!(l.files.is_none());
    }

    #[test]
    fn files_tile_sits_below_the_plus() {
        let m = Metrics::default();
        let l = cluster(
            &m,
            1,
            false,
            None,
            None,
            Some(14.0 * m.file_row_h + m.file_foot),
        );
        let add = l.add.unwrap();
        let f = l.files.as_ref().unwrap();
        assert_eq!(f.rect.y - add.bottom(), m.gap);
        assert_eq!(f.rows.len(), 14);
        assert_eq!(f.rows[0].y, f.header.bottom());
        assert_eq!(l.size.1, f.rect.bottom() + m.pad);
        assert_eq!(hit(&l, 20.0, f.header.y + 1.0), Hit::FilesHeader);
        assert_eq!(hit(&l, 20.0, f.rows[2].y + 1.0), Hit::File(2));
        assert_eq!(f.body().y, f.header.bottom());
    }

    #[test]
    fn a_files_tile_between_rows_holds_the_whole_rows() {
        let m = Metrics::default();
        let l = cluster(
            &m,
            1,
            false,
            None,
            None,
            Some(10.0 * m.file_row_h + m.file_foot + 13.0),
        );
        let f = l.files.unwrap();
        assert_eq!(f.rows.len(), 10);
        assert_eq!(
            f.rect.bottom(),
            f.header.bottom() + 10.0 * m.file_row_h + m.file_foot + 13.0
        );
        // Back from physical pixels at 150%, a hair short of 7 rows.
        let l = cluster(
            &m,
            1,
            false,
            None,
            None,
            Some(7.0 * m.file_row_h + m.file_foot - 0.1),
        );
        assert_eq!(l.files.unwrap().rows.len(), 7);
    }

    #[test]
    fn collapsed_files_tile_is_its_header() {
        let m = Metrics::default();
        let l = cluster(&m, 1, false, None, None, Some(0.0));
        let f = l.files.unwrap();
        assert!(f.rows.is_empty());
        assert_eq!(f.rect, f.header);
    }

    #[test]
    fn the_tasks_tile_sits_between_the_plus_and_the_files() {
        let m = Metrics::default();
        let approve = [false, true, false];
        let l = cluster(
            &m,
            1,
            false,
            Some(&approve),
            None,
            Some(5.0 * m.file_row_h + m.file_foot),
        );
        let add = l.add.unwrap();
        let t = l.tasks.as_ref().unwrap();
        let f = l.files.as_ref().unwrap();
        assert_eq!(t.rect.y - add.bottom(), m.gap);
        assert_eq!(f.rect.y - t.rect.bottom(), m.gap);
        assert_eq!(t.rows.len(), 3);
        assert_eq!(t.rows[0].y, t.header.bottom());
        assert_eq!(t.body().y, t.header.bottom());
        assert_eq!(t.rect.bottom(), t.rows[2].bottom() + m.task_foot);
        assert!(t.approve[0].is_none() && t.approve[2].is_none());
        let a = t.approve[1].unwrap();
        assert!(t.rows[1].contains(a.x, a.y) && a.right() < t.rows[1].right());
        // The header's buttons come before the header, the approve button
        // before its row.
        assert_eq!(hit(&l, t.mode.x + 1.0, t.mode.y + 1.0), Hit::TasksMode);
        assert_eq!(hit(&l, t.add.x + 1.0, t.add.y + 1.0), Hit::TasksAdd);
        assert_eq!(hit(&l, 20.0, t.header.y + 1.0), Hit::TasksHeader);
        assert_eq!(hit(&l, a.x + 1.0, a.y + 1.0), Hit::TaskApprove(1));
        assert_eq!(hit(&l, 20.0, t.rows[1].y + 1.0), Hit::Task(1));
        assert_eq!(hit(&l, t.give.x + 1.0, t.give.y + 1.0), Hit::TasksGive);
        assert!(t.mode.right() <= t.give.x && t.give.right() <= t.add.x);
        assert_eq!(t.add.right(), t.header.right() - TASKS_ADD_IN);
    }

    #[test]
    fn the_tome_sits_under_the_tasks_tile_its_stones_in_rows() {
        let m = Metrics::default();
        let l = cluster(&m, 1, false, Some(&[]), Some(6), Some(0.0));
        let tasks = l.tasks.as_ref().unwrap();
        let t = l.tome.as_ref().unwrap();
        let f = l.files.as_ref().unwrap();
        assert_eq!(t.rect.y - tasks.rect.bottom(), m.gap);
        assert_eq!(f.rect.y - t.rect.bottom(), m.gap);
        assert_eq!(t.stones.len(), 6);
        // Four to a row, the fifth under the first.
        assert_eq!(t.stones[1].y, t.stones[0].y);
        assert!(t.stones[1].x > t.stones[0].right());
        assert_eq!(t.stones[4].x, t.stones[0].x);
        assert!(t.stones[4].y > t.labels[0].bottom());
        assert!(t.stones[0].y >= t.header.bottom());
        assert!(t.rect.bottom() >= t.labels[5].bottom());
        let s = t.stones[5];
        assert_eq!(hit(&l, s.x + 1.0, s.y + 1.0), Hit::Stone(5));
        let lb = t.labels[2];
        assert_eq!(hit(&l, lb.x + 1.0, lb.y + 1.0), Hit::Stone(2));
        assert_eq!(hit(&l, 20.0, t.header.y + 1.0), Hit::TomeHeader);
        // Folded, the header alone.
        let folded = cluster(&m, 1, false, Some(&[]), Some(0), None);
        let t = folded.tome.unwrap();
        assert_eq!(t.rect, t.header);
        assert!(cluster(&m, 1, true, Some(&[]), Some(6), None)
            .tome
            .is_none());
    }

    #[test]
    fn an_empty_or_folded_tasks_tile_is_its_header() {
        let m = Metrics::default();
        let l = cluster(&m, 1, false, Some(&[]), None, None);
        let t = l.tasks.unwrap();
        assert!(t.rows.is_empty());
        assert_eq!(t.rect, t.header);
        assert_eq!(l.size.1, t.rect.bottom() + m.pad);
        assert!(cluster(&m, 1, true, Some(&[true]), None, None)
            .tasks
            .is_none());
    }

    #[test]
    fn tiles_stack_with_gaps() {
        let m = Metrics::default();
        let l = cluster(&m, 3, false, None, None, None);
        assert_eq!(l.tiles.len(), 3);
        assert_eq!(l.tiles[1].y - l.tiles[0].bottom(), m.gap);
        let add = l.add.unwrap();
        let shell = l.shell.unwrap();
        assert_eq!(add.y - l.tiles[2].bottom(), m.gap);
        // Side by side, together as wide as a tile.
        assert_eq!(shell.y, add.y);
        assert_eq!(shell.x - add.right(), m.gap);
        assert_eq!(shell.right(), l.tiles[2].right());
        assert_eq!(add.x, l.tiles[2].x);
        assert_eq!(l.size.1, add.bottom() + m.pad);
        assert_eq!(hit(&l, 20.0, l.tiles[2].y + 1.0), Hit::Tile(2));
        assert_eq!(hit(&l, 20.0, add.y + 1.0), Hit::Add);
        assert_eq!(hit(&l, shell.x + 2.0, shell.y + 1.0), Hit::Shell);
        assert_eq!(hit(&l, 20.0, m.pad + 1.0), Hit::Header);
        assert_eq!(hit(&l, m.width - m.pad - 2.0, m.pad + 1.0), Hit::New);
        assert_eq!(hit(&l, 1.0, 1.0), Hit::Nothing);
    }

    #[test]
    fn a_button_lights_under_the_cursor_and_presses_only_where_it_went_down() {
        assert_eq!(button(Hit::Add, Hit::Nothing, None), Button::Idle);
        assert_eq!(button(Hit::Add, Hit::Add, None), Button::Hover);
        assert_eq!(button(Hit::Add, Hit::New, None), Button::Idle);
        assert_eq!(button(Hit::Add, Hit::Add, Some(Hit::Add)), Button::Pressed);
        // Held down on it, then slid off.
        assert_eq!(button(Hit::Add, Hit::Tile(0), Some(Hit::Add)), Button::Idle);
        // Held down on the header, then slid over it.
        assert_eq!(button(Hit::Add, Hit::Add, Some(Hit::Header)), Button::Idle);
        // Each tile is its own button.
        assert_eq!(button(Hit::Tile(1), Hit::Tile(0), None), Button::Idle);
        assert_eq!(button(Hit::Tile(1), Hit::Tile(1), None), Button::Hover);
    }

    #[test]
    fn the_header_its_plus_tiles_and_the_bottom_plus_light_up() {
        for h in [Hit::New, Hit::Header, Hit::Tile(3), Hit::Add, Hit::Shell] {
            assert!(h.lights(), "{h:?}");
        }
        for h in [Hit::FilesHeader, Hit::File(0), Hit::Nothing] {
            assert!(!h.lights(), "{h:?}");
        }
    }

    #[test]
    fn a_browser_button_sits_on_the_second_line_of_marked_tiles_only() {
        let m = Metrics::default();
        let mut l = cluster(&m, 3, false, None, None, None);
        assert_eq!(l.marks, vec![None; 3]);
        mark(&mut l, &m, &[false, true], &[]);
        assert_eq!(l.marks.len(), 3);
        assert!(l.marks[0].is_none() && l.marks[2].is_none());
        let b = l.marks[1].unwrap();
        let t = l.tiles[1];
        assert_eq!(b.right(), t.right() - 4.0);
        assert!(b.y > t.y + t.h / 2.0 - 4.0 && b.bottom() < t.bottom());
        // The button wins over its tile, the rest of the tile is the tile.
        assert_eq!(hit(&l, b.x + 1.0, b.y + 1.0), Hit::Browser(1));
        assert_eq!(hit(&l, t.x + 1.0, b.y + 1.0), Hit::Tile(1));
        let unmarked = l.tiles[0];
        assert_eq!(hit(&l, b.x + 1.0, unmarked.y + 40.0), Hit::Tile(0));
        assert!(Hit::Browser(1).lights());
    }

    #[test]
    fn a_worktree_button_stands_left_of_the_browser_or_in_its_place() {
        let m = Metrics::default();
        let mut l = cluster(&m, 3, false, None, None, None);
        mark(&mut l, &m, &[true, false, false], &[true, true, false]);
        let (browser, both) = (l.marks[0].unwrap(), l.codes[0].unwrap());
        assert_eq!(both.right() + 2.0, browser.x);
        assert_eq!(both.y, browser.y);
        let alone = l.codes[1].unwrap();
        assert_eq!(alone.right(), l.tiles[1].right() - 4.0);
        assert!(l.codes[2].is_none());
        assert_eq!(hit(&l, both.x + 1.0, both.y + 1.0), Hit::Code(0));
        assert_eq!(hit(&l, alone.x + 1.0, alone.y + 1.0), Hit::Code(1));
        assert!(Hit::Code(0).lights());
    }

    #[test]
    fn collapsing_drops_the_browser_buttons() {
        let m = Metrics::default();
        let mut l = cluster(&m, 2, true, None, None, None);
        mark(&mut l, &m, &[true, true], &[true, true]);
        assert!(l.marks.is_empty() && l.codes.is_empty());
    }

    #[test]
    fn a_browser_docks_against_the_stage_on_the_side_with_room() {
        let tiles = [[12, 12, 292, 300]];
        let stage = [304, 12, 1360, 1068];
        assert_eq!(
            beside_stage(WORK, &tiles, stage, (400, 700), 12, 12, 300),
            Some([1372, 12, 1772, 712])
        );
        // Too wide for the space: it fills it.
        assert_eq!(
            beside_stage(WORK, &tiles, stage, (1280, 2000), 12, 12, 300),
            Some([1372, 12, 1908, 1068])
        );
        // Tiles and stage on the right: the browser goes left of the stage.
        let tiles = [[1628, 12, 1908, 300]];
        let stage = [560, 12, 1616, 1068];
        assert_eq!(
            beside_stage(WORK, &tiles, stage, (400, 700), 12, 12, 300),
            Some([148, 12, 548, 712])
        );
    }

    #[test]
    fn a_browser_stays_put_when_there_is_no_room_beside_the_stage() {
        let tiles = [[12, 12, 292, 300]];
        let stage = [304, 12, 1700, 1068];
        assert_eq!(
            beside_stage(WORK, &tiles, stage, (400, 700), 12, 12, 300),
            None
        );
    }

    #[test]
    fn a_tile_line_keeps_room_for_its_text() {
        let all = LineParts {
            diff: true,
            context: true,
            trace: true,
        };
        assert_eq!(tile_line(400.0, Some(60.0), Some(60.0), Some(50.0)), all);
        // The trace goes first, then the context warning, the diff last.
        let no_trace = LineParts {
            trace: false,
            ..all
        };
        assert_eq!(
            tile_line(210.0, Some(60.0), Some(60.0), Some(50.0)),
            no_trace
        );
        let diff_only = LineParts {
            diff: true,
            ..LineParts::default()
        };
        assert_eq!(
            tile_line(170.0, Some(60.0), Some(60.0), Some(50.0)),
            diff_only
        );
        assert_eq!(
            tile_line(100.0, Some(60.0), None, None),
            LineParts::default()
        );
        // A smaller part still fits where a bigger one did not.
        let trace = LineParts {
            trace: true,
            ..LineParts::default()
        };
        assert_eq!(
            tile_line(150.0, None, Some(60.0), Some(50.0)),
            LineParts {
                context: true,
                ..LineParts::default()
            }
        );
        assert_eq!(tile_line(140.0, Some(70.0), None, Some(50.0)), trace);
    }

    #[test]
    fn empty_cluster_still_offers_another_session() {
        let m = Metrics::default();
        let l = cluster(&m, 0, false, None, None, None);
        let add = l.add.unwrap();
        assert_eq!(add.y, l.header.bottom() + m.gap);
        assert_eq!(l.size.1, add.bottom() + m.pad);
    }

    const SPACING: Spacing = Spacing {
        margin: 12,
        gap: 12,
        reach: 10,
    };
    const WORK: (i32, i32, i32, i32) = (0, 0, 1920, 1080);

    #[test]
    fn beside_fills_the_work_area_without_tiles() {
        assert_eq!(beside(WORK, &[], 12, 12), ([12, 12, 1908, 1068], true));
    }

    #[test]
    fn beside_takes_the_space_right_of_the_column() {
        let column = [[12, 12, 292, 300], [12, 312, 292, 500]];
        assert_eq!(beside(WORK, &column, 12, 12), ([304, 12, 1908, 1068], true));
    }

    #[test]
    fn beside_takes_the_left_when_tiles_are_moved_right() {
        let tiles = [[1500, 100, 1780, 300], [1600, 400, 1880, 600]];
        assert_eq!(beside(WORK, &tiles, 12, 12), ([12, 12, 1488, 1068], false));
    }

    #[test]
    fn beside_ignores_tiles_on_another_monitor() {
        let elsewhere = [[-1900, 12, -1620, 300]];
        assert_eq!(
            beside(WORK, &elsewhere, 12, 12),
            ([12, 12, 1908, 1068], true)
        );
    }

    #[test]
    fn dock_keeps_the_size_against_the_tiles() {
        let area = [304, 12, 1908, 1068];
        assert_eq!(dock((800, 600), area, true), [304, 12, 1104, 612]);
        assert_eq!(dock((800, 600), area, false), [1108, 12, 1908, 612]);
    }

    #[test]
    fn dock_shrinks_to_fit_the_area() {
        let area = [304, 12, 1908, 1068];
        assert_eq!(dock((3000, 2000), area, true), area);
    }

    #[test]
    fn square_fills_top_to_bottom_against_the_tiles() {
        let area = [304, 12, 1908, 1068];
        assert_eq!(square(area, true), [304, 12, 1360, 1068]);
        assert_eq!(square(area, false), [852, 12, 1908, 1068]);
    }

    #[test]
    fn a_stage_gives_way_to_a_new_column() {
        let stage = [304, 40, 1500, 900];
        assert_eq!(
            follow_tiles(stage, Some(304), 596, (0, 1920), 400),
            Some([596, 40, 1500, 900])
        );
    }

    #[test]
    fn a_stage_against_the_tiles_takes_the_room_back() {
        let stage = [596, 40, 1500, 900];
        assert_eq!(
            follow_tiles(stage, Some(596), 304, (0, 1920), 400),
            Some([304, 40, 1500, 900])
        );
    }

    #[test]
    fn a_stage_away_from_the_tiles_stays() {
        let stage = [800, 40, 1500, 900];
        assert_eq!(follow_tiles(stage, Some(596), 304, (0, 1920), 400), None);
        assert_eq!(follow_tiles(stage, Some(304), 596, (0, 1920), 400), None);
    }

    #[test]
    fn a_narrow_stage_keeps_its_least_width() {
        let stage = [304, 40, 700, 900];
        assert_eq!(
            follow_tiles(stage, None, 596, (0, 1920), 400),
            Some([596, 40, 996, 900])
        );
        assert_eq!(
            follow_tiles(stage, None, 1600, (0, 1920), 400),
            Some([1600, 40, 1920, 900])
        );
    }

    #[test]
    fn a_stage_on_another_screen_stays() {
        let stage = [2000, 40, 3000, 900];
        assert_eq!(follow_tiles(stage, None, 596, (0, 1920), 400), None);
    }

    #[test]
    fn square_narrows_to_fit_the_area() {
        let area = [304, 12, 1000, 1068];
        assert_eq!(square(area, true), area);
    }

    #[test]
    fn snap_leaves_a_window_far_from_everything() {
        assert_eq!(snap((500, 400), (280, 200), WORK, &[], SPACING), (500, 400));
    }

    #[test]
    fn snap_pulls_to_the_screen_edges_at_the_margin() {
        assert_eq!(snap((5, 20), (280, 200), WORK, &[], SPACING), (12, 12));
        let right = 1920 - 12 - 280;
        let bottom = 1080 - 12 - 200;
        assert_eq!(
            snap((right + 7, bottom - 9), (280, 200), WORK, &[], SPACING),
            (right, bottom)
        );
    }

    #[test]
    fn snap_releases_past_the_reach() {
        assert_eq!(snap((23, 23), (280, 200), WORK, &[], SPACING), (23, 23));
    }

    #[test]
    fn snap_sits_beside_another_window_with_the_gap() {
        let other = (1000, 300, 1280, 500);
        // Dropped just right of it, tops a little off: beside it, tops lined up.
        assert_eq!(
            snap((1296, 306), (280, 100), WORK, &[other], SPACING),
            (1292, 300)
        );
        // Dropped just left of it.
        assert_eq!(
            snap((712, 350), (280, 100), WORK, &[other], SPACING),
            (708, 350)
        );
    }

    #[test]
    fn snap_stacks_below_and_lines_up_the_sides() {
        let other = (1000, 300, 1280, 500);
        assert_eq!(
            snap((1006, 515), (280, 100), WORK, &[other], SPACING),
            (1000, 512)
        );
        // A narrower window lines up its right side with the one above.
        assert_eq!(
            snap((1083, 515), (200, 100), WORK, &[other], SPACING),
            (1080, 512)
        );
    }

    #[test]
    fn snap_ignores_windows_that_are_not_near_on_the_other_axis() {
        // Same column, far below: the left edges must not pull together.
        let other = (1000, 900, 1280, 1000);
        assert_eq!(
            snap((1006, 300), (280, 100), WORK, &[other], SPACING),
            (1006, 300)
        );
    }

    #[test]
    fn snap_edges_moves_only_the_edges_being_dragged() {
        // Right edge dragged to 7 short of the margin: it snaps, the left
        // edge near the other margin does not, it is not being dragged.
        assert_eq!(
            snap_edges(
                [5, 300, 1901, 700],
                [false, false, true, false],
                WORK,
                &[],
                SPACING
            ),
            [5, 300, 1908, 700]
        );
        assert_eq!(
            snap_edges(
                [5, 300, 1901, 700],
                [true, false, true, false],
                WORK,
                &[],
                SPACING
            ),
            [12, 300, 1908, 700]
        );
    }

    #[test]
    fn snap_edges_stops_short_of_a_neighbour_and_lines_up_with_it() {
        let other = (1000, 300, 1280, 500);
        // A window to its left grows right: it stops the gap short.
        assert_eq!(
            snap_edges(
                [400, 350, 985, 700],
                [false, false, true, false],
                WORK,
                &[other],
                SPACING
            ),
            [400, 350, 988, 700]
        );
        // A window below grows up and down: the top stops the gap below, the
        // bottom is far from everything.
        assert_eq!(
            snap_edges(
                [1000, 518, 1280, 800],
                [false, true, false, true],
                WORK,
                &[other],
                SPACING
            ),
            [1000, 512, 1280, 800]
        );
        // A window below lines its right edge up with the one above.
        assert_eq!(
            snap_edges(
                [1000, 512, 1273, 800],
                [false, false, true, false],
                WORK,
                &[other],
                SPACING
            ),
            [1000, 512, 1280, 800]
        );
    }

    #[test]
    fn snap_edges_ignores_windows_that_are_not_near_on_the_other_axis() {
        let other = (1000, 900, 1280, 1000);
        assert_eq!(
            snap_edges(
                [400, 100, 985, 400],
                [false, false, true, false],
                WORK,
                &[other],
                SPACING
            ),
            [400, 100, 985, 400]
        );
    }

    #[test]
    fn grid_of_one_fills_the_area() {
        assert_eq!(grid(1, (0, 0, 1000, 800), 10), vec![[0, 0, 1000, 800]]);
        assert!(grid(0, (0, 0, 1000, 800), 10).is_empty());
    }

    #[test]
    fn grid_puts_two_side_by_side_and_four_two_by_two() {
        assert_eq!(
            grid(2, (0, 0, 1010, 800), 10),
            vec![[0, 0, 500, 800], [510, 0, 1010, 800]]
        );
        let four = grid(4, (0, 0, 1010, 810), 10);
        assert_eq!(four[0], [0, 0, 500, 400]);
        assert_eq!(four[3], [510, 410, 1010, 810]);
    }

    #[test]
    fn grid_stretches_a_short_last_row() {
        let three = grid(3, (0, 0, 1010, 810), 10);
        assert_eq!(three[0], [0, 0, 500, 400]);
        assert_eq!(three[1], [510, 0, 1010, 400]);
        assert_eq!(three[2], [0, 410, 1010, 810]);
        // Five: three columns, then two wider ones below.
        let five = grid(5, (0, 0, 920, 410), 10);
        assert_eq!(five[2], [620, 0, 920, 200]);
        assert_eq!(five[4], [465, 210, 920, 410]);
    }

    #[test]
    fn snap_picks_the_closest_line() {
        // Across, the screen margin is 8 away and the window's side 4.
        let other = (0, 400, 280, 600);
        assert_eq!(
            snap((4, 603), (280, 100), WORK, &[other], SPACING),
            (0, 612)
        );
        assert_eq!(
            snap((4, 615), (280, 100), WORK, &[other], SPACING),
            (0, 612)
        );
    }

    #[test]
    fn a_docked_pane_takes_its_whole_side_and_the_rest_share_the_grid() {
        let area = (10, 40, 1010, 840);
        let right = docked_grid(3, area, 8, Side::Right, Some(400), 200).unwrap();
        assert_eq!(right[2], [610, 40, 1010, 840]);
        assert_eq!(right[..2], grid(2, (10, 40, 602, 840), 8)[..]);
        let left = docked_grid(3, area, 8, Side::Left, Some(400), 200).unwrap();
        assert_eq!(left[2], [10, 40, 410, 840]);
        assert_eq!(left[..2], grid(2, (418, 40, 1010, 840), 8)[..]);
        let top = docked_grid(3, area, 8, Side::Top, Some(300), 200).unwrap();
        assert_eq!(top[2], [10, 40, 1010, 340]);
        assert_eq!(top[..2], grid(2, (10, 348, 1010, 840), 8)[..]);
        // Kept to leave the sessions their minimum, and itself its own.
        let wide = docked_grid(2, area, 8, Side::Right, Some(5000), 200).unwrap();
        assert_eq!(wide[1][0], 218);
        let narrow = docked_grid(2, area, 8, Side::Right, Some(10), 200).unwrap();
        assert_eq!(narrow[1][0], 810);
        // Without a size of its own it shares the area half and half.
        let half = docked_grid(2, area, 8, Side::Right, None, 200).unwrap();
        assert_eq!(half[1], [514, 40, 1010, 840]);
        assert_eq!(half[0], [10, 40, 506, 840]);
        let top = docked_grid(2, area, 8, Side::Top, None, 200).unwrap();
        assert_eq!(top[1], [10, 40, 1010, 436]);
        assert_eq!(
            docked_grid(1, area, 8, Side::Right, Some(400), 200),
            None,
            "alone it fills"
        );
        assert_eq!(
            docked_grid(2, (0, 0, 300, 100), 8, Side::Top, Some(50), 200),
            None
        );
    }

    #[test]
    fn dragging_the_seam_sizes_the_docked_pane_from_its_side() {
        let area = (10, 40, 1010, 840);
        for (side, size) in [(Side::Left, 400), (Side::Right, 400), (Side::Top, 300)] {
            let cells = docked_grid(3, area, 8, side, Some(size), 200).unwrap();
            let (at, _) = seam(side, cells[2], 8);
            assert_eq!(seam_size(side, area, 8, at), size, "{side:?}");
        }
    }

    #[test]
    fn grid_order_keeps_places_for_paused_sessions_and_appends_new_ones() {
        let v = |x: &[&str]| x.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let mut order = v(&["b", "paused", "a"]);
        assert_eq!(
            grid_order(&mut order, &v(&["a", "b", "c"])),
            v(&["b", "a", "c"])
        );
        assert_eq!(order, v(&["b", "paused", "a", "c"]));
        // Resumed, it is back between b and a.
        assert_eq!(
            grid_order(&mut order, &v(&["a", "b", "c", "paused"])),
            v(&["b", "paused", "a", "c"])
        );
        assert!(grid_order(&mut Vec::new(), &[]).is_empty());
    }

    #[test]
    fn tiles_follow_the_order_and_new_ones_go_last() {
        let order = ["b".to_string(), "a".to_string()];
        let mut ids = vec!["a", "b", "c"];
        ids.sort_by_key(|id| rank(&order, id));
        assert_eq!(ids, ["b", "a", "c"]);
    }

    #[test]
    fn reordered_puts_the_tiles_first_and_keeps_the_rest() {
        let v = |x: &[&str]| x.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            reordered(&v(&["a", "gone", "b"]), &v(&["b", "c", "a"])),
            v(&["b", "c", "a", "gone"])
        );
    }

    #[test]
    fn tile_slot_takes_the_nearest_place() {
        let tiles: Vec<Rect> = (0..3)
            .map(|i| Rect::new(0.0, 40.0 + 60.0 * i as f32, 280.0, 54.0))
            .collect();
        assert_eq!(tile_slot(&tiles, 40.0), 0);
        assert_eq!(tile_slot(&tiles, 75.0), 1);
        assert_eq!(tile_slot(&tiles, 400.0), 2);
        assert_eq!(tile_slot(&tiles, -50.0), 0);
        assert_eq!(tile_slot(&[], 10.0), 0);
    }

    #[test]
    fn swapped_cell_trades_two_places_and_keeps_the_rest() {
        assert_eq!(swapped_cell(0, Some((0, 2))), 2);
        assert_eq!(swapped_cell(2, Some((0, 2))), 0);
        assert_eq!(swapped_cell(1, Some((0, 2))), 1);
        assert_eq!(swapped_cell(1, Some((1, 1))), 1);
        assert_eq!(swapped_cell(3, None), 3);
    }

    #[test]
    fn clamp_into_keeps_a_box_inside_the_area() {
        let area = [10, 20, 110, 220];
        assert_eq!(clamp_into((30, 40), (50, 50), area), (30, 40));
        assert_eq!(clamp_into((-5, 0), (50, 50), area), (10, 20));
        assert_eq!(clamp_into((90, 200), (50, 50), area), (60, 170));
        assert_eq!(clamp_into((90, 200), (500, 50), area), (10, 170));
    }

    #[test]
    fn slot_at_finds_the_rect_under_the_point() {
        let rects = grid(2, (0, 0, 1010, 800), 10);
        assert_eq!(slot_at(&rects, 10, 10), Some(0));
        assert_eq!(slot_at(&rects, 700, 799), Some(1));
        // In the gap between them, or outside.
        assert_eq!(slot_at(&rects, 505, 10), None);
        assert_eq!(slot_at(&rects, 10, 800), None);
    }

    #[test]
    fn neighbour_follows_the_grid() {
        // Two above one wide.
        let rects = grid(3, (0, 0, 1010, 810), 10);
        assert_eq!(neighbour(&rects, 0, Dir::Right), Some(1));
        assert_eq!(neighbour(&rects, 1, Dir::Left), Some(0));
        assert_eq!(neighbour(&rects, 0, Dir::Down), Some(2));
        assert_eq!(neighbour(&rects, 1, Dir::Down), Some(2));
        // From the wide one, both are as near; the first wins.
        assert_eq!(neighbour(&rects, 2, Dir::Up), Some(0));
        assert_eq!(neighbour(&rects, 0, Dir::Left), None);
        assert_eq!(neighbour(&rects, 2, Dir::Down), None);
        // Two by two: straight across, not diagonal.
        let rects = grid(4, (0, 0, 1010, 810), 10);
        assert_eq!(neighbour(&rects, 3, Dir::Up), Some(1));
        assert_eq!(neighbour(&rects, 3, Dir::Left), Some(2));
        assert_eq!(neighbour(&[], 0, Dir::Up), None);
    }

    #[test]
    fn the_catchup_places_every_row_that_fits() {
        use CatchupKind::*;
        let rows = [Heading, Line { detail: true }, Line { detail: false }];
        let l = catchup(&rows, 1000.0, 0);
        assert_eq!(l.rows.len(), 3);
        assert!(l.more.is_none());
        let bottoms: Vec<f32> = l
            .rows
            .iter()
            .map(|r| match r {
                CatchupRow::Heading(r) => r.bottom(),
                CatchupRow::Line { rect, .. } => rect.bottom(),
            })
            .collect();
        assert!(bottoms.windows(2).all(|w| w[0] <= w[1]));
        assert!(l.size.1 > bottoms[2]);
        assert_eq!(catchup_hit(&l, 100.0, bottoms[0] + 2.0), Some(1));
        assert_eq!(catchup_hit(&l, 100.0, bottoms[0] - 2.0), None);
    }

    #[test]
    fn the_catchup_stops_at_its_height_and_says_there_is_more() {
        use CatchupKind::*;
        let mut rows = vec![Heading];
        rows.extend([Line { detail: false }; 40]);
        let l = catchup(&rows, 400.0, 0);
        assert!(l.rows.len() < rows.len());
        assert!(l.more.is_some());
        assert!(l.size.1 <= 400.0);
    }

    #[test]
    fn a_catchup_heading_never_stands_last_alone() {
        use CatchupKind::*;
        let one = catchup(&[Heading, Line { detail: false }], 1000.0, 0);
        let h = one.size.1;
        let rows = [
            Heading,
            Line { detail: false },
            Heading,
            Line { detail: false },
        ];
        // Room for the first project and the second's heading, not its line.
        let l = catchup(&rows, h + CATCHUP_HEADING_H + CATCHUP_LINE_H + 4.0, 0);
        assert!(matches!(l.rows.last(), Some(CatchupRow::Line { .. })));
    }

    #[test]
    fn a_scrolled_catchup_keeps_its_size_and_starts_at_its_row() {
        use CatchupKind::*;
        let mut rows = vec![Heading];
        rows.extend([Line { detail: false }; 40]);
        let top = catchup(&rows, 400.0, 0);
        let down = catchup(&rows, 400.0, 5);
        assert_eq!(top.size, down.size);
        assert_eq!(down.first, 5);
        assert_eq!(
            top.rows.len(),
            down.rows.len() + 1,
            "no heading to take room"
        );
        assert_eq!(down.below(rows.len()).start, 5 + down.rows.len());
        assert_eq!(top.more.unwrap().bottom(), down.more.unwrap().bottom());
    }

    #[test]
    fn the_catchup_scrolls_a_row_a_notch_and_stops_at_either_end() {
        use CatchupKind::*;
        let mut rows = vec![Heading];
        rows.extend([Line { detail: false }; 20]);
        assert_eq!(catchup_scroll(&rows, 400.0, 0, 1), 0, "already at the top");
        assert_eq!(catchup_scroll(&rows, 400.0, 0, -3), 3);
        assert_eq!(catchup_scroll(&rows, 400.0, 3, 2), 1);
        let end = catchup_scroll(&rows, 400.0, 0, -100);
        let l = catchup(&rows, 400.0, end);
        assert!(l.below(rows.len()).is_empty(), "the last row shows");
        assert!(!catchup(&rows, 400.0, end - 1).below(rows.len()).is_empty());
        assert!(l.more.is_some(), "still says what is above");
    }

    #[test]
    fn a_catchup_that_fits_never_scrolls() {
        use CatchupKind::*;
        let rows = [Heading, Line { detail: false }];
        assert_eq!(catchup_scroll(&rows, 1000.0, 0, -5), 0);
    }

    #[test]
    fn the_catchup_stands_a_little_above_the_middle() {
        assert_eq!(catchup_place((400, 300), [0, 0, 1000, 900]), (300, 200));
        assert_eq!(catchup_place((400, 1000), [0, 0, 1000, 900]), (300, 0));
    }

    #[test]
    fn the_cube_has_a_row_of_parts_over_a_well_of_three() {
        let m = Metrics::default();
        let l = cube(&m);
        assert_eq!(l.slots.len(), 3);
        assert_eq!(l.size.0, m.width);
        assert!(l.cube.right() < l.transmute.x);
        assert!(l.transmute.right() < l.main.x);
        assert!(l.main.right() <= m.width - m.pad + 0.01);
        assert!(l.transmute.w > 80.0);
        for r in &l.slots {
            assert!(r.x >= l.well.x && r.right() <= l.well.right() + 0.01);
            assert!(r.y >= l.well.y && r.bottom() <= l.well.bottom() + 0.01);
        }
        assert!(l.well.y > l.main.bottom());
        assert!(l.well.bottom() < l.size.1);
        let at = |r: &Rect| cube_hit(&l, r.x + 1.0, r.y + 1.0);
        assert_eq!(at(&l.slots[2]), CubeHit::Slot(2));
        assert_eq!(at(&l.main), CubeHit::Main);
        assert_eq!(at(&l.transmute), CubeHit::Transmute);
        assert_eq!(at(&l.cube), CubeHit::Nothing);
    }

    #[test]
    fn the_stash_is_three_by_three_inside_its_well_and_the_window() {
        let m = Metrics::default();
        let l = stash(&m);
        assert_eq!(l.slots.len(), 9);
        assert_eq!(l.size.0, m.width);
        for r in &l.slots {
            assert!(r.x >= l.well.x && r.right() <= l.well.right() + 0.01);
            assert!(r.y >= l.well.y && r.bottom() <= l.well.bottom() + 0.01);
        }
        assert!(l.well.bottom() < l.size.1);
        // Filled row by row, left to right.
        assert_eq!(l.slots[1].y, l.slots[0].y);
        assert!(l.slots[3].y > l.slots[2].y);
        let (x, y) = (l.slots[4].x + 1.0, l.slots[4].y + 1.0);
        assert_eq!(stash_hit(&l, x, y), Some(4));
        assert_eq!(stash_hit(&l, l.header.x + 1.0, l.header.y + 1.0), None);
    }
}
