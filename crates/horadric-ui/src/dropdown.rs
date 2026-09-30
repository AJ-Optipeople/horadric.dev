//! A setting's list, dropped down under its row in the usage window.
//!
//! Drawn like the rest of the app rather than as a Windows menu. It is the
//! one window of the app that takes the focus, and it holds the mouse while
//! open, so a click anywhere else closes it, as a list box's does, and the
//! arrow keys, Enter and Escape work on it. Whatever had the focus gets it
//! back when it closes.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::rc::Rc;

use horadric_core::{Agent, Setting};
use windows::core::{w, Result, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
    DWM_WINDOW_CORNER_PREFERENCE,
};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, InvalidateRect, MonitorFromRect, ValidateRect, MONITORINFO,
    MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetCapture, ReleaseCapture, SetCapture, VK_DOWN, VK_ESCAPE, VK_RETURN, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect, GetForegroundWindow,
    GetWindowLongPtrW, IsWindow, LoadCursorW, RegisterClassW, SetForegroundWindow,
    SetWindowLongPtrW, ShowWindow, CREATESTRUCTW, CS_HREDRAW, CS_VREDRAW, GWLP_USERDATA, IDC_ARROW,
    SW_HIDE, SW_SHOW, WA_INACTIVE, WM_ACTIVATE, WM_CAPTURECHANGED, WM_ERASEBKGND, WM_KEYDOWN,
    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MOUSEMOVE, WM_NCCREATE, WM_NCDESTROY,
    WM_PAINT, WM_RBUTTONDOWN, WNDCLASSW, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

use crate::app::{self, Input};
use crate::backdrop;
use crate::layout::{self, DropdownLayout};
use crate::render::{DropdownScene, Target};
use crate::window::Shared;

pub(crate) const CLASS: PCWSTR = w!("HoradricDropdown");

/// How far below its row the list drops, in DIPs.
const DROP: f32 = 4.0;

pub struct Dropdown {
    pub hwnd: HWND,
    /// Whose setting it is: the usage window's screen it dropped from.
    pub agent: Agent,
    pub setting: Setting,
    shared: Rc<Shared>,
    target: RefCell<Option<Target>>,
    layout: DropdownLayout,
    labels: Vec<&'static str>,
    /// What each row sets, None for the default.
    values: Vec<Option<&'static str>>,
    current: usize,
    hot: Cell<Option<usize>>,
    pressed: Cell<Option<usize>>,
    /// The window that had the focus before, which gets it back.
    before: HWND,
    closed: Cell<bool>,
}

pub fn register_class() -> Result<()> {
    unsafe {
        let wc = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
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

/// When a pick takes hold, the note on top of the list. A running Claude
/// Code session switches once it is free, see
/// [`horadric_core::Session::free_for_command`]. The other agents have no
/// command Horadric can type in.
fn note(agent: Agent, setting: Setting) -> &'static str {
    if agent == Agent::Claude && setting.command(None).is_some() {
        "Running sessions switch too"
    } else {
        "From the next start or resume"
    }
}

impl Dropdown {
    /// Opens the list for `setting` under `row`, the setting's row in screen
    /// pixels at `dpi`, or over it when there is no room below.
    pub fn open(
        shared: Rc<Shared>,
        agent: Agent,
        setting: Setting,
        current: Option<&str>,
        row: RECT,
        dpi: u32,
    ) -> Result<Box<Self>> {
        let choices = setting.choices(agent);
        let mut labels = vec!["Default"];
        let mut values = vec![None];
        for (v, name) in choices {
            labels.push(name);
            values.push(Some(*v));
        }
        let current = values
            .iter()
            .position(|v| *v == current)
            .unwrap_or_default();
        // The last permission mode sits apart, so it is never picked by a slip.
        let apart = (setting == Setting::Permissions).then(|| labels.len() - 1);
        let s = dpi.max(96) as f32 / 96.0;
        let width = (row.right - row.left) as f32 / s;
        let layout = layout::dropdown(&shared.metrics, width, labels.len(), apart);
        let (w, h) = (
            (layout.size.0 * s).round() as i32,
            (layout.size.1 * s).round() as i32,
        );
        let drop = (DROP * s).round() as i32;
        let below = row.bottom + drop;
        let y = if below + h > work_bottom(&row) {
            row.top - drop - h
        } else {
            below
        };
        let mut win = Box::new(Dropdown {
            hwnd: HWND::default(),
            agent,
            setting,
            shared,
            target: RefCell::new(None),
            layout,
            labels,
            values,
            current,
            hot: Cell::new(None),
            pressed: Cell::new(None),
            before: unsafe { GetForegroundWindow() },
            closed: Cell::new(false),
        });
        unsafe {
            // Born where it shows and at its size: a window that starts
            // elsewhere and moves has been seen not to paint.
            let hwnd = CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
                CLASS,
                w!("Horadric setting"),
                WS_POPUP,
                row.left,
                y,
                w,
                h,
                None,
                None,
                Some(GetModuleHandleW(None)?.into()),
                Some(&*win as *const Dropdown as *const c_void),
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
            SetCapture(hwnd);
        }
        Ok(win)
    }

    pub fn destroy(&self) {
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }

    fn scale(&self) -> f32 {
        unsafe { GetDpiForWindow(self.hwnd) }.max(96) as f32 / 96.0
    }

    fn invalidate(&self) {
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    fn paint(&self) {
        let mut r = RECT::default();
        unsafe {
            let _ = GetClientRect(self.hwnd, &mut r);
        }
        let dpi = unsafe { GetDpiForWindow(self.hwnd) }.max(96);
        let mut slot = self.target.borrow_mut();
        if slot.is_none() {
            let (w, h) = (r.right as u32, r.bottom as u32);
            match Target::new(&self.shared.gpu, self.hwnd, w, h, dpi) {
                Ok(t) => *slot = Some(t),
                Err(e) => {
                    eprintln!("horadric: render target for a setting's list: {e}");
                    return;
                }
            }
        }
        let scene = DropdownScene {
            layout: &self.layout,
            note: note(self.agent, self.setting),
            items: &self.labels,
            current: self.current,
            hot: self.hot.get(),
            pressed: self.pressed.get(),
        };
        let result = slot
            .as_ref()
            .map(|t| t.draw_dropdown(&self.shared.gpu, &self.shared.metrics, &scene));
        if let Some(Err(_)) = result {
            *slot = None;
        }
    }

    /// The row under a point in client pixels. None off the list, which
    /// with the mouse held can be anywhere on screen.
    fn hit(&self, lparam: LPARAM) -> Option<usize> {
        let s = self.scale();
        let x = (lparam.0 & 0xffff) as i16 as f32 / s;
        let y = ((lparam.0 >> 16) & 0xffff) as i16 as f32 / s;
        layout::dropdown_hit(&self.layout, x, y)
    }

    fn inside(&self, lparam: LPARAM) -> bool {
        let s = self.scale();
        let x = (lparam.0 & 0xffff) as i16 as f32 / s;
        let y = ((lparam.0 >> 16) & 0xffff) as i16 as f32 / s;
        let (w, h) = self.layout.size;
        x >= 0.0 && y >= 0.0 && x < w && y < h
    }

    fn set_hot(&self, hot: Option<usize>) {
        if self.hot.replace(hot) != hot {
            self.invalidate();
        }
    }

    /// Hands the focus back and tells the app, which destroys the window.
    /// `pick` is the row picked, if one was.
    fn close(&self, pick: Option<usize>) {
        if self.closed.replace(true) {
            return;
        }
        unsafe {
            if GetCapture() == self.hwnd {
                let _ = ReleaseCapture();
            }
            // Before hiding: hiding the active window hands the focus to
            // whatever Windows picks.
            if !self.before.is_invalid() && IsWindow(Some(self.before)).as_bool() {
                let _ = SetForegroundWindow(self.before);
            }
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
        let value = pick
            .and_then(|i| self.values.get(i))
            .map(|v| v.map(str::to_string));
        app::push(Input::Picked(self.agent, self.setting, value));
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
                self.set_hot(self.hit(lparam));
                Some(LRESULT(0))
            }
            WM_LBUTTONDOWN => {
                if self.inside(lparam) {
                    let hit = self.hit(lparam);
                    if self.pressed.replace(hit) != hit {
                        self.invalidate();
                    }
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
                let hit = self.hit(lparam);
                match (pressed, hit) {
                    (Some(p), Some(h)) if p == h => self.close(Some(h)),
                    _ => self.invalidate(),
                }
                Some(LRESULT(0))
            }
            WM_KEYDOWN => {
                let n = self.labels.len();
                let at = self.hot.get().unwrap_or(self.current);
                match wparam.0 as u16 {
                    k if k == VK_ESCAPE.0 => self.close(None),
                    k if k == VK_RETURN.0 => self.close(Some(at)),
                    k if k == VK_UP.0 => self.set_hot(Some((at + n - 1) % n)),
                    k if k == VK_DOWN.0 => self.set_hot(Some((at + 1) % n)),
                    _ => {}
                }
                Some(LRESULT(0))
            }
            WM_ACTIVATE => {
                if (wparam.0 & 0xffff) as u32 == WA_INACTIVE {
                    self.close(None);
                }
                None
            }
            WM_CAPTURECHANGED => {
                // Someone else took the mouse, a drag or a menu: the list
                // would no longer hear the click that closes it.
                if HWND(lparam.0 as *mut c_void) != self.hwnd {
                    self.close(None);
                }
                None
            }
            _ => None,
        }
    }
}

/// The bottom of the work area of the screen `row` is on.
fn work_bottom(row: &RECT) -> i32 {
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    unsafe {
        let monitor = MonitorFromRect(row, MONITOR_DEFAULTTONEAREST);
        if GetMonitorInfoW(monitor, &mut info).as_bool() {
            info.rcWork.bottom
        } else {
            i32::MAX
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_NCCREATE {
        let cs = &*(lparam.0 as *const CREATESTRUCTW);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, cs.lpCreateParams as isize);
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const Dropdown;
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

    #[test]
    fn the_note_says_when_a_pick_takes_hold() {
        let claude = |s| note(Agent::Claude, s);
        assert!(claude(Setting::Model).starts_with("Running"));
        assert!(claude(Setting::Effort).starts_with("Running"));
        assert!(claude(Setting::Permissions).starts_with("From the next"));
        assert!(note(Agent::Codex, Setting::Model).starts_with("From the next"));
    }
}
