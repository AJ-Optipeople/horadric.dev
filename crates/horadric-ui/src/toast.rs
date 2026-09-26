//! The app's notifications: a session needs you, a task finished, an
//! update is ready or failed.
//!
//! Drawn like the rest of the app, on a small plate in the bottom right
//! corner over the tray, rather than as a Windows toast. One shows at a
//! time and a new one takes its place, as the tray's balloons did, since
//! a click acts on whatever was said last. It never takes the focus, fades
//! in and out, stays while the mouse is on it, and holds its tongue during
//! a full screen game or a presentation.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::rc::Rc;

use windows::core::{w, Result, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::DirectWrite::IDWriteTextLayout;
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
use windows::Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT};
use windows::Win32::UI::Shell::{SHQueryUserNotificationState, NIN_BALLOONUSERCLICK};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetWindowLongPtrW, KillTimer, LoadCursorW,
    PostMessageW, RegisterClassW, SetLayeredWindowAttributes, SetTimer, SetWindowLongPtrW,
    ShowWindow, CREATESTRUCTW, GWLP_USERDATA, IDC_HAND, LWA_ALPHA, MA_NOACTIVATE, SW_HIDE,
    SW_SHOWNOACTIVATE, WM_ERASEBKGND, WM_LBUTTONUP, WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_NCCREATE,
    WM_NCDESTROY, WM_PAINT, WM_TIMER, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_POPUP,
};

use crate::backdrop;
use crate::layout::{self, ToastLayout};
use crate::render::{self, Target, ToastScene};
use crate::theme::{self, Color};
use crate::window::Shared;

pub(crate) const CLASS: PCWSTR = w!("HoradricToast");

/// Not in the `windows` crate's WindowsAndMessaging.
const WM_MOUSELEAVE: u32 = 0x02A3;

/// How long a toast stands before it fades, not counting while the mouse
/// is on it.
const LIFE_MS: u32 = 7000;
const FADE_IN_MS: f32 = 140.0;
const FADE_OUT_MS: f32 = 220.0;
const FRAME: usize = 1;
const FRAME_MS: u32 = 15;
/// Between the toast and the edges of the work area, in DIPs.
const MARGIN: f32 = 12.0;

/// What a notification is about, for the lamp by its title.
#[derive(Clone, Copy)]
pub enum Kind {
    /// A session or a task waits on you.
    Waiting,
    /// Something finished or went through.
    Done,
    Info,
    Failed,
}

impl Kind {
    fn colour(self) -> Color {
        match self {
            Kind::Waiting => theme::WAITING,
            Kind::Done => theme::DONE,
            Kind::Info => theme::WORKING,
            Kind::Failed => theme::ERROR,
        }
    }
}

/// Whether Windows asks apps to keep quiet, from what
/// `SHQueryUserNotificationState` says: 3 a full screen game, 4
/// presentation mode. Not 2, a window the size of the screen, which the
/// stage often is: the toasts would never show while it is in front.
pub(crate) fn hold_back(state: i32) -> bool {
    matches!(state, 3 | 4)
}

pub fn register_class() -> Result<()> {
    unsafe {
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: GetModuleHandleW(None)?.into(),
            lpszClassName: CLASS,
            hCursor: LoadCursorW(None, IDC_HAND)?,
            ..Default::default()
        };
        RegisterClassW(&wc);
        Ok(())
    }
}

/// The notifications of one app. A click on one comes back to `owner` as
/// `message` with `NIN_BALLOONUSERCLICK`, the way a click on the tray's
/// balloon did, so the app handles both the same.
pub struct Toasts {
    shared: Rc<Shared>,
    owner: HWND,
    message: u32,
    current: RefCell<Option<Box<Toast>>>,
}

impl Toasts {
    pub fn new(shared: Rc<Shared>, owner: HWND, message: u32) -> Toasts {
        Toasts {
            shared,
            owner,
            message,
            current: RefCell::new(None),
        }
    }

    pub fn show(&self, kind: Kind, title: &str, text: &str) {
        let debug = std::env::var_os("HORADRIC_DEBUG").is_some();
        let state = unsafe { SHQueryUserNotificationState() }.map_or(5, |s| s.0);
        if hold_back(state) {
            if debug {
                eprintln!("horadric: notification \"{title}\" held back, state {state}");
            }
            return;
        }
        if let Some(old) = self.current.borrow_mut().take() {
            old.destroy();
        }
        match Toast::open(Rc::clone(&self.shared), kind, title, text, self) {
            Ok(t) => *self.current.borrow_mut() = Some(t),
            Err(e) => eprintln!("horadric: cannot show notification \"{title}\": {e}"),
        }
        if debug {
            eprintln!("horadric: notification \"{title}\"");
        }
    }
}

impl Drop for Toasts {
    fn drop(&mut self) {
        if let Some(t) = self.current.get_mut().take() {
            t.destroy();
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Fade {
    In,
    Standing,
    Out,
    Gone,
}

struct Toast {
    hwnd: Cell<HWND>,
    shared: Rc<Shared>,
    target: RefCell<Option<Target>>,
    layout: ToastLayout,
    scale: f32,
    title: String,
    text: Option<IDWriteTextLayout>,
    kind: Kind,
    owner: HWND,
    message: u32,
    hover: Cell<bool>,
    close_hot: Cell<bool>,
    fade: Cell<Fade>,
    alpha: Cell<f32>,
    /// Milliseconds left standing.
    left: Cell<f32>,
}

impl Toast {
    fn open(
        shared: Rc<Shared>,
        kind: Kind,
        title: &str,
        text: &str,
        toasts: &Toasts,
    ) -> Result<Box<Self>> {
        let corner = POINT { x: 0, y: 0 };
        let monitor = unsafe { MonitorFromPoint(corner, MONITOR_DEFAULTTOPRIMARY) };
        let (mut dx, mut dy) = (96u32, 96u32);
        unsafe {
            let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
        }
        let s = dx.max(96) as f32 / 96.0;
        let gpu = &shared.gpu;
        let text = (!text.is_empty())
            .then(|| render::wrapped(gpu, &gpu.small, text, layout::toast_text_w()))
            .transpose()?;
        let text_h = text.as_ref().map_or(0.0, |t| render::text_size(t).1.ceil());
        let layout = layout::toast(text_h);
        let size = (
            (layout.size.0 * s).round() as i32,
            (layout.size.1 * s).round() as i32,
        );
        let margin = (MARGIN * s).round() as i32;
        let (x, y) = layout::toast_place(size, work_area(monitor), margin);
        let toast = Box::new(Toast {
            hwnd: Cell::new(HWND::default()),
            shared,
            target: RefCell::new(None),
            layout,
            scale: s,
            title: title.to_string(),
            text,
            kind,
            owner: toasts.owner,
            message: toasts.message,
            hover: Cell::new(false),
            close_hot: Cell::new(false),
            fade: Cell::new(Fade::In),
            alpha: Cell::new(0.0),
            left: Cell::new(LIFE_MS as f32),
        });
        let name: Vec<u16> = title.encode_utf16().chain([0]).collect();
        unsafe {
            // Born where it shows and at its size: a window that starts
            // elsewhere and moves has been seen not to paint.
            let hwnd = CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE | WS_EX_LAYERED,
                CLASS,
                PCWSTR(name.as_ptr()),
                WS_POPUP,
                x,
                y,
                size.0,
                size.1,
                None,
                None,
                Some(GetModuleHandleW(None)?.into()),
                Some(&*toast as *const Toast as *const c_void),
            )?;
            toast.hwnd.set(hwnd);
            let pref: DWM_WINDOW_CORNER_PREFERENCE = DWMWCP_ROUND;
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_WINDOW_CORNER_PREFERENCE,
                &pref as *const _ as *const c_void,
                std::mem::size_of::<DWM_WINDOW_CORNER_PREFERENCE>() as u32,
            );
            backdrop::border(hwnd, None);
            let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 0, LWA_ALPHA);
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            SetTimer(Some(hwnd), FRAME, FRAME_MS, None);
        }
        Ok(toast)
    }

    fn destroy(&self) {
        unsafe {
            let _ = DestroyWindow(self.hwnd.get());
        }
    }

    fn invalidate(&self) {
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd.get()), None, false);
        }
    }

    fn set_alpha(&self, a: f32) {
        self.alpha.set(a.clamp(0.0, 1.0));
        let byte = (self.alpha.get() * 255.0).round() as u8;
        unsafe {
            let _ = SetLayeredWindowAttributes(self.hwnd.get(), COLORREF(0), byte, LWA_ALPHA);
        }
    }

    /// One frame of the fade, and the clock while it stands.
    fn tick(&self) {
        let dt = FRAME_MS as f32;
        match self.fade.get() {
            Fade::In => {
                self.set_alpha(self.alpha.get() + dt / FADE_IN_MS);
                if self.alpha.get() >= 1.0 {
                    self.fade.set(Fade::Standing);
                }
            }
            Fade::Standing => {
                if !self.hover.get() {
                    self.left.set(self.left.get() - dt);
                    if self.left.get() <= 0.0 {
                        self.fade.set(Fade::Out);
                    }
                }
            }
            Fade::Out => {
                // The mouse coming back holds it, as it does while standing.
                if self.hover.get() {
                    self.fade.set(Fade::In);
                    return;
                }
                self.set_alpha(self.alpha.get() - dt / FADE_OUT_MS);
                if self.alpha.get() <= 0.0 {
                    self.gone();
                }
            }
            Fade::Gone => {}
        }
    }

    fn gone(&self) {
        self.fade.set(Fade::Gone);
        let hwnd = self.hwnd.get();
        unsafe {
            let _ = KillTimer(Some(hwnd), FRAME);
            let _ = ShowWindow(hwnd, SW_HIDE);
        }
    }

    fn paint(&self) {
        let hwnd = self.hwnd.get();
        let mut slot = self.target.borrow_mut();
        if slot.is_none() {
            let s = self.scale;
            let (w, h) = (
                (self.layout.size.0 * s).round() as u32,
                (self.layout.size.1 * s).round() as u32,
            );
            match Target::new(&self.shared.gpu, hwnd, w, h, (s * 96.0).round() as u32) {
                Ok(t) => *slot = Some(t),
                Err(e) => {
                    eprintln!("horadric: render target for a notification: {e}");
                    return;
                }
            }
        }
        let scene = ToastScene {
            layout: &self.layout,
            title: &self.title,
            text: self.text.as_ref(),
            tone: self.kind.colour(),
            hover: self.hover.get(),
            close_hot: self.close_hot.get(),
        };
        let failed = slot
            .as_ref()
            .map(|t| t.draw_toast(&self.shared.gpu, &self.shared.metrics, &scene))
            .is_some_and(|r| r.is_err());
        if failed {
            *slot = None;
        }
    }

    fn on_close(&self, lparam: LPARAM) -> bool {
        let x = (lparam.0 & 0xffff) as i16 as f32 / self.scale;
        let y = ((lparam.0 >> 16) & 0xffff) as i16 as f32 / self.scale;
        self.layout.close.contains(x, y)
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
            WM_TIMER if wparam.0 == FRAME => {
                self.tick();
                Some(LRESULT(0))
            }
            WM_MOUSEACTIVATE => Some(LRESULT(MA_NOACTIVATE as isize)),
            WM_MOUSEMOVE => {
                if !self.hover.replace(true) {
                    let mut track = TRACKMOUSEEVENT {
                        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: self.hwnd.get(),
                        dwHoverTime: 0,
                    };
                    unsafe {
                        let _ = TrackMouseEvent(&mut track);
                    }
                    self.invalidate();
                }
                let on = self.on_close(lparam);
                if self.close_hot.replace(on) != on {
                    self.invalidate();
                }
                Some(LRESULT(0))
            }
            WM_MOUSELEAVE => {
                self.hover.set(false);
                self.close_hot.set(false);
                // A little time to read it again after the mouse leaves.
                self.left.set(self.left.get().max(1500.0));
                self.invalidate();
                Some(LRESULT(0))
            }
            WM_LBUTTONUP => {
                if !self.on_close(lparam) {
                    unsafe {
                        let _ = PostMessageW(
                            Some(self.owner),
                            self.message,
                            WPARAM(0),
                            LPARAM(NIN_BALLOONUSERCLICK as isize),
                        );
                    }
                }
                self.hover.set(false);
                self.gone();
                Some(LRESULT(0))
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
    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const Toast;
    if ptr.is_null() {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    if msg == WM_NCDESTROY {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    // The Box lives in `Toasts` until after the window is destroyed.
    let toast = &*ptr;
    match toast.handle(msg, wparam, lparam) {
        Some(r) => r,
        None => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_asking_for_quiet_holds_a_toast_back() {
        for quiet in [3, 4] {
            assert!(hold_back(quiet), "{quiet}");
        }
        for fine in [1, 2, 5, 6, 7] {
            assert!(!hold_back(fine), "{fine}");
        }
    }
}
