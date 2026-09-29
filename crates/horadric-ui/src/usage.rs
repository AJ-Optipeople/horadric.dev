//! The usage window: how much of the account's Claude limits is used, and
//! the model, effort and permission mode every session Horadric starts gets.
//!
//! It belongs to no project, so it is a window of its own rather than a tile
//! in a cluster, and it sits at the top of the first column of tiles. It
//! behaves like a cluster: it never takes the focus, and it drags to
//! another place in the columns the same way. A click on the limits folds
//! it down to the session's budget alone. A list setting drops its list,
//! which the app opens, since it owns the defaults. Effort is a slider in
//! the window itself. Under the settings, the Claude account in use, whose
//! row opens the accounts to switch to.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::rc::Rc;
use std::time::SystemTime;

use horadric_core::Setting;
use windows::core::{w, Result, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
    DWM_WINDOW_CORNER_PREFERENCE,
};
use windows::Win32::Graphics::Gdi::{InvalidateRect, ScreenToClient, ValidateRect};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetCursorPos, GetWindowLongPtrW, GetWindowRect,
    LoadCursorW, RegisterClassW, SetWindowLongPtrW, SetWindowPos, ShowWindow, CREATESTRUCTW,
    CS_HREDRAW, CS_VREDRAW, GWLP_USERDATA, HWND_NOTOPMOST, HWND_TOPMOST, IDC_ARROW, MA_NOACTIVATE,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SW_SHOWNOACTIVATE, WM_CAPTURECHANGED,
    WM_DPICHANGED, WM_ERASEBKGND, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEACTIVATE, WM_MOUSEMOVE,
    WM_MOUSEWHEEL, WM_NCCREATE, WM_NCDESTROY, WM_PAINT, WM_RBUTTONUP, WM_SIZE, WNDCLASSW,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_POPUP,
};

use crate::app::{self, Input};
use crate::layout::{self, UsageHit, UsageLayout};
use crate::render::{SettingLook, Target, UsageScene};
use crate::window::Shared;
use crate::{backdrop, columns};

pub(crate) const CLASS: PCWSTR = w!("HoradricUsage");
const DRAG_THRESHOLD: i32 = 4;
/// The windows crate files this under `Win32_UI_Controls`.
const WM_MOUSELEAVE: u32 = 0x02A3;

pub struct UsageWindow {
    pub hwnd: HWND,
    pub collapsed: Cell<bool>,
    shared: Rc<Shared>,
    target: RefCell<Option<Target>>,
    layout: RefCell<UsageLayout>,
    drag: RefCell<Option<Drag>>,
    hot: Cell<UsageHit>,
    pressed: Cell<Option<UsageHit>>,
    tracking: Cell<bool>,
    /// The setting whose list is dropped down, set by the app.
    pub open: Cell<Option<usize>>,
    /// A slider held down: its setting and the stop it is at so far. It is
    /// set when let go.
    slide: Cell<Option<(usize, usize)>>,
    /// A newer release's version, set by the app when a check finds one.
    pub update: RefCell<Option<String>>,
}

/// Which rows of settings are sliders, in the order the window shows
/// them: the settings, then the account and the version.
fn scales() -> Vec<bool> {
    Setting::ALL
        .iter()
        .map(|s| s.is_scale())
        .chain([false, false])
        .collect()
}

/// The row after the settings, which names the account.
const ACCOUNT_ROW: usize = Setting::ALL.len();
/// The last row, which says which Horadric this is.
const VERSION_ROW: usize = ACCOUNT_ROW + 1;

/// What the Version row says: this build's version, or the release to
/// update to when a check found one, which a click then offers.
fn version_value(current: &str, update: Option<&str>) -> String {
    match update {
        Some(v) => format!("Update to {v}"),
        None => current.to_string(),
    }
}

/// What each stop of a scale sets: the default, then its values.
fn stops(setting: Setting) -> Vec<Option<&'static str>> {
    std::iter::once(None)
        .chain(setting.choices().iter().map(|(v, _)| Some(*v)))
        .collect()
}

/// What the Account row says: the address, or only the part before its @
/// when the whole would crowd the label.
fn account_name(email: Option<&str>) -> String {
    const ROOM: usize = 20;
    match email {
        Some(e) if e.chars().count() > ROOM => e.split('@').next().unwrap_or(e).to_string(),
        Some(e) => e.to_string(),
        None => "Not logged in".to_string(),
    }
}

struct Drag {
    start_cursor: POINT,
    start_window: POINT,
    moved: bool,
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

impl UsageWindow {
    /// Creates the window at `(x, y)` in physical pixels and shows it
    /// without activating it.
    pub fn create(shared: Rc<Shared>, collapsed: bool, x: i32, y: i32) -> Result<Box<Self>> {
        let initial = layout::usage(&shared.metrics, 0, &scales(), collapsed);
        let mut win = Box::new(UsageWindow {
            hwnd: HWND::default(),
            collapsed: Cell::new(collapsed),
            shared,
            target: RefCell::new(None),
            layout: RefCell::new(initial),
            drag: RefCell::new(None),
            hot: Cell::new(UsageHit::Nothing),
            pressed: Cell::new(None),
            tracking: Cell::new(false),
            open: Cell::new(None),
            slide: Cell::new(None),
            update: RefCell::new(None),
        });
        unsafe {
            let hwnd = CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                CLASS,
                w!("Horadric usage"),
                WS_POPUP,
                x,
                y,
                10,
                10,
                None,
                None,
                Some(GetModuleHandleW(None)?.into()),
                Some(&*win as *const UsageWindow as *const c_void),
            )?;
            win.hwnd = hwnd;
            let pref: DWM_WINDOW_CORNER_PREFERENCE = DWMWCP_ROUND;
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_WINDOW_CORNER_PREFERENCE,
                &pref as *const _ as *const c_void,
                std::mem::size_of::<DWM_WINDOW_CORNER_PREFERENCE>() as u32,
            );
            // The clay has its own edge.
            backdrop::border(hwnd, None);
            win.fit();
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        }
        Ok(win)
    }

    pub fn destroy(&self) {
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }

    pub fn dpi(&self) -> u32 {
        unsafe { GetDpiForWindow(self.hwnd) }.max(96)
    }

    fn scale(&self) -> f32 {
        self.dpi() as f32 / 96.0
    }

    pub fn size_px(&self) -> (i32, i32) {
        let l = self.layout.borrow();
        let s = self.scale();
        ((l.size.0 * s).round() as i32, (l.size.1 * s).round() as i32)
    }

    pub fn position(&self) -> (i32, i32) {
        let mut r = RECT::default();
        unsafe {
            let _ = GetWindowRect(self.hwnd, &mut r);
        }
        (r.left, r.top)
    }

    pub fn move_to(&self, x: i32, y: i32) {
        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                None,
                x,
                y,
                0,
                0,
                SWP_NOACTIVATE | SWP_NOZORDER | SWP_NOSIZE,
            );
        }
    }

    /// Above other windows without activating, as a cluster does it.
    pub fn raise(&self) {
        for after in [HWND_TOPMOST, HWND_NOTOPMOST] {
            unsafe {
                let _ = SetWindowPos(
                    self.hwnd,
                    Some(after),
                    0,
                    0,
                    0,
                    0,
                    SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE,
                );
            }
        }
    }

    pub fn invalidate(&self) {
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    fn limits(&self) -> usize {
        self.shared
            .usage
            .lock()
            .ok()
            .and_then(|u| u.as_ref().map(|u| u.limits.named().len()))
            .unwrap_or(0)
    }

    /// Lays out again for the limits known now and resizes to fit. True
    /// when the size changed, so the stack needs arranging again.
    pub fn fit(&self) -> bool {
        let l = layout::usage(
            &self.shared.metrics,
            self.limits(),
            &scales(),
            self.collapsed.get(),
        );
        *self.layout.borrow_mut() = l;
        // Folding moves the rows out from under a cursor that has not moved.
        self.refresh_hover();
        let (w, h) = self.size_px();
        let mut r = RECT::default();
        unsafe {
            let _ = GetWindowRect(self.hwnd, &mut r);
        }
        let changed = (r.right - r.left, r.bottom - r.top) != (w, h);
        if changed {
            unsafe {
                let _ = SetWindowPos(
                    self.hwnd,
                    None,
                    0,
                    0,
                    w,
                    h,
                    SWP_NOACTIVATE | SWP_NOZORDER | SWP_NOMOVE,
                );
            }
        }
        self.invalidate();
        changed
    }

    fn paint(&self) {
        let (w, h) = self.size_px();
        let dpi = self.dpi();
        let mut slot = self.target.borrow_mut();
        if slot.is_none() {
            match Target::new(&self.shared.gpu, self.hwnd, w as u32, h as u32, dpi) {
                Ok(t) => *slot = Some(t),
                Err(e) => {
                    eprintln!("horadric: render target for the usage window: {e}");
                    return;
                }
            }
        }
        let usage = self.shared.usage.lock().ok().and_then(|u| u.clone());
        let defaults = self.shared.defaults.borrow();
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let layout = self.layout.borrow();
        let scene = UsageScene {
            layout: &layout,
            collapsed: self.collapsed.get(),
            usage: usage.as_ref(),
            now,
            settings: Setting::ALL
                .iter()
                .enumerate()
                .map(|(i, &s)| {
                    let value = match self.slide.get() {
                        Some((j, stop)) if j == i => stops(s).get(stop).copied().flatten(),
                        _ => defaults.get(s),
                    };
                    let all = stops(s);
                    SettingLook {
                        label: s.label(),
                        value: s.name_of(value).to_string(),
                        stop: s.is_scale().then(|| {
                            let at = all.iter().position(|v| *v == value).unwrap_or(0);
                            (at, all.len())
                        }),
                        list: true,
                    }
                })
                .chain([SettingLook {
                    label: "Account",
                    value: account_name(self.shared.account.borrow().as_deref()),
                    stop: None,
                    list: true,
                }])
                .chain([SettingLook {
                    label: "Version",
                    value: version_value(
                        env!("CARGO_PKG_VERSION"),
                        self.update.borrow().as_deref(),
                    ),
                    stop: None,
                    list: false,
                }])
                .collect(),
            hot: self.hot.get(),
            pressed: self.pressed.get(),
            open: self.open.get(),
        };
        let result = slot
            .as_ref()
            .map(|t| t.draw_usage(&self.shared.gpu, &self.shared.metrics, &scene));
        if let Some(Err(_)) = result {
            *slot = None;
        }
    }

    fn hit(&self, lparam: LPARAM) -> UsageHit {
        let s = self.scale();
        let x = (lparam.0 & 0xffff) as i16 as f32 / s;
        let y = ((lparam.0 >> 16) & 0xffff) as i16 as f32 / s;
        layout::usage_hit(&self.layout.borrow(), x, y)
    }

    fn hover(&self, hot: UsageHit) {
        if self.hot.replace(hot) != hot {
            self.invalidate();
        }
    }

    fn press(&self, pressed: Option<UsageHit>) {
        if self.pressed.replace(pressed) != pressed {
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

    fn click(&self, hit: UsageHit) {
        match hit {
            UsageHit::Limits => {
                self.collapsed.set(!self.collapsed.get());
                self.fit();
                app::push(Input::Arrange);
            }
            UsageHit::Setting(ACCOUNT_ROW) => app::push(Input::AccountMenu),
            UsageHit::Setting(VERSION_ROW) => app::push(Input::Version),
            UsageHit::Setting(i) => {
                if let (Some(&s), Some(row)) = (Setting::ALL.get(i), self.row_on_screen(i)) {
                    app::push(Input::SettingMenu(s, row));
                }
            }
            UsageHit::Nothing => {}
        }
    }

    /// Setting `i`'s row in screen pixels, for its list to drop from.
    fn row_on_screen(&self, i: usize) -> Option<RECT> {
        let r = self.layout.borrow().settings.get(i)?.rect;
        let (x, y) = self.position();
        let s = self.scale();
        let px = |v: f32| (v * s).round() as i32;
        Some(RECT {
            left: x + px(r.x),
            top: y + px(r.y),
            right: x + px(r.right()),
            bottom: y + px(r.bottom()),
        })
    }

    /// The slider under a point, if the point is on one's half of its row:
    /// the setting and the stop nearest.
    fn slider_at(&self, lparam: LPARAM) -> Option<(usize, usize)> {
        let s = self.scale();
        let x = (lparam.0 & 0xffff) as i16 as f32 / s;
        let y = ((lparam.0 >> 16) & 0xffff) as i16 as f32 / s;
        let l = self.layout.borrow();
        let (i, row) = l
            .settings
            .iter()
            .enumerate()
            .find(|(_, r)| r.rect.contains(x, y))?;
        let track = row.track?;
        if y < row.line.y + row.line.h / 2.0 {
            return None;
        }
        let n = stops(*Setting::ALL.get(i)?).len();
        Some((i, layout::slider_stop(&track, n, x)))
    }

    /// Moves a held slider to the stop under the cursor, past its ends too.
    fn slide_to(&self, lparam: LPARAM) {
        let Some((i, _)) = self.slide.get() else {
            return;
        };
        let s = self.scale();
        let x = (lparam.0 & 0xffff) as i16 as f32 / s;
        let l = self.layout.borrow();
        let (Some(track), Some(&setting)) =
            (l.settings.get(i).and_then(|r| r.track), Setting::ALL.get(i))
        else {
            return;
        };
        let stop = layout::slider_stop(&track, stops(setting).len(), x);
        if self.slide.replace(Some((i, stop))) != Some((i, stop)) {
            self.invalidate();
        }
    }

    /// Lets go of a held slider, which sets its value.
    fn release_slider(&self) {
        let Some((i, stop)) = self.slide.take() else {
            return;
        };
        let Some(&setting) = Setting::ALL.get(i) else {
            return;
        };
        let value = stops(setting).get(stop).copied().flatten();
        if value != self.shared.defaults.borrow().get(setting) {
            app::push(Input::SetDefault(setting, value.map(str::to_string)));
        }
        self.invalidate();
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
            WM_RBUTTONUP => {
                app::push(Input::UsageMenu);
                Some(LRESULT(0))
            }
            WM_ERASEBKGND => Some(LRESULT(1)),
            WM_MOUSEACTIVATE => Some(LRESULT(MA_NOACTIVATE as isize)),
            WM_SIZE => {
                let w = (lparam.0 & 0xffff) as u32;
                let h = ((lparam.0 >> 16) & 0xffff) as u32;
                if let Some(t) = self.target.borrow().as_ref() {
                    let _ = t.resize(w, h);
                }
                Some(LRESULT(0))
            }
            WM_DPICHANGED => {
                let dpi = (wparam.0 & 0xffff) as u32;
                if let Some(t) = self.target.borrow().as_ref() {
                    t.set_dpi(dpi);
                }
                let suggested = unsafe { *(lparam.0 as *const RECT) };
                let (w, h) = self.size_px();
                unsafe {
                    let _ = SetWindowPos(
                        self.hwnd,
                        None,
                        suggested.left,
                        suggested.top,
                        w,
                        h,
                        SWP_NOACTIVATE | SWP_NOZORDER,
                    );
                }
                app::push(Input::Arrange);
                Some(LRESULT(0))
            }
            WM_MOUSEWHEEL => {
                let notches = ((wparam.0 >> 16) & 0xffff) as i16 as i32 / 120;
                app::push(Input::Scroll(columns::USAGE.into(), notches));
                Some(LRESULT(0))
            }
            WM_LBUTTONDOWN => {
                self.raise();
                let mut cursor = POINT::default();
                unsafe {
                    let _ = GetCursorPos(&mut cursor);
                    SetCapture(self.hwnd);
                }
                self.press(Some(self.hit(lparam)));
                // A slider moves under the mouse instead of the window.
                if let Some(at) = self.slider_at(lparam) {
                    self.slide.set(Some(at));
                    self.invalidate();
                    return Some(LRESULT(0));
                }
                let (x, y) = self.position();
                *self.drag.borrow_mut() = Some(Drag {
                    start_cursor: cursor,
                    start_window: POINT { x, y },
                    moved: false,
                });
                Some(LRESULT(0))
            }
            WM_MOUSEMOVE => {
                self.track();
                self.hover(self.hit(lparam));
                if self.slide.get().is_some() {
                    self.slide_to(lparam);
                    return Some(LRESULT(0));
                }
                let mut drag = self.drag.borrow_mut();
                if let Some(d) = drag.as_mut() {
                    let mut cursor = POINT::default();
                    unsafe {
                        let _ = GetCursorPos(&mut cursor);
                    }
                    let dx = cursor.x - d.start_cursor.x;
                    let dy = cursor.y - d.start_cursor.y;
                    if d.moved || dx.abs() > DRAG_THRESHOLD || dy.abs() > DRAG_THRESHOLD {
                        d.moved = true;
                        self.press(None);
                        self.move_to(d.start_window.x + dx, d.start_window.y + dy);
                        app::push(Input::Carry(
                            columns::USAGE.into(),
                            Some((cursor.x, cursor.y)),
                        ));
                    }
                }
                Some(LRESULT(0))
            }
            WM_LBUTTONUP => {
                // Before letting go: releasing capture sends
                // WM_CAPTURECHANGED at once, and that clears the press.
                let pressed = self.pressed.get();
                let sliding = self.slide.get().is_some();
                if sliding {
                    self.slide_to(lparam);
                    self.release_slider();
                }
                // Taken first: letting go of the capture sends
                // WM_CAPTURECHANGED, which calls off a drag it still finds.
                let drag = self.drag.borrow_mut().take();
                unsafe {
                    let _ = ReleaseCapture();
                }
                self.press(None);
                if sliding {
                    return Some(LRESULT(0));
                }
                match drag {
                    Some(d) if d.moved => {
                        let mut cursor = POINT::default();
                        unsafe {
                            let _ = GetCursorPos(&mut cursor);
                        }
                        app::push(Input::Drop(columns::USAGE.into(), cursor.x, cursor.y));
                    }
                    // Only where the press began, as a button does.
                    Some(_) if pressed == Some(self.hit(lparam)) => self.click(self.hit(lparam)),
                    _ => {}
                }
                Some(LRESULT(0))
            }
            WM_MOUSELEAVE => {
                self.tracking.set(false);
                self.hover(UsageHit::Nothing);
                Some(LRESULT(0))
            }
            WM_CAPTURECHANGED => {
                self.press(None);
                if self.drag.borrow_mut().take().is_some_and(|d| d.moved) {
                    app::push(Input::Carry(columns::USAGE.into(), None));
                }
                // Taken away mid slide: the stop it was at counts.
                self.release_slider();
                None
            }
            _ => None,
        }
    }

    /// Notes what the cursor is over now, when it is over the window.
    fn refresh_hover(&self) {
        if !self.tracking.get() {
            return;
        }
        let mut p = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut p);
            let _ = ScreenToClient(self.hwnd, &mut p);
        }
        let s = self.scale();
        let hit = layout::usage_hit(&self.layout.borrow(), p.x as f32 / s, p.y as f32 / s);
        self.hover(hit);
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_NCCREATE {
        let cs = &*(lparam.0 as *const CREATESTRUCTW);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, cs.lpCreateParams as isize);
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const UsageWindow;
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
    fn a_scale_starts_at_its_default_then_runs_through_its_values() {
        let all = stops(Setting::Effort);
        assert_eq!(all.first(), Some(&None));
        assert_eq!(all.last(), Some(&Some("max")));
        assert_eq!(all.len(), Setting::Effort.choices().len() + 1);
        assert_eq!(scales(), [false, true, false, false, false]);
    }

    #[test]
    fn the_version_row_offers_a_newer_release() {
        assert_eq!(version_value("0.3.1", None), "0.3.1");
        assert_eq!(version_value("0.3.1", Some("0.4.0")), "Update to 0.4.0");
    }

    #[test]
    fn an_account_is_named_by_its_address_short_enough_to_fit() {
        assert_eq!(account_name(Some("a@x.dk")), "a@x.dk");
        assert_eq!(
            account_name(Some("someone.long@example.com")),
            "someone.long"
        );
        assert_eq!(account_name(None), "Not logged in");
    }
}
