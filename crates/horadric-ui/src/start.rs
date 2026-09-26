//! The start window: a ghost cluster where the first project will go,
//! shown only while no project is open. Without it a fresh start is an
//! empty desktop with a tray icon, and nothing says how to begin.
//!
//! Its tile opens the folder picker, a recent project starts a session
//! there, and a folder dropped from Explorer opens as the project. The app
//! does the starting, so each is queued as input. The window is gone the
//! moment the first cluster appears.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use windows::core::{w, Result, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
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
use windows::Win32::UI::Shell::{DragAcceptFiles, DragFinish, HDROP};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetWindowLongPtrW, GetWindowRect, LoadCursorW,
    RegisterClassW, SetWindowLongPtrW, SetWindowPos, ShowWindow, CREATESTRUCTW, CS_HREDRAW,
    CS_VREDRAW, GWLP_USERDATA, HWND_NOTOPMOST, HWND_TOPMOST, IDC_ARROW, MA_NOACTIVATE,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SW_SHOWNOACTIVATE, WM_CAPTURECHANGED,
    WM_DPICHANGED, WM_DROPFILES, WM_ERASEBKGND, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEACTIVATE,
    WM_MOUSEMOVE, WM_NCCREATE, WM_NCDESTROY, WM_PAINT, WM_RBUTTONUP, WM_SIZE, WM_TIMER, WNDCLASSW,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_POPUP,
};

use crate::app::{self, Input};
use crate::appear;
use crate::backdrop;
use crate::clipboard;
use crate::layout::{self, StartHit, StartLayout};
use crate::recent;
use crate::render::{StartScene, Target};
use crate::window::Shared;

pub(crate) const CLASS: PCWSTR = w!("HoradricStart");
/// The windows crate files this under `Win32_UI_Controls`.
const WM_MOUSELEAVE: u32 = 0x02A3;
/// More than this and the window outgrows the cluster it stands in for.
/// The tray menu has the rest.
const MAX_RECENT: usize = 5;

pub struct StartWindow {
    pub hwnd: HWND,
    shared: Rc<Shared>,
    /// The recent projects shown, as full paths.
    recent: RefCell<Vec<String>>,
    target: RefCell<Option<Target>>,
    layout: RefCell<StartLayout>,
    hot: Cell<StartHit>,
    pressed: Cell<Option<StartHit>>,
    tracking: Cell<bool>,
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

impl StartWindow {
    /// Creates the window at `(x, y)` in physical pixels and shows it
    /// without activating it.
    pub fn create(shared: Rc<Shared>, recent: &[String], x: i32, y: i32) -> Result<Box<Self>> {
        let recent = shown(recent);
        let initial = layout::start(&shared.metrics, recent.len());
        let mut win = Box::new(StartWindow {
            hwnd: HWND::default(),
            shared,
            recent: RefCell::new(recent),
            target: RefCell::new(None),
            layout: RefCell::new(initial),
            hot: Cell::new(StartHit::Nothing),
            pressed: Cell::new(None),
            tracking: Cell::new(false),
        });
        unsafe {
            let hwnd = CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                CLASS,
                w!("Horadric"),
                WS_POPUP,
                x,
                y,
                10,
                10,
                None,
                None,
                Some(GetModuleHandleW(None)?.into()),
                Some(&*win as *const StartWindow as *const c_void),
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
            DragAcceptFiles(hwnd, true);
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

    /// Shows these recent projects instead. True when the size changed, so
    /// the stack needs arranging again.
    pub fn set_recent(&self, recent: &[String]) -> bool {
        let recent = shown(recent);
        if *self.recent.borrow() == recent {
            return false;
        }
        *self.recent.borrow_mut() = recent;
        self.fit()
    }

    /// Lays out again and resizes to fit. True when the size changed.
    fn fit(&self) -> bool {
        *self.layout.borrow_mut() = layout::start(&self.shared.metrics, self.recent.borrow().len());
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
        let mut slot = self.target.borrow_mut();
        if slot.is_none() {
            match Target::new(&self.shared.gpu, self.hwnd, w as u32, h as u32, self.dpi()) {
                Ok(t) => *slot = Some(t),
                Err(e) => {
                    eprintln!("horadric: render target for the start window: {e}");
                    return;
                }
            }
        }
        let labels: Vec<(String, String)> = self
            .recent
            .borrow()
            .iter()
            .map(|p| recent::label(p))
            .collect();
        let layout = self.layout.borrow();
        let scene = StartScene {
            layout: &layout,
            recent: &labels,
            hot: self.hot.get(),
            pressed: self.pressed.get(),
        };
        let result = slot
            .as_ref()
            .map(|t| t.draw_start(&self.shared.gpu, &self.shared.metrics, &scene));
        if let Some(Err(_)) = result {
            *slot = None;
        }
    }

    fn hit(&self, lparam: LPARAM) -> StartHit {
        let s = self.scale();
        let x = (lparam.0 & 0xffff) as i16 as f32 / s;
        let y = ((lparam.0 >> 16) & 0xffff) as i16 as f32 / s;
        layout::start_hit(&self.layout.borrow(), x, y)
    }

    fn hover(&self, hot: StartHit) {
        if self.hot.replace(hot) != hot {
            self.invalidate();
        }
    }

    fn press(&self, pressed: Option<StartHit>) {
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

    fn click(&self, hit: StartHit) {
        match hit {
            StartHit::Open => app::push(Input::Pick),
            StartHit::Recent(i) => {
                if let Some(p) = self.recent.borrow().get(i) {
                    app::push(Input::StartIn(PathBuf::from(p)));
                }
            }
            StartHit::Nothing => {}
        }
    }

    /// A dropped folder is the project. A dropped file means the folder it
    /// is in, which is what someone dragging from inside a project meant.
    fn on_drop(&self, hdrop: HDROP) {
        let paths = clipboard::drop_paths(hdrop);
        unsafe { DragFinish(hdrop) };
        let dir = paths.first().map(PathBuf::from).and_then(|p| {
            if p.is_dir() {
                Some(p)
            } else {
                p.parent().map(Path::to_path_buf)
            }
        });
        if let Some(dir) = dir {
            app::push(Input::StartIn(dir));
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
                Some(LRESULT(0))
            }
            WM_LBUTTONDOWN => {
                self.raise();
                unsafe {
                    SetCapture(self.hwnd);
                }
                self.press(Some(self.hit(lparam)));
                Some(LRESULT(0))
            }
            WM_MOUSEMOVE => {
                self.track();
                self.hover(self.hit(lparam));
                Some(LRESULT(0))
            }
            WM_LBUTTONUP => {
                // Before letting go: releasing capture sends
                // WM_CAPTURECHANGED at once, and that clears the press.
                let pressed = self.pressed.get();
                unsafe {
                    let _ = ReleaseCapture();
                }
                self.press(None);
                let hit = self.hit(lparam);
                // Only where the press began, as a button does.
                if pressed == Some(hit) {
                    self.click(hit);
                }
                Some(LRESULT(0))
            }
            WM_RBUTTONUP => {
                if let StartHit::Recent(i) = self.hit(lparam) {
                    if let Some(p) = self.recent.borrow().get(i) {
                        app::push(Input::RecentMenu(PathBuf::from(p)));
                    }
                }
                Some(LRESULT(0))
            }
            WM_MOUSELEAVE => {
                self.tracking.set(false);
                self.hover(StartHit::Nothing);
                Some(LRESULT(0))
            }
            WM_CAPTURECHANGED => {
                self.press(None);
                None
            }
            WM_DROPFILES => {
                self.on_drop(HDROP(wparam.0 as *mut c_void));
                Some(LRESULT(0))
            }
            _ => None,
        }
    }
}

/// The recent projects worth a row: ones still on disk, at most
/// [`MAX_RECENT`].
fn shown(recent: &[String]) -> Vec<String> {
    recent
        .iter()
        .filter(|p| Path::new(p).is_dir())
        .take(MAX_RECENT)
        .cloned()
        .collect()
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
    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const StartWindow;
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
