//! The stage's caption, drawn in place of the Windows title bar.
//!
//! A child window along the top of the stage, painted with Direct2D like
//! the panes. It answers every hit test with `HTTRANSPARENT`, so the stage
//! underneath does the hit testing: `HTCAPTION` to drag, the maximise key
//! as `HTMAXBUTTON` so Windows 11 offers its snap layouts over it, and the
//! other keys as theirs. The stage tells it what to show and what the
//! mouse is over.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::rc::Rc;

use windows::core::{w, Result, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::{InvalidateRect, ValidateRect};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GetWindowLongPtrW, RegisterClassW, SetWindowLongPtrW,
    SetWindowPos, CREATESTRUCTW, GWLP_USERDATA, HTTRANSPARENT, SWP_NOACTIVATE, SWP_NOZORDER,
    WINDOW_EX_STYLE, WM_ERASEBKGND, WM_NCCREATE, WM_NCDESTROY, WM_NCHITTEST, WM_PAINT, WNDCLASSW,
    WS_CHILD, WS_CLIPSIBLINGS, WS_VISIBLE,
};

use crate::layout::{self, CaptionHit, CaptionLayout};
use crate::render::{CaptionScene, Target};
use crate::theme::{self, Color};
use crate::window::Shared;

pub(crate) const CLASS: PCWSTR = w!("HoradricCaption");

pub fn register_class() -> Result<()> {
    unsafe {
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: GetModuleHandleW(None)?.into(),
            lpszClassName: CLASS,
            ..Default::default()
        };
        RegisterClassW(&wc);
        Ok(())
    }
}

pub struct Caption {
    hwnd: Cell<HWND>,
    shared: Rc<Shared>,
    target: RefCell<Option<Target>>,
    /// In pixels: the caption's size, and the height of the whole plate
    /// its light belongs to.
    size: Cell<(i32, i32)>,
    plate_h: Cell<i32>,
    project: RefCell<String>,
    detail: RefCell<String>,
    accent: Cell<Color>,
    active: Cell<bool>,
    maximized: Cell<bool>,
    hot: Cell<Option<CaptionHit>>,
    pressed: Cell<Option<CaptionHit>>,
    /// How far in the plate's seam is cut, in DIPs.
    seam: f32,
}

impl Caption {
    pub fn create(shared: Rc<Shared>, parent: HWND, seam: f32) -> Result<Box<Self>> {
        let caption = Box::new(Caption {
            hwnd: Cell::new(HWND::default()),
            shared,
            target: RefCell::new(None),
            size: Cell::new((0, 0)),
            plate_h: Cell::new(1),
            project: RefCell::new(String::new()),
            detail: RefCell::new(String::new()),
            accent: Cell::new(theme::TEXT_DIM),
            active: Cell::new(true),
            maximized: Cell::new(false),
            hot: Cell::new(None),
            pressed: Cell::new(None),
            seam,
        });
        unsafe {
            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                CLASS,
                w!(""),
                WS_CHILD | WS_VISIBLE | WS_CLIPSIBLINGS,
                0,
                0,
                1,
                1,
                Some(parent),
                None,
                Some(GetModuleHandleW(None)?.into()),
                Some(&*caption as *const Caption as *const c_void),
            )?;
            caption.hwnd.set(hwnd);
        }
        Ok(caption)
    }

    fn scale(&self) -> f32 {
        unsafe { GetDpiForWindow(self.hwnd.get()) }.max(96) as f32 / 96.0
    }

    /// The layout at the caption's width now.
    pub fn layout(&self) -> CaptionLayout {
        layout::caption(self.size.get().0 as f32 / self.scale())
    }

    /// What a point in the stage's client pixels is over, None below the
    /// caption.
    pub fn hit(&self, x: i32, y: i32) -> Option<CaptionHit> {
        let s = self.scale();
        layout::caption_hit(&self.layout(), x as f32 / s, y as f32 / s)
    }

    /// Along the top of a stage `width` pixels wide whose plate is
    /// `plate_h` tall.
    pub fn place(&self, width: i32, height: i32, plate_h: i32) {
        self.plate_h.set(plate_h.max(1));
        let resized = self.size.replace((width, height)) != (width, height);
        unsafe {
            let _ = SetWindowPos(
                self.hwnd.get(),
                None,
                0,
                0,
                width,
                height,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
        if resized {
            if let Some(t) = self.target.borrow().as_ref() {
                if t.resize(width.max(1) as u32, height.max(1) as u32).is_err() {
                    self.target.replace(None);
                }
            }
        }
        self.invalidate();
    }

    pub fn set_text(&self, project: &str, detail: &str) {
        let changed = *self.project.borrow() != project || *self.detail.borrow() != detail;
        if changed {
            self.project.replace(project.to_string());
            self.detail.replace(detail.to_string());
            self.invalidate();
        }
    }

    pub fn set_accent(&self, c: Color) {
        self.accent.set(c);
        self.invalidate();
    }

    pub fn set_active(&self, on: bool) {
        if self.active.replace(on) != on {
            self.invalidate();
        }
    }

    pub fn set_maximized(&self, on: bool) {
        if self.maximized.replace(on) != on {
            self.invalidate();
        }
    }

    pub fn set_hot(&self, hot: Option<CaptionHit>) {
        if self.hot.replace(hot) != hot {
            self.invalidate();
        }
    }

    pub fn set_pressed(&self, pressed: Option<CaptionHit>) {
        if self.pressed.replace(pressed) != pressed {
            self.invalidate();
        }
    }

    pub fn pressed(&self) -> Option<CaptionHit> {
        self.pressed.get()
    }

    /// The target goes with the DPI, so the next paint makes one at the new
    /// scale.
    pub fn set_dpi(&self) {
        self.target.replace(None);
        self.invalidate();
    }

    fn invalidate(&self) {
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd.get()), None, false);
        }
    }

    fn paint(&self) {
        let hwnd = self.hwnd.get();
        let (w, h) = self.size.get();
        if w <= 0 || h <= 0 {
            return;
        }
        let s = self.scale();
        let mut slot = self.target.borrow_mut();
        if slot.is_none() {
            let dpi = (s * 96.0).round() as u32;
            match Target::new(&self.shared.gpu, hwnd, w as u32, h as u32, dpi) {
                Ok(t) => *slot = Some(t),
                Err(e) => {
                    eprintln!("horadric: render target for the caption: {e}");
                    return;
                }
            }
        }
        let share = h as f32 / self.plate_h.get() as f32;
        let layout = self.layout();
        let project = self.project.borrow();
        let detail = self.detail.borrow();
        let scene = CaptionScene {
            layout: &layout,
            size: (w as f32 / s, h as f32 / s),
            top: theme::PLATE_TOP,
            bottom: theme::PLATE_TOP.mix(theme::PLATE_BOTTOM, share.min(1.0)),
            seam: self.seam,
            project: &project,
            detail: &detail,
            accent: self.accent.get(),
            active: self.active.get(),
            maximized: self.maximized.get(),
            hot: self.hot.get(),
            pressed: self.pressed.get(),
        };
        let failed = slot
            .as_ref()
            .map(|t| t.draw_caption(&self.shared.gpu, &scene))
            .is_some_and(|r| r.is_err());
        if failed {
            *slot = None;
        }
    }

    fn handle(&self, msg: u32) -> Option<LRESULT> {
        match msg {
            // The stage underneath does the hit testing.
            WM_NCHITTEST => Some(LRESULT(HTTRANSPARENT as isize)),
            WM_PAINT => {
                self.paint();
                unsafe {
                    let _ = ValidateRect(Some(self.hwnd.get()), None);
                }
                Some(LRESULT(0))
            }
            WM_ERASEBKGND => Some(LRESULT(1)),
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
    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const Caption;
    if ptr.is_null() {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    if msg == WM_NCDESTROY {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    // The stage owns the Box and its window, this one's parent, goes first.
    let caption = &*ptr;
    match caption.handle(msg) {
        Some(r) => r,
        None => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
