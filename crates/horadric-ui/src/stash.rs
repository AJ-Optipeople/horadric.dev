//! The stash: a list of sessions put away for later, a row each.
//!
//! History has every conversation; the stash has the few you chose. A
//! stashed session is paused and out of the columns, and a click on its
//! slot brings it back as it was, resumed on the stage. Like the usage
//! window it belongs to no project, so it is a window of its own in the
//! columns, dragged about the same way, and there only while it holds
//! something. The app owns what it holds and hands the window each slot's
//! look.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::rc::Rc;

use windows::core::{w, Result, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
    DWM_WINDOW_CORNER_PREFERENCE,
};
use windows::Win32::Graphics::Gdi::{InvalidateRect, ValidateRect};
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
    WM_MOUSEWHEEL, WM_NCCREATE, WM_NCDESTROY, WM_PAINT, WM_RBUTTONUP, WM_SIZE, WM_TIMER, WNDCLASSW,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_POPUP,
};

use crate::app::{self, Input};
use crate::layout::{self, StashLayout};
use crate::render::{StashLook, StashScene, Target};
use crate::tip;
use crate::window::Shared;
use crate::{appear, backdrop, columns};

pub(crate) const CLASS: PCWSTR = w!("HoradricStash");
const DRAG_THRESHOLD: i32 = 4;
/// The windows crate files this under `Win32_UI_Controls`.
const WM_MOUSELEAVE: u32 = 0x02A3;

pub struct StashWindow {
    pub hwnd: HWND,
    shared: Rc<Shared>,
    target: RefCell<Option<Target>>,
    /// Made again whenever the number of rows changes.
    layout: RefCell<StashLayout>,
    /// Each stashed session's id and look, oldest first, set by the app.
    items: RefCell<Vec<(String, StashLook)>>,
    drag: RefCell<Option<Drag>>,
    hot: Cell<Option<usize>>,
    pressed: Cell<Option<Option<usize>>>,
    tracking: Cell<bool>,
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

impl StashWindow {
    /// Creates the window at `(x, y)` in physical pixels and shows it
    /// without activating it.
    pub fn create(shared: Rc<Shared>, x: i32, y: i32) -> Result<Box<Self>> {
        let layout = RefCell::new(layout::stash(&shared.metrics, 0));
        let mut win = Box::new(StashWindow {
            hwnd: HWND::default(),
            shared,
            target: RefCell::new(None),
            layout,
            items: RefCell::new(Vec::new()),
            drag: RefCell::new(None),
            hot: Cell::new(None),
            pressed: Cell::new(None),
            tracking: Cell::new(false),
        });
        unsafe {
            let hwnd = CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                CLASS,
                w!("Horadric stash"),
                WS_POPUP,
                x,
                y,
                10,
                10,
                None,
                None,
                Some(GetModuleHandleW(None)?.into()),
                Some(&*win as *const StashWindow as *const c_void),
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
        let s = self.scale();
        let (w, h) = self.layout.borrow().size;
        ((w * s).round() as i32, (h * s).round() as i32)
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

    /// What the rows show from now on. A row more or fewer resizes the
    /// window, and the columns are laid out again to make room.
    pub fn set_items(&self, items: Vec<(String, StashLook)>) {
        let rows = items.len();
        *self.items.borrow_mut() = items;
        let next = layout::stash(&self.shared.metrics, rows);
        let resized = self.layout.borrow().size != next.size;
        *self.layout.borrow_mut() = next;
        if resized {
            self.fit();
            app::push(Input::Arrange);
        } else {
            self.invalidate();
        }
    }

    /// Sizes the window to its layout, at the DPI it is on.
    fn fit(&self) {
        let (w, h) = self.size_px();
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
        self.invalidate();
    }

    fn paint(&self) {
        let (w, h) = self.size_px();
        let dpi = self.dpi();
        let mut slot = self.target.borrow_mut();
        if slot.is_none() {
            match Target::new(&self.shared.gpu, self.hwnd, w as u32, h as u32, dpi) {
                Ok(t) => *slot = Some(t),
                Err(e) => {
                    eprintln!("horadric: render target for the stash: {e}");
                    return;
                }
            }
        }
        let items = self.items.borrow();
        let looks: Vec<&StashLook> = items.iter().map(|(_, l)| l).collect();
        let layout = self.layout.borrow();
        let scene = StashScene {
            layout: &layout,
            items: &looks,
            hot: self.hot.get(),
            pressed: self.pressed.get(),
        };
        let result = slot
            .as_ref()
            .map(|t| t.draw_stash(&self.shared.gpu, &self.shared.metrics, &scene));
        if let Some(Err(_)) = result {
            *slot = None;
        }
    }

    /// The filled slot under a point, if any.
    fn hit(&self, lparam: LPARAM) -> Option<usize> {
        let s = self.scale();
        let x = (lparam.0 & 0xffff) as i16 as f32 / s;
        let y = ((lparam.0 >> 16) & 0xffff) as i16 as f32 / s;
        layout::stash_hit(&self.layout.borrow(), x, y).filter(|&i| i < self.items.borrow().len())
    }

    fn id_at(&self, slot: Option<usize>) -> Option<String> {
        self.items.borrow().get(slot?).map(|(id, _)| id.clone())
    }

    fn hover(&self, hot: Option<usize>) {
        let line = hot.and_then(|i| {
            let items = self.items.borrow();
            let (_, l) = items.get(i)?;
            Some(tip::stashed(&l.name, &l.project, l.branch.as_deref(), &l.last).into())
        });
        tip::over_line(&self.shared, self.hwnd, line);
        if self.hot.replace(hot) != hot {
            self.invalidate();
        }
    }

    fn press(&self, pressed: Option<Option<usize>>) {
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
                app::push(Input::Scroll(columns::STASH.into(), notches));
                Some(LRESULT(0))
            }
            WM_LBUTTONDOWN => {
                tip::press(self.hwnd);
                self.raise();
                let mut cursor = POINT::default();
                unsafe {
                    let _ = GetCursorPos(&mut cursor);
                    SetCapture(self.hwnd);
                }
                self.press(Some(self.hit(lparam)));
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
                            columns::STASH.into(),
                            Some((cursor.x, cursor.y)),
                        ));
                    }
                }
                Some(LRESULT(0))
            }
            WM_LBUTTONUP => {
                // Taken first: letting go of the capture sends
                // WM_CAPTURECHANGED at once, which clears the press and
                // calls off a drag it still finds.
                let pressed = self.pressed.get();
                let drag = self.drag.borrow_mut().take();
                unsafe {
                    let _ = ReleaseCapture();
                }
                self.press(None);
                let hit = self.hit(lparam);
                match drag {
                    Some(d) if d.moved => {
                        let mut cursor = POINT::default();
                        unsafe {
                            let _ = GetCursorPos(&mut cursor);
                        }
                        app::push(Input::Drop(columns::STASH.into(), cursor.x, cursor.y));
                    }
                    // Only where the press began, as a button does.
                    Some(_) if pressed == Some(hit) => {
                        if let Some(id) = self.id_at(hit) {
                            app::push(Input::Unstash(id));
                        }
                    }
                    _ => {}
                }
                Some(LRESULT(0))
            }
            WM_RBUTTONUP => {
                tip::press(self.hwnd);
                if let Some(id) = self.id_at(self.hit(lparam)) {
                    app::push(Input::StashMenu(id));
                }
                Some(LRESULT(0))
            }
            WM_MOUSELEAVE => {
                self.tracking.set(false);
                self.hover(None);
                tip::away(self.hwnd);
                Some(LRESULT(0))
            }
            WM_CAPTURECHANGED => {
                self.press(None);
                if self.drag.borrow_mut().take().is_some_and(|d| d.moved) {
                    app::push(Input::Carry(columns::STASH.into(), None));
                }
                None
            }
            _ => None,
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_NCCREATE {
        let cs = &*(lparam.0 as *const CREATESTRUCTW);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, cs.lpCreateParams as isize);
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    if msg == WM_TIMER && wparam.0 == appear::TIMER {
        appear::tick(hwnd);
        return LRESULT(0);
    }
    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const StashWindow;
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
