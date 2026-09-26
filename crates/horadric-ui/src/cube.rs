//! The Horadric Cube: tiles dropped into it, and a recipe that runs on
//! what it holds. A tile carried from a cluster or a pane from the stage
//! and let go over it goes in; its lid lifts while one is carried over.
//! Like the stash it belongs to no project, so it is a window of its own
//! in the columns. The app owns what it holds and which recipe that makes
//! (`horadric_core::cube`), and hands the window its look.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::rc::Rc;
use std::time::Instant;

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
    IsWindow, LoadCursorW, PostMessageW, RegisterClassW, SetWindowLongPtrW, SetWindowPos,
    ShowWindow, CREATESTRUCTW, CS_HREDRAW, CS_VREDRAW, GWLP_USERDATA, HWND_NOTOPMOST, HWND_TOPMOST,
    IDC_ARROW, MA_NOACTIVATE, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER,
    SW_SHOWNOACTIVATE, WM_APP, WM_CAPTURECHANGED, WM_DPICHANGED, WM_ERASEBKGND, WM_LBUTTONDOWN,
    WM_LBUTTONUP, WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_NCCREATE, WM_NCDESTROY,
    WM_PAINT, WM_SIZE, WM_TIMER, WNDCLASSW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_POPUP,
};

use crate::app::{self, Input};
use crate::layout::{self, CubeHit, CubeLayout};
use crate::motion::{self};
use crate::render::{CubeScene, Flying, StashLook, Target, TransmuteLook};
use crate::theme;
use crate::window::Shared;
use crate::{appear, backdrop, columns};

pub(crate) const CLASS: PCWSTR = w!("HoradricCube");
const DRAG_THRESHOLD: i32 = 4;
/// The windows crate files this under `Win32_UI_Controls`.
const WM_MOUSELEAVE: u32 = 0x02A3;
/// A tile is carried over the cube (wparam 1) or no longer is (0).
const WM_CUBE_OVER: u32 = WM_APP + 22;
/// The frames of a transmute. Only while one plays: at rest the cube
/// paints when what it shows changes and not otherwise.
const TRANSMUTE_TIMER: usize = 1;

/// What the cube shows, set by the app.
#[derive(Default)]
pub struct Contents {
    /// Each session in it, by id and look, in the order they went in.
    pub items: Vec<(String, StashLook)>,
    pub main: bool,
    pub recipe: Option<String>,
    pub hint: String,
}

pub struct CubeWindow {
    pub hwnd: HWND,
    shared: Rc<Shared>,
    target: RefCell<Option<Target>>,
    layout: CubeLayout,
    contents: RefCell<Contents>,
    open: Cell<bool>,
    drag: RefCell<Option<Drag>>,
    hot: Cell<CubeHit>,
    pressed: Cell<Option<CubeHit>>,
    tracking: Cell<bool>,
    transmute: RefCell<Option<Transmute>>,
}

/// A transmute playing: what went in, and what came of it.
struct Transmute {
    began: Instant,
    flying: Vec<Flying>,
    outcome: String,
    /// The secret recipe: a portal opens instead of the burst.
    portal: bool,
}

impl Transmute {
    fn frame(&self) -> motion::Transmuting {
        let elapsed = self.began.elapsed();
        if self.portal {
            motion::portal(elapsed)
        } else {
            motion::transmuting(elapsed)
        }
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

/// Whether the cursor is over the cube, for a tile or a pane carried
/// about. False while there is no cube.
pub fn under_cursor(shared: &Shared) -> bool {
    let Some(hwnd) = shared.cube.get() else {
        return false;
    };
    let mut p = POINT::default();
    let mut r = RECT::default();
    unsafe {
        if !IsWindow(Some(hwnd)).as_bool()
            || GetCursorPos(&mut p).is_err()
            || GetWindowRect(hwnd, &mut r).is_err()
        {
            return false;
        }
    }
    p.x >= r.left && p.x < r.right && p.y >= r.top && p.y < r.bottom
}

/// Lifts the cube's lid while something is carried over it, and lets it
/// down again. Posted, since the carrier holds the mouse and may be inside
/// the app's borrow.
pub fn lid(shared: &Shared, open: bool) {
    if let Some(hwnd) = shared.cube.get() {
        unsafe {
            let _ = PostMessageW(Some(hwnd), WM_CUBE_OVER, WPARAM(open as usize), LPARAM(0));
        }
    }
}

impl CubeWindow {
    /// Creates the window at `(x, y)` in physical pixels and shows it
    /// without activating it.
    pub fn create(shared: Rc<Shared>, x: i32, y: i32) -> Result<Box<Self>> {
        let layout = layout::cube(&shared.metrics);
        let mut win = Box::new(CubeWindow {
            hwnd: HWND::default(),
            shared,
            target: RefCell::new(None),
            layout,
            contents: RefCell::new(Contents::default()),
            open: Cell::new(false),
            drag: RefCell::new(None),
            hot: Cell::new(CubeHit::Nothing),
            pressed: Cell::new(None),
            tracking: Cell::new(false),
            transmute: RefCell::new(None),
        });
        unsafe {
            let hwnd = CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                CLASS,
                w!("Horadric cube"),
                WS_POPUP,
                x,
                y,
                10,
                10,
                None,
                None,
                Some(GetModuleHandleW(None)?.into()),
                Some(&*win as *const CubeWindow as *const c_void),
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
            win.fit();
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        }
        win.shared.cube.set(Some(win.hwnd));
        Ok(win)
    }

    pub fn destroy(&self) {
        self.shared.cube.set(None);
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
        let (w, h) = self.layout.size;
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

    /// What the cube shows from now on.
    pub fn set_contents(&self, contents: Contents) {
        *self.contents.borrow_mut() = contents;
        self.invalidate();
    }

    /// Plays what the cube holds swirling into it and `outcome` coming
    /// out, or with `portal` a portal opening. Called before the app
    /// empties it, so the slots still say what went in. With Windows'
    /// animations off nothing plays.
    pub fn transmute(&self, outcome: &str, portal: bool) {
        let c = self.contents.borrow();
        let mut flying: Vec<Flying> = c
            .items
            .iter()
            .zip(&self.layout.slots)
            .map(|((_, look), r)| Flying {
                from: *r,
                ink: look.ink,
                accent: look.accent,
            })
            .collect();
        if c.main {
            flying.push(self.main_rune());
        }
        drop(c);
        self.play(flying, outcome, portal);
    }

    /// Plays a batch closing: `batch` swirling in from the slots, taken in
    /// turn when there are more than three, with the `main` rune after
    /// them when the winner merged, and `outcome` coming out. What the cube
    /// holds stays in it.
    pub fn transmute_batch(&self, batch: &[StashLook], main: bool, outcome: &str) {
        let mut flying: Vec<Flying> = batch
            .iter()
            .zip(self.layout.slots.iter().cycle())
            .map(|(look, r)| Flying {
                from: *r,
                ink: look.ink,
                accent: look.accent,
            })
            .collect();
        if main {
            flying.push(self.main_rune());
        }
        self.play(flying, outcome, false);
    }

    fn main_rune(&self) -> Flying {
        Flying {
            from: self.layout.main,
            ink: theme::TEXT,
            accent: theme::rarity_color(horadric_core::rarity::Rarity::Unique),
        }
    }

    fn play(&self, flying: Vec<Flying>, outcome: &str, portal: bool) {
        if !backdrop::animations_on() || flying.is_empty() {
            return;
        }
        *self.transmute.borrow_mut() = Some(Transmute {
            began: Instant::now(),
            flying,
            outcome: outcome.to_string(),
            portal,
        });
        crate::vsync::start(self.hwnd, TRANSMUTE_TIMER);
        self.invalidate();
    }

    /// A transmute is playing, so the cube stays though nothing is left
    /// for it to hold.
    pub fn transmuting(&self) -> bool {
        self.transmute.borrow().is_some()
    }

    fn tick(&self) {
        crate::vsync::took(self.hwnd, TRANSMUTE_TIMER);
        let done = self
            .transmute
            .borrow()
            .as_ref()
            .is_none_or(|t| t.frame().done);
        if done {
            crate::vsync::stop(self.hwnd, TRANSMUTE_TIMER);
            if self.transmute.borrow_mut().take().is_some() {
                app::push(Input::CubeSettled);
            }
        }
        self.invalidate();
    }

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
                    eprintln!("horadric: render target for the cube: {e}");
                    return;
                }
            }
        }
        let c = self.contents.borrow();
        let looks: Vec<&StashLook> = c.items.iter().map(|(_, l)| l).collect();
        let playing = self.transmute.borrow();
        let transmute = playing.as_ref().map(|t| TransmuteLook {
            frame: t.frame(),
            flying: &t.flying,
            outcome: &t.outcome,
            portal: t.portal.then(|| t.began.elapsed().as_secs_f32()),
        });
        let scene = CubeScene {
            layout: &self.layout,
            items: &looks,
            main: c.main,
            recipe: c.recipe.as_deref(),
            hint: &c.hint,
            open: self.open.get(),
            transmute,
            hot: self.hot.get(),
            pressed: self.pressed.get(),
        };
        let result = slot
            .as_ref()
            .map(|t| t.draw_cube(&self.shared.gpu, &self.shared.metrics, &scene));
        if let Some(Err(_)) = result {
            *slot = None;
        }
    }

    /// What a point is on, an empty slot counting as nothing and the
    /// button only while there is a recipe to run.
    fn hit(&self, lparam: LPARAM) -> CubeHit {
        let s = self.scale();
        let x = (lparam.0 & 0xffff) as i16 as f32 / s;
        let y = ((lparam.0 >> 16) & 0xffff) as i16 as f32 / s;
        let c = self.contents.borrow();
        match layout::cube_hit(&self.layout, x, y) {
            CubeHit::Slot(i) if i >= c.items.len() => CubeHit::Nothing,
            CubeHit::Transmute if c.recipe.is_none() => CubeHit::Nothing,
            hit => hit,
        }
    }

    fn hover(&self, hot: CubeHit) {
        if self.hot.replace(hot) != hot {
            self.invalidate();
        }
    }

    fn press(&self, pressed: Option<CubeHit>) {
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

    fn click(&self, hit: CubeHit) {
        match hit {
            CubeHit::Slot(i) => {
                if let Some((id, _)) = self.contents.borrow().items.get(i) {
                    app::push(Input::CubeOut(id.clone()));
                }
            }
            CubeHit::Main => app::push(Input::CubeMain),
            CubeHit::Transmute => app::push(Input::Transmute),
            CubeHit::Nothing => {}
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
            WM_TIMER if wparam.0 == TRANSMUTE_TIMER => {
                self.tick();
                Some(LRESULT(0))
            }
            WM_MOUSEACTIVATE => Some(LRESULT(MA_NOACTIVATE as isize)),
            WM_CUBE_OVER => {
                let open = wparam.0 != 0;
                if self.open.replace(open) != open {
                    // Seen over the cluster being carried onto it.
                    if open {
                        self.raise();
                    }
                    self.invalidate();
                }
                Some(LRESULT(0))
            }
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
                app::push(Input::Scroll(columns::CUBE.into(), notches));
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
                            columns::CUBE.into(),
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
                        app::push(Input::Drop(columns::CUBE.into(), cursor.x, cursor.y));
                    }
                    // Only where the press began, as a button does.
                    Some(_) if pressed == Some(hit) => self.click(hit),
                    _ => {}
                }
                Some(LRESULT(0))
            }
            WM_MOUSELEAVE => {
                self.tracking.set(false);
                self.hover(CubeHit::Nothing);
                Some(LRESULT(0))
            }
            WM_CAPTURECHANGED => {
                self.press(None);
                if self.drag.borrow_mut().take().is_some_and(|d| d.moved) {
                    app::push(Input::Carry(columns::CUBE.into(), None));
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
    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const CubeWindow;
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
