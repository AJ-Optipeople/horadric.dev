//! Stay a while and listen: what happened while you were away, on a plate
//! of its own in the look of the toasts. It opens when you come back after
//! a while, if anything happened, and from the tray menu and a hotkey.
//!
//! It takes the focus like a setting's list, so Esc and a click anywhere
//! else dismiss it, and whatever had the focus gets it back. A click on a
//! line shows that session the way a tile click does. Opening it marks
//! nothing read: identifying is still looking.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::rc::Rc;
use std::time::Duration;

use horadric_core::journal::{Group, Section};
use horadric_core::usage::format_until;
use windows::core::{w, Result, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
    DWM_WINDOW_CORNER_PREFERENCE,
};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, InvalidateRect, MonitorFromPoint, ValidateRect, MONITORINFO,
    MONITOR_DEFAULTTOPRIMARY,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetCapture, ReleaseCapture, SetCapture, VK_ESCAPE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetForegroundWindow, GetWindowLongPtrW,
    IsWindow, KillTimer, LoadCursorW, RegisterClassW, SetForegroundWindow, SetTimer,
    SetWindowLongPtrW, ShowWindow, CREATESTRUCTW, GWLP_USERDATA, IDC_ARROW, SW_HIDE, SW_SHOW,
    WA_INACTIVE, WM_ACTIVATE, WM_CAPTURECHANGED, WM_ERASEBKGND, WM_KEYDOWN, WM_LBUTTONDOWN,
    WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MOUSEMOVE, WM_NCCREATE, WM_NCDESTROY, WM_PAINT,
    WM_RBUTTONDOWN, WM_TIMER, WNDCLASSW, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

use crate::app::{self, Input};
use crate::backdrop;
use crate::layout::{self, CatchupKind, CatchupLayout};
use crate::render::{CatchupLook, CatchupScene, Target};
use crate::theme::{self, Color};
use crate::window::Shared;

pub(crate) const CLASS: PCWSTR = w!("HoradricCatchup");

pub const TITLE: &str = "Stay a while and listen";

/// No real input for this long counts as away, in seconds.
pub const AWAY_AFTER: u64 = 15 * 60;
/// Input this recent counts as being back, in seconds.
const BACK_WITHIN: u64 = 5;
/// Watches whether the focus went elsewhere while it never had it.
const WATCH: usize = 1;
/// Between the panel and the edges of the work area, in DIPs.
const MARGIN: f32 = 24.0;

/// Whether you are away, from how long since the last input and whether
/// the screen is locked.
#[derive(Debug, Default)]
pub struct Away {
    /// Since when, in Unix seconds, while away.
    since: Option<u64>,
    locked: bool,
}

impl Away {
    pub fn lock(&mut self, now: u64) {
        self.locked = true;
        self.since.get_or_insert(now);
    }

    /// Unlocking is coming back. Returns since when you were away.
    pub fn unlock(&mut self) -> Option<u64> {
        self.locked = false;
        self.since.take()
    }

    /// A look at the input clock, `idle` seconds since the last input.
    /// Returns since when you were away, once, as you come back. Input on
    /// the lock screen is not coming back: unlocking is.
    pub fn idle(&mut self, idle: u64, now: u64) -> Option<u64> {
        if self.locked {
            return None;
        }
        if idle >= AWAY_AFTER {
            self.since.get_or_insert(now.saturating_sub(idle));
            None
        } else if idle <= BACK_WITHIN {
            self.since.take()
        } else {
            None
        }
    }
}

/// What the panel says under its title.
pub fn covers(away: Option<u64>, since: u64, now: u64, clock: &str) -> String {
    match away {
        Some(from) => format!(
            "While you were away, {}",
            format_until(now.saturating_sub(from))
        ),
        None if clock.is_empty() => format!("The last {}", format_until(now.saturating_sub(since))),
        None => format!("Since {clock}"),
    }
}

/// The local time of day at `at`, as "09:05", from the seconds since local
/// midnight `local` at `now`.
pub fn clock(local: u64, now: u64, at: u64) -> String {
    let secs = (local as i64 - (now as i64 - at as i64)).rem_euclid(86_400);
    format!("{:02}:{:02}", secs / 3600, secs % 3600 / 60)
}

/// One row of the panel.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub kind: CatchupKind,
    pub text: String,
    pub detail: String,
    pub age: String,
    pub section: Option<Section>,
    /// The session a click shows, empty for none.
    pub session: String,
}

/// The rows for `groups`: each project's name, then its lines.
pub fn rows(groups: &[Group], now: u64, name: impl Fn(&str) -> String) -> Vec<Row> {
    let mut out = Vec::new();
    for g in groups {
        out.push(Row {
            kind: CatchupKind::Heading,
            text: if g.project.is_empty() {
                "Account".to_string()
            } else {
                name(&g.project)
            },
            detail: String::new(),
            age: String::new(),
            section: None,
            session: String::new(),
        });
        for l in &g.lines {
            out.push(Row {
                kind: CatchupKind::Line {
                    detail: !l.detail.is_empty(),
                },
                text: l.text.clone(),
                detail: l.detail.clone(),
                age: horadric_core::format_age(Duration::from_secs(now.saturating_sub(l.at))),
                section: Some(l.section),
                session: l.session.clone(),
            });
        }
    }
    out
}

fn tone(section: Option<Section>) -> Option<Color> {
    match section? {
        Section::Waiting => Some(theme::WAITING),
        Section::Review => Some(theme::WORKING),
        Section::Unread => Some(theme::DONE),
        Section::Blocked => Some(theme::ERROR),
        Section::Happened => None,
    }
}

pub fn register_class() -> Result<()> {
    unsafe {
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: GetModuleHandleW(None)?.into(),
            lpszClassName: CLASS,
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            ..Default::default()
        };
        RegisterClassW(&wc);
        Ok(())
    }
}

pub struct Catchup {
    pub hwnd: HWND,
    shared: Rc<Shared>,
    target: RefCell<Option<Target>>,
    layout: CatchupLayout,
    scale: f32,
    sub: String,
    rows: Vec<Row>,
    more: String,
    hot: Cell<Option<usize>>,
    close_hot: Cell<bool>,
    pressed: Cell<Option<usize>>,
    /// The window that had the focus before, which gets it back.
    before: HWND,
    /// It has had the focus. Windows may refuse it to an app you are not
    /// using, and then it goes when the focus moves anywhere else.
    had_focus: Cell<bool>,
    closed: Cell<bool>,
}

impl Catchup {
    pub fn open(shared: Rc<Shared>, sub: String, rows: Vec<Row>) -> Result<Box<Self>> {
        let monitor = unsafe { MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY) };
        let (mut dx, mut dy) = (96u32, 96u32);
        unsafe {
            let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
        }
        let s = dx.max(96) as f32 / 96.0;
        let work = work_area(monitor);
        let max_h = (work[3] - work[1]) as f32 / s - 2.0 * MARGIN;
        let kinds: Vec<CatchupKind> = rows.iter().map(|r| r.kind).collect();
        let layout = layout::catchup(&kinds, max_h);
        let hidden = rows[layout.rows.len()..]
            .iter()
            .filter(|r| r.kind != CatchupKind::Heading)
            .count();
        let size = (
            (layout.size.0 * s).round() as i32,
            (layout.size.1 * s).round() as i32,
        );
        let (x, y) = layout::catchup_place(size, work);
        let before = unsafe { GetForegroundWindow() };
        let mut win = Box::new(Catchup {
            hwnd: HWND::default(),
            shared,
            target: RefCell::new(None),
            layout,
            scale: s,
            sub,
            rows,
            more: format!("and {hidden} more"),
            hot: Cell::new(None),
            close_hot: Cell::new(false),
            pressed: Cell::new(None),
            before,
            had_focus: Cell::new(false),
            closed: Cell::new(false),
        });
        unsafe {
            // Born where it shows and at its size: a window that starts
            // elsewhere and moves has been seen not to paint.
            let hwnd = CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
                CLASS,
                w!("Horadric: stay a while and listen"),
                WS_POPUP,
                x,
                y,
                size.0,
                size.1,
                None,
                None,
                Some(GetModuleHandleW(None)?.into()),
                Some(&*win as *const Catchup as *const c_void),
            )?;
            win.hwnd = hwnd;
            let pref: DWM_WINDOW_CORNER_PREFERENCE = DWMWCP_ROUND;
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_WINDOW_CORNER_PREFERENCE,
                &pref as *const _ as *const c_void,
                std::mem::size_of::<DWM_WINDOW_CORNER_PREFERENCE>() as u32,
            );
            backdrop::border(hwnd, None);
            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = SetForegroundWindow(hwnd);
            if GetForegroundWindow() == hwnd {
                win.had_focus.set(true);
                SetCapture(hwnd);
            }
            SetTimer(Some(hwnd), WATCH, 250, None);
        }
        Ok(win)
    }

    pub fn destroy(&self) {
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }

    fn invalidate(&self) {
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    fn paint(&self) {
        let mut slot = self.target.borrow_mut();
        if slot.is_none() {
            let s = self.scale;
            let (w, h) = (
                (self.layout.size.0 * s).round() as u32,
                (self.layout.size.1 * s).round() as u32,
            );
            match Target::new(&self.shared.gpu, self.hwnd, w, h, (s * 96.0).round() as u32) {
                Ok(t) => *slot = Some(t),
                Err(e) => {
                    eprintln!("horadric: render target for the catch-up: {e}");
                    return;
                }
            }
        }
        let looks: Vec<CatchupLook> = self
            .rows
            .iter()
            .map(|r| CatchupLook {
                text: &r.text,
                detail: &r.detail,
                age: &r.age,
                tone: tone(r.section),
            })
            .collect();
        let scene = CatchupScene {
            layout: &self.layout,
            title: TITLE,
            sub: &self.sub,
            rows: &looks,
            more: &self.more,
            hot: self.hot.get(),
            close_hot: self.close_hot.get(),
        };
        let failed = slot
            .as_ref()
            .map(|t| t.draw_catchup(&self.shared.gpu, &self.shared.metrics, &scene))
            .is_some_and(|r| r.is_err());
        if failed {
            *slot = None;
        }
    }

    fn point(&self, lparam: LPARAM) -> (f32, f32) {
        let x = (lparam.0 & 0xffff) as i16 as f32 / self.scale;
        let y = ((lparam.0 >> 16) & 0xffff) as i16 as f32 / self.scale;
        (x, y)
    }

    /// The line under a point, only one a click can show.
    fn hit(&self, lparam: LPARAM) -> Option<usize> {
        let (x, y) = self.point(lparam);
        layout::catchup_hit(&self.layout, x, y)
            .filter(|&i| self.rows.get(i).is_some_and(|r| !r.session.is_empty()))
    }

    fn on_close(&self, lparam: LPARAM) -> bool {
        let (x, y) = self.point(lparam);
        self.layout.close.contains(x, y)
    }

    fn inside(&self, lparam: LPARAM) -> bool {
        let (x, y) = self.point(lparam);
        let (w, h) = self.layout.size;
        x >= 0.0 && y >= 0.0 && x < w && y < h
    }

    /// Hands the focus back and tells the app, which destroys the window,
    /// with the session to show if a line was clicked.
    fn close(&self, pick: Option<usize>) {
        if self.closed.replace(true) {
            return;
        }
        let session = pick
            .and_then(|i| self.rows.get(i))
            .map(|r| r.session.clone());
        unsafe {
            let _ = KillTimer(Some(self.hwnd), WATCH);
            if GetCapture() == self.hwnd {
                let _ = ReleaseCapture();
            }
            // A line clicked shows its session, which takes the focus.
            if session.is_none()
                && self.had_focus.get()
                && !self.before.is_invalid()
                && IsWindow(Some(self.before)).as_bool()
            {
                let _ = SetForegroundWindow(self.before);
            }
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
        app::push(Input::Listened(session));
    }

    fn handle(&self, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
        match msg {
            WM_PAINT => {
                self.paint();
                unsafe {
                    let _ = ValidateRect(Some(self.hwnd), None);
                }
                Some(LRESULT(0))
            }
            WM_ERASEBKGND => Some(LRESULT(1)),
            WM_MOUSEMOVE => {
                let hot = self.hit(lparam);
                let close = self.on_close(lparam);
                if self.hot.replace(hot) != hot || self.close_hot.replace(close) != close {
                    self.invalidate();
                }
                Some(LRESULT(0))
            }
            WM_LBUTTONDOWN => {
                if self.inside(lparam) {
                    self.pressed.set(self.hit(lparam));
                } else {
                    self.close(None);
                }
                Some(LRESULT(0))
            }
            WM_RBUTTONDOWN | WM_MBUTTONDOWN => {
                if !self.inside(lparam) {
                    self.close(None);
                }
                Some(LRESULT(0))
            }
            WM_LBUTTONUP => {
                let pressed = self.pressed.take();
                if self.on_close(lparam) {
                    self.close(None);
                } else if let (Some(p), Some(h)) = (pressed, self.hit(lparam)) {
                    if p == h {
                        self.close(Some(h));
                    }
                }
                Some(LRESULT(0))
            }
            WM_KEYDOWN => {
                if wparam.0 as u16 == VK_ESCAPE.0 {
                    self.close(None);
                }
                Some(LRESULT(0))
            }
            WM_ACTIVATE => {
                if (wparam.0 & 0xffff) as u32 == WA_INACTIVE {
                    if self.had_focus.get() {
                        self.close(None);
                    }
                } else if !self.had_focus.replace(true) {
                    // Clicked into after Windows refused it the focus.
                    unsafe { SetCapture(self.hwnd) };
                }
                None
            }
            WM_TIMER if wparam.0 == WATCH => {
                let front = unsafe { GetForegroundWindow() };
                if !self.had_focus.get() && front != self.before && front != self.hwnd {
                    self.close(None);
                }
                Some(LRESULT(0))
            }
            WM_CAPTURECHANGED => {
                // Someone else took the mouse: the panel would no longer
                // hear the click that closes it.
                if self.had_focus.get() && HWND(lparam.0 as *mut c_void) != self.hwnd {
                    self.close(None);
                }
                None
            }
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
    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const Catchup;
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
    use horadric_core::journal::Line;

    #[test]
    fn idling_long_enough_is_away_and_input_after_it_is_back() {
        let mut a = Away::default();
        assert_eq!(a.idle(60, 1000), None);
        assert_eq!(a.idle(0, 1000), None, "never away, nothing to tell");
        assert_eq!(a.idle(AWAY_AFTER, 5000), None);
        assert_eq!(a.idle(AWAY_AFTER + 600, 5600), None);
        assert_eq!(a.idle(1, 5700), Some(5000 - AWAY_AFTER));
        assert_eq!(a.idle(1, 5701), None, "said once");
    }

    #[test]
    fn a_locked_screen_is_away_until_it_unlocks() {
        let mut a = Away::default();
        a.lock(100);
        assert_eq!(a.idle(0, 200), None, "typing the password");
        assert_eq!(a.idle(AWAY_AFTER + 10, 2000), None);
        assert_eq!(a.unlock(), Some(100));
        assert_eq!(a.unlock(), None);
    }

    #[test]
    fn it_says_how_long_you_were_away_or_since_when() {
        assert_eq!(
            covers(Some(0), 0, 2 * 3600 + 300, "09:00"),
            "While you were away, 2 h 05 min"
        );
        assert_eq!(covers(None, 0, 100, "08:00"), "Since 08:00");
        assert_eq!(covers(None, 0, 3600, ""), "The last 1 h 00 min");
    }

    #[test]
    fn each_project_gets_its_name_over_its_lines() {
        let line = |text: &str, detail: &str, session: &str| Line {
            section: Section::Waiting,
            at: 40,
            session: session.to_string(),
            text: text.to_string(),
            detail: detail.to_string(),
        };
        let groups = [
            Group {
                project: "c:/code/app".into(),
                lines: vec![line("one", "allow Bash?", "a"), line("two", "", "b")],
            },
            Group {
                project: String::new(),
                lines: vec![line("Usage limit reached", "", "")],
            },
        ];
        let rows = rows(&groups, 100, |k| k.rsplit('/').next().unwrap().to_string());
        let kinds: Vec<CatchupKind> = rows.iter().map(|r| r.kind).collect();
        assert_eq!(
            kinds,
            [
                CatchupKind::Heading,
                CatchupKind::Line { detail: true },
                CatchupKind::Line { detail: false },
                CatchupKind::Heading,
                CatchupKind::Line { detail: false },
            ]
        );
        assert_eq!(rows[0].text, "app");
        assert_eq!(rows[3].text, "Account");
        assert_eq!(rows[1].age, "1 min");
        assert_eq!(rows[2].session, "b");
    }

    #[test]
    fn the_clock_gives_the_local_time_of_a_moment() {
        // 10:30 local now; an hour and a quarter ago was 09:15.
        let now = 1_000_000;
        assert_eq!(clock(10 * 3600 + 1800, now, now - 4500), "09:15");
        // Before midnight wraps to the day before.
        assert_eq!(clock(600, now, now - 1200), "23:50");
    }
}
