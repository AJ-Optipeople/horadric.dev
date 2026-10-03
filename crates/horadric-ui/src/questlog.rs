//! The quest log: every quest a project has ever taken, as a branching
//! diagram down the left, the main line with each quest's lane growing off
//! it and back into it, and what came of the one picked on the right. A
//! quest leaves the quests tile once it is done and its session goes, so
//! this is where its story is read afterwards, and where its conversation
//! is opened again to read or to carry on. The project's other
//! conversations, the main sessions quests grow out of, sit on the main
//! line among them, so this is the way back to any of them too.
//!
//! One window, showing one project at a time: opening it for another
//! project switches it. It draws its own chrome like the stage, its header
//! the caption to drag by, and sizes from any edge. The app owns the data
//! and hands it a fresh list when the chronicle or the quest list changes.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::rc::Rc;

use horadric_core::chronicle::{self, Outcome, Quest, Row};
use horadric_core::Agent;
use windows::core::{w, Result, BOOL, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWA_WINDOW_CORNER_PREFERENCE,
    DWMWCP_ROUND, DWM_WINDOW_CORNER_PREFERENCE,
};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, InvalidateRect, MonitorFromPoint, ScreenToClient, ValidateRect, MONITORINFO,
    MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::UI::HiDpi::{
    GetDpiForMonitor, GetDpiForWindow, GetSystemMetricsForDpi, MDT_EFFECTIVE_DPI,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT, VK_DOWN, VK_END,
    VK_ESCAPE, VK_HOME, VK_NEXT, VK_PRIOR, VK_RETURN, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect, GetCursorPos, GetWindowLongPtrW,
    GetWindowRect, IsIconic, IsZoomed, LoadCursorW, LoadIconW, RegisterClassW, SetForegroundWindow,
    SetWindowLongPtrW, SetWindowPos, ShowWindow, CREATESTRUCTW, GWLP_USERDATA, HTBOTTOM,
    HTBOTTOMLEFT, HTBOTTOMRIGHT, HTCAPTION, HTCLIENT, HTLEFT, HTRIGHT, HTTOP, HTTOPLEFT,
    HTTOPRIGHT, IDC_ARROW, MINMAXINFO, NCCALCSIZE_PARAMS, SM_CXPADDEDBORDER, SM_CYFRAME,
    SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SW_RESTORE,
    SW_SHOWNORMAL, WINDOW_EX_STYLE, WM_CAPTURECHANGED, WM_CLOSE, WM_DPICHANGED, WM_ERASEBKGND,
    WM_GETMINMAXINFO, WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL,
    WM_NCACTIVATE, WM_NCCALCSIZE, WM_NCCREATE, WM_NCDESTROY, WM_NCHITTEST, WM_PAINT, WM_SIZE,
    WNDCLASSW, WS_MINIMIZEBOX, WS_POPUP, WS_SYSMENU, WS_THICKFRAME,
};

use crate::app::{self, Input};
use crate::layout::Rect;
use crate::render::{QuestDetail, QuestLogScene, QuestRowLook, Target};
use crate::theme::{self, Color};
use crate::window::Shared;

pub(crate) const CLASS: PCWSTR = w!("HoradricQuestLog");
/// The windows crate files this under `Win32_UI_Controls`.
const WM_MOUSELEAVE: u32 = 0x02A3;

/// The header, the caption the window is dragged by, in DIPs.
pub const HEADER_H: f32 = 58.0;
/// One quest's band of the diagram.
pub const ROW_H: f32 = 48.0;
/// The band over the rows where the main line is named.
const HEAD_H: f32 = 28.0;
/// The room each lane of the diagram takes, at most and at least.
const LANE_W: f32 = 14.0;
const LANE_MIN: f32 = 7.0;
/// Between the plate's edge and what is on it.
const MARGIN: f32 = 14.0;
const BUTTON_H: f32 = 30.0;
/// Narrower than this, the quest picked shows under the list, not beside.
const SIDE_BY_SIDE: f32 = 660.0;
/// The smallest the window may be made, in DIPs.
pub const MIN_SIZE: (f32, f32) = (380.0, 380.0);
/// The size it first opens at, in DIPs, when the screen has room.
const FIRST_SIZE: (f32, f32) = (940.0, 640.0);
/// How far in from its edge the window sizes, in DIPs.
const EDGE: f32 = 6.0;

/// Where every part of the window is, in DIPs.
#[derive(Debug, Clone, PartialEq)]
pub struct QuestLogLayout {
    pub size: (f32, f32),
    pub close: Rect,
    /// "QUEST LOG", and the project's name under it.
    pub label: Rect,
    pub project: Rect,
    /// The screen the diagram is on, the band naming the main line at its
    /// top, and the rows under it, which scroll.
    pub list: Rect,
    pub head: Rect,
    /// The key in the head's band for the agent's own picker of every
    /// conversation.
    pub all: Rect,
    pub rows: Rect,
    /// Where lane 0 starts and how wide each lane is.
    pub graph_x: f32,
    pub lane_w: f32,
    /// Where a row's words start, after the widest the diagram gets.
    pub text_x: f32,
    /// The section for the quest picked, its words, which scroll, and its
    /// two keys.
    pub detail: Rect,
    pub body: Rect,
    pub read: Rect,
    pub carry: Rect,
}

/// What a point in the window is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuestHit {
    Nothing,
    /// The header: it drags the window.
    Caption,
    Close,
    /// A quest's row, by its place in the diagram's rows.
    Row(usize),
    /// All conversations...
    All,
    Read,
    Carry,
}

/// The layout for a window `size` DIPs big whose diagram is `lanes` wide,
/// the main line counted.
pub fn layout(size: (f32, f32), lanes: usize) -> QuestLogLayout {
    let (w, h) = (size.0.max(MIN_SIZE.0), size.1.max(MIN_SIZE.1));
    let close = Rect::new(w - 12.0 - 34.0, 12.0, 34.0, 28.0);
    let label = Rect::new(MARGIN + 6.0, 12.0, close.x - MARGIN - 18.0, 16.0);
    let project = Rect::new(label.x, 28.0, label.w, 22.0);
    let top = HEADER_H;
    let bottom = h - MARGIN;
    let inner_w = w - 2.0 * MARGIN;
    let (list, detail) = if w >= SIDE_BY_SIDE {
        let list_w = ((inner_w - MARGIN) * 0.56).round();
        (
            Rect::new(MARGIN, top, list_w, bottom - top),
            Rect::new(
                MARGIN * 2.0 + list_w,
                top,
                inner_w - MARGIN - list_w,
                bottom - top,
            ),
        )
    } else {
        let list_h = ((bottom - top - MARGIN) * 0.52).round();
        (
            Rect::new(MARGIN, top, inner_w, list_h),
            Rect::new(
                MARGIN,
                top + list_h + MARGIN,
                inner_w,
                bottom - top - list_h - MARGIN,
            ),
        )
    };
    let head = Rect::new(list.x, list.y + 4.0, list.w, HEAD_H);
    let all_w = 136.0;
    let all = Rect::new(
        head.right() - 10.0 - all_w,
        head.y + 3.0,
        all_w,
        HEAD_H - 4.0,
    );
    let rows = Rect::new(
        list.x,
        head.bottom(),
        list.w,
        list.bottom() - head.bottom() - 4.0,
    );
    let graph_x = list.x + 12.0;
    // A diagram that would crowd out the words gets narrower lanes.
    let room = list.w * 0.4;
    let lanes = lanes.max(1) as f32;
    let lane_w = (room / lanes).clamp(LANE_MIN, LANE_W);
    let text_x = (graph_x + lanes * lane_w + 8.0).min(list.right() - 120.0);
    let pad = 12.0;
    let key_y = detail.bottom() - pad - BUTTON_H;
    let key_w = ((detail.w - 3.0 * pad) / 2.0).floor();
    let read = Rect::new(detail.x + pad, key_y, key_w, BUTTON_H);
    let carry = Rect::new(read.right() + pad, key_y, key_w, BUTTON_H);
    let body = Rect::new(
        detail.x + pad + 2.0,
        detail.y + pad,
        detail.w - 2.0 * pad - 4.0,
        (key_y - 10.0 - detail.y - pad).max(0.0),
    );
    QuestLogLayout {
        size: (w, h),
        close,
        label,
        project,
        list,
        head,
        all,
        rows,
        graph_x,
        lane_w,
        text_x,
        detail,
        body,
        read,
        carry,
    }
}

impl QuestLogLayout {
    /// The middle of `lane` across.
    pub fn lane_x(&self, lane: usize) -> f32 {
        self.graph_x + self.lane_w * (lane as f32 + 0.5)
    }

    /// Row `i`'s band, `scroll` DIPs down the list.
    pub fn row(&self, i: usize, scroll: f32) -> Rect {
        Rect::new(
            self.rows.x,
            self.rows.y + i as f32 * ROW_H - scroll,
            self.rows.w,
            ROW_H,
        )
    }

    /// The rows that show of `n`, at least partly.
    pub fn visible(&self, n: usize, scroll: f32) -> std::ops::Range<usize> {
        let first = (scroll / ROW_H).floor().max(0.0) as usize;
        let last = ((scroll + self.rows.h) / ROW_H).ceil().max(0.0) as usize;
        first.min(n)..last.min(n)
    }

    /// How far `n` rows scroll at most.
    pub fn max_scroll(&self, n: usize) -> f32 {
        (n as f32 * ROW_H - self.rows.h).max(0.0)
    }

    /// The scroll that brings row `i` whole into view, moving as little as
    /// it can from `scroll`.
    pub fn reveal(&self, i: usize, scroll: f32) -> f32 {
        let top = i as f32 * ROW_H;
        if top < scroll {
            top
        } else if top + ROW_H > scroll + self.rows.h {
            (top + ROW_H - self.rows.h).max(0.0)
        } else {
            scroll
        }
    }

    /// What is under `(x, y)`, with `n` rows scrolled `scroll` down.
    pub fn hit(&self, n: usize, scroll: f32, x: f32, y: f32) -> QuestHit {
        if self.close.contains(x, y) {
            return QuestHit::Close;
        }
        if y < HEADER_H {
            return QuestHit::Caption;
        }
        if self.read.contains(x, y) {
            return QuestHit::Read;
        }
        if self.carry.contains(x, y) {
            return QuestHit::Carry;
        }
        if self.all.contains(x, y) {
            return QuestHit::All;
        }
        if self.rows.contains(x, y) {
            let i = ((y - self.rows.y + scroll) / ROW_H).floor();
            if i >= 0.0 && (i as usize) < n {
                return QuestHit::Row(i as usize);
            }
        }
        QuestHit::Nothing
    }
}

/// An edge or corner of the window, which sizes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    Left,
    Right,
    Top,
    Bottom,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

/// The edge `(x, y)` is on in a window `w` by `h`, within `band` of it. A
/// corner reaches twice as far along each side, as Windows' own do.
pub fn edge(x: f32, y: f32, w: f32, h: f32, band: f32) -> Option<Edge> {
    let (left, right) = (x < band, x >= w - band);
    let (top, bottom) = (y < band, y >= h - band);
    let (near_left, near_right) = (x < band * 2.0, x >= w - band * 2.0);
    let (near_top, near_bottom) = (y < band * 2.0, y >= h - band * 2.0);
    Some(match () {
        _ if (top && near_left) || (left && near_top) => Edge::TopLeft,
        _ if (top && near_right) || (right && near_top) => Edge::TopRight,
        _ if (bottom && near_left) || (left && near_bottom) => Edge::BottomLeft,
        _ if (bottom && near_right) || (right && near_bottom) => Edge::BottomRight,
        _ if left => Edge::Left,
        _ if right => Edge::Right,
        _ if top => Edge::Top,
        _ if bottom => Edge::Bottom,
        _ => return None,
    })
}

/// For each row of the diagram, newest first, the quest on each lane as
/// the band starts, before its own dot: what colours the lines passing
/// through it.
pub fn occupants(rows: &[Row], width: usize) -> Vec<Vec<Option<usize>>> {
    let mut on: Vec<Option<usize>> = vec![None; width.max(1)];
    let mut out = vec![Vec::new(); rows.len()];
    for (r, row) in rows.iter().enumerate().rev() {
        out[r] = on.clone();
        // A conversation is on the trunk, which is nobody's to colour.
        if row.lane != 0 {
            if let Some(slot) = on.get_mut(row.lane) {
                *slot = Some(row.quest);
            }
        }
        for &(lane, quest) in &row.forks {
            if let Some(slot) = on.get_mut(lane) {
                *slot = Some(quest);
            }
        }
        for (lane, _) in &row.ends {
            if let Some(slot) = on.get_mut(*lane) {
                *slot = None;
            }
        }
        if row.end.is_some() {
            if let Some(slot) = on.get_mut(row.lane) {
                *slot = None;
            }
        }
    }
    out
}

/// How long ago, or for how long, at a glance: "40 min", "3 h", "2 d".
pub fn short_span(secs: u64) -> String {
    match secs {
        0..60 => "<1 min".into(),
        60..3600 => format!("{} min", secs / 60),
        3600..86_400 => format!("{} h", secs / 3600),
        86_400..1_209_600 => format!("{} d", secs / 86_400),
        _ => format!("{} w", secs / 604_800),
    }
}

/// How long a quest took, in full: "1 h 20 min", "3 d 4 h".
pub fn duration(secs: u64) -> String {
    let (d, h, m) = (secs / 86_400, secs % 86_400 / 3600, secs % 3600 / 60);
    match (d, h) {
        (0, 0) if m == 0 => "under a minute".into(),
        (0, 0) => format!("{m} min"),
        (0, h) if m == 0 => format!("{h} h"),
        (0, h) => format!("{h} h {m} min"),
        (d, h) => format!("{d} d {h} h"),
    }
}

/// What a row says after the outcome: how long ago it ended, or how long
/// it has run.
pub fn row_age(q: &Quest, now: u64) -> String {
    match (q.outcome, q.accepted, q.ended) {
        (Outcome::Working, Some(at), _) => format!("for {}", short_span(now.saturating_sub(at))),
        (_, _, Some(at)) if now.saturating_sub(at) < 60 => "just now".into(),
        (_, _, Some(at)) => format!("{} ago", short_span(now.saturating_sub(at))),
        _ => String::new(),
    }
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// The year, month and day of a count of days since 1970, proleptic
/// Gregorian.
fn civil(days: i64) -> (i64, usize, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as usize;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// When `at` was, in local time `offset` seconds ahead of UTC, as seen at
/// `now`: "today 14:05", "yesterday 09:30", "3 Oct 14:05", or with the
/// year when it is not this one.
pub fn when(at: u64, now: u64, offset: i64) -> String {
    let local = at as i64 + offset;
    let (day, secs) = (local.div_euclid(86_400), local.rem_euclid(86_400));
    let today = (now as i64 + offset).div_euclid(86_400);
    let clock = format!("{:02}:{:02}", secs / 3600, secs % 3600 / 60);
    let (year, month, date) = civil(day);
    let month = MONTHS[month - 1];
    match today - day {
        0 => format!("today {clock}"),
        1 => format!("yesterday {clock}"),
        _ if civil(today).0 == year => format!("{date} {month} {clock}"),
        _ => format!("{date} {month} {year}"),
    }
}

/// Seconds local time is ahead of UTC, from the local clock now: rounded
/// to the quarter hour, the finest any zone is cut to, so the second the
/// two clocks are read apart does not show.
fn utc_offset(now: u64) -> i64 {
    let t = unsafe { GetLocalTime() };
    let local = i64::from(t.wHour) * 3600 + i64::from(t.wMinute) * 60 + i64::from(t.wSecond);
    let utc = (now % 86_400) as i64;
    let mut d = local - utc;
    if d > 14 * 3600 {
        d -= 86_400;
    } else if d < -12 * 3600 {
        d += 86_400;
    }
    (d as f64 / 900.0).round() as i64 * 900
}

/// Where the quest came from: the quest it grew out of, the session that
/// added it, or the main line.
pub fn origin(q: &Quest, quests: &[Quest]) -> String {
    match (q.parent.and_then(|p| quests.get(p)), &q.added_by) {
        (Some(p), _) => format!("grew out of \"{}\"", one_line(&p.title)),
        (None, Some(by)) => format!("added by {by}"),
        (None, None) => "the main line".into(),
    }
}

fn one_line(s: &str) -> String {
    horadric_core::tasks::one_line(s).to_string()
}

/// The colour a quest burns in, or a conversation on the main line.
pub fn color_of(q: &Quest) -> Color {
    if q.main {
        conversation_color()
    } else {
        outcome_color(q.outcome)
    }
}

/// A conversation's own colour, apart from every lamp a quest can be lit
/// by.
pub fn conversation_color() -> Color {
    theme::palette().magic
}

/// The word a row and the detail show for how a quest stands, or that it
/// is a conversation.
pub fn word_of(q: &Quest) -> &'static str {
    if q.main {
        "conversation"
    } else {
        q.outcome.word()
    }
}

/// The colour a quest burns in, the lamp colours the quests tile uses, so
/// a quest reads the same in both.
pub fn outcome_color(o: Outcome) -> Color {
    match o {
        Outcome::Working => theme::working(),
        Outcome::Review => theme::waiting(),
        Outcome::Blocked => theme::error(),
        Outcome::Done => theme::done(),
        Outcome::Returned => theme::idle(),
    }
}

/// The file a quest's session is written out to, under the store's
/// `chronicle` folder: its id, kept to what any file system takes.
pub fn session_file(id: &str) -> String {
    let safe: String = id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '-'
            }
        })
        .collect();
    format!("chronicle/{}.md", safe.trim_matches('.'))
}

/// The page "Read the session" opens: what the quest was and how it went,
/// then the conversation.
pub fn session_doc(q: &Quest, transcript: &str) -> String {
    let mut out = format!("# {}\n\n", one_line(&q.title));
    if q.main {
        out.push_str(&transcript_part(transcript));
        return out;
    }
    let result = q.result().trim();
    out.push_str(&format!("**{}**", q.outcome.word()));
    if !result.is_empty() {
        out.push_str(&format!(". {}", one_line(result)));
    }
    out.push_str("\n\n");
    if !q.notes.is_empty() {
        out.push_str(&q.notes.join("\n"));
        out.push_str("\n\n");
    }
    for c in &q.commits {
        out.push_str(&format!("- `{}` {}\n", c.hash, c.subject));
    }
    if !q.commits.is_empty() {
        out.push('\n');
    }
    out.push_str(&transcript_part(transcript));
    out
}

fn transcript_part(transcript: &str) -> String {
    let mut out = "# The conversation\n\n".to_string();
    if transcript.trim().is_empty() {
        out.push_str("The conversation has nothing to read.\n");
    } else {
        out.push_str(transcript);
    }
    out
}

/// Something the window asks of the app, which owns the quests and the
/// sessions.
pub enum Ask {
    /// Show the quest log for the project with this key.
    Open(String),
    Close,
    /// Write out the conversation of the quest with this id, of the
    /// project with this key, and show it on the stage.
    Read(String, String),
    /// Carry on that conversation in a new session.
    Carry(String, String),
    /// Start a session in the project with this key on the agent's own
    /// picker of every conversation.
    All(String),
}

pub fn register_class() -> Result<()> {
    unsafe {
        let instance = GetModuleHandleW(None)?;
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            lpszClassName: CLASS,
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            // The app's icon, which the build put in the executable as
            // resource 1, so the window shows as Horadric on the taskbar.
            hIcon: LoadIconW(
                Some(instance.into()),
                PCWSTR(std::ptr::without_provenance(1)),
            )
            .unwrap_or_default(),
            ..Default::default()
        };
        RegisterClassW(&wc);
        Ok(())
    }
}

/// What the window shows, as the app last handed it.
struct Data {
    key: String,
    name: String,
    quests: Vec<Quest>,
    rows: Vec<Row>,
    width: usize,
    occupants: Vec<Vec<Option<usize>>>,
}

impl Data {
    fn new(key: String, name: String, quests: Vec<Quest>) -> Self {
        let (rows, width) = chronicle::graph(&quests);
        let occupants = occupants(&rows, width);
        Data {
            key,
            name,
            quests,
            rows,
            width,
            occupants,
        }
    }

    /// The row of the quest with this id.
    fn row_of(&self, id: &str) -> Option<usize> {
        self.rows
            .iter()
            .position(|r| self.quests.get(r.quest).is_some_and(|q| q.id == id))
    }
}

pub struct QuestLog {
    pub hwnd: HWND,
    shared: Rc<Shared>,
    target: RefCell<Option<Target>>,
    data: RefCell<Data>,
    /// The quest picked, by id, so it stays picked as the list changes.
    picked: RefCell<Option<String>>,
    scroll: Cell<f32>,
    /// How far the words about the quest picked are scrolled, and how tall
    /// they were when last drawn.
    detail_scroll: Cell<f32>,
    detail_h: Cell<f32>,
    hot: Cell<QuestHit>,
    pressed: Cell<Option<QuestHit>>,
    tracking: Cell<bool>,
    active: Cell<bool>,
}

thread_local! {
    /// Where the window was when it last closed, so it opens there again.
    static LAST: Cell<Option<RECT>> = const { Cell::new(None) };
}

impl QuestLog {
    /// Opens the window for the project `key`, named `name`, with its
    /// quests, oldest first as [`chronicle::quests`] gives them.
    pub fn open(
        shared: Rc<Shared>,
        key: String,
        name: String,
        quests: Vec<Quest>,
    ) -> Result<Box<Self>> {
        let data = Data::new(key, name, quests);
        let picked = data
            .rows
            .first()
            .and_then(|r| data.quests.get(r.quest))
            .map(|q| q.id.clone());
        let mut win = Box::new(QuestLog {
            hwnd: HWND::default(),
            shared,
            target: RefCell::new(None),
            data: RefCell::new(data),
            picked: RefCell::new(picked),
            scroll: Cell::new(0.0),
            detail_scroll: Cell::new(0.0),
            detail_h: Cell::new(0.0),
            hot: Cell::new(QuestHit::Nothing),
            pressed: Cell::new(None),
            tracking: Cell::new(false),
            active: Cell::new(true),
        });
        let place = LAST.with(Cell::get).unwrap_or_else(first_place);
        unsafe {
            // Born where it shows and at its size: a window that starts
            // elsewhere and moves has been seen not to paint.
            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                CLASS,
                w!("Horadric quest log"),
                WS_POPUP | WS_THICKFRAME | WS_SYSMENU | WS_MINIMIZEBOX,
                place.left,
                place.top,
                place.right - place.left,
                place.bottom - place.top,
                None,
                None,
                Some(GetModuleHandleW(None)?.into()),
                Some(&*win as *const QuestLog as *const c_void),
            )?;
            win.hwnd = hwnd;
            let dark = BOOL(1);
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_USE_IMMERSIVE_DARK_MODE,
                &dark as *const _ as *const c_void,
                std::mem::size_of::<BOOL>() as u32,
            );
            let pref: DWM_WINDOW_CORNER_PREFERENCE = DWMWCP_ROUND;
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_WINDOW_CORNER_PREFERENCE,
                &pref as *const _ as *const c_void,
                std::mem::size_of::<DWM_WINDOW_CORNER_PREFERENCE>() as u32,
            );
            // The frame is worked out again without a title bar, now that
            // the window answers for its own.
            let _ = SetWindowPos(
                hwnd,
                None,
                0,
                0,
                0,
                0,
                SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
            let _ = ShowWindow(hwnd, SW_SHOWNORMAL);
            let _ = SetForegroundWindow(hwnd);
        }
        Ok(win)
    }

    /// Shows the project `key` instead, or the same one's quests as they
    /// are now. A new project starts at its newest quest; the same one
    /// keeps the quest picked and where the list was scrolled.
    pub fn set(&self, key: String, name: String, quests: Vec<Quest>) {
        let same = self.data.borrow().key == key;
        if same && self.data.borrow().quests == quests && self.data.borrow().name == name {
            return;
        }
        let data = Data::new(key, name, quests);
        let kept = self
            .picked
            .borrow()
            .as_deref()
            .filter(|_| same)
            .and_then(|id| data.row_of(id));
        if kept.is_none() {
            let newest = data.rows.first().and_then(|r| data.quests.get(r.quest));
            *self.picked.borrow_mut() = newest.map(|q| q.id.clone());
            self.scroll.set(0.0);
            self.detail_scroll.set(0.0);
        }
        *self.data.borrow_mut() = data;
        self.clamp_scroll();
        self.invalidate();
    }

    pub fn key(&self) -> String {
        self.data.borrow().key.clone()
    }

    /// Brings the window to the front, out of the taskbar if minimised.
    pub fn raise(&self) {
        unsafe {
            if IsIconic(self.hwnd).as_bool() {
                let _ = ShowWindow(self.hwnd, SW_RESTORE);
            }
            let _ = SetForegroundWindow(self.hwnd);
        }
    }

    pub fn destroy(&self) {
        unsafe {
            if !IsIconic(self.hwnd).as_bool() && !IsZoomed(self.hwnd).as_bool() {
                let mut r = RECT::default();
                if GetWindowRect(self.hwnd, &mut r).is_ok() {
                    LAST.with(|l| l.set(Some(r)));
                }
            }
            let _ = DestroyWindow(self.hwnd);
        }
    }

    pub fn invalidate(&self) {
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    fn dpi(&self) -> u32 {
        unsafe { GetDpiForWindow(self.hwnd) }.max(96)
    }

    fn scale(&self) -> f32 {
        self.dpi() as f32 / 96.0
    }

    fn client_px(&self) -> (i32, i32) {
        let mut r = RECT::default();
        unsafe {
            let _ = GetClientRect(self.hwnd, &mut r);
        }
        (r.right - r.left, r.bottom - r.top)
    }

    fn layout(&self) -> QuestLogLayout {
        let (w, h) = self.client_px();
        let s = self.scale();
        layout((w as f32 / s, h as f32 / s), self.data.borrow().width)
    }

    fn clamp_scroll(&self) {
        let l = self.layout();
        let n = self.data.borrow().rows.len();
        self.scroll
            .set(self.scroll.get().clamp(0.0, l.max_scroll(n)));
        let most = (self.detail_h.get() - l.body.h).max(0.0);
        self.detail_scroll
            .set(self.detail_scroll.get().clamp(0.0, most));
    }

    /// The row of the quest picked.
    fn picked_row(&self) -> Option<usize> {
        let id = self.picked.borrow();
        self.data.borrow().row_of(id.as_deref()?)
    }

    /// The quest picked, if it still is one.
    fn picked_quest(&self) -> Option<Quest> {
        let row = self.picked_row()?;
        let data = self.data.borrow();
        data.quests.get(data.rows.get(row)?.quest).cloned()
    }

    fn pick_row(&self, row: usize) {
        let id = {
            let data = self.data.borrow();
            let Some(q) = data.rows.get(row).and_then(|r| data.quests.get(r.quest)) else {
                return;
            };
            q.id.clone()
        };
        if self.picked.borrow().as_deref() != Some(id.as_str()) {
            *self.picked.borrow_mut() = Some(id);
            self.detail_scroll.set(0.0);
        }
        let l = self.layout();
        self.scroll.set(l.reveal(row, self.scroll.get()));
        self.invalidate();
    }

    fn paint(&self) {
        let (w, h) = self.client_px();
        if w <= 0 || h <= 0 {
            return;
        }
        let mut slot = self.target.borrow_mut();
        if slot.is_none() {
            match Target::new(&self.shared.gpu, self.hwnd, w as u32, h as u32, self.dpi()) {
                Ok(t) => *slot = Some(t),
                Err(e) => {
                    eprintln!("horadric: render target for the quest log: {e}");
                    return;
                }
            }
        }
        let l = self.layout();
        let data = self.data.borrow();
        let now = unix_now();
        let offset = utc_offset(now);
        let looks: Vec<QuestRowLook> = data
            .rows
            .iter()
            .map(|r| {
                let q = &data.quests[r.quest];
                QuestRowLook {
                    title: one_line(&q.title),
                    word: word_of(q),
                    age: row_age(q, now),
                    result: match q.accepted {
                        Some(at) if q.main => format!("Started {}", when(at, now, offset)),
                        _ => one_line(q.result()),
                    },
                    outcome: q.outcome,
                    color: color_of(q),
                    main: q.main,
                }
            })
            .collect();
        let colors: Vec<Color> = data.quests.iter().map(color_of).collect();
        let picked = self.picked_row();
        let detail = picked
            .and_then(|i| data.quests.get(data.rows[i].quest))
            .map(|q| detail_of(q, &data.quests, now, offset));
        let scene = QuestLogScene {
            layout: &l,
            name: &data.name,
            rows: &data.rows,
            looks: &looks,
            occupants: &data.occupants,
            colors: &colors,
            picked,
            hot: self.hot.get(),
            pressed: self.pressed.get(),
            scroll: self.scroll.get(),
            detail: detail.as_ref(),
            detail_scroll: self.detail_scroll.get(),
            active: self.active.get(),
        };
        let drawn = slot
            .as_ref()
            .map(|t| t.draw_questlog(&self.shared.gpu, &self.shared.metrics, &scene));
        match drawn {
            Some(Ok(h)) => self.detail_h.set(h),
            Some(Err(_)) => *slot = None,
            None => {}
        }
    }

    fn point(&self, lparam: LPARAM) -> (f32, f32) {
        let s = self.scale();
        let x = (lparam.0 & 0xffff) as i16 as f32 / s;
        let y = ((lparam.0 >> 16) & 0xffff) as i16 as f32 / s;
        (x, y)
    }

    fn hit_at(&self, x: f32, y: f32) -> QuestHit {
        let n = self.data.borrow().rows.len();
        let hit = self.layout().hit(n, self.scroll.get(), x, y);
        // A key with nothing to do lights for nothing.
        match hit {
            QuestHit::Read if !self.picked_quest().as_ref().is_some_and(can_read) => {
                QuestHit::Nothing
            }
            QuestHit::Carry if !self.picked_quest().as_ref().is_some_and(can_carry) => {
                QuestHit::Nothing
            }
            h => h,
        }
    }

    fn hover(&self, hot: QuestHit) {
        if self.hot.replace(hot) != hot {
            self.invalidate();
        }
    }

    fn track(&self) {
        if self.tracking.replace(true) {
            return;
        }
        let mut t = TRACKMOUSEEVENT {
            cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
            dwFlags: TME_LEAVE,
            hwndTrack: self.hwnd,
            dwHoverTime: 0,
        };
        unsafe {
            let _ = TrackMouseEvent(&mut t);
        }
    }

    fn click(&self, hit: QuestHit) {
        let key = self.key();
        match hit {
            QuestHit::Close => app::push(Input::QuestLog(Ask::Close)),
            QuestHit::Row(i) => self.pick_row(i),
            QuestHit::Read => {
                if let Some(q) = self.picked_quest() {
                    app::push(Input::QuestLog(Ask::Read(key, q.id)));
                }
            }
            QuestHit::Carry => {
                if let Some(q) = self.picked_quest() {
                    app::push(Input::QuestLog(Ask::Carry(key, q.id)));
                }
            }
            QuestHit::All => app::push(Input::QuestLog(Ask::All(key))),
            QuestHit::Caption | QuestHit::Nothing => {}
        }
    }

    /// The wheel over the words about the quest scrolls them, anywhere
    /// else the list.
    fn wheel(&self, wparam: WPARAM, lparam: LPARAM) {
        let notches = ((wparam.0 >> 16) & 0xffff) as i16 as f32 / 120.0;
        // Screen coordinates, unlike every other mouse message.
        let mut p = POINT {
            x: (lparam.0 & 0xffff) as i16 as i32,
            y: ((lparam.0 >> 16) & 0xffff) as i16 as i32,
        };
        unsafe {
            let _ = ScreenToClient(self.hwnd, &mut p);
        }
        let s = self.scale();
        let (x, y) = (p.x as f32 / s, p.y as f32 / s);
        let l = self.layout();
        if l.detail.contains(x, y) {
            self.detail_scroll
                .set(self.detail_scroll.get() - notches * 3.0 * 20.0);
        } else {
            self.scroll.set(self.scroll.get() - notches * ROW_H * 1.5);
        }
        self.clamp_scroll();
        self.hot.set(self.hit_at(x, y));
        self.invalidate();
    }

    fn key_down(&self, vk: u16) {
        let n = self.data.borrow().rows.len();
        if vk == VK_ESCAPE.0 {
            app::push(Input::QuestLog(Ask::Close));
            return;
        }
        if vk == VK_RETURN.0 {
            if self.picked_quest().as_ref().is_some_and(can_read) {
                self.click(QuestHit::Read);
            }
            return;
        }
        if n == 0 {
            return;
        }
        let at = self.picked_row().unwrap_or(0);
        let page = (self.layout().rows.h / ROW_H).floor().max(1.0) as usize;
        let to = match vk {
            v if v == VK_UP.0 => at.saturating_sub(1),
            v if v == VK_DOWN.0 => (at + 1).min(n - 1),
            v if v == VK_PRIOR.0 => at.saturating_sub(page),
            v if v == VK_NEXT.0 => (at + page).min(n - 1),
            v if v == VK_HOME.0 => 0,
            v if v == VK_END.0 => n - 1,
            _ => return,
        };
        self.pick_row(to);
    }

    /// How thick the sizing frame is, in pixels: how far a maximised
    /// window hangs past the screen.
    fn frame(&self) -> i32 {
        let dpi = self.dpi();
        unsafe {
            GetSystemMetricsForDpi(SM_CYFRAME, dpi) + GetSystemMetricsForDpi(SM_CXPADDEDBORDER, dpi)
        }
    }

    fn handle(&self, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
        match msg {
            // The client is the whole window: the plate is drawn edge to
            // edge. Maximised, the frame hangs past the screen's edge, so
            // the client starts where the screen does.
            WM_NCCALCSIZE if wparam.0 != 0 => {
                if unsafe { IsZoomed(self.hwnd) }.as_bool() {
                    let f = self.frame();
                    let params = unsafe { &mut *(lparam.0 as *mut NCCALCSIZE_PARAMS) };
                    let r = &mut params.rgrc[0];
                    r.left += f;
                    r.top += f;
                    r.right -= f;
                    r.bottom -= f;
                }
                Some(LRESULT(0))
            }
            WM_NCHITTEST => {
                let mut p = POINT {
                    x: (lparam.0 & 0xffff) as i16 as i32,
                    y: ((lparam.0 >> 16) & 0xffff) as i16 as i32,
                };
                unsafe {
                    let _ = ScreenToClient(self.hwnd, &mut p);
                }
                let s = self.scale();
                let (x, y) = (p.x as f32 / s, p.y as f32 / s);
                let l = self.layout();
                let zoomed = unsafe { IsZoomed(self.hwnd) }.as_bool();
                if !zoomed {
                    if let Some(e) = edge(x, y, l.size.0, l.size.1, EDGE) {
                        return Some(LRESULT(ht(e) as isize));
                    }
                }
                let hit = match self.hit_at(x, y) {
                    QuestHit::Caption => HTCAPTION,
                    _ => HTCLIENT,
                };
                Some(LRESULT(hit as isize))
            }
            WM_GETMINMAXINFO => {
                let info = unsafe { &mut *(lparam.0 as *mut MINMAXINFO) };
                let s = unsafe { GetDpiForWindow(self.hwnd) }.max(96) as f32 / 96.0;
                info.ptMinTrackSize = POINT {
                    x: (MIN_SIZE.0 * s).round() as i32,
                    y: (MIN_SIZE.1 * s).round() as i32,
                };
                Some(LRESULT(0))
            }
            // Windows repaints its frame on activation; with none to see,
            // only the header goes quiet.
            WM_NCACTIVATE => {
                if self.active.replace(wparam.0 != 0) != (wparam.0 != 0) {
                    self.invalidate();
                }
                Some(unsafe { DefWindowProcW(self.hwnd, msg, wparam, LPARAM(-1)) })
            }
            WM_PAINT => {
                self.paint();
                unsafe {
                    let _ = ValidateRect(Some(self.hwnd), None);
                }
                Some(LRESULT(0))
            }
            WM_ERASEBKGND => Some(LRESULT(1)),
            WM_SIZE => {
                let w = (lparam.0 & 0xffff) as u32;
                let h = ((lparam.0 >> 16) & 0xffff) as u32;
                if let Some(t) = self.target.borrow().as_ref() {
                    if t.resize(w, h).is_err() {
                        self.target.replace(None);
                    }
                }
                self.clamp_scroll();
                self.invalidate();
                Some(LRESULT(0))
            }
            WM_DPICHANGED => {
                self.target.replace(None);
                let suggested = unsafe { *(lparam.0 as *const RECT) };
                unsafe {
                    let _ = SetWindowPos(
                        self.hwnd,
                        None,
                        suggested.left,
                        suggested.top,
                        suggested.right - suggested.left,
                        suggested.bottom - suggested.top,
                        SWP_NOACTIVATE | SWP_NOZORDER,
                    );
                }
                self.invalidate();
                Some(LRESULT(0))
            }
            WM_MOUSEMOVE => {
                self.track();
                let (x, y) = self.point(lparam);
                self.hover(self.hit_at(x, y));
                Some(LRESULT(0))
            }
            WM_LBUTTONDOWN => {
                let (x, y) = self.point(lparam);
                let hit = self.hit_at(x, y);
                // A row picks as it goes down, as a list does.
                if let QuestHit::Row(i) = hit {
                    self.pick_row(i);
                    return Some(LRESULT(0));
                }
                unsafe {
                    SetCapture(self.hwnd);
                }
                if self.pressed.replace(Some(hit)) != Some(hit) {
                    self.invalidate();
                }
                Some(LRESULT(0))
            }
            WM_LBUTTONUP => {
                // Taken first: releasing capture sends WM_CAPTURECHANGED at
                // once, and that clears the press.
                let pressed = self.pressed.take();
                unsafe {
                    let _ = ReleaseCapture();
                }
                let (x, y) = self.point(lparam);
                let hit = self.hit_at(x, y);
                self.invalidate();
                // Only where the press began, as a button does.
                if pressed == Some(hit) {
                    self.click(hit);
                }
                Some(LRESULT(0))
            }
            WM_CAPTURECHANGED => {
                if self.pressed.take().is_some() {
                    self.invalidate();
                }
                None
            }
            WM_MOUSELEAVE => {
                self.tracking.set(false);
                self.hover(QuestHit::Nothing);
                Some(LRESULT(0))
            }
            WM_MOUSEWHEEL => {
                self.wheel(wparam, lparam);
                Some(LRESULT(0))
            }
            WM_KEYDOWN => {
                self.key_down(wparam.0 as u16);
                Some(LRESULT(0))
            }
            // Alt+F4 and the taskbar's Close go through the app, which
            // owns the window.
            WM_CLOSE => {
                app::push(Input::QuestLog(Ask::Close));
                Some(LRESULT(0))
            }
            _ => None,
        }
    }
}

/// The hit test answer for an edge.
fn ht(e: Edge) -> u32 {
    match e {
        Edge::Left => HTLEFT,
        Edge::Right => HTRIGHT,
        Edge::Top => HTTOP,
        Edge::Bottom => HTBOTTOM,
        Edge::TopLeft => HTTOPLEFT,
        Edge::TopRight => HTTOPRIGHT,
        Edge::BottomLeft => HTBOTTOMLEFT,
        Edge::BottomRight => HTBOTTOMRIGHT,
    }
}

/// The quest's conversation can be written out to read: only Claude
/// Code's transcripts are read.
pub fn can_read(q: &Quest) -> bool {
    q.conversation.is_some() && q.agent == Agent::Claude
}

pub fn can_carry(q: &Quest) -> bool {
    q.conversation.is_some()
}

/// What the section beside the list says about a conversation on the main
/// line: when, and the quests it added.
fn talk_detail(q: &Quest, quests: &[Quest], now: u64, offset: i64) -> QuestDetail {
    let mut facts: Vec<(&'static str, String)> = Vec::new();
    if let Some(at) = q.accepted {
        facts.push(("Started", when(at, now, offset)));
    }
    if let Some(at) = q.ended {
        facts.push(("Last touched", when(at, now, offset)));
    }
    if q.agent != Agent::Claude {
        facts.push(("Agent", q.agent.label().to_string()));
    }
    let me = quests.iter().position(|o| o.main && o.id == q.id);
    let added = quests
        .iter()
        .filter(|c| me.is_some() && c.parent == me)
        .map(|c| one_line(&c.title))
        .collect();
    QuestDetail {
        title: one_line(&q.title),
        word: word_of(q),
        color: color_of(q),
        facts,
        result: String::new(),
        notes: String::new(),
        commits: Vec::new(),
        main: true,
        added,
        can_read: can_read(q),
        can_carry: can_carry(q),
    }
}

/// What the section beside the list says about `q`.
fn detail_of(q: &Quest, quests: &[Quest], now: u64, offset: i64) -> QuestDetail {
    if q.main {
        return talk_detail(q, quests, now, offset);
    }
    let mut facts: Vec<(&'static str, String)> = Vec::new();
    if let Some(at) = q.accepted {
        facts.push(("Accepted", when(at, now, offset)));
    }
    match (q.outcome, q.accepted, q.ended) {
        (Outcome::Working, Some(at), _) => {
            facts.push(("Running", duration(now.saturating_sub(at))));
        }
        // Review and blocked still hold the quest: they are where it
        // stands, not how it ended.
        (o, _, Some(end)) if !o.over() => facts.push(("Marked", when(end, now, offset))),
        (_, accepted, Some(end)) => {
            facts.push(("Ended", when(end, now, offset)));
            if let Some(at) = accepted.filter(|a| *a <= end) {
                facts.push(("Took", duration(end - at)));
            }
        }
        _ => {}
    }
    if !q.name.is_empty() {
        facts.push(("Session", q.name.clone()));
    }
    facts.push(("From", origin(q, quests)));
    if let Some(b) = &q.merged {
        facts.push(("Merged", b.clone()));
    }
    QuestDetail {
        title: one_line(&q.title),
        word: q.outcome.word(),
        color: outcome_color(q.outcome),
        facts,
        result: q.result().trim().to_string(),
        notes: q.notes.join("\n"),
        commits: q
            .commits
            .iter()
            .map(|c| (c.hash.clone(), one_line(&c.subject)))
            .collect(),
        main: false,
        added: Vec::new(),
        can_read: can_read(q),
        can_carry: can_carry(q),
    }
}

/// Where the window first opens: in the middle of the screen the cursor is
/// on, at its first size or as much of it as the screen has room for.
fn first_place() -> RECT {
    let mut cursor = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut cursor);
    }
    let monitor = unsafe { MonitorFromPoint(cursor, MONITOR_DEFAULTTONEAREST) };
    let (mut dx, mut dy) = (96u32, 96u32);
    unsafe {
        let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
    }
    let s = dx.max(96) as f32 / 96.0;
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    let work = unsafe {
        if GetMonitorInfoW(monitor, &mut info).as_bool() {
            info.rcWork
        } else {
            RECT {
                left: 0,
                top: 0,
                right: 1280,
                bottom: 720,
            }
        }
    };
    let room = 48;
    let w = ((FIRST_SIZE.0 * s) as i32).min(work.right - work.left - room);
    let h = ((FIRST_SIZE.1 * s) as i32).min(work.bottom - work.top - room);
    let x = work.left + (work.right - work.left - w) / 2;
    let y = work.top + (work.bottom - work.top - h) / 2;
    RECT {
        left: x,
        top: y,
        right: x + w,
        bottom: y + h,
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_NCCREATE {
        let cs = &*(lparam.0 as *const CREATESTRUCTW);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, cs.lpCreateParams as isize);
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const QuestLog;
    if ptr.is_null() {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    if msg == WM_NCDESTROY {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    // The Box lives in the app for as long as the window exists, and the app
    // destroys the window before dropping the Box.
    let win = &*ptr;
    match win.handle(msg, wparam, lparam) {
        Some(r) => r,
        None => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use horadric_core::chronicle::End;
    use horadric_core::journal::Commit;

    fn quest(id: &str, accepted: u64, ended: Option<u64>, outcome: Outcome) -> Quest {
        Quest {
            id: id.into(),
            title: id.to_uppercase(),
            notes: vec![],
            name: String::new(),
            accepted: Some(accepted),
            ended,
            outcome,
            reason: String::new(),
            summary: String::new(),
            last: String::new(),
            conversation: None,
            commits: vec![],
            merged: None,
            parent: None,
            added_by: None,
            added_in: None,
            main: false,
            agent: Agent::Claude,
        }
    }

    fn talk(id: &str, started: u64, touched: u64) -> chronicle::Talk {
        chronicle::Talk {
            id: id.into(),
            title: id.to_uppercase(),
            started,
            touched,
            cwd: "C:/p".into(),
            agent: Agent::Claude,
        }
    }

    #[test]
    fn side_by_side_when_wide_and_stacked_when_narrow() {
        let wide = layout((1000.0, 600.0), 3);
        assert!(wide.detail.x > wide.list.right());
        assert_eq!(wide.list.y, wide.detail.y);
        let narrow = layout((420.0, 700.0), 3);
        assert!(narrow.detail.y > narrow.list.bottom());
        assert_eq!(narrow.list.w, narrow.detail.w);
        // Smaller than the least is laid out at the least.
        assert_eq!(layout((10.0, 10.0), 1).size, MIN_SIZE);
        for l in [wide, narrow] {
            assert!(l.read.right() < l.carry.x);
            assert!(l.carry.right() <= l.detail.right());
            assert!(l.body.bottom() < l.read.y);
            assert!(l.rows.y >= l.head.bottom());
        }
    }

    #[test]
    fn a_wide_diagram_gets_narrower_lanes_and_leaves_room_for_words() {
        let few = layout((1000.0, 600.0), 3);
        assert_eq!(few.lane_w, LANE_W);
        assert!(few.lane_x(0) < few.lane_x(1));
        let many = layout((1000.0, 600.0), 40);
        assert_eq!(many.lane_w, LANE_MIN);
        assert!(many.text_x <= many.list.right() - 120.0);
    }

    #[test]
    fn a_point_finds_the_row_under_it_scrolled_or_not() {
        let l = layout((1000.0, 600.0), 2);
        let x = l.text_x + 10.0;
        let y = l.rows.y + ROW_H * 1.5;
        assert_eq!(l.hit(5, 0.0, x, y), QuestHit::Row(1));
        assert_eq!(l.hit(5, ROW_H, x, y), QuestHit::Row(2));
        assert_eq!(l.hit(1, 0.0, x, y), QuestHit::Nothing, "past the last row");
        assert_eq!(l.hit(5, 0.0, 200.0, 10.0), QuestHit::Caption);
        assert_eq!(
            l.hit(5, 0.0, l.close.x + 2.0, l.close.y + 2.0),
            QuestHit::Close
        );
        assert_eq!(
            l.hit(5, 0.0, l.read.x + 2.0, l.read.y + 2.0),
            QuestHit::Read
        );
        assert_eq!(
            l.hit(5, 0.0, l.carry.x + 2.0, l.carry.y + 2.0),
            QuestHit::Carry
        );
        assert_eq!(l.hit(5, 0.0, l.all.x + 2.0, l.all.y + 2.0), QuestHit::All);
        assert!(l.all.y >= l.head.y && l.all.bottom() <= l.head.bottom());
        assert!(l.all.right() <= l.list.right());
    }

    #[test]
    fn the_list_scrolls_no_further_than_its_rows_and_reveals_a_row() {
        let l = layout((1000.0, 600.0), 2);
        let fit = (l.rows.h / ROW_H).floor() as usize;
        assert_eq!(l.max_scroll(fit), 0.0);
        assert_eq!(l.max_scroll(fit + 10), (fit + 10) as f32 * ROW_H - l.rows.h);
        assert_eq!(l.visible(100, 0.0).start, 0);
        assert_eq!(l.visible(3, 0.0), 0..3);
        assert_eq!(l.visible(100, ROW_H * 2.5).start, 2);
        // Above the view it scrolls up to it, below down, inside not at all.
        assert_eq!(l.reveal(1, ROW_H * 4.0), ROW_H);
        assert_eq!(l.reveal(2, 0.0), 0.0);
        let below = fit + 4;
        assert_eq!(l.reveal(below, 0.0), (below + 1) as f32 * ROW_H - l.rows.h);
    }

    #[test]
    fn the_edges_size_and_the_corners_reach_further() {
        let e = |x, y| edge(x, y, 400.0, 300.0, 6.0);
        assert_eq!(e(200.0, 150.0), None);
        assert_eq!(e(2.0, 150.0), Some(Edge::Left));
        assert_eq!(e(398.0, 150.0), Some(Edge::Right));
        assert_eq!(e(200.0, 1.0), Some(Edge::Top));
        assert_eq!(e(200.0, 299.0), Some(Edge::Bottom));
        assert_eq!(e(1.0, 1.0), Some(Edge::TopLeft));
        assert_eq!(e(10.0, 2.0), Some(Edge::TopLeft), "along the top near");
        assert_eq!(e(399.0, 299.0), Some(Edge::BottomRight));
        assert_eq!(e(2.0, 295.0), Some(Edge::BottomLeft));
        assert_eq!(e(390.0, 3.0), Some(Edge::TopRight));
    }

    #[test]
    fn each_band_knows_the_quest_on_every_lane() {
        let q = [
            quest("a", 1, Some(5), Outcome::Done),
            quest("b", 2, Some(3), Outcome::Returned),
            quest("c", 4, None, Outcome::Working),
        ];
        let (rows, width) = chronicle::graph(&q);
        let on = occupants(&rows, width);
        // Newest first: c, b, a. Under a nothing ran; under b, a's lane;
        // under c, a's lane still, b's gone.
        assert_eq!(on[2], vec![None, None, None]);
        assert_eq!(on[1], vec![None, Some(0), None]);
        assert_eq!(on[0], vec![None, Some(0), None]);
        assert_eq!(rows[0].ends, vec![(1, End::Converge)]);
    }

    #[test]
    fn a_conversation_leaves_the_trunk_uncoloured_and_colours_its_fork() {
        let mut child = quest("b", 5, Some(6), Outcome::Done);
        child.parent = Some(1);
        let q = chronicle::with_talks(vec![child], &[talk("t", 1, 9)]);
        let (rows, width) = chronicle::graph(&q);
        let on = occupants(&rows, width);
        // Newest first: b, then t. Under t nothing ran; under b, b's own
        // lane rising from t's dot.
        assert_eq!(on[1], vec![None, None]);
        assert_eq!(on[0], vec![None, Some(0)]);
    }

    #[test]
    fn a_conversation_reads_and_burns_apart_from_any_quest() {
        let q = chronicle::with_talks(Vec::new(), &[talk("t", 1, 9)]);
        let t = &q[0];
        assert_eq!(word_of(t), "conversation");
        assert_eq!(color_of(t), conversation_color());
        for o in [
            Outcome::Working,
            Outcome::Review,
            Outcome::Blocked,
            Outcome::Done,
            Outcome::Returned,
        ] {
            assert_ne!(conversation_color(), outcome_color(o));
        }
        assert!(can_read(t) && can_carry(t));
        let mut codex = t.clone();
        codex.agent = Agent::Codex;
        assert!(!can_read(&codex) && can_carry(&codex));
        assert!(!can_carry(&quest("a", 1, None, Outcome::Done)));
        assert_eq!(
            session_doc(t, "## You\n\nHi\n"),
            "# T\n\n# The conversation\n\n## You\n\nHi\n"
        );
    }

    #[test]
    fn spans_read_short_on_a_row_and_whole_in_the_detail() {
        assert_eq!(short_span(30), "<1 min");
        assert_eq!(short_span(125), "2 min");
        assert_eq!(short_span(3 * 3600 + 5), "3 h");
        assert_eq!(short_span(2 * 86_400), "2 d");
        assert_eq!(short_span(30 * 86_400), "4 w");
        assert_eq!(duration(20), "under a minute");
        assert_eq!(duration(5 * 60), "5 min");
        assert_eq!(duration(3600 + 20 * 60), "1 h 20 min");
        assert_eq!(duration(2 * 3600), "2 h");
        assert_eq!(duration(3 * 86_400 + 4 * 3600), "3 d 4 h");
    }

    #[test]
    fn a_row_says_how_long_ago_or_how_long_it_has_run() {
        let now = 1_000_000;
        assert_eq!(
            row_age(&quest("a", now - 7200, None, Outcome::Working), now),
            "for 2 h"
        );
        assert_eq!(
            row_age(&quest("a", 1, Some(now - 3 * 86_400), Outcome::Done), now),
            "3 d ago"
        );
        assert_eq!(
            row_age(&quest("a", 1, Some(now - 5), Outcome::Blocked), now),
            "just now"
        );
        let mut unknown = quest("a", 1, None, Outcome::Done);
        unknown.accepted = None;
        assert_eq!(row_age(&unknown, now), "");
    }

    #[test]
    fn a_moment_reads_as_today_yesterday_a_date_or_a_year() {
        // 2026-10-03 12:00 UTC.
        let now = 1_791_028_800;
        assert_eq!(when(now - 3600, now, 0), "today 11:00");
        assert_eq!(when(now - 86_400, now, 7200), "yesterday 14:00");
        assert_eq!(when(now - 5 * 86_400, now, 0), "28 Sep 12:00");
        assert_eq!(when(now - 400 * 86_400, now, 0), "29 Aug 2025");
        // Local midnight decides the day, not UTC's.
        assert_eq!(when(now - 13 * 3600, now, -3600), "yesterday 22:00");
        assert_eq!(civil(0), (1970, 1, 1));
        assert_eq!(civil(20_510), (2026, 2, 26));
    }

    #[test]
    fn a_quest_comes_from_its_parent_its_adder_or_the_main_line() {
        let mut q = [
            quest("a", 1, Some(2), Outcome::Done),
            quest("b", 3, None, Outcome::Working),
        ];
        assert_eq!(origin(&q[1], &q), "the main line");
        q[1].added_by = Some("quest-giver-5".into());
        assert_eq!(origin(&q[1], &q), "added by quest-giver-5");
        q[1].parent = Some(0);
        assert_eq!(origin(&q[1], &q), "grew out of \"A\"");
    }

    #[test]
    fn a_quest_burns_in_the_colours_of_the_quests_tile() {
        use crate::board::RowState;
        assert_eq!(outcome_color(Outcome::Working), RowState::Working.color());
        assert_eq!(outcome_color(Outcome::Review), RowState::Review.color());
        assert_eq!(outcome_color(Outcome::Blocked), RowState::Blocked.color());
        assert_eq!(outcome_color(Outcome::Done), theme::done());
        assert_eq!(outcome_color(Outcome::Returned), RowState::Gone.color());
    }

    #[test]
    fn a_session_is_written_out_under_a_safe_name_with_its_story_on_top() {
        assert_eq!(session_file("fix-1.x2"), "chronicle/fix-1.x2.md");
        assert_eq!(session_file("../a b"), "chronicle/-a-b.md");
        let mut q = quest("a-1", 1, Some(2), Outcome::Done);
        q.summary = "The clock ticks again.".into();
        q.commits = vec![Commit {
            hash: "abc123".into(),
            subject: "Fix the clock".into(),
        }];
        let doc = session_doc(&q, "## You\n\nFix it\n");
        assert_eq!(
            doc,
            "# A-1\n\n**completed**. The clock ticks again.\n\n- `abc123` Fix the clock\n\n\
             # The conversation\n\n## You\n\nFix it\n"
        );
        assert!(session_doc(&q, "").ends_with("nothing to read.\n"));
    }
}
