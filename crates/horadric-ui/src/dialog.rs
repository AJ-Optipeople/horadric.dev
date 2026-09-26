//! A question the app asks with buttons: quit and keep the sessions going,
//! end them, merge a branch, or an error there is nothing to do about but
//! read.
//!
//! Drawn like the rest of the app, on a plate in the middle of the screen
//! under the mouse, rather than as a Windows message box. Each answer is a
//! key named for what it does, not Yes and No. Enter presses the ringed
//! one, the arrows and Tab move the ring, and Esc or a click anywhere else
//! answers nothing. Like the menus it runs a modal loop until answered, so
//! the caller must not hold anything the message handlers need.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;

use windows::core::{w, Result, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::DirectWrite::IDWriteTextLayout;
use windows::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
    DWM_WINDOW_CORNER_PREFERENCE,
};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, InvalidateRect, MonitorFromPoint, ValidateRect, MONITORINFO,
    MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetCapture, GetKeyState, ReleaseCapture, SetCapture, VIRTUAL_KEY, VK_ESCAPE, VK_LEFT,
    VK_RETURN, VK_RIGHT, VK_SHIFT, VK_SPACE, VK_TAB,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetCursorPos,
    GetForegroundWindow, GetMessageW, GetWindowLongPtrW, IsWindow, LoadCursorW, PostQuitMessage,
    RegisterClassW, SetForegroundWindow, SetWindowLongPtrW, ShowWindow, TranslateMessage,
    CREATESTRUCTW, GWLP_USERDATA, IDC_ARROW, MSG, SW_HIDE, SW_SHOW, WA_INACTIVE, WM_ACTIVATE,
    WM_CAPTURECHANGED, WM_CLOSE, WM_ERASEBKGND, WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MBUTTONDOWN, WM_MOUSEMOVE, WM_NCCREATE, WM_NCDESTROY, WM_PAINT, WM_RBUTTONDOWN, WM_TIMER,
    WNDCLASSW, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

use crate::appear;
use crate::backdrop;
use crate::layout::{self, DialogLayout, Metrics};
use crate::render::{self, DialogScene, Gpu, Target};
use crate::theme::{self, Color};

pub(crate) const CLASS: PCWSTR = w!("HoradricDialog");

/// What kind of question, for the lamp by its title.
#[derive(Clone, Copy)]
pub enum Tone {
    Question,
    /// Something will stop or be lost.
    Warning,
    Error,
}

impl Tone {
    fn colour(self) -> Color {
        match self {
            Tone::Question => theme::WORKING,
            Tone::Warning => theme::WAITING,
            Tone::Error => theme::ERROR,
        }
    }
}

pub struct Dialog<'a> {
    pub tone: Tone,
    pub title: &'a str,
    pub text: &'a str,
    /// Left to right. Each says what it does.
    pub buttons: &'a [&'a str],
    /// The one Enter presses at first.
    pub default: usize,
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

/// Asks, and returns the button pressed. None when dismissed.
pub fn show(gpu: &Gpu, metrics: &Metrics, d: &Dialog) -> Option<usize> {
    let popup = Popup::open(gpu, metrics, d)
        .map_err(|e| eprintln!("horadric: cannot ask \"{}\": {e}", d.title))
        .ok()?;
    let mut msg = MSG::default();
    while popup.outcome.get().is_none() {
        let got = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        if got.0 == 0 {
            // The app is quitting: the main loop has to see it too.
            unsafe { PostQuitMessage(msg.wParam.0 as i32) };
            break;
        }
        if got.0 == -1 {
            break;
        }
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    popup.close(None);
    unsafe {
        let _ = DestroyWindow(popup.hwnd.get());
    }
    popup.outcome.get().flatten()
}

/// Says what went wrong, for a process that has no app to draw with, such
/// as `horadricw`. Falls silent only if even Direct2D cannot start.
pub fn error_alone(title: &str, text: &str) {
    unsafe {
        let _ = windows::Win32::UI::HiDpi::SetProcessDpiAwarenessContext(
            windows::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        );
    }
    let Ok(gpu) = Gpu::new() else {
        eprintln!("horadric: {title}: {text}");
        return;
    };
    let _ = register_class();
    show(
        &gpu,
        &Metrics::default(),
        &Dialog {
            tone: Tone::Error,
            title,
            text,
            buttons: &["Close"],
            default: 0,
        },
    );
}

struct Popup<'a> {
    hwnd: Cell<HWND>,
    gpu: &'a Gpu,
    metrics: &'a Metrics,
    target: RefCell<Option<Target>>,
    layout: DialogLayout,
    tone: Tone,
    title: String,
    text: IDWriteTextLayout,
    buttons: Vec<String>,
    focus: Cell<usize>,
    hot: Cell<Option<usize>>,
    pressed: Cell<Option<usize>>,
    /// The window that had the focus before, which gets it back.
    before: HWND,
    /// Some once closed, holding the button pressed if one was.
    outcome: Cell<Option<Option<usize>>>,
}

impl<'a> Popup<'a> {
    fn open(gpu: &'a Gpu, metrics: &'a Metrics, d: &Dialog) -> Result<Box<Self>> {
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
        let text = render::wrapped(gpu, &gpu.body, d.text, layout::dialog_text_w())?;
        let widths: Vec<f32> = d
            .buttons
            .iter()
            .map(|b| {
                render::wrapped(gpu, &gpu.small_centre, b, 10_000.0)
                    .map(|l| render::text_size(&l).0)
                    .unwrap_or(60.0)
            })
            .collect();
        let layout = layout::dialog(render::text_size(&text).1.ceil(), &widths);
        let size = (
            (layout.size.0 * s).round() as i32,
            (layout.size.1 * s).round() as i32,
        );
        let (x, y) = layout::dialog_place(size, work_area(cursor));

        let popup = Box::new(Popup {
            hwnd: Cell::new(HWND::default()),
            gpu,
            metrics,
            target: RefCell::new(None),
            layout,
            tone: d.tone,
            title: d.title.to_string(),
            text,
            buttons: d.buttons.iter().map(|b| b.to_string()).collect(),
            focus: Cell::new(d.default.min(d.buttons.len().saturating_sub(1))),
            hot: Cell::new(None),
            pressed: Cell::new(None),
            before: unsafe { GetForegroundWindow() },
            outcome: Cell::new(None),
        });
        let title: Vec<u16> = d.title.encode_utf16().chain([0]).collect();
        unsafe {
            // Born where it shows and at its size: a window that starts
            // elsewhere and moves has been seen not to paint.
            let hwnd = CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
                CLASS,
                PCWSTR(title.as_ptr()),
                WS_POPUP,
                x,
                y,
                size.0,
                size.1,
                None,
                None,
                Some(GetModuleHandleW(None)?.into()),
                Some(&*popup as *const Popup as *const c_void),
            )?;
            popup.hwnd.set(hwnd);
            let pref: DWM_WINDOW_CORNER_PREFERENCE = DWMWCP_ROUND;
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_WINDOW_CORNER_PREFERENCE,
                &pref as *const _ as *const c_void,
                std::mem::size_of::<DWM_WINDOW_CORNER_PREFERENCE>() as u32,
            );
            backdrop::border(hwnd, None);
            appear::begin(hwnd, appear::DIALOG, (appear::RISE * s).round() as i32);
            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = SetForegroundWindow(hwnd);
            SetCapture(hwnd);
        }
        Ok(popup)
    }

    fn scale(&self) -> f32 {
        unsafe { windows::Win32::UI::HiDpi::GetDpiForWindow(self.hwnd.get()) }.max(96) as f32 / 96.0
    }

    fn invalidate(&self) {
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd.get()), None, false);
        }
    }

    fn paint(&self) {
        let hwnd = self.hwnd.get();
        let mut slot = self.target.borrow_mut();
        if slot.is_none() {
            let s = self.scale();
            let (w, h) = (
                (self.layout.size.0 * s).round() as u32,
                (self.layout.size.1 * s).round() as u32,
            );
            match Target::new(self.gpu, hwnd, w, h, (s * 96.0).round() as u32) {
                Ok(t) => *slot = Some(t),
                Err(e) => {
                    eprintln!("horadric: render target for a dialog: {e}");
                    return;
                }
            }
        }
        let scene = DialogScene {
            layout: &self.layout,
            title: &self.title,
            text: &self.text,
            tone: self.tone.colour(),
            buttons: &self.buttons,
            focus: self.focus.get(),
            hot: self.hot.get(),
            pressed: self.pressed.get(),
        };
        let failed = slot
            .as_ref()
            .map(|t| t.draw_dialog(self.gpu, self.metrics, &scene))
            .is_some_and(|r| r.is_err());
        if failed {
            *slot = None;
        }
    }

    fn point(&self, lparam: LPARAM) -> (f32, f32) {
        let s = self.scale();
        let x = (lparam.0 & 0xffff) as i16 as f32 / s;
        let y = ((lparam.0 >> 16) & 0xffff) as i16 as f32 / s;
        (x, y)
    }

    fn inside(&self, (x, y): (f32, f32)) -> bool {
        let (w, h) = self.layout.size;
        x >= 0.0 && y >= 0.0 && x < w && y < h
    }

    fn hit(&self, lparam: LPARAM) -> Option<usize> {
        let (x, y) = self.point(lparam);
        layout::dialog_hit(&self.layout, x, y)
    }

    fn move_focus(&self, forward: bool) {
        let n = self.buttons.len();
        let f = self.focus.get();
        self.focus.set(if forward {
            (f + 1) % n
        } else {
            (f + n - 1) % n
        });
        self.invalidate();
    }

    /// Hands the focus back and ends the loop, with the button pressed if
    /// one was.
    fn close(&self, pressed: Option<usize>) {
        if self.outcome.get().is_some() {
            return;
        }
        self.outcome.set(Some(pressed));
        let hwnd = self.hwnd.get();
        unsafe {
            if GetCapture() == hwnd {
                let _ = ReleaseCapture();
            }
            // Before hiding: hiding the active window hands the focus to
            // whatever Windows picks.
            if !self.before.is_invalid() && IsWindow(Some(self.before)).as_bool() {
                let _ = SetForegroundWindow(self.before);
            }
            let _ = ShowWindow(hwnd, SW_HIDE);
        }
    }

    fn handle(&self, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
        match msg {
            WM_PAINT => {
                self.paint();
                unsafe {
                    let _ = ValidateRect(Some(self.hwnd.get()), None);
                }
                Some(LRESULT(0))
            }
            WM_ERASEBKGND => Some(LRESULT(1)),
            WM_MOUSEMOVE => {
                let hot = self.hit(lparam);
                if self.hot.replace(hot) != hot {
                    self.invalidate();
                }
                Some(LRESULT(0))
            }
            WM_LBUTTONDOWN => {
                if self.inside(self.point(lparam)) {
                    let hit = self.hit(lparam);
                    if self.pressed.replace(hit) != hit {
                        self.invalidate();
                    }
                } else {
                    self.close(None);
                }
                Some(LRESULT(0))
            }
            WM_LBUTTONUP => {
                let pressed = self.pressed.take();
                match (pressed, self.hit(lparam)) {
                    (Some(p), Some(h)) if p == h => self.close(Some(h)),
                    _ => self.invalidate(),
                }
                Some(LRESULT(0))
            }
            WM_RBUTTONDOWN | WM_MBUTTONDOWN => {
                if !self.inside(self.point(lparam)) {
                    self.close(None);
                }
                Some(LRESULT(0))
            }
            WM_KEYDOWN => {
                let shift = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
                match VIRTUAL_KEY(wparam.0 as u16) {
                    VK_ESCAPE => self.close(None),
                    VK_RETURN | VK_SPACE => self.close(Some(self.focus.get())),
                    VK_LEFT => self.move_focus(false),
                    VK_RIGHT => self.move_focus(true),
                    VK_TAB => self.move_focus(!shift),
                    _ => {}
                }
                Some(LRESULT(0))
            }
            WM_CLOSE => {
                self.close(None);
                Some(LRESULT(0))
            }
            WM_ACTIVATE => {
                if (wparam.0 & 0xffff) as u32 == WA_INACTIVE {
                    self.close(None);
                }
                None
            }
            WM_CAPTURECHANGED => {
                // Someone else took the mouse: the dialog would no longer
                // hear the click that dismisses it.
                if HWND(lparam.0 as *mut c_void) != self.hwnd.get() {
                    self.close(None);
                }
                None
            }
            _ => None,
        }
    }
}

/// The work area of the screen `at` is on, as left, top, right, bottom.
fn work_area(at: POINT) -> [i32; 4] {
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    unsafe {
        let monitor = MonitorFromPoint(at, MONITOR_DEFAULTTONEAREST);
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
    if msg == WM_TIMER && wparam.0 == appear::TIMER {
        appear::tick(hwnd);
        return LRESULT(0);
    }
    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const Popup;
    if ptr.is_null() {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    if msg == WM_NCDESTROY {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    // The Box lives in `show` until after the window is destroyed.
    let popup = &*ptr;
    match popup.handle(msg, wparam, lparam) {
        Some(r) => r,
        None => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
