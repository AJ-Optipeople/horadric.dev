//! The line that says what a button does, under the cursor once it rests
//! on one.
//!
//! Drawn like the rest of the app rather than as a Windows tooltip, on a
//! small plate below the cursor. One shows at a time for the whole thread,
//! so every window just says what is under the cursor and this decides
//! when the plate comes and goes, as Windows does it: after a rest the
//! first time, at once while moving from one button to the next, never
//! again on a button just clicked until the cursor leaves it. The plate
//! lets the mouse through and never takes the focus.

use std::borrow::Cow;
use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::rc::Rc;
use std::time::Instant;

use windows::core::{w, Result, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::DirectWrite::IDWriteTextLayout;
use windows::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUNDSMALL,
    DWM_WINDOW_CORNER_PREFERENCE,
};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromPoint, ValidateRect, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetCursorPos, GetWindowLongPtrW, KillTimer,
    RegisterClassW, SetTimer, SetWindowLongPtrW, ShowWindow, CREATESTRUCTW, GWLP_USERDATA,
    HTTRANSPARENT, MA_NOACTIVATE, SW_SHOWNOACTIVATE, WM_ERASEBKGND, WM_MOUSEACTIVATE, WM_NCCREATE,
    WM_NCDESTROY, WM_NCHITTEST, WM_PAINT, WNDCLASSW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};

use horadric_core::saved::Side;

use crate::backdrop;
use crate::glyphs::{BarHit, TabHit};
use crate::layout::{CaptionHit, CubeHit, Hit, StartHit, UsageHit};
use crate::render::{self, Target};
use crate::window::Shared;

const CLASS: PCWSTR = w!("HoradricTip");

/// How long the cursor rests on a button before its line shows, in
/// milliseconds. Windows waits half a second.
pub const DELAY: u64 = 500;
/// How soon after one line goes another shows at once, so running the
/// cursor along a row of buttons reads each without waiting again.
pub const RESHOW: u64 = 400;
/// The widest a line gets before it wraps, in DIPs.
const MAX_W: f32 = 300.0;
/// Between the text and the plate's edges, in DIPs.
const PAD_X: f32 = 9.0;
const PAD_Y: f32 = 6.0;
/// How far below the cursor's point the plate's top sits, clear of the
/// arrow, in DIPs.
const BELOW: f32 = 22.0;

/// What the plate should do next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Do {
    Nothing,
    /// Go, if it shows, and stop waiting.
    Hide,
    /// Wait this many milliseconds, then ask again with [`Tips::due`].
    Wait(u64),
    /// Show the line now, in place of any other.
    Show(Line),
}

/// A line to show: most are fixed, a rune stone's is made from its steps.
pub type Line = Cow<'static, str>;

/// When the line shows, apart from drawing it. Times are milliseconds
/// on any one clock.
#[derive(Debug, Default)]
pub struct Tips {
    /// The window the cursor is over, and what it says is under it.
    owner: isize,
    text: Option<Line>,
    showing: bool,
    /// Clicked: nothing shows until the cursor is on something else.
    quiet: bool,
    /// When a line last went, for [`RESHOW`].
    gone_at: Option<u64>,
}

impl Tips {
    /// The cursor moved over `owner`, and is on something that says `text`.
    pub fn over(&mut self, owner: isize, text: Option<Line>, now: u64) -> Do {
        if owner == self.owner && text == self.text {
            return Do::Nothing;
        }
        self.owner = owner;
        self.text = text.clone();
        self.quiet = false;
        let Some(text) = text else {
            return self.hide(now);
        };
        let recent = self.gone_at.is_some_and(|t| now.saturating_sub(t) < RESHOW);
        if self.showing || recent {
            self.showing = true;
            Do::Show(text)
        } else {
            Do::Wait(DELAY)
        }
    }

    /// The cursor left `owner`. Said by a window it has already moved on
    /// from, it is too late and means nothing.
    pub fn away(&mut self, owner: isize, now: u64) -> Do {
        if owner != self.owner {
            return Do::Nothing;
        }
        self.text = None;
        self.quiet = false;
        self.hide(now)
    }

    /// A button went down on `owner`. What it does is being done, so the
    /// line goes, and stays gone while the cursor stays.
    pub fn press(&mut self, owner: isize) -> Do {
        if owner != self.owner {
            return Do::Nothing;
        }
        self.quiet = true;
        // Not a moment for the next button to show at once either.
        self.gone_at = None;
        self.showing = false;
        Do::Hide
    }

    /// The wait asked for is over.
    pub fn due(&mut self) -> Do {
        match &self.text {
            Some(t) if !self.showing && !self.quiet => {
                self.showing = true;
                Do::Show(t.clone())
            }
            _ => Do::Nothing,
        }
    }

    fn hide(&mut self, now: u64) -> Do {
        if std::mem::take(&mut self.showing) {
            self.gone_at = Some(now);
        }
        Do::Hide
    }
}

/// Where a plate `size` goes for the cursor at `cursor`, `below` pixels
/// under it: its left edge on the cursor, kept inside the work area
/// `work` (left, top, right, bottom), and above the cursor when there is
/// no room under it. All in screen pixels.
pub fn place(cursor: (i32, i32), size: (i32, i32), work: [i32; 4], below: i32) -> (i32, i32) {
    let [wl, wt, wr, wb] = work;
    let x = cursor.0.min(wr - size.0).max(wl);
    let under = cursor.1 + below;
    let y = if under + size.1 <= wb {
        under
    } else {
        cursor.1 - size.1 - below / 4
    };
    (x, y.max(wt))
}

/// What a part of a cluster does.
pub fn cluster(hit: Hit) -> Option<&'static str> {
    Some(match hit {
        Hit::New => "Open another folder as a project",
        Hit::Header => "Fold or unfold this project. Drag to move it, right click for its menu",
        Hit::Tile(_) => "Show this session on the stage. Right click for more",
        Hit::Browser(_) => "Show this session's browser",
        Hit::Code(_) => "Open this session's worktree in VS Code",
        Hit::Add => "Start another session in this project",
        Hit::Shell => "Open a terminal in this project",
        Hit::FilesHeader => "Fold or unfold the changed files",
        Hit::File(_) => "Open on the stage, or with Ctrl in your editor. A folder opens or closes",
        Hit::TasksHeader => "Fold or unfold the quest log. Right click to pick how quests run",
        Hit::TasksMode => "Pick how the quests run",
        Hit::TasksAdd => "Add a quest",
        Hit::Task(_) => "Read this quest before taking it on, or show the session doing it. Right click for more",
        Hit::TasksGive => "Ask an agent to suggest quests for this project. You pick which go in the log",
        Hit::TaskApprove(_) => "Mark this quest completed",
        Hit::TomeHeader => "Fold or unfold the Runetome. Right click for more",
        Hit::Stone(_) => "Cast this stone, drag it onto a session, or drag it within the tome to move it. Right click for more",
        Hit::Nothing => return None,
    })
}

/// What a part of the cube does.
pub fn cube(hit: CubeHit) -> Option<&'static str> {
    Some(match hit {
        CubeHit::Slot(_) => "Take this session back out of the cube",
        CubeHit::Main => "Put main in the cube, or take it out",
        CubeHit::Transmute => "Run the recipe on what the cube holds",
        CubeHit::Nothing => return None,
    })
}

/// What a part of the start window does.
pub fn start(hit: StartHit) -> Option<&'static str> {
    Some(match hit {
        StartHit::Open => "Pick a folder to open as a project",
        StartHit::Recent(_) => "Open this project again",
        StartHit::Nothing => return None,
    })
}

/// What a part of the usage window does, `row` naming the setting row and
/// `locked` saying whether the padlock is closed.
pub fn usage(hit: UsageHit, row: Option<Row>, locked: bool) -> Option<&'static str> {
    Some(match (hit, row) {
        (UsageHit::Header, _) => "Show the next agent's limits",
        (UsageHit::Lock, _) if locked => "Unpin, so this scrolls with the tiles and drags",
        (UsageHit::Lock, _) => "Pin this to the top while the tiles below it scroll",
        (UsageHit::Limits, _) => "Fold or unfold the limits",
        (UsageHit::Setting(_), Some(Row::Model)) => "Pick the model new sessions start with",
        (UsageHit::Setting(_), Some(Row::Effort)) => "Pick how hard new sessions think",
        (UsageHit::Setting(_), Some(Row::Permissions)) => {
            "Pick what new sessions may do without asking"
        }
        (UsageHit::Setting(_), Some(Row::Account)) => "Switch to another account",
        (UsageHit::Setting(_), Some(Row::Version)) => "Read the release notes",
        (UsageHit::Setting(_), None) | (UsageHit::Nothing, _) => return None,
    })
}

/// A row of the usage window, as far as its line goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Row {
    Model,
    Effort,
    Permissions,
    Account,
    Version,
}

/// What a key of the stage's caption does, `zoomed` while the stage fills
/// the screen.
pub fn caption(hit: CaptionHit, zoomed: bool) -> Option<&'static str> {
    Some(match hit {
        CaptionHit::Min => "Minimize",
        CaptionHit::Max if zoomed => "Restore down",
        CaptionHit::Max => "Maximize",
        CaptionHit::Close => "Close the stage. The sessions keep running",
        CaptionHit::Bar => return None,
    })
}

/// A key of a pane's header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Stash,
    Zoom,
    Close,
}

/// What a pane shows, as far as what its cross does goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shows {
    Agent,
    Shell,
    File,
    Browser,
}

/// What a key of a pane's header does, `zoomed` while the pane fills the
/// stage.
pub fn pane(key: Key, shows: Shows, zoomed: bool) -> &'static str {
    match (key, shows) {
        (Key::Stash, _) => "Stash this session. It stops, and its conversation waits in the stash",
        (Key::Zoom, _) if zoomed => "Put this pane back beside the others",
        (Key::Zoom, _) => "Fill the stage with this pane",
        (Key::Close, Shows::Agent) => "End this session for good",
        (Key::Close, Shows::Shell) => "Close this terminal",
        (Key::Close, Shows::File) => "Close this file",
        (Key::Close, Shows::Browser) => "Close the browser",
    }
}

/// What a key of the browser's bar does, `docked` naming the side of the
/// stage the browser stands on. The address field says so itself.
pub fn bar(hit: BarHit, docked: Option<Side>) -> Option<&'static str> {
    Some(match hit {
        BarHit::Back => "Back",
        BarHit::Forward => "Forward",
        BarHit::Reload => "Reload",
        BarHit::Size => "Pick the size the page is laid out at",
        BarHit::Place(side) if docked == Some(side) => "Put the browser back in the grid",
        BarHit::Place(Side::Left) => "Stand the browser on the left of the stage",
        BarHit::Place(Side::Top) => "Stand the browser along the top of the stage",
        BarHit::Place(Side::Right) => "Stand the browser on the right of the stage",
        BarHit::Field => return None,
    })
}

/// What a part of the browser's tab strip does.
pub fn tab(hit: TabHit) -> &'static str {
    match hit {
        TabHit::Tab(_) => "Show this tab. A middle click closes it",
        TabHit::Close(_) => "Close this tab",
        TabHit::New => "Open a new tab",
    }
}

/// What a stashed session's row does.
pub const STASHED: &str = "Click to bring it back to the stage. Right click for more";

/// A stashed session's whole story, since its row has room for only a
/// line of each: its name, where it worked, what it last did, then what
/// a click does.
pub fn stashed(name: &str, project: &str, branch: Option<&str>, last: &str) -> String {
    let mut s = name.trim().to_string();
    s.push('\n');
    s.push_str(project);
    if let Some(b) = branch.filter(|b| !b.is_empty()) {
        s.push_str(", on ");
        s.push_str(b);
    }
    let last = last.trim();
    if !last.is_empty() {
        s.push('\n');
        s.push_str(last);
    }
    s.push_str("\n\n");
    s.push_str(STASHED);
    s
}

thread_local! {
    static TIPS: RefCell<Tips> = RefCell::new(Tips::default());
    static PLATE: RefCell<Option<Box<Plate>>> = const { RefCell::new(None) };
    static SHARED: RefCell<Option<Rc<Shared>>> = const { RefCell::new(None) };
    static TIMER: Cell<usize> = const { Cell::new(0) };
    static EPOCH: Instant = Instant::now();
}

fn now() -> u64 {
    EPOCH.with(|e| e.elapsed().as_millis() as u64)
}

/// The cursor moved over `owner` and is on what `text` describes, or on
/// nothing that has a line.
pub fn over(shared: &Rc<Shared>, owner: HWND, text: Option<&'static str>) {
    over_line(shared, owner, text.map(Cow::Borrowed));
}

/// As [`over`], for a line made while the app runs.
pub fn over_line(shared: &Rc<Shared>, owner: HWND, text: Option<Line>) {
    SHARED.with(|s| {
        if s.borrow().is_none() {
            *s.borrow_mut() = Some(Rc::clone(shared));
        }
    });
    let next = TIPS.with(|t| t.borrow_mut().over(owner.0 as isize, text, now()));
    act(next);
}

/// The cursor left `owner`.
pub fn away(owner: HWND) {
    act(TIPS.with(|t| t.borrow_mut().away(owner.0 as isize, now())));
}

/// A mouse button went down on `owner`.
pub fn press(owner: HWND) {
    act(TIPS.with(|t| t.borrow_mut().press(owner.0 as isize)));
}

fn act(next: Do) {
    match next {
        Do::Nothing => {}
        Do::Hide => {
            stop_timer();
            close();
        }
        Do::Wait(ms) => {
            stop_timer();
            close();
            let id = unsafe { SetTimer(None, 0, ms as u32, Some(fire)) };
            TIMER.with(|t| t.set(id));
        }
        Do::Show(text) => {
            stop_timer();
            close();
            show(text);
        }
    }
}

fn stop_timer() {
    let id = TIMER.with(|t| t.replace(0));
    if id != 0 {
        unsafe {
            let _ = KillTimer(None, id);
        }
    }
}

unsafe extern "system" fn fire(_: HWND, _: u32, _: usize, _: u32) {
    stop_timer();
    act(TIPS.with(|t| t.borrow_mut().due()));
}

fn close() {
    if let Some(p) = PLATE.with(|p| p.borrow_mut().take()) {
        unsafe {
            let _ = DestroyWindow(p.hwnd.get());
        }
    }
}

fn show(text: Line) {
    let Some(shared) = SHARED.with(|s| s.borrow().clone()) else {
        return;
    };
    match Plate::open(shared, &text) {
        Ok(p) => PLATE.with(|slot| *slot.borrow_mut() = Some(p)),
        Err(e) => eprintln!("horadric: cannot show the line \"{text}\": {e}"),
    }
}

fn register_class() {
    thread_local!(static DONE: Cell<bool> = const { Cell::new(false) });
    if DONE.with(|d| d.replace(true)) {
        return;
    }
    unsafe {
        let Ok(module) = GetModuleHandleW(None) else {
            return;
        };
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: module.into(),
            lpszClassName: CLASS,
            ..Default::default()
        };
        RegisterClassW(&wc);
    }
}

struct Plate {
    hwnd: Cell<HWND>,
    shared: Rc<Shared>,
    target: RefCell<Option<Target>>,
    text: IDWriteTextLayout,
    /// In DIPs.
    size: (f32, f32),
    scale: f32,
}

impl Plate {
    fn open(shared: Rc<Shared>, text: &str) -> Result<Box<Self>> {
        register_class();
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
        let layout = render::wrapped(&shared.gpu, &shared.gpu.small, text, MAX_W)?;
        let (tw, th) = render::text_size(&layout);
        let size = (tw.ceil() + 2.0 * PAD_X, th.ceil() + 2.0 * PAD_Y);
        let px = ((size.0 * s).round() as i32, (size.1 * s).round() as i32);
        let (x, y) = place(
            (cursor.x, cursor.y),
            px,
            work_area(monitor),
            (BELOW * s).round() as i32,
        );
        let plate = Box::new(Plate {
            hwnd: Cell::new(HWND::default()),
            shared,
            target: RefCell::new(None),
            text: layout,
            size,
            scale: s,
        });
        unsafe {
            // Born where it shows and at its size: a window that starts
            // elsewhere and moves has been seen not to paint.
            let hwnd = CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE | WS_EX_TRANSPARENT,
                CLASS,
                w!(""),
                WS_POPUP,
                x,
                y,
                px.0,
                px.1,
                None,
                None,
                Some(GetModuleHandleW(None)?.into()),
                Some(&*plate as *const Plate as *const c_void),
            )?;
            plate.hwnd.set(hwnd);
            let pref: DWM_WINDOW_CORNER_PREFERENCE = DWMWCP_ROUNDSMALL;
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_WINDOW_CORNER_PREFERENCE,
                &pref as *const _ as *const c_void,
                std::mem::size_of::<DWM_WINDOW_CORNER_PREFERENCE>() as u32,
            );
            backdrop::border(hwnd, None);
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        }
        Ok(plate)
    }

    fn paint(&self) {
        let mut slot = self.target.borrow_mut();
        if slot.is_none() {
            let s = self.scale;
            let (w, h) = (
                (self.size.0 * s).round() as u32,
                (self.size.1 * s).round() as u32,
            );
            match Target::new(
                &self.shared.gpu,
                self.hwnd.get(),
                w,
                h,
                (s * 96.0).round() as u32,
            ) {
                Ok(t) => *slot = Some(t),
                Err(e) => {
                    eprintln!("horadric: render target for a line: {e}");
                    return;
                }
            }
        }
        let failed = slot
            .as_ref()
            .map(|t| t.draw_tip(&self.shared.metrics, &self.text, self.size, (PAD_X, PAD_Y)))
            .is_some_and(|r| r.is_err());
        if failed {
            *slot = None;
        }
    }

    fn handle(&self, msg: u32) -> Option<LRESULT> {
        match msg {
            WM_PAINT => {
                self.paint();
                unsafe {
                    let _ = ValidateRect(Some(self.hwnd.get()), None);
                }
                Some(LRESULT(0))
            }
            WM_ERASEBKGND => Some(LRESULT(1)),
            // The cursor goes through it to whatever is under it.
            WM_NCHITTEST => Some(LRESULT(HTTRANSPARENT as isize)),
            WM_MOUSEACTIVATE => Some(LRESULT(MA_NOACTIVATE as isize)),
            _ => None,
        }
    }
}

/// The work area of `monitor`, as left, top, right, bottom.
fn work_area(monitor: windows::Win32::Graphics::Gdi::HMONITOR) -> [i32; 4] {
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    unsafe {
        if GetMonitorInfoW(monitor, &mut info).as_bool() {
            let w = info.rcWork;
            [w.left, w.top, w.right, w.bottom]
        } else {
            [0, 0, 1280, 720]
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_NCCREATE {
        let cs = &*(lparam.0 as *const CREATESTRUCTW);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, cs.lpCreateParams as isize);
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const Plate;
    if ptr.is_null() {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    if msg == WM_NCDESTROY {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    // The Box lives in PLATE until after the window is destroyed.
    let plate = &*ptr;
    match plate.handle(msg) {
        Some(r) => r,
        None => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_waits_for_the_cursor_to_rest_then_shows() {
        let mut t = Tips::default();
        assert_eq!(t.over(1, Some("a".into()), 0), Do::Wait(DELAY));
        assert_eq!(t.over(1, Some("a".into()), 100), Do::Nothing);
        assert_eq!(t.due(), Do::Show("a".into()));
        assert_eq!(t.due(), Do::Nothing);
    }

    #[test]
    fn the_next_button_shows_at_once_while_one_shows() {
        let mut t = Tips::default();
        t.over(1, Some("a".into()), 0);
        t.due();
        assert_eq!(t.over(1, Some("b".into()), 700), Do::Show("b".into()));
    }

    #[test]
    fn a_short_gap_between_buttons_still_shows_at_once() {
        let mut t = Tips::default();
        t.over(1, Some("a".into()), 0);
        t.due();
        assert_eq!(t.over(1, None, 1000), Do::Hide);
        assert_eq!(
            t.over(1, Some("b".into()), 1000 + RESHOW - 1),
            Do::Show("b".into())
        );
    }

    #[test]
    fn after_a_long_gap_it_waits_again() {
        let mut t = Tips::default();
        t.over(1, Some("a".into()), 0);
        t.due();
        t.over(1, None, 1000);
        assert_eq!(t.over(1, Some("b".into()), 1000 + RESHOW), Do::Wait(DELAY));
    }

    #[test]
    fn moving_off_before_the_wait_is_over_shows_nothing() {
        let mut t = Tips::default();
        t.over(1, Some("a".into()), 0);
        assert_eq!(t.over(1, None, 100), Do::Hide);
        assert_eq!(t.due(), Do::Nothing);
        // And it never showed, so the next one waits.
        assert_eq!(t.over(1, Some("b".into()), 150), Do::Wait(DELAY));
    }

    #[test]
    fn a_click_hides_it_until_the_cursor_moves_on() {
        let mut t = Tips::default();
        t.over(1, Some("a".into()), 0);
        t.due();
        assert_eq!(t.press(1), Do::Hide);
        assert_eq!(t.over(1, Some("a".into()), 900), Do::Nothing);
        assert_eq!(t.due(), Do::Nothing);
        assert_eq!(t.over(1, Some("b".into()), 950), Do::Wait(DELAY));
    }

    #[test]
    fn a_late_leave_from_the_window_left_behind_changes_nothing() {
        let mut t = Tips::default();
        t.over(1, Some("a".into()), 0);
        t.over(2, Some("b".into()), 10);
        assert_eq!(t.away(1, 20), Do::Nothing);
        assert_eq!(t.due(), Do::Show("b".into()));
        assert_eq!(t.press(1), Do::Nothing);
        assert_eq!(t.away(2, 30), Do::Hide);
    }

    #[test]
    fn the_same_line_in_another_window_starts_over() {
        let mut t = Tips::default();
        t.over(1, Some("a".into()), 0);
        assert_eq!(t.over(2, Some("a".into()), 10), Do::Wait(DELAY));
    }

    #[test]
    fn the_plate_sits_under_the_cursor() {
        assert_eq!(
            place((100, 100), (50, 20), [0, 0, 800, 600], 20),
            (100, 120)
        );
    }

    #[test]
    fn the_plate_stays_inside_the_work_area() {
        assert_eq!(
            place((790, 100), (50, 20), [0, 0, 800, 600], 20),
            (750, 120)
        );
        assert_eq!(place((-5, 100), (50, 20), [0, 0, 800, 600], 20), (0, 120));
    }

    #[test]
    fn at_the_bottom_it_goes_above_the_cursor() {
        assert_eq!(
            place((100, 590), (50, 20), [0, 0, 800, 600], 20),
            (100, 565)
        );
    }

    #[test]
    fn every_part_of_a_cluster_that_lights_has_a_line() {
        let all = [
            Hit::New,
            Hit::Header,
            Hit::Tile(0),
            Hit::Browser(0),
            Hit::Code(0),
            Hit::Add,
            Hit::Shell,
            Hit::FilesHeader,
            Hit::File(0),
            Hit::TasksHeader,
            Hit::TasksMode,
            Hit::TasksAdd,
            Hit::TasksGive,
            Hit::Task(0),
            Hit::TaskApprove(0),
            Hit::TomeHeader,
            Hit::Stone(0),
        ];
        for h in all {
            assert!(cluster(h).is_some(), "{h:?}");
        }
        assert_eq!(cluster(Hit::Nothing), None);
    }

    #[test]
    fn the_maximize_key_says_what_it_will_do() {
        assert_eq!(caption(CaptionHit::Max, false), Some("Maximize"));
        assert_eq!(caption(CaptionHit::Max, true), Some("Restore down"));
        assert_eq!(caption(CaptionHit::Bar, false), None);
    }

    #[test]
    fn the_cube_start_and_usage_windows_have_lines() {
        assert!(cube(CubeHit::Transmute).is_some());
        assert_eq!(cube(CubeHit::Nothing), None);
        assert!(start(StartHit::Recent(2)).is_some());
        assert_eq!(start(StartHit::Nothing), None);
        assert!(usage(UsageHit::Setting(0), Some(Row::Account), false).is_some());
        assert_eq!(usage(UsageHit::Setting(0), None, false), None);
        assert!(usage(UsageHit::Limits, None, false).is_some());
        assert_ne!(
            usage(UsageHit::Lock, None, true),
            usage(UsageHit::Lock, None, false)
        );
    }

    #[test]
    fn the_zoom_key_says_which_way_it_goes() {
        assert_ne!(
            pane(Key::Zoom, Shows::Agent, false),
            pane(Key::Zoom, Shows::Agent, true)
        );
    }

    #[test]
    fn the_cross_says_what_it_closes() {
        let all = [Shows::Agent, Shows::Shell, Shows::File, Shows::Browser];
        for (i, a) in all.iter().enumerate() {
            for b in &all[i + 1..] {
                assert_ne!(pane(Key::Close, *a, false), pane(Key::Close, *b, false));
            }
        }
    }

    #[test]
    fn a_place_key_on_the_side_the_browser_is_on_puts_it_back() {
        let back = bar(BarHit::Place(Side::Left), Some(Side::Left));
        assert_eq!(back, Some("Put the browser back in the grid"));
        assert_ne!(bar(BarHit::Place(Side::Left), Some(Side::Right)), back);
        assert_ne!(bar(BarHit::Place(Side::Top), None), back);
        assert_eq!(bar(BarHit::Field, None), None);
    }

    #[test]
    fn a_stashed_line_says_name_place_and_last_words_before_the_click() {
        let s = stashed(" fix login ", "shop", Some("fix-login"), "Ran the tests");
        assert_eq!(
            s,
            format!("fix login\nshop, on fix-login\nRan the tests\n\n{STASHED}")
        );
        let bare = stashed("x", "shop", Some(""), "  ");
        assert_eq!(bare, format!("x\nshop\n\n{STASHED}"));
    }

    #[test]
    fn no_line_uses_a_dash() {
        let mut lines: Vec<&str> = Vec::new();
        lines.extend(cluster(Hit::Tile(0)));
        lines.extend(cluster(Hit::File(0)));
        lines.push(STASHED);
        for k in [Key::Stash, Key::Zoom, Key::Close] {
            for s in [Shows::Agent, Shows::Shell, Shows::File, Shows::Browser] {
                lines.push(pane(k, s, false));
                lines.push(pane(k, s, true));
            }
        }
        for h in [BarHit::Back, BarHit::Size, BarHit::Place(Side::Top)] {
            lines.extend(bar(h, None));
        }
        for h in [TabHit::Tab(0), TabHit::Close(0), TabHit::New] {
            lines.push(tab(h));
        }
        for l in lines {
            assert!(!l.contains('\u{2014}') && !l.contains('\u{2013}') && !l.contains("--"));
        }
    }
}
