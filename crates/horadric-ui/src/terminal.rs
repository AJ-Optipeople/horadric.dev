//! The stage: the one terminal window, showing every session of one
//! project at a time.
//!
//! Each session is a [`Pane`] inside it, laid out in a grid: one fills the
//! window, two sit side by side, four are two by two. A click on a tile
//! switches the stage to that tile's project and gives its session the
//! keyboard. With more than one pane, dragging a pane's header onto another
//! swaps the two.
//!
//! Zoomed, the pane with the keyboard fills the stage alone and the rest
//! wait hidden, keeping their size. Moving the keyboard to another pane
//! (Ctrl+Alt+arrow, or a tile) zooms that one instead, so zoom reads as
//! looking at one pane at a time. Switching project ends it.
//!
//! Unlike a cluster, this window takes focus, because you type into it. It
//! is a normal window: resizable, in the taskbar and in alt-tab. Closing it
//! only collapses the sessions back into their tiles; the agents keep
//! running.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use windows::core::{w, Result, BOOL, HSTRING, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_USE_IMMERSIVE_DARK_MODE};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateSolidBrush, DeleteObject, EndPaint, FrameRect, GradientFill, InvalidateRect,
    ScreenToClient, GRADIENT_FILL_RECT_V, GRADIENT_RECT, PAINTSTRUCT, TRIVERTEX,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{GetDpiForWindow, GetSystemMetricsForDpi};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetFocus, ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TME_NONCLIENT,
    TRACKMOUSEEVENT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect, GetCursorPos,
    GetForegroundWindow, GetSystemMetrics, GetWindowLongPtrW, GetWindowRect, IsIconic, IsZoomed,
    LoadCursorW, LoadIconW, RegisterClassW, SendMessageW, SetCursor, SetForegroundWindow,
    SetWindowLongPtrW, SetWindowPos, SetWindowTextW, ShowWindow, CREATESTRUCTW, CW_USEDEFAULT,
    GWLP_USERDATA, HICON, HTCAPTION, HTCLIENT, HTCLOSE, HTMAXBUTTON, HTMINBUTTON, HTTOP, HTTOPLEFT,
    HTTOPRIGHT, ICON_BIG, IDC_ARROW, IDC_SIZEALL, IDC_SIZENS, IDC_SIZEWE, NCCALCSIZE_PARAMS,
    SC_KEYMENU, SIZE_MINIMIZED, SM_CXMINTRACK, SM_CXPADDEDBORDER, SM_CYFRAME, SM_CYMINTRACK,
    SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SW_MAXIMIZE,
    SW_MINIMIZE, SW_RESTORE, SW_SHOWNOACTIVATE, SW_SHOWNORMAL, WINDOW_EX_STYLE, WMSZ_BOTTOM,
    WMSZ_BOTTOMLEFT, WMSZ_BOTTOMRIGHT, WMSZ_LEFT, WMSZ_RIGHT, WMSZ_TOP, WMSZ_TOPLEFT,
    WMSZ_TOPRIGHT, WM_CAPTURECHANGED, WM_CLOSE, WM_DPICHANGED, WM_ENTERSIZEMOVE, WM_ERASEBKGND,
    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_MOVING, WM_NCACTIVATE, WM_NCCALCSIZE,
    WM_NCCREATE, WM_NCDESTROY, WM_NCHITTEST, WM_NCLBUTTONDBLCLK, WM_NCLBUTTONDOWN, WM_NCLBUTTONUP,
    WM_NCMOUSEMOVE, WM_NCRBUTTONUP, WM_PAINT, WM_SETCURSOR, WM_SETFOCUS, WM_SETICON, WM_SIZE,
    WM_SIZING, WM_SYSCOMMAND, WM_TIMER, WNDCLASSW, WS_CLIPCHILDREN, WS_OVERLAPPEDWINDOW,
};

/// Not in the `windows` crate's WindowsAndMessaging.
const WM_NCMOUSELEAVE: u32 = 0x02A2;

use horadric_core::saved::{Dock, Side};

use crate::app::{self, Input};
use crate::backdrop;
use crate::caption::Caption;
use crate::console::Console;
use crate::cube;
use crate::glide::Glides;
use crate::layout::CaptionHit;
use crate::menu::{self, Item};
use crate::pane::{self, Pane, DIRS, WM_PANE_FOCUS, WM_PANE_GRAB, WM_PANE_MOVE, WM_PANE_ZOOM};
use crate::tip;
use crate::window::Shared;
use crate::{layout, snapping, theme, web};

pub(crate) const CLASS: PCWSTR = w!("HoradricTerminal");
/// Between panes, and around them, in DIPs. Each pane has a bezel of its
/// own inside this, so the glass of two panes is this plus two bezels apart.
const PANE_GAP_DIP: f32 = 8.0;
/// The seam cut round the stage's faceplate, in from its edge, in DIPs.
const SEAM_DIP: f32 = 4.0;
/// Runs while a pane glides to its new place in the grid.
const GLIDE_TIMER: usize = 1;
/// Carries a dragged pane on the display's frames.
const DRAG_TIMER: usize = 2;
/// How far a header has to move before a press becomes a drag.
const DRAG_THRESHOLD: i32 = 4;
/// The least a docked browser pane and the grid beside it each keep, in
/// DIPs.
const DOCK_MIN_DIP: f32 = 320.0;

/// Posted to the stage when a pane's place changes from outside it: the
/// browser pane docked on the right or put back in the grid.
pub const WM_STAGE_LAYOUT: u32 = windows::Win32::UI::WindowsAndMessaging::WM_USER + 7;

/// Where the stage opens, in physical pixels, as left, top, right, bottom.
pub enum Place {
    /// The window rectangle it had before.
    Rect([i32; 4]),
    /// These visible edges (see [`TerminalWindow::set_visible_rect`]).
    Visible([i32; 4]),
}

#[derive(Clone, Copy)]
struct Drag {
    /// The pane whose header was pressed.
    serial: usize,
    start: POINT,
    /// Where in the pane it was taken, so it stays under the cursor there.
    grab: (i32, i32),
    moved: bool,
    /// The grid cell the pane is over, whose pane has moved to where the
    /// dragged one came from.
    over: Option<usize>,
    /// The pane's size, which it keeps until it is let go.
    size: (i32, i32),
    /// The cursor, in client pixels, the last time the pane was carried.
    at: Option<(i32, i32)>,
    /// Whether the cube was last told to lift its lid.
    lid: bool,
}

pub struct TerminalWindow {
    pub hwnd: HWND,
    shared: Rc<Shared>,
    /// The project key of the sessions shown, and its name.
    project: RefCell<String>,
    project_name: RefCell<String>,
    /// In grid order. Boxed on purpose: each pane's window procedure holds
    /// a raw pointer to it, so it must not move.
    #[allow(clippy::vec_box)]
    panes: RefCell<Vec<Box<Pane>>>,
    /// Where each pane is in the grid, zoomed or not: what the keyboard
    /// moves across.
    grid: RefCell<Vec<[i32; 4]>>,
    /// Only the pane with the keyboard is shown.
    zoomed: Cell<bool>,
    /// The session that has, or last had, the keyboard.
    active: RefCell<Option<String>>,
    title: RefCell<String>,
    drag: Cell<Option<Drag>>,
    /// The other Horadric windows, read when a move or resize starts: they
    /// cannot move while this one does.
    others: RefCell<Vec<snapping::Edges>>,
    /// How far the cursor is from each edge, from the first message of a
    /// move or resize until it ends.
    grab: Cell<Option<[i32; 4]>>,
    /// The rect Horadric is moving the window to, while it does. A move
    /// onto a screen of another DPI brings Windows' own suggestion, scaled
    /// from the old DPI, and this one is kept instead.
    placing: Cell<Option<[i32; 4]>>,
    /// Drawn in place of the Windows title bar. None only while the window
    /// is being made.
    caption: RefCell<Option<Box<Caption>>>,
    /// The mouse is being followed over the caption, for the leave that
    /// puts its keys out.
    tracking: Cell<bool>,
    /// Panes on their way to a new place in the grid, by window.
    glides: RefCell<Glides>,
    /// When the glides last moved on, while they move.
    glided: Cell<Option<Instant>>,
    /// The gap between the grid and a docked browser pane, while there is
    /// one: the pane's side, and the gap's start and end across it in
    /// client pixels, x for a side and y on top.
    seam: Cell<Option<(Side, (i32, i32))>>,
    /// The seam is being dragged: how far into it the cursor took it.
    seam_drag: Cell<Option<i32>>,
}

pub fn register_class() -> Result<()> {
    pane::register_class()?;
    unsafe {
        let instance = GetModuleHandleW(None)?;
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            lpszClassName: CLASS,
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            // No background brush: the faceplate is painted on WM_PAINT.
            // The icon the build script put in the executable, so a terminal
            // shows as Horadric on the taskbar. Resource 1 is the app icon.
            hIcon: LoadIconW(
                Some(instance.into()),
                PCWSTR(std::ptr::without_provenance(1)),
            )
            .unwrap_or_default(),
            ..Default::default()
        };
        RegisterClassW(&wc);
        Ok(())
    }
}

impl TerminalWindow {
    /// Opens the stage, empty until [`show`](Self::show) fills it, and
    /// brings it to the front.
    pub fn open(shared: Rc<Shared>, place: Place) -> Result<Box<Self>> {
        let mut win = Box::new(TerminalWindow {
            hwnd: HWND::default(),
            shared,
            project: RefCell::new(String::new()),
            project_name: RefCell::new(String::new()),
            panes: RefCell::new(Vec::new()),
            grid: RefCell::new(Vec::new()),
            zoomed: Cell::new(false),
            active: RefCell::new(None),
            title: RefCell::new(String::new()),
            drag: Cell::new(None),
            others: RefCell::new(Vec::new()),
            grab: Cell::new(None),
            placing: Cell::new(None),
            caption: RefCell::new(None),
            tracking: Cell::new(false),
            glides: RefCell::default(),
            seam: Cell::new(None),
            seam_drag: Cell::new(None),
            glided: Cell::new(None),
        });
        unsafe {
            let instance = GetModuleHandleW(None)?;
            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                CLASS,
                w!("Horadric"),
                WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                None,
                None,
                Some(instance.into()),
                Some(&*win as *const TerminalWindow as *const c_void),
            )?;
            win.hwnd = hwnd;

            let dark = BOOL(1);
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_USE_IMMERSIVE_DARK_MODE,
                &dark as *const _ as *const c_void,
                std::mem::size_of::<BOOL>() as u32,
            );
            match Caption::create(Rc::clone(&win.shared), hwnd, SEAM_DIP) {
                Ok(c) => *win.caption.borrow_mut() = Some(c),
                Err(e) => eprintln!("horadric: cannot draw the stage's caption: {e}"),
            }
            // The frame is worked out again without Windows' title bar, now
            // that there is a caption to take its place.
            let _ = SetWindowPos(
                hwnd,
                None,
                0,
                0,
                0,
                0,
                SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
            );

            match place {
                Place::Rect(r) => win.set_rect(r),
                Place::Visible(r) => win.set_visible_rect(r),
            }
            let _ = ShowWindow(hwnd, SW_SHOWNORMAL);
            let _ = SetForegroundWindow(hwnd);
        }
        Ok(win)
    }

    /// Shows these sessions of a project, in this order, each with its
    /// name. A pane already showing a console stays, with its selection and
    /// scroll position; the rest are made or destroyed to match.
    pub fn show(&self, key: &str, name: &str, sessions: Vec<(Arc<Console>, String)>) {
        // Taken out while panes are made and destroyed: both send messages
        // to this window, whose handlers read the list.
        let mut old = std::mem::take(&mut *self.panes.borrow_mut());
        let before: Vec<usize> = old.iter().map(|p| p.serial()).collect();
        let mut new = Vec::with_capacity(sessions.len());
        for (console, label) in sessions {
            match old.iter().position(|p| p.serial() == console.serial) {
                Some(i) => {
                    let p = old.remove(i);
                    p.set_name(label);
                    new.push(p);
                }
                None => match Pane::create(Rc::clone(&self.shared), console, label, self.hwnd) {
                    Ok(p) => {
                        // A session opening on a stage already showing
                        // its project fades in where it lands.
                        if !before.is_empty() {
                            p.reveal();
                        }
                        new.push(p);
                    }
                    Err(e) => eprintln!("horadric: cannot open a pane: {e}"),
                },
            }
        }
        let changed = new.iter().map(|p| p.serial()).ne(before);
        *self.panes.borrow_mut() = new;
        // The cells a drag was aiming at are not these any more.
        if changed && self.drag.take().is_some() {
            self.put_down();
            unsafe {
                let _ = ReleaseCapture();
            }
        }
        for p in old {
            self.glides.borrow_mut().forget(p.hwnd.0 as isize);
            p.destroy();
        }
        let switched = *self.project.borrow() != key;
        // A new project's panes are a new set, laid out below.
        if switched {
            self.zoomed.set(false);
        }
        *self.project.borrow_mut() = key.to_string();
        *self.project_name.borrow_mut() = name.to_string();
        let accent = theme::accent(key);
        // The window's edge in the project's colour, like its cluster's
        // mark, sunk most of the way into the clay so it tints the edge
        // rather than outlining the window.
        backdrop::border(self.hwnd, Some(theme::window_bg().mix(accent, 0.35)));
        if let Some(c) = self.caption.borrow().as_ref() {
            c.set_accent(accent);
        }
        for p in self.panes.borrow().iter() {
            p.set_accent(accent);
            if switched {
                p.reveal();
            }
        }
        let keep = self
            .active
            .borrow()
            .as_ref()
            .is_some_and(|a| self.panes.borrow().iter().any(|p| p.session() == a));
        if !keep {
            *self.active.borrow_mut() = self.panes.borrow().first().map(|p| p.session().into());
        }
        // A new set glides nowhere: its panes all start out fresh.
        if changed {
            self.layout_with(!switched);
        }
        // A pane that had the keyboard may be gone.
        if self.is_foreground() && !self.pane_has_focus() {
            self.focus_active();
        }
        self.spotlight();
        self.refresh_title();
    }

    /// Lights the pane with the keyboard and steps the others back, and
    /// tells the tiles, so its key latches down.
    fn spotlight(&self) {
        let active = self.active.borrow().clone();
        let panes = self.panes.borrow();
        let many = panes.len() > 1;
        for p in panes.iter() {
            p.set_dimmed(many && Some(p.session()) != active.as_deref());
        }
        if self.shared.active.replace(active.clone()) != active {
            app::push(Input::Spotlight);
        }
    }

    /// Puts each pane in its place in the grid, or, zoomed, the one with
    /// the keyboard over all of it, at once.
    fn layout(&self) {
        self.layout_with(false);
    }

    /// Lays the panes out, gliding each on screen to its new place when
    /// `glide`. A pane takes its new size at once, since a terminal that
    /// changes size every frame redraws its agent's screen every frame.
    /// While a pane is dragged it is left to the mouse, and the pane whose
    /// cell it is over takes the cell it came from.
    fn layout_with(&self, glide: bool) {
        let glide = glide && crate::backdrop::animations_on();
        let mut r = RECT::default();
        unsafe {
            let _ = GetClientRect(self.hwnd, &mut r);
        }
        let panes = self.panes.borrow();
        let gap = (PANE_GAP_DIP * self.dpi() as f32 / 96.0).round() as i32;
        let top = self.caption_h();
        if let Some(c) = self.caption.borrow().as_ref() {
            c.place(r.right, top, r.bottom);
            c.set_maximized(unsafe { IsZoomed(self.hwnd) }.as_bool());
        }
        let area = (gap, top, r.right - gap, r.bottom - gap);
        let dock = self.dock(&panes);
        let docked = dock.and_then(|d| {
            let px = |dip: f32| (dip * self.dpi() as f32 / 96.0).round() as i32;
            let size = d.size.map(px);
            layout::docked_grid(panes.len(), area, gap, d.side, size, px(DOCK_MIN_DIP))
        });
        self.seam.set(
            dock.zip(docked.as_ref().and_then(|g| g.last()))
                .map(|(d, c)| (d.side, layout::seam(d.side, *c, gap))),
        );
        let grid = docked.unwrap_or_else(|| layout::grid(panes.len(), area, gap));
        let many = panes.len() > 1;
        let zoomed = many && self.zoomed.get();
        let active = self.active.borrow().clone();
        let drag = self.drag.get().filter(|d| d.moved && !zoomed);
        let dragged = drag.and_then(|d| panes.iter().position(|p| p.serial() == d.serial));
        let swap = dragged.zip(drag.and_then(|d| d.over));
        for (i, p) in panes.iter().enumerate() {
            if Some(i) == dragged {
                continue;
            }
            let cell = &grid[layout::swapped_cell(i, swap)];
            let shown = !zoomed || Some(p.session()) == active.as_deref();
            p.set_zoom(many.then_some(zoomed));
            if shown {
                let rect = if zoomed {
                    [area.0, area.1, area.2, area.3]
                } else {
                    *cell
                };
                p.set_size(rect[2] - rect[0], rect[3] - rect[1]);
                let at = self.child_at(p.hwnd);
                let to = (rect[0], rect[1]);
                let jump = self
                    .glides
                    .borrow_mut()
                    .aim(p.hwnd.0 as isize, at, to, glide);
                if let Some((x, y)) = jump {
                    p.move_to(x, y);
                }
            }
            p.set_visible(shown);
        }
        *self.grid.borrow_mut() = grid;
        if self.glides.borrow().moving() && self.glided.get().is_none() {
            self.glided.set(Some(Instant::now()));
            crate::vsync::start(self.hwnd, GLIDE_TIMER);
        }
    }

    /// Where the browser pane stands beside the grid, when it does. It is
    /// always the last pane.
    fn dock(&self, panes: &[Box<Pane>]) -> Option<Dock> {
        let key = panes.last()?.console().web.as_deref()?;
        web::dock(key)
    }

    /// Where a client point is across the seam: x beside a side dock, y
    /// below one on top.
    fn across(side: Side, p: POINT) -> i32 {
        if side == Side::Top {
            p.y
        } else {
            p.x
        }
    }

    /// Whether a client point is on the seam beside a docked browser pane.
    fn on_seam(&self, p: POINT) -> bool {
        self.seam.get().is_some_and(|(side, (a, b))| {
            let at = Self::across(side, p);
            at >= a && at < b
        })
    }

    /// The client area the panes are laid out in, as `layout_with` has it.
    fn pane_area(&self) -> ((i32, i32, i32, i32), i32) {
        let mut r = RECT::default();
        unsafe {
            let _ = GetClientRect(self.hwnd, &mut r);
        }
        let gap = (PANE_GAP_DIP * self.dpi() as f32 / 96.0).round() as i32;
        ((gap, self.caption_h(), r.right - gap, r.bottom - gap), gap)
    }

    /// Follows the seam with the cursor: the docked pane grows or shrinks
    /// to meet it.
    fn drag_seam(&self, grab: i32) {
        let Some((side, _)) = self.seam.get() else {
            return;
        };
        let (area, gap) = self.pane_area();
        let at = Self::across(side, self.cursor()) - grab;
        let px = layout::seam_size(side, area, gap, at);
        let size = (px as f32 * 96.0 / self.dpi() as f32).max(DOCK_MIN_DIP);
        let key = self
            .panes
            .borrow()
            .last()
            .and_then(|p| p.console().web.clone());
        if let Some(key) = key {
            web::set_dock(
                &key,
                Some(Dock {
                    side,
                    size: Some(size),
                }),
            );
            self.layout();
        }
    }

    /// Where a pane's top left corner is in the stage's client area.
    fn child_at(&self, child: HWND) -> (i32, i32) {
        let mut r = RECT::default();
        unsafe {
            let _ = GetWindowRect(child, &mut r);
        }
        let mut p = POINT {
            x: r.left,
            y: r.top,
        };
        unsafe {
            let _ = ScreenToClient(self.hwnd, &mut p);
        }
        (p.x, p.y)
    }

    /// Moves every gliding pane on a frame. Once they are all in place
    /// the timer stops and each paints again, for the faceplate's light
    /// to run across it from where it now sits.
    fn glide(&self) {
        let now = Instant::now();
        let dt = self.glided.get().map_or(Duration::ZERO, |t| now - t);
        self.glided.set(Some(now));
        let moves = self
            .glides
            .borrow_mut()
            .step(dt, |id| self.child_at(HWND(id as *mut c_void)));
        let panes = self.panes.borrow();
        for (id, (x, y)) in moves {
            if let Some(p) = panes.iter().find(|p| p.hwnd.0 as isize == id) {
                p.move_to(x, y);
            }
        }
        if !self.glides.borrow().moving() {
            self.glided.set(None);
            crate::vsync::stop(self.hwnd, GLIDE_TIMER);
            for p in panes.iter() {
                p.invalidate();
            }
        }
    }

    /// Zooms the pane with this serial in, giving it the keyboard, or the
    /// grid back.
    fn toggle_zoom(&self, serial: usize) {
        let id = self
            .panes
            .borrow()
            .iter()
            .find(|p| p.serial() == serial)
            .map(|p| p.session().to_string());
        let Some(id) = id else { return };
        *self.active.borrow_mut() = Some(id);
        self.zoomed.set(!self.zoomed.get());
        self.layout_with(true);
        self.focus_active();
        self.spotlight();
        self.refresh_title();
    }

    /// Moves the keyboard from the pane with this serial to the one beside
    /// it in the grid. Zoomed, that one is shown instead.
    fn move_focus(&self, serial: usize, dir: layout::Dir) {
        let next = {
            let panes = self.panes.borrow();
            let Some(from) = panes.iter().position(|p| p.serial() == serial) else {
                return;
            };
            layout::neighbour(&self.grid.borrow(), from, dir)
                .and_then(|i| panes.get(i))
                .map(|p| p.session().to_string())
        };
        if let Some(id) = next {
            self.set_active(id);
            self.focus_active();
        }
    }

    /// Another session has the keyboard now. Zoomed, the stage shows it.
    fn set_active(&self, id: String) {
        let changed = self.active.borrow().as_deref() != Some(id.as_str());
        *self.active.borrow_mut() = Some(id);
        if changed && self.zoomed.get() {
            self.layout();
        }
        self.spotlight();
        self.refresh_title();
    }

    /// The theme changed: the window's edge takes the new plate.
    pub fn retheme(&self) {
        let accent = theme::accent(&self.project.borrow());
        backdrop::border(self.hwnd, Some(theme::window_bg().mix(accent, 0.35)));
    }

    /// The font changed size: every pane fits its grid again.
    pub fn refont(&self) {
        for p in self.panes.borrow().iter() {
            p.refont();
        }
    }

    fn pane_has_focus(&self) -> bool {
        let focus = unsafe { GetFocus() };
        self.panes.borrow().iter().any(|p| p.hwnd == focus)
    }

    /// Gives the keyboard to the active session's pane.
    fn focus_active(&self) {
        let active = self.active.borrow().clone();
        let panes = self.panes.borrow();
        let pane = panes
            .iter()
            .find(|p| Some(p.session()) == active.as_deref())
            .or(panes.first());
        if let Some(p) = pane {
            p.focus();
        }
    }

    /// Brings the stage forward with the keyboard in this session's pane.
    pub fn focus_session(&self, id: &str) {
        self.set_active(id.to_string());
        self.bring_to_front();
        self.focus_active();
    }

    /// Shows this session's pane as the active one without taking the
    /// foreground: the keyboard goes into it only while the stage is in
    /// front already.
    pub fn point_at(&self, id: &str) {
        self.set_active(id.to_string());
        if self.is_foreground() {
            self.focus_active();
        }
    }

    /// Which pane is active and whether it is zoomed, to be given back
    /// with [`TerminalWindow::set_view`].
    pub fn view(&self) -> (Option<String>, bool) {
        (self.active(), self.zoomed.get())
    }

    /// Makes `active` the active pane, zoomed or not, as [`TerminalWindow::view`]
    /// said. Like [`TerminalWindow::point_at`], it never takes the foreground.
    pub fn set_view(&self, active: Option<String>, zoomed: bool) {
        if let Some(id) = active.filter(|a| self.panes.borrow().iter().any(|p| p.session() == a)) {
            self.point_at(&id);
        }
        if self.zoomed.get() != zoomed {
            self.zoomed.set(zoomed);
            self.layout_with(true);
            self.spotlight();
        }
    }

    /// The project key of the sessions shown.
    pub fn project(&self) -> String {
        self.project.borrow().clone()
    }

    /// The sessions shown, in grid order.
    pub fn sessions(&self) -> Vec<String> {
        self.panes
            .borrow()
            .iter()
            .map(|p| p.session().to_string())
            .collect()
    }

    /// The session whose pane is at this point on the screen, when the
    /// stage is what shows there.
    pub fn session_at(&self, at: POINT) -> Option<String> {
        self.panes
            .borrow()
            .iter()
            .find(|p| crate::app::window_under(at, p.hwnd))
            .map(|p| p.session().to_string())
    }

    /// The session that has the keyboard, or last had it.
    pub fn active(&self) -> Option<String> {
        self.active.borrow().clone()
    }

    pub fn shows(&self, serial: usize) -> bool {
        self.panes.borrow().iter().any(|p| p.serial() == serial)
    }

    /// A console has news: its pane redraws, and the title follows the
    /// active one.
    pub fn refresh(&self, serial: usize) {
        if let Some(p) = self.panes.borrow().iter().find(|p| p.serial() == serial) {
            p.invalidate();
        }
        self.refresh_title();
    }

    /// Redraws every pane, for a change the headers show, such as a phase.
    pub fn invalidate(&self) {
        for p in self.panes.borrow().iter() {
            p.invalidate();
        }
    }

    /// "what the agent says it is doing · session name, project", for the
    /// session with the keyboard.
    pub fn refresh_title(&self) {
        let project = self.project_name.borrow().clone();
        let active = self.active.borrow().clone();
        // The caption shows the project, then the session and what its
        // agent says it is doing.
        let (title, detail) = {
            let panes = self.panes.borrow();
            match panes
                .iter()
                .find(|p| Some(p.session()) == active.as_deref())
            {
                Some(p) => {
                    let name = self
                        .shared
                        .registry
                        .lock()
                        .ok()
                        .and_then(|r| r.get(p.session()).map(|s| s.label().to_string()))
                        .unwrap_or_else(|| p.name());
                    let label = format!("{name}, {project}");
                    let doing = p
                        .console()
                        .title()
                        .map(|t| t.trim().to_string())
                        .filter(|t| !t.is_empty());
                    let (title, detail) = match doing {
                        Some(t) => (
                            format!("{t} \u{00B7} {label}"),
                            format!("{name} \u{00B7} {t}"),
                        ),
                        None => (label, name),
                    };
                    match p.console().exit_code() {
                        Some(code) => (
                            format!("{title} (exited {code})"),
                            format!("{detail} (exited {code})"),
                        ),
                        None => (title, detail),
                    }
                }
                None => (project.clone(), String::new()),
            }
        };
        if let Some(c) = self.caption.borrow().as_ref() {
            c.set_text(&project, &detail);
        }
        if *self.title.borrow() != title {
            unsafe {
                let _ = SetWindowTextW(self.hwnd, &HSTRING::from(title.as_str()));
            }
            *self.title.borrow_mut() = title;
        }
    }

    /// Moves and sizes the window without taking the focus, restoring it
    /// first when minimised or maximised.
    pub fn set_rect(&self, [l, t, r, b]: [i32; 4]) {
        unsafe {
            if IsIconic(self.hwnd).as_bool() || IsZoomed(self.hwnd).as_bool() {
                let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
            }
            self.placing.set(Some([l, t, r, b]));
            let _ = SetWindowPos(
                self.hwnd,
                None,
                l,
                t,
                r - l,
                b - t,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
            self.placing.set(None);
        }
    }

    /// Like [`set_rect`](Self::set_rect), but for the edges you can see.
    /// A Windows 11 window has an invisible resize border outside its
    /// visible frame, which would otherwise double every gap between
    /// windows laid out side by side.
    pub fn set_visible_rect(&self, [l, t, r, b]: [i32; 4]) {
        self.set_rect([l, t, r, b]);
        let [bl, bt, br, bb] = snapping::border(self.hwnd);
        if [bl, bt, br, bb] != [0; 4] {
            self.set_rect([l - bl, t - bt, r + br, b + bb]);
        }
    }

    /// Where the window is now, unless minimised, when it has no useful
    /// position.
    pub fn rect(&self) -> Option<[i32; 4]> {
        unsafe {
            if IsIconic(self.hwnd).as_bool() {
                return None;
            }
            let mut r = RECT::default();
            GetWindowRect(self.hwnd, &mut r).ok()?;
            Some([r.left, r.top, r.right, r.bottom])
        }
    }

    /// Puts `icon` on the taskbar button, which is how the stage breathes
    /// with the tray while a session works.
    pub fn set_icon(&self, icon: HICON) {
        unsafe {
            SendMessageW(
                self.hwnd,
                WM_SETICON,
                Some(WPARAM(ICON_BIG as usize)),
                Some(LPARAM(icon.0 as isize)),
            );
        }
    }

    /// Destroys the window and its panes. The consoles live on.
    pub fn destroy(&self) {
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }

    pub fn bring_to_front(&self) {
        unsafe {
            if IsIconic(self.hwnd).as_bool() {
                let _ = ShowWindow(self.hwnd, SW_RESTORE);
            }
            let _ = SetForegroundWindow(self.hwnd);
        }
    }

    pub fn is_foreground(&self) -> bool {
        unsafe { GetForegroundWindow() == self.hwnd }
    }

    /// The faceplate behind the panes, the same as a cluster's: lighter at
    /// the top where the light falls, a seam cut round it. The panes paint
    /// their own bezels with the same light, placed by where they sit.
    fn paint_plate(&self) {
        let channel = |v: f32| (v.clamp(0.0, 1.0) * 65535.0).round() as u16;
        let vertex = |x: i32, y: i32, c: theme::Color| TRIVERTEX {
            x,
            y,
            Red: channel(c.r),
            Green: channel(c.g),
            Blue: channel(c.b),
            Alpha: 0,
        };
        let colorref = |c: theme::Color| {
            let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
            COLORREF(byte(c.r) | byte(c.g) << 8 | byte(c.b) << 16)
        };
        unsafe {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(self.hwnd, &mut ps);
            let mut r = RECT::default();
            let _ = GetClientRect(self.hwnd, &mut r);
            let verts = [
                vertex(0, 0, theme::plate_top()),
                vertex(r.right, r.bottom, theme::plate_bottom()),
            ];
            let mesh = GRADIENT_RECT {
                UpperLeft: 0,
                LowerRight: 1,
            };
            let _ = GradientFill(
                hdc,
                &verts,
                &mesh as *const GRADIENT_RECT as *const c_void,
                1,
                GRADIENT_FILL_RECT_V,
            );
            // GDI has no alpha, so the groove's colours are the plate's own
            // mixed toward black and white, as the clusters' blend comes out.
            let inset = (SEAM_DIP * self.dpi() as f32 / 96.0).round() as i32;
            let seam = RECT {
                left: inset,
                top: inset,
                right: r.right - inset,
                bottom: r.bottom - inset,
            };
            let lit = RECT {
                top: seam.top + 1,
                bottom: seam.bottom + 1,
                ..seam
            };
            let black = theme::Color::rgb(0);
            let white = theme::Color::rgb(0xFFFFFF);
            let light = CreateSolidBrush(colorref(theme::window_bg().mix(white, 0.05)));
            let dark = CreateSolidBrush(colorref(theme::window_bg().mix(black, 0.5)));
            FrameRect(hdc, &lit, light);
            FrameRect(hdc, &seam, dark);
            let _ = DeleteObject(light.into());
            let _ = DeleteObject(dark.into());
            let _ = EndPaint(self.hwnd, &ps);
        }
    }

    pub fn dpi(&self) -> u32 {
        unsafe { GetDpiForWindow(self.hwnd) }.max(96)
    }

    /// How tall the caption is, in pixels.
    fn caption_h(&self) -> i32 {
        (layout::CAPTION_H * self.dpi() as f32 / 96.0).round() as i32
    }

    /// How thick the sizing frame is, in pixels: the band along the top
    /// that resizes, and how far a maximised window hangs past the screen.
    fn frame_y(&self) -> i32 {
        let dpi = self.dpi();
        unsafe {
            GetSystemMetricsForDpi(SM_CYFRAME, dpi) + GetSystemMetricsForDpi(SM_CXPADDEDBORDER, dpi)
        }
    }

    /// What a hit test in the client area lands on, with the caption in
    /// place of Windows' title bar. `p` is in client pixels.
    fn hit_test(&self, p: POINT) -> u32 {
        let band = if unsafe { IsZoomed(self.hwnd) }.as_bool() {
            0
        } else {
            self.frame_y()
        };
        if p.y < band {
            let mut r = RECT::default();
            unsafe {
                let _ = GetClientRect(self.hwnd, &mut r);
            }
            return if p.x < band * 2 {
                HTTOPLEFT
            } else if p.x >= r.right - band * 2 {
                HTTOPRIGHT
            } else {
                HTTOP
            };
        }
        let hit = self.caption.borrow().as_ref().and_then(|c| c.hit(p.x, p.y));
        match hit {
            Some(CaptionHit::Min) => HTMINBUTTON,
            Some(CaptionHit::Max) => HTMAXBUTTON,
            Some(CaptionHit::Close) => HTCLOSE,
            Some(CaptionHit::Bar) => HTCAPTION,
            None => HTCLIENT,
        }
    }

    fn with_caption(&self, f: impl FnOnce(&Caption)) {
        if let Some(c) = self.caption.borrow().as_ref() {
            f(c);
        }
    }

    /// The caption's key under a hit test's answer.
    fn key_of(hit: u32) -> Option<CaptionHit> {
        match hit {
            HTMINBUTTON => Some(CaptionHit::Min),
            HTMAXBUTTON => Some(CaptionHit::Max),
            HTCLOSE => Some(CaptionHit::Close),
            _ => None,
        }
    }

    fn press_key(&self, key: CaptionHit) {
        unsafe {
            match key {
                CaptionHit::Min => {
                    let _ = ShowWindow(self.hwnd, SW_MINIMIZE);
                }
                CaptionHit::Max if IsZoomed(self.hwnd).as_bool() => {
                    let _ = ShowWindow(self.hwnd, SW_RESTORE);
                }
                CaptionHit::Max => {
                    let _ = ShowWindow(self.hwnd, SW_MAXIMIZE);
                }
                CaptionHit::Close => app::push(Input::Close(self.hwnd.0 as isize)),
                CaptionHit::Bar => {}
            }
        }
    }

    /// The window's own menu, drawn like the rest: what a right click on
    /// the caption or Alt+Space opens.
    fn window_menu(&self) {
        const RESTORE: usize = 1;
        const MIN: usize = 2;
        const MAX: usize = 3;
        const CLOSE: usize = 4;
        let zoomed = unsafe { IsZoomed(self.hwnd) }.as_bool();
        let items = vec![
            if zoomed {
                Item::action(RESTORE, "Restore")
            } else {
                Item::Disabled("Restore".into())
            },
            Item::action(MIN, "Minimise"),
            if zoomed {
                Item::Disabled("Maximise".into())
            } else {
                Item::action(MAX, "Maximise")
            },
            Item::Separator,
            Item::action(CLOSE, "Close\tAlt+F4"),
        ];
        match menu::popup(&items) {
            Some(RESTORE) => unsafe {
                let _ = ShowWindow(self.hwnd, SW_RESTORE);
            },
            Some(MIN) => self.press_key(CaptionHit::Min),
            Some(MAX) => self.press_key(CaptionHit::Max),
            Some(CLOSE) => self.press_key(CaptionHit::Close),
            _ => {}
        }
    }

    /// The cursor in client pixels.
    fn cursor(&self) -> POINT {
        let mut p = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut p);
            let _ = ScreenToClient(self.hwnd, &mut p);
        }
        p
    }

    /// A header was pressed. The stage takes the mouse until it is let go.
    fn start_drag(&self, serial: usize) {
        let mut start = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut start);
        }
        let at = self
            .panes
            .borrow()
            .iter()
            .find(|p| p.serial() == serial)
            .map_or((0, 0), |p| self.child_at(p.hwnd));
        let c = self.cursor();
        self.drag.set(Some(Drag {
            serial,
            start,
            grab: (c.x - at.0, c.y - at.1),
            moved: false,
            over: None,
            size: (0, 0),
            at: None,
            lid: false,
        }));
        unsafe {
            SetCapture(self.hwnd);
        }
    }

    /// Lifts the dragged pane once the cursor has moved far enough. From
    /// then on the pane is carried on the display's frames, not on every
    /// mouse move: a fast mouse reports a thousand moves a second, and
    /// each move of the pane repaints what it uncovers.
    fn drag_to(&self) {
        let Some(mut d) = self.drag.get() else {
            return;
        };
        if !d.moved {
            let mut p = POINT::default();
            unsafe {
                let _ = GetCursorPos(&mut p);
            }
            if (p.x - d.start.x).abs() < DRAG_THRESHOLD && (p.y - d.start.y).abs() < DRAG_THRESHOLD
            {
                return;
            }
            d.moved = true;
            d.size = self.lift(d.serial);
            self.drag.set(Some(d));
        }
        unsafe {
            SetCursor(LoadCursorW(None, IDC_SIZEALL).ok());
        }
        crate::vsync::start(self.hwnd, DRAG_TIMER);
    }

    /// Carries the dragged pane to the cursor as it is at this frame. The
    /// pane whose cell it comes over glides into the cell it left, so the
    /// grid shows the swap before it is let go. A frame with the cursor
    /// where it was stops the frames until the mouse moves again.
    fn carry(&self) {
        let Some(mut d) = self.drag.get().filter(|d| d.moved) else {
            crate::vsync::stop(self.hwnd, DRAG_TIMER);
            return;
        };
        let c = self.cursor();
        if d.at == Some((c.x, c.y)) {
            crate::vsync::stop(self.hwnd, DRAG_TIMER);
            return;
        }
        d.at = Some((c.x, c.y));
        let cube = cube::under_cursor(&self.shared);
        if d.lid != cube {
            d.lid = cube;
            cube::lid(&self.shared, cube);
        }
        self.drag.set(Some(d));
        // Zoomed, the pane fills the stage and has nowhere to go but the
        // cube.
        if self.zoomed.get() {
            return;
        }
        let mut client = RECT::default();
        unsafe {
            let _ = GetClientRect(self.hwnd, &mut client);
        }
        if let Some(p) = self.panes.borrow().iter().find(|p| p.serial() == d.serial) {
            // Kept on the stage: a pane partly off it would repaint the
            // strip it brings back into view on every frame.
            let gap = (PANE_GAP_DIP * self.dpi() as f32 / 96.0).round() as i32;
            let area = [
                gap,
                self.caption_h(),
                client.right - gap,
                client.bottom - gap,
            ];
            let (x, y) = layout::clamp_into((c.x - d.grab.0, c.y - d.grab.1), d.size, area);
            p.move_to(x, y);
        }
        let inside = c.x >= 0 && c.y >= 0 && c.x < client.right && c.y < client.bottom;
        // In a gap between cells the swap shown stays, so crossing one does
        // not send a pane home and back. Out of the stage, or over the
        // cube, everything goes home.
        let hit = layout::slot_at(&self.grid.borrow(), c.x, c.y);
        let over = if inside && !cube {
            hit.or(d.over)
        } else {
            None
        };
        if over != d.over {
            d.over = over;
            self.drag.set(Some(d));
            self.layout_with(true);
        }
    }

    /// Floats the dragged pane above the stage and holds every grid at its
    /// size until the drag ends. Returns the lifted pane's size.
    fn lift(&self, serial: usize) -> (i32, i32) {
        let mut size = (0, 0);
        for p in self.panes.borrow().iter() {
            p.hold(true);
            if p.serial() != serial {
                continue;
            }
            p.set_lifted(true);
            let mut r = RECT::default();
            unsafe {
                let _ = GetClientRect(p.hwnd, &mut r);
            }
            size = (r.right, r.bottom);
            self.glides.borrow_mut().halt(p.hwnd.0 as isize);
            // Zoomed, it stays where it is and only the cube takes it.
            if !self.zoomed.get() {
                p.float(self.hwnd, true);
            }
        }
        size
    }

    /// Let go: over the cube the session goes in it, over another pane's
    /// cell the two swap. Either way the dragged pane glides into its cell.
    fn drop_drag(&self) {
        let Some(d) = self.drag.take() else {
            return;
        };
        if d.moved && cube::under_cursor(&self.shared) {
            let panes = self.panes.borrow();
            if let Some(p) = panes.iter().find(|p| p.serial() == d.serial) {
                app::push(Input::ToCube(p.session().into()));
            }
        } else if let Some(over) = d.over {
            let mut panes = self.panes.borrow_mut();
            let from = panes.iter().position(|p| p.serial() == d.serial);
            if let Some(from) = from.filter(|&f| f != over && over < panes.len()) {
                // Swapped here too, so the grid is in its new order at once
                // and the app's show of that order changes nothing.
                panes.swap(from, over);
                app::push(Input::Swap(
                    panes[over].session().into(),
                    panes[from].session().into(),
                ));
            }
        }
        self.put_down();
        if d.moved {
            self.recentre(d.serial);
            self.layout_with(true);
        }
    }

    /// Moves a pane about to take the size of another cell so its centre
    /// stays where it is. It then grows or shrinks about the point it was
    /// let go at, instead of from its corner, on its way to the cell.
    fn recentre(&self, serial: usize) {
        if self.zoomed.get() {
            return;
        }
        let panes = self.panes.borrow();
        let Some(i) = panes.iter().position(|p| p.serial() == serial) else {
            return;
        };
        let Some(&[l, t, r, b]) = self.grid.borrow().get(i) else {
            return;
        };
        let p = &panes[i];
        let (x, y) = self.child_at(p.hwnd);
        let mut now = RECT::default();
        unsafe {
            let _ = GetClientRect(p.hwnd, &mut now);
        }
        let (w, h) = (r - l, b - t);
        if (w, h) != (now.right, now.bottom) {
            p.move_to(x + (now.right - w) / 2, y + (now.bottom - h) / 2);
        }
    }

    /// Undoes what a drag changed, other than where the panes are.
    fn put_down(&self) {
        crate::vsync::stop(self.hwnd, DRAG_TIMER);
        cube::lid(&self.shared, false);
        for p in self.panes.borrow().iter() {
            p.float(self.hwnd, false);
            p.set_lifted(false);
            p.hold(false);
        }
    }

    /// Where the drag alone would put the window, as left, top, right,
    /// bottom. Windows builds each rect in the move and size loops from the
    /// one returned last, so a snapped rect would snap again on every small
    /// step and never let go. The cursor is the honest measure: the first
    /// message of a drag records how far it is from each edge, and every
    /// edge being dragged keeps that distance.
    fn free_rect(&self, r: &RECT, edges: [bool; 4]) -> [i32; 4] {
        let mut c = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut c);
        }
        let at = [c.x, c.y, c.x, c.y];
        let now = [r.left, r.top, r.right, r.bottom];
        let grab = self.grab.get().unwrap_or_else(|| {
            let g = std::array::from_fn(|i| at[i] - now[i]);
            self.grab.set(Some(g));
            g
        });
        std::array::from_fn(|i| if edges[i] { at[i] - grab[i] } else { now[i] })
    }

    /// Pulls a move in progress onto the lines its visible edges can snap to.
    fn snap_move(&self, r: &mut RECT) {
        let [l, t, rr, b] = self.free_rect(r, [true; 4]);
        let (mut dx, mut dy) = (0, 0);
        if let Some((work, spacing)) = snapping::frame(self.dpi()) {
            // Read each time: a restore from maximised or a new monitor's
            // DPI changes the invisible border mid drag.
            let [bl, bt, br, bb] = snapping::border(self.hwnd);
            let pos = (l + bl, t + bt);
            let size = (rr - br - pos.0, b - bb - pos.1);
            let (x, y) = layout::snap(pos, size, work, &self.others.borrow(), spacing);
            (dx, dy) = (x - pos.0, y - pos.1);
        }
        *r = RECT {
            left: l + dx,
            top: t + dy,
            right: rr + dx,
            bottom: b + dy,
        };
    }

    /// Pulls the edges a resize is dragging onto the lines they can snap to.
    fn snap_size(&self, r: &mut RECT, side: u32) {
        let edges = match side {
            WMSZ_LEFT => [true, false, false, false],
            WMSZ_RIGHT => [false, false, true, false],
            WMSZ_TOP => [false, true, false, false],
            WMSZ_BOTTOM => [false, false, false, true],
            WMSZ_TOPLEFT => [true, true, false, false],
            WMSZ_TOPRIGHT => [false, true, true, false],
            WMSZ_BOTTOMLEFT => [true, false, false, true],
            WMSZ_BOTTOMRIGHT => [false, false, true, true],
            _ => return,
        };
        let mut out = self.free_rect(r, edges);
        if let Some((work, spacing)) = snapping::frame(self.dpi()) {
            let [bl, bt, br, bb] = snapping::border(self.hwnd);
            let [l, t, rr, b] = out;
            let seen = [l + bl, t + bt, rr - br, b - bb];
            let [l, t, rr, b] =
                layout::snap_edges(seen, edges, work, &self.others.borrow(), spacing);
            out = [l - bl, t - bt, rr + br, b + bb];
        }
        // Windows keeps its own rect above the smallest size, the free one
        // has to be kept there by hand.
        let min = unsafe {
            [
                GetSystemMetrics(SM_CXMINTRACK),
                GetSystemMetrics(SM_CYMINTRACK),
            ]
        };
        for (lo, least) in min.into_iter().enumerate() {
            let hi = lo + 2;
            if out[hi] - out[lo] < least {
                if edges[lo] {
                    out[lo] = out[hi] - least;
                } else {
                    out[hi] = out[lo] + least;
                }
            }
        }
        let [l, t, rr, b] = out;
        *r = RECT {
            left: l,
            top: t,
            right: rr,
            bottom: b,
        };
    }

    fn handle(&self, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
        match msg {
            WM_TIMER if wparam.0 == DRAG_TIMER => {
                crate::vsync::took(self.hwnd, DRAG_TIMER);
                self.carry();
                Some(LRESULT(0))
            }
            WM_TIMER if wparam.0 == GLIDE_TIMER => {
                crate::vsync::took(self.hwnd, GLIDE_TIMER);
                self.glide();
                Some(LRESULT(0))
            }
            // The client runs up to the top of the frame, over where the
            // title bar was: the caption is drawn there instead. Maximised,
            // the frame hangs past the screen's edge, so the client starts
            // where the screen does.
            WM_NCCALCSIZE if wparam.0 != 0 => {
                let top = unsafe { (*(lparam.0 as *const NCCALCSIZE_PARAMS)).rgrc[0].top };
                unsafe {
                    DefWindowProcW(self.hwnd, msg, wparam, lparam);
                }
                let zoomed = unsafe { IsZoomed(self.hwnd) }.as_bool();
                let params = unsafe { &mut *(lparam.0 as *mut NCCALCSIZE_PARAMS) };
                params.rgrc[0].top = top + if zoomed { self.frame_y() } else { 0 };
                Some(LRESULT(0))
            }
            WM_NCHITTEST => {
                let hit = unsafe { DefWindowProcW(self.hwnd, msg, wparam, lparam) };
                if hit.0 != HTCLIENT as isize {
                    return Some(hit);
                }
                let mut p = POINT {
                    x: (lparam.0 & 0xffff) as i16 as i32,
                    y: ((lparam.0 >> 16) & 0xffff) as i16 as i32,
                };
                unsafe {
                    let _ = ScreenToClient(self.hwnd, &mut p);
                }
                Some(LRESULT(self.hit_test(p) as isize))
            }
            WM_NCMOUSEMOVE => {
                if !self.tracking.replace(true) {
                    let mut track = TRACKMOUSEEVENT {
                        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE | TME_NONCLIENT,
                        hwndTrack: self.hwnd,
                        dwHoverTime: 0,
                    };
                    unsafe {
                        let _ = TrackMouseEvent(&mut track);
                    }
                }
                let key = Self::key_of(wparam.0 as u32);
                let zoomed = unsafe { IsZoomed(self.hwnd) }.as_bool();
                let line = key.and_then(|k| tip::caption(k, zoomed));
                tip::over(&self.shared, self.hwnd, line);
                self.with_caption(|c| {
                    c.set_hot(key);
                    if c.pressed().is_some() && c.pressed() != key {
                        c.set_pressed(None);
                    }
                });
                None
            }
            WM_NCMOUSELEAVE => {
                self.tracking.set(false);
                tip::away(self.hwnd);
                self.with_caption(|c| {
                    c.set_hot(None);
                    c.set_pressed(None);
                });
                None
            }
            // Windows would draw its own keys over the caption on a press,
            // so the caption's keys are pressed and let go here.
            WM_NCLBUTTONDOWN | WM_NCLBUTTONDBLCLK => {
                tip::press(self.hwnd);
                let key = Self::key_of(wparam.0 as u32)?;
                self.with_caption(|c| c.set_pressed(Some(key)));
                Some(LRESULT(0))
            }
            WM_NCLBUTTONUP => {
                let key = Self::key_of(wparam.0 as u32)?;
                let pressed = self.caption.borrow().as_ref().and_then(|c| c.pressed());
                self.with_caption(|c| c.set_pressed(None));
                if pressed == Some(key) {
                    self.press_key(key);
                }
                Some(LRESULT(0))
            }
            WM_NCRBUTTONUP if wparam.0 as u32 == HTCAPTION => {
                self.window_menu();
                Some(LRESULT(0))
            }
            WM_SYSCOMMAND
                if wparam.0 & 0xFFF0 == SC_KEYMENU as usize && lparam.0 == ' ' as isize =>
            {
                self.window_menu();
                Some(LRESULT(0))
            }
            // Windows repaints its frame on activation; with no title bar
            // there is nothing of it to see, and the caption goes quiet
            // instead.
            WM_NCACTIVATE => {
                self.with_caption(|c| c.set_active(wparam.0 != 0));
                if wparam.0 != 0 {
                    app::push(Input::StageActive);
                }
                Some(unsafe { DefWindowProcW(self.hwnd, msg, wparam, LPARAM(-1)) })
            }
            // Minimized, the client area is empty and there is nothing to
            // lay out; the restore brings its own WM_SIZE.
            WM_SIZE if wparam.0 == SIZE_MINIMIZED as usize => None,
            WM_SIZE => {
                // A new session wobbles the stage's size as it shows, which
                // would stop its panes mid glide. An edge dragged by hand
                // starts with none under way, and places them at once.
                let gliding = self.glides.borrow().moving();
                self.layout_with(gliding);
                // The faceplate's light runs top to bottom of the whole
                // window, so a new height moves all of it.
                unsafe {
                    let _ = InvalidateRect(Some(self.hwnd), None, false);
                }
                None
            }
            WM_PAINT => {
                self.paint_plate();
                Some(LRESULT(0))
            }
            WM_ERASEBKGND => Some(LRESULT(1)),
            WM_DPICHANGED => {
                self.with_caption(Caption::set_dpi);
                let s = unsafe { *(lparam.0 as *const RECT) };
                let [l, t, r, b] = self
                    .placing
                    .get()
                    .unwrap_or([s.left, s.top, s.right, s.bottom]);
                unsafe {
                    let _ = SetWindowPos(
                        self.hwnd,
                        None,
                        l,
                        t,
                        r - l,
                        b - t,
                        SWP_NOZORDER | SWP_NOACTIVATE,
                    );
                }
                Some(LRESULT(0))
            }
            WM_ENTERSIZEMOVE => {
                *self.others.borrow_mut() = snapping::others(self.hwnd);
                self.grab.set(None);
                None
            }
            WM_MOVING => {
                self.snap_move(unsafe { &mut *(lparam.0 as *mut RECT) });
                Some(LRESULT(1))
            }
            WM_SIZING => {
                self.snap_size(unsafe { &mut *(lparam.0 as *mut RECT) }, wparam.0 as u32);
                Some(LRESULT(1))
            }
            WM_CLOSE => {
                app::push(Input::Close(self.hwnd.0 as isize));
                Some(LRESULT(0))
            }
            // The window was activated: the keyboard goes back to the pane
            // that had it.
            WM_SETFOCUS => {
                self.focus_active();
                Some(LRESULT(0))
            }
            WM_PANE_FOCUS => {
                let id = self
                    .panes
                    .borrow()
                    .iter()
                    .find(|p| p.serial() == wparam.0)
                    .map(|p| p.session().to_string());
                if let Some(id) = id {
                    self.set_active(id);
                }
                // Headers show which pane has the keyboard.
                for p in self.panes.borrow().iter() {
                    p.invalidate();
                }
                Some(LRESULT(0))
            }
            WM_PANE_GRAB => {
                self.start_drag(wparam.0);
                Some(LRESULT(0))
            }
            WM_STAGE_LAYOUT => {
                self.layout();
                Some(LRESULT(0))
            }
            WM_SETCURSOR
                if (lparam.0 & 0xffff) as u32 == HTCLIENT
                    && (self.seam_drag.get().is_some() || self.on_seam(self.cursor())) =>
            {
                let cursor = match self.seam.get() {
                    Some((Side::Top, _)) => IDC_SIZENS,
                    _ => IDC_SIZEWE,
                };
                unsafe {
                    SetCursor(LoadCursorW(None, cursor).ok());
                }
                Some(LRESULT(1))
            }
            WM_LBUTTONDOWN if self.on_seam(self.cursor()) => {
                if let Some((side, (start, _))) = self.seam.get() {
                    self.seam_drag
                        .set(Some(Self::across(side, self.cursor()) - start));
                }
                unsafe {
                    SetCapture(self.hwnd);
                }
                Some(LRESULT(0))
            }
            WM_PANE_ZOOM => {
                self.toggle_zoom(wparam.0);
                Some(LRESULT(0))
            }
            WM_PANE_MOVE => {
                if let Some(&dir) = DIRS.get(lparam.0 as usize) {
                    self.move_focus(wparam.0, dir);
                }
                Some(LRESULT(0))
            }
            WM_MOUSEMOVE if self.seam_drag.get().is_some() => {
                if let Some(grab) = self.seam_drag.get() {
                    self.drag_seam(grab);
                }
                Some(LRESULT(0))
            }
            WM_LBUTTONUP if self.seam_drag.get().is_some() => {
                self.seam_drag.set(None);
                unsafe {
                    let _ = ReleaseCapture();
                }
                Some(LRESULT(0))
            }
            WM_MOUSEMOVE => {
                self.drag_to();
                Some(LRESULT(0))
            }
            WM_LBUTTONUP => {
                self.drop_drag();
                unsafe {
                    let _ = ReleaseCapture();
                }
                Some(LRESULT(0))
            }
            // Taken away mid drag: everything goes back where it was.
            WM_CAPTURECHANGED => {
                self.seam_drag.set(None);
                if let Some(d) = self.drag.take() {
                    self.put_down();
                    if d.moved {
                        self.layout_with(true);
                    }
                }
                Some(LRESULT(0))
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
    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const TerminalWindow;
    if ptr.is_null() {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    if msg == WM_NCDESTROY {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    // The app owns the Box and destroys the window before dropping it.
    let win = &*ptr;
    match win.handle(msg, wparam, lparam) {
        Some(r) => r,
        None => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
