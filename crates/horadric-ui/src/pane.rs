//! One session inside the stage: a child window that draws its console's
//! grid and takes its keyboard and mouse.
//!
//! The stage lays its panes out in a grid, one per session of the project
//! it shows. Each pane has a header naming its session, with buttons to
//! stash, zoom and end it, and dragging a header onto another pane swaps
//! the two. The drag itself is
//! the stage's: a pane only says it was grabbed.
//!
//! A pane can also show a file instead of a session (see
//! [`Console::view`]). It is read only: keys scroll it, Ctrl+C copies, and
//! Esc or the cross in its header closes it.
//!
//! Ctrl+Shift+T in any pane opens a plain terminal in the project on the
//! stage. A few more chords belong to the stage, not the program
//! ([`keys::chord`]): zoom, moving to the next pane, the font size, and
//! Ctrl+Shift+F, which opens a search bar over the pane. While it is open
//! the keyboard types into it: Enter finds the next match up the history,
//! Shift+Enter the next one down, Esc closes it.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use alacritty_terminal::event::EventListener;
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Boundary, Column, Direction, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::search::{Match, RegexSearch};
use alacritty_terminal::term::{Term, TermMode};
use alacritty_terminal::vte::ansi::Rgb;
use windows::core::{w, Result, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::LOGFONTW;
use windows::Win32::Graphics::Gdi::{ClientToScreen, InvalidateRect, ScreenToClient, ValidateRect};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::Ime::{
    ImmGetContext, ImmReleaseContext, ImmSetCandidateWindow, ImmSetCompositionFontW,
    ImmSetCompositionWindow, CANDIDATEFORM, CFS_EXCLUDE, CFS_POINT, COMPOSITIONFORM,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, VK_LBUTTON, VK_MBUTTON, VK_RBUTTON,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, MapVirtualKeyW, ReleaseCapture, SetCapture, SetFocus, TrackMouseEvent,
    MAPVK_VK_TO_CHAR, TME_LEAVE, TRACKMOUSEEVENT, VIRTUAL_KEY, VK_BACK, VK_CONTROL, VK_DELETE,
    VK_DOWN, VK_END, VK_ESCAPE, VK_F1, VK_F12, VK_F3, VK_F4, VK_HOME, VK_INSERT, VK_LEFT, VK_MENU,
    VK_NEXT, VK_PRIOR, VK_RETURN, VK_RIGHT, VK_SHIFT, VK_SPACE, VK_UP,
};
use windows::Win32::UI::Shell::{DragAcceptFiles, DragFinish, HDROP};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetCaretBlinkTime, GetClientRect, GetCursorPos,
    GetParent, GetWindowLongPtrW, IsWindowVisible, KillTimer, LoadCursorW, PeekMessageW,
    RegisterClassW, SendMessageW, SetCursor, SetTimer, SetWindowLongPtrW, SetWindowPos, ShowWindow,
    CREATESTRUCTW, CS_DBLCLKS, GWLP_USERDATA, HTCLIENT, IDC_ARROW, IDC_HAND, IDC_IBEAM, IDC_SIZENS,
    IDC_SIZENWSE, IDC_SIZEWE, MSG, PM_NOREMOVE, PM_REMOVE, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
    SWP_NOZORDER, SW_HIDE, SW_SHOWNA, WINDOW_EX_STYLE, WINDOW_STYLE, WM_CAPTURECHANGED, WM_CHAR,
    WM_DEADCHAR, WM_DESTROY, WM_DPICHANGED_AFTERPARENT, WM_DROPFILES, WM_ERASEBKGND,
    WM_IME_STARTCOMPOSITION, WM_KEYDOWN, WM_KEYUP, WM_KILLFOCUS, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN,
    WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MOUSEHWHEEL, WM_MOUSEMOVE, WM_MOUSEWHEEL,
    WM_NCCREATE, WM_NCDESTROY, WM_PAINT, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SETCURSOR, WM_SETFOCUS,
    WM_SIZE, WM_SYSCHAR, WM_SYSDEADCHAR, WM_SYSKEYDOWN, WM_SYSKEYUP, WM_TIMER, WM_USER, WNDCLASSW,
    WS_CHILD, WS_CLIPCHILDREN, WS_CLIPSIBLINGS, WS_VISIBLE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetWindowRect, SetParent, GWLP_HWNDPARENT, GWL_EXSTYLE, GWL_STYLE, HWND_TOP, SWP_FRAMECHANGED,
    SWP_NOCOPYBITS, SWP_NOOWNERZORDER, SWP_NOREDRAW, SWP_NOSENDCHANGING, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_POPUP,
};
use windows::Win32::UI::WindowsAndMessaging::{IsChild, PostMessageW};

use crate::app::{self, Input, PaneAsk};
use crate::clipboard;
use crate::console::{Console, GridSize};
use crate::field::{self, Field};
use crate::frame::{Decoration, Stroke};
use crate::glyphs::{
    self, Bar, BarEdit, BarHit, BarLayout, Buttons, CellSize, FindBar, GridTarget, Header,
    PageFrame, TabHit, BAR_INSET, HEADER_H,
};
use crate::keys::{
    self, Button, CharAction, Chord, FontStep, Key, KeyEvent, Kitty, Mods, MouseEncoding,
    MouseEvent,
};
use crate::layout::Dir;
use crate::links::{self, Target};
use crate::motion::{self, REVEAL, SPOTLIGHT};
use crate::paste::{self, Source};
use crate::theme::{self, Color};
use crate::viewer::Hit;
use crate::viewport::{self, Fit, Grip};
use crate::web::{self, TabStep};
use crate::window::Shared;
use crate::{find, frame, tip, watch};

const CLASS: PCWSTR = w!("HoradricPane");
const SYNC_TIMER: usize = 1;
/// Asks for the next frame while the pane fades in or steps back.
const ANIM_TIMER: usize = 2;
/// Fires when a blinking cursor next turns on or off.
const BLINK_TIMER: usize = 3;
/// The next step of the Matrix rain behind a terminal's text.
const RAIN_TIMER: usize = 4;
/// How often the rain moves on, about as often as it did on the film's
/// monitors.
const RAIN_MS: u32 = 80;
/// How far a pane without the keyboard steps back: the background laid
/// over it at this strength.
const DIMMED: f32 = 0.32;
const WHEEL_LINES: i32 = 3;
/// Wheel movement per column a file view scrolls sideways: six a notch.
const WHEEL_COL: i32 = 20;
/// Columns an arrow key scrolls a file view sideways.
const ARROW_COLS: isize = 4;
/// Longer than any address worth typing; some sign in links run long.
const MAX_ADDRESS: usize = 8000;
/// With `WM_WEB_EDIT`: give the address field back the keyboard it had,
/// leaving what is typed as it is.
const RETAKE: usize = 1;
/// In the Controls part of the Windows API, which is not worth the feature
/// for one number.
const WM_MOUSELEAVE: u32 = 0x02A3;
/// The underline of the link under the mouse while Ctrl is held.
const LINK: Rgb = Rgb {
    r: 0x6C,
    g: 0xB6,
    b: 0xFF,
};

/// Sent to the stage when a pane gets the keyboard. `wparam` is its serial.
pub const WM_PANE_FOCUS: u32 = WM_USER + 1;
/// Sent to the stage when a pane's header is pressed, which may start a
/// drag. `wparam` is its serial.
pub const WM_PANE_GRAB: u32 = WM_USER + 2;
/// Sent to the stage to zoom a pane in or out: its zoom button, a double
/// click on its header, or Ctrl+Shift+Enter. `wparam` is its serial.
pub const WM_PANE_ZOOM: u32 = WM_USER + 3;
/// Sent to the stage to move the keyboard to the next pane. `wparam` is
/// the serial of the pane it leaves, `lparam` the [`Dir`] as a number.
pub const WM_PANE_MOVE: u32 = WM_USER + 4;

/// Directions as they travel in a message.
pub const DIRS: [Dir; 4] = [Dir::Left, Dir::Right, Dir::Up, Dir::Down];

/// A search of the pane's history, while its bar is open.
struct Search {
    query: String,
    /// None while the query is empty or can not be searched for.
    regex: Option<RegexSearch>,
    /// The match shown, which is also the selection.
    found: Option<Match>,
    /// In a file view, the same match as it is in the file.
    hit: Option<Hit>,
}

/// A sized page's grip held down: where it was pressed in DIPs, and the
/// size and zoom the page had then.
#[derive(Clone, Copy)]
struct Resize {
    grip: Grip,
    at: (f32, f32),
    from: (u32, u32),
    zoom: f32,
}

/// Everything a pane's frame is drawn from, besides a browser's bar.
#[derive(PartialEq)]
struct Shown {
    frame: frame::Frame,
    name: String,
    detail: String,
    phase: Option<Color>,
    accent: Color,
    active: bool,
    lifted: bool,
    stash: bool,
    zoom: Option<bool>,
    find: Option<(String, String)>,
    veil: f32,
    plate: (f32, f32),
    size: (i32, i32),
    dpi: u32,
    font: (f32, String),
    theme: theme::Theme,
}

pub struct Pane {
    pub hwnd: HWND,
    console: Arc<Console>,
    shared: Rc<Shared>,
    /// The session's name, for the header.
    name: RefCell<String>,
    lifted: Cell<bool>,
    /// The grid keeps its size while the window's changes, until let go.
    held: Cell<bool>,
    /// The stage, while the pane floats above it as a window of its own.
    floating: Cell<Option<HWND>>,
    /// Something shown has changed since the last frame was drawn. Until
    /// it has, a paint only presents that frame again: Windows asks for
    /// one when a neighbour uncovers a strip of this pane, or when this
    /// pane is moved, and neither changes what it shows.
    stale: Cell<bool>,
    /// What the last frame drawn showed. Output that changed nothing on
    /// screen, such as a title whose spinner is left out, presents it
    /// again instead of drawing it.
    drawn: RefCell<Option<Shown>>,
    /// Its rain timer runs.
    raining: Cell<bool>,
    target: RefCell<Option<GridTarget>>,
    /// The DPI the render target was made for. A child window hears of a
    /// new monitor only through its parent.
    dpi: Cell<u32>,
    focused: Cell<bool>,
    /// Where the input method's window was last put, in client pixels, so
    /// a paint moves it only when the cursor has.
    ime_at: Cell<Option<[i32; 4]>>,
    selecting: Cell<bool>,
    /// The button whose press went to the program, until it comes up.
    reported: Cell<Option<Button>>,
    /// The cell of the last reported move, so a move within a cell is not
    /// sent again.
    moved_to: Cell<Option<(usize, usize)>>,
    /// First half of a character outside the BMP, until the second arrives.
    high_surrogate: Cell<Option<u16>>,
    /// Keys whose press went to the program as a kitty escape code, so
    /// their release does too. A chord the stage kept stays unreported.
    kitty_down: RefCell<Vec<u16>>,
    /// Wheel movement below one notch, from precision touchpads.
    wheel: Cell<i32>,
    /// The same, sideways, for a file view.
    hwheel: Cell<i32>,
    /// The project's colour, for the header of the pane with the keyboard.
    accent: Cell<Color>,
    /// How far the pane has stepped back, from 0 to 1, and whether it is
    /// on its way back or forward.
    dim: Cell<f32>,
    dimmed: Cell<bool>,
    /// When the pane was last shown fresh, for the fade in.
    shown: Cell<Option<Instant>>,
    /// The frame before, for how far a fade has got.
    last_frame: Cell<Option<Instant>>,
    animating: Cell<bool>,
    /// The header's zoom button, when the stage shows more than one pane,
    /// and whether this one is zoomed.
    zoom: Cell<Option<bool>>,
    search: RefCell<Option<Search>>,
    /// Where the cursor was at the last paint and since when, which is
    /// where its blink starts: a cursor on the move stays lit.
    caret_at: Cell<Option<Point>>,
    caret_since: Cell<Instant>,
    /// A browser pane's address as it is typed, while its field has the
    /// keyboard.
    address: RefCell<Option<Field>>,
    /// A sized page's grip, while it is held down.
    resizing: Cell<Option<Resize>>,
    /// The cells of the link under the mouse while Ctrl is held, drawn
    /// underlined.
    link: RefCell<Option<Vec<Point>>>,
    /// Whether Windows was asked to say when the mouse leaves.
    tracking: Cell<bool>,
}

/// The keys that make no character, which `WM_CHAR` never carries.
fn function_key(vk: VIRTUAL_KEY) -> Option<Key> {
    let key = match vk {
        VK_UP => Key::Up,
        VK_DOWN => Key::Down,
        VK_LEFT => Key::Left,
        VK_RIGHT => Key::Right,
        VK_HOME => Key::Home,
        VK_END => Key::End,
        VK_PRIOR => Key::PageUp,
        VK_NEXT => Key::PageDown,
        VK_INSERT => Key::Insert,
        VK_DELETE => Key::Delete,
        v if (VK_F1.0..=VK_F12.0).contains(&v.0) => Key::F((v.0 - VK_F1.0 + 1) as u8),
        _ => return None,
    };
    Some(key)
}

pub fn register_class() -> Result<()> {
    unsafe {
        let instance = GetModuleHandleW(None)?;
        let wc = WNDCLASSW {
            style: CS_DBLCLKS,
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            lpszClassName: CLASS,
            hCursor: LoadCursorW(None, IDC_IBEAM)?,
            ..Default::default()
        };
        RegisterClassW(&wc);
        Ok(())
    }
}

impl Pane {
    /// A pane for a console inside `parent`, with no size until the stage
    /// lays it out. A size it was born with would reach the agent as a
    /// resize and a redraw.
    pub fn create(
        shared: Rc<Shared>,
        console: Arc<Console>,
        name: String,
        parent: HWND,
    ) -> Result<Box<Self>> {
        let mut pane = Box::new(Pane {
            hwnd: HWND::default(),
            console,
            shared,
            name: RefCell::new(name),
            lifted: Cell::new(false),
            held: Cell::new(false),
            floating: Cell::new(None),
            stale: Cell::new(true),
            drawn: RefCell::new(None),
            raining: Cell::new(false),
            target: RefCell::new(None),
            dpi: Cell::new(0),
            focused: Cell::new(false),
            ime_at: Cell::new(None),
            selecting: Cell::new(false),
            link: RefCell::new(None),
            tracking: Cell::new(false),
            reported: Cell::new(None),
            moved_to: Cell::new(None),
            high_surrogate: Cell::new(None),
            kitty_down: RefCell::new(Vec::new()),
            wheel: Cell::new(0),
            hwheel: Cell::new(0),
            accent: Cell::new(theme::ACCENTS[0].0),
            dim: Cell::new(0.0),
            dimmed: Cell::new(false),
            shown: Cell::new(None),
            last_frame: Cell::new(None),
            animating: Cell::new(false),
            zoom: Cell::new(None),
            search: RefCell::new(None),
            caret_at: Cell::new(None),
            caret_since: Cell::new(Instant::now()),
            address: RefCell::new(None),
            resizing: Cell::new(None),
        });
        // A browser pane's page is a window inside it, which its own
        // drawing must leave alone.
        let clip = if pane.console.web.is_some() {
            WS_CLIPCHILDREN
        } else {
            WINDOW_STYLE(0)
        };
        unsafe {
            let instance = GetModuleHandleW(None)?;
            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                CLASS,
                PCWSTR::null(),
                WS_CHILD | WS_VISIBLE | WS_CLIPSIBLINGS | clip,
                0,
                0,
                0,
                0,
                Some(parent),
                None,
                Some(instance.into()),
                Some(&*pane as *const Pane as *const c_void),
            )?;
            pane.hwnd = hwnd;
            // A dropped path would have nowhere to go in a file.
            DragAcceptFiles(hwnd, !pane.console.is_view());
        }
        if let Some(key) = &pane.console.web {
            let (bounds, zoom) = pane.page_place();
            web::attach(key, pane.hwnd, bounds, zoom);
        }
        Ok(pane)
    }

    pub fn destroy(&self) {
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }

    pub fn console(&self) -> &Arc<Console> {
        &self.console
    }

    pub fn serial(&self) -> usize {
        self.console.serial
    }

    pub fn session(&self) -> &str {
        &self.console.id
    }

    pub fn name(&self) -> String {
        self.name.borrow().clone()
    }

    pub fn set_name(&self, name: String) {
        if *self.name.borrow() != name {
            *self.name.borrow_mut() = name;
            self.invalidate();
        }
    }

    /// Sizes the pane, leaving it where it is.
    pub fn set_size(&self, w: i32, h: i32) {
        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                None,
                0,
                0,
                w,
                h,
                SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOMOVE,
            );
        }
    }

    /// Moves the pane inside the stage, in client pixels, keeping its size.
    pub fn move_to(&self, x: i32, y: i32) {
        let mut at = POINT { x, y };
        let mut flags = SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOSIZE;
        if let Some(stage) = self.floating.get() {
            unsafe {
                let _ = ClientToScreen(stage, &mut at);
            }
            // The compositor moves a window of its own as it is: there are
            // no pixels to copy and nothing uncovered to paint.
            flags |= SWP_NOCOPYBITS | SWP_NOREDRAW | SWP_NOSENDCHANGING | SWP_NOOWNERZORDER;
        }
        unsafe {
            let _ = SetWindowPos(self.hwnd, None, at.x, at.y, 0, 0, flags);
        }
    }

    /// Takes the pane out of the stage into a window of its own, owned by
    /// the stage so it stays above it, or puts it back, where it is on
    /// screen either way. A child moved across the stage damages the stage
    /// and every pane it passes over, and each of them paints again; a
    /// window of its own is only moved by the compositor.
    pub fn float(&self, stage: HWND, on: bool) {
        if self.floating.get().is_some() == on {
            return;
        }
        let mut r = RECT::default();
        unsafe {
            let _ = GetWindowRect(self.hwnd, &mut r);
            let style = GetWindowLongPtrW(self.hwnd, GWL_STYLE) as u32;
            let ex = GetWindowLongPtrW(self.hwnd, GWL_EXSTYLE) as u32;
            let loose = WS_EX_NOACTIVATE.0 | WS_EX_TOOLWINDOW.0;
            let mut at = POINT {
                x: r.left,
                y: r.top,
            };
            // Windows leaves the child and popup styles to the caller, in
            // the order SetParent's documentation gives.
            if on {
                let _ = SetParent(self.hwnd, None);
                SetWindowLongPtrW(
                    self.hwnd,
                    GWL_STYLE,
                    ((style & !WS_CHILD.0) | WS_POPUP.0) as isize,
                );
                SetWindowLongPtrW(self.hwnd, GWL_EXSTYLE, (ex | loose) as isize);
                SetWindowLongPtrW(self.hwnd, GWLP_HWNDPARENT, stage.0 as isize);
                self.floating.set(Some(stage));
            } else {
                SetWindowLongPtrW(self.hwnd, GWLP_HWNDPARENT, 0);
                SetWindowLongPtrW(
                    self.hwnd,
                    GWL_STYLE,
                    ((style & !WS_POPUP.0) | WS_CHILD.0) as isize,
                );
                SetWindowLongPtrW(self.hwnd, GWL_EXSTYLE, (ex & !loose) as isize);
                let _ = SetParent(self.hwnd, Some(stage));
                let _ = ScreenToClient(stage, &mut at);
                self.floating.set(None);
            }
            let _ = SetWindowPos(
                self.hwnd,
                Some(HWND_TOP),
                at.x,
                at.y,
                0,
                0,
                SWP_NOSIZE | SWP_NOACTIVATE | SWP_FRAMECHANGED,
            );
        }
    }

    pub fn set_accent(&self, c: Color) {
        if self.accent.replace(c) != c {
            self.invalidate();
        }
    }

    /// Steps the pane back while another has the keyboard, so the one you
    /// type into is the one lit.
    pub fn set_dimmed(&self, on: bool) {
        if self.dimmed.replace(on) != on {
            self.invalidate();
        }
    }

    /// Which session's agent works in each of the browser's tabs, in the
    /// colour of what it is doing. None where none does, or its session
    /// has ended.
    fn tab_badges(&self, key: &str) -> Vec<Option<glyphs::TabBadge>> {
        let drivers = web::drivers(key);
        let Ok(r) = self.shared.registry.lock() else {
            return Vec::new();
        };
        drivers
            .iter()
            .map(|d| {
                let s = r.get(d.as_deref()?)?;
                (s.phase != horadric_core::Phase::Ended).then(|| glyphs::TabBadge {
                    letter: glyphs::badge_letter(s.label()),
                    ink: match s.phase {
                        horadric_core::Phase::Idle | horadric_core::Phase::Paused => {
                            theme::text_dim()
                        }
                        _ => theme::phase_color(&s.phase),
                    },
                })
            })
            .collect()
    }

    /// Fades the pane in from the background, for a stage that has just
    /// switched to its project.
    pub fn reveal(&self) {
        self.shown.set(Some(Instant::now()));
        self.invalidate();
    }

    /// Moves the fades on to now and returns how much background to lay
    /// over the pane, asking for another frame while one is under way.
    fn veil(&self) -> f32 {
        let now = Instant::now();
        let dt = self
            .last_frame
            .replace(Some(now))
            .map_or(Duration::ZERO, |t| now.duration_since(t))
            .min(Duration::from_millis(100));
        let target = if self.dimmed.get() { 1.0 } else { 0.0 };
        let dim = motion::fade(self.dim.get(), target, dt, SPOTLIGHT);
        self.dim.set(dim);
        let reveal = self
            .shown
            .get()
            .map_or(0.0, |t| motion::decay(now.duration_since(t), REVEAL));
        if reveal == 0.0 {
            self.shown.set(None);
        }
        let busy = dim != target || reveal > 0.0;
        if busy != self.animating.replace(busy) {
            if busy {
                crate::vsync::start(self.hwnd, ANIM_TIMER);
            } else {
                crate::vsync::stop(self.hwnd, ANIM_TIMER);
            }
        }
        (motion::ease_in_out(dim) * DIMMED).max(reveal)
    }

    pub fn set_lifted(&self, on: bool) {
        if self.lifted.replace(on) != on {
            self.invalidate();
        }
    }

    /// Holds the grid at its size while a drag rearranges the stage. A
    /// pane passing through a cell of another size would otherwise make
    /// its agent redraw for a size it keeps for a moment. Let go, the grid
    /// takes the size the window has by then.
    pub fn hold(&self, on: bool) {
        if self.held.replace(on) && !on {
            self.fit_grid();
            self.invalidate();
        }
    }

    /// Shows the header's zoom button, or not, and which way it points.
    pub fn set_zoom(&self, zoom: Option<bool>) {
        if self.zoom.replace(zoom) != zoom {
            self.invalidate();
        }
    }

    /// Hidden while another pane is zoomed. It keeps its size, so the
    /// program behind it is not told of a resize it would redraw for.
    pub fn set_visible(&self, on: bool) {
        unsafe {
            let _ = ShowWindow(self.hwnd, if on { SW_SHOWNA } else { SW_HIDE });
        }
    }

    /// The font changed size: the grid takes the rows and columns that fit
    /// now.
    pub fn refont(&self) {
        self.fit_grid();
        self.invalidate();
    }

    pub fn focus(&self) {
        unsafe {
            let _ = SetFocus(Some(self.hwnd));
        }
        if let Some(key) = &self.console.web {
            web::focus(key);
        }
    }

    /// The glass, in DIPs.
    fn glass(&self) -> [f32; 4] {
        let mut r = RECT::default();
        unsafe {
            let _ = GetClientRect(self.hwnd, &mut r);
        }
        let scale = self.dpi_now() as f32 / 96.0;
        [
            glyphs::BEZEL,
            glyphs::SCREEN_TOP,
            (r.right as f32 / scale - glyphs::BEZEL).max(glyphs::BEZEL),
            (r.bottom as f32 / scale - glyphs::BEZEL).max(glyphs::BEZEL),
        ]
    }

    /// The glass below a browser pane's tab strip, in DIPs: what its page
    /// has.
    fn page_glass(&self) -> [f32; 4] {
        let [left, top, right, bottom] = self.glass();
        [left, (top + glyphs::TABS_H).min(bottom), right, bottom]
    }

    /// Where a browser pane's page goes in the glass, in DIPs: all of it,
    /// or its own size when it has one.
    fn page_fit(&self) -> Option<(Fit, Option<(u32, u32)>)> {
        let size = web::size(self.console.web.as_deref()?);
        Some((viewport::fit(self.page_glass(), size), size))
    }

    /// The page's bounds in the pane's pixels and its zoom factor.
    fn page_place(&self) -> (RECT, f64) {
        let Some((fit, size)) = self.page_fit() else {
            return (RECT::default(), 1.0);
        };
        if let Some(key) = &self.console.web {
            web::set_room(key, viewport::unscaled(self.page_glass()));
        }
        let scale = self.dpi_now() as f32 / 96.0;
        let ([left, top, right, bottom], zoom) = viewport::pixels(&fit, size, scale);
        (
            RECT {
                left,
                top,
                right: right.max(left),
                bottom: bottom.max(top),
            },
            zoom,
        )
    }

    /// The grip of a sized page under a client point.
    fn grip_at(&self, lparam: LPARAM) -> Option<Grip> {
        let (fit, _) = self.page_fit().filter(|(_, size)| size.is_some())?;
        let (x, y) = self.dip(lparam);
        viewport::grip_at(&fit, x, y)
    }

    /// Resizes a sized page to follow the grip held since the press.
    fn drag_grip(&self, lparam: LPARAM) {
        let (Some(key), Some(r)) = (&self.console.web, self.resizing.get()) else {
            return;
        };
        let (x, y) = self.dip(lparam);
        let (dx, dy) = (x - r.at.0, y - r.at.1);
        web::set_size(key, Some(viewport::drag(r.from, r.grip, dx, dy, r.zoom)));
        // At once, not when the message it posts comes round.
        self.fit_grid();
        self.invalidate();
    }

    /// The time the rain behind the text is at, while the theme rains on a
    /// terminal, with its timer kept running, and the pane's own pattern.
    fn rain(&self) -> Option<(u64, f32)> {
        static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
        if !theme::rains() || !self.is_session() {
            return None;
        }
        if !self.raining.replace(true) {
            unsafe {
                SetTimer(Some(self.hwnd), RAIN_TIMER, RAIN_MS, None);
            }
        }
        let t = START.get_or_init(Instant::now).elapsed().as_secs_f32();
        Some((self.serial() as u64, t))
    }

    /// A session's own pane, not a file view or a browser pane.
    fn is_session(&self) -> bool {
        !self.console.is_view() && self.console.web.is_none()
    }

    /// Has the button that stashes it: an agent's session, since a plain
    /// shell has no conversation to bring back.
    fn stashes(&self) -> bool {
        self.is_session() && !self.console.shell
    }

    pub fn invalidate(&self) {
        self.stale.set(true);
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    fn dpi_now(&self) -> u32 {
        unsafe { GetDpiForWindow(self.hwnd) }.max(96)
    }

    fn cell(&self) -> CellSize {
        self.shared.font.cell(self.dpi_now())
    }

    /// Resizes the grid to fill the pane below its header, or a browser
    /// pane's page to fill its glass.
    fn fit_grid(&self) {
        if let Some(key) = &self.console.web {
            let (bounds, zoom) = self.page_place();
            web::set_bounds(key, bounds, zoom);
            return;
        }
        let mut r = RECT::default();
        unsafe {
            let _ = GetClientRect(self.hwnd, &mut r);
        }
        if r.right <= 0 || r.bottom <= 0 {
            return;
        }
        let scale = self.dpi_now() as f32 / 96.0;
        let cell = self.cell();
        let (w, h) = glyphs::grid_room(r.right as f32 / scale, r.bottom as f32 / scale);
        let cols = (w / cell.w).floor().max(2.0);
        let rows = (h / cell.h).floor().max(1.0);
        self.console.resize(GridSize {
            cols: cols as u16,
            rows: rows as u16,
        });
    }

    fn paint(&self) {
        if self.target.borrow().as_ref().is_some_and(|t| t.outdated()) {
            *self.target.borrow_mut() = None;
            self.stale.set(true);
        }
        if !self.stale.get() {
            let shown = self.target.borrow().as_ref().map(|t| t.present());
            match shown {
                Some(Ok(())) => return,
                Some(Err(_)) => *self.target.borrow_mut() = None,
                None => {}
            }
        }
        // Before drawing, so a change the drawing itself asks to show is
        // not lost.
        self.stale.set(false);
        if let Some(wait) = self.console.flush_sync() {
            unsafe {
                SetTimer(
                    Some(self.hwnd),
                    SYNC_TIMER,
                    wait.as_millis() as u32 + 1,
                    None,
                );
            }
        }
        let mut r = RECT::default();
        unsafe {
            let _ = GetClientRect(self.hwnd, &mut r);
        }
        let dpi = self.dpi_now();
        let mut slot = self.target.borrow_mut();
        if slot.is_none() {
            match GridTarget::new(
                &self.shared.gpu,
                self.hwnd,
                r.right as u32,
                r.bottom as u32,
                dpi,
            ) {
                Ok(t) => {
                    *slot = Some(t);
                    self.dpi.set(dpi);
                    *self.drawn.borrow_mut() = None;
                }
                Err(e) => {
                    eprintln!("horadric: pane render target: {e}");
                    self.stale.set(true);
                    return;
                }
            }
        }
        if self.dpi.replace(dpi) != dpi {
            if let Some(t) = slot.as_ref() {
                t.set_dpi(dpi);
            }
        }
        let mut name = self.name.borrow().clone();
        let mut detail = match (self.console.exit_code(), self.console.title()) {
            (Some(code), _) => format!("exited {code}"),
            (None, Some(t)) => t.trim().to_string(),
            (None, None) => String::new(),
        };
        if let Some((title, url)) = self.console.web.as_deref().and_then(web::label) {
            if !title.is_empty() && title != url {
                name = title;
            }
            detail = url;
        }
        let phase = self
            .shared
            .registry
            .lock()
            .ok()
            .and_then(|r| r.get(&self.console.id).map(|s| s.phase.clone()));
        let phase =
            phase.and_then(|p| (theme::edge_strength(&p) > 0.0).then(|| theme::phase_color(&p)));
        let edit = self.address.borrow().clone();
        let shown = match &edit {
            Some(f) => f.text.clone(),
            None if detail == "about:blank" => String::new(),
            None => detail.clone(),
        };
        let (tab_names, tab) = self
            .console
            .web
            .as_deref()
            .and_then(web::tabs)
            .unwrap_or_default();
        let badges = self
            .console
            .web
            .as_deref()
            .map(|key| self.tab_badges(key))
            .unwrap_or_default();
        let bar = self.console.web.as_deref().map(|key| {
            let (back, forward) = web::history(key);
            Bar {
                tabs: &tab_names,
                tab,
                badges: &badges,
                text: &shown,
                edit: edit.as_ref().map(|f| {
                    let (a, b) = f.selection();
                    BarEdit {
                        caret: field::utf16_at(&f.text, f.caret),
                        selection: (field::utf16_at(&f.text, a), field::utf16_at(&f.text, b)),
                    }
                }),
                back,
                forward,
                sized: web::size(key).is_some(),
                dock: web::dock(key).map(|d| d.side),
            }
        });
        let sized = self.page_fit().and_then(|(fit, size)| Some((fit, size?)));
        let label = sized.map(|(fit, size)| viewport::label(size, fit.zoom));
        let page = sized
            .zip(label.as_deref())
            .map(|((fit, _), label)| PageFrame {
                page: fit.page,
                label,
                dragging: self.resizing.get().is_some(),
            });
        let header = Header {
            name: &name,
            detail: &detail,
            phase,
            accent: self.accent.get(),
            active: self.focused.get(),
            lifted: self.lifted.get(),
            close: true,
            stash: self.stashes(),
            zoom: self.zoom.get(),
            bar,
        };
        let search = self.search.borrow();
        let find = search.as_ref().map(|s| FindBar {
            query: &s.query,
            status: find::status(&s.query, s.found.is_some(), self.console.is_view()),
        });
        let veil = self.veil();
        let plate = self.place_in_stage();
        let font = &self.shared.font;
        let cell = font.cell(dpi);
        let (mut frame, at, offset) = match self.console.screen.lock() {
            Ok(s) => {
                let lit = self.caret_lit(&s.term);
                let frame = frame::build(&s.term, self.focused.get(), lit, |c, style| {
                    font.glyph(c, style)
                });
                let offset = s.term.grid().display_offset() as i32;
                (frame, frame::cursor_cell(&s.term), offset)
            }
            Err(_) => {
                self.stale.set(true);
                return;
            }
        };
        // A browser pane's grid is empty and unseen, but a page smaller
        // than the glass, or the tab strip over it, would show its cursor.
        if self.console.web.is_some() {
            frame.caret = None;
            frame.fills.clear();
            frame.runs.clear();
            frame.loose.clear();
        }
        if let Some(cells) = self.link.borrow().as_ref() {
            let rows = self.console.size().rows as i32;
            for p in cells {
                let row = p.line.0 + offset;
                if !(0..rows).contains(&row) {
                    continue;
                }
                let (row, col) = (row as usize, p.column.0);
                match frame.strokes.last_mut() {
                    Some(s) if s.color == LINK && s.row == row && s.col + s.cells == col => {
                        s.cells += 1
                    }
                    _ => frame.strokes.push(Stroke {
                        row,
                        col,
                        cells: 1,
                        kind: Decoration::Underline,
                        color: LINK,
                    }),
                }
            }
        }
        if let (true, Some((row, col))) = (self.focused.get(), at) {
            let rect = glyphs::cell_rect(&cell, row, col, dpi as f32 / 96.0);
            if self.ime_at.replace(Some(rect)) != Some(rect) {
                self.place_ime(rect);
            }
        }
        let rain = self.rain();
        // A browser pane's header has its tabs and address besides, and its
        // frame is empty and cheap, so it always draws, and so does a pane
        // the rain moves behind.
        let look = Shown {
            frame,
            name: name.clone(),
            detail: detail.clone(),
            phase,
            accent: header.accent,
            active: header.active,
            lifted: header.lifted,
            stash: header.stash,
            zoom: header.zoom,
            find: find
                .as_ref()
                .map(|f| (f.query.to_string(), f.status.to_string())),
            veil,
            plate,
            size: (r.right, r.bottom),
            dpi,
            font: (font.size(), font.family()),
            theme: theme::current(),
        };
        let same = self.console.web.is_none()
            && rain.is_none()
            && self.drawn.borrow().as_ref() == Some(&look);
        if same {
            if let Some(Err(_)) = slot.as_ref().map(GridTarget::present) {
                *slot = None;
                self.stale.set(true);
            }
            return;
        }
        let result = slot.as_ref().map(|t| {
            t.draw(
                &self.shared.gpu,
                font,
                &cell,
                &look.frame,
                &header,
                find.as_ref(),
                page.as_ref(),
                veil,
                plate,
                rain,
            )
        });
        let drawn = matches!(result, Some(Ok(())));
        if !drawn {
            *slot = None;
            self.stale.set(true);
        }
        *self.drawn.borrow_mut() = drawn.then_some(look);
    }

    /// Whether the cursor is lit in this paint, and a timer for the next
    /// turn while it blinks. Only the pane with the keyboard blinks, and
    /// only when Windows blinks carets and the program has not asked for a
    /// steady cursor.
    fn caret_lit<T: EventListener>(&self, term: &Term<T>) -> bool {
        let now = Instant::now();
        let at = term.grid().cursor.point;
        if self.caret_at.replace(Some(at)) != Some(at) {
            self.caret_since.set(now);
        }
        // INFINITE when caret blinking is off in Settings, zero on failure.
        let half = match unsafe { GetCaretBlinkTime() } {
            u32::MAX => Duration::ZERO,
            ms => Duration::from_millis(ms as u64),
        };
        let blinks = self.focused.get()
            && term.mode().contains(TermMode::SHOW_CURSOR)
            && term.cursor_style().blinking;
        let elapsed = now.duration_since(self.caret_since.get());
        let next = motion::caret_turns(elapsed, half).filter(|_| blinks);
        unsafe {
            match next {
                Some(wait) => {
                    SetTimer(
                        Some(self.hwnd),
                        BLINK_TIMER,
                        wait.as_millis() as u32 + 1,
                        None,
                    );
                }
                None => {
                    let _ = KillTimer(Some(self.hwnd), BLINK_TIMER);
                }
            }
        }
        !blinks || motion::caret_lit(elapsed, half)
    }

    /// Puts the input method's composition at the cursor cell, in the
    /// terminal's font, and keeps its candidate list off that cell.
    fn place_ime(&self, [left, top, right, bottom]: [i32; 4]) {
        let scale = self.dpi_now() as f32 / 96.0;
        let mut font = LOGFONTW {
            lfHeight: -(self.shared.font.size() * scale).round() as i32,
            ..Default::default()
        };
        let family: Vec<u16> = self.shared.font.family().encode_utf16().collect();
        let n = family.len().min(font.lfFaceName.len() - 1);
        font.lfFaceName[..n].copy_from_slice(&family[..n]);
        let composition = COMPOSITIONFORM {
            dwStyle: CFS_POINT,
            ptCurrentPos: POINT { x: left, y: top },
            rcArea: RECT::default(),
        };
        let candidates = CANDIDATEFORM {
            dwIndex: 0,
            dwStyle: CFS_EXCLUDE,
            ptCurrentPos: POINT { x: left, y: bottom },
            rcArea: RECT {
                left,
                top,
                right,
                bottom,
            },
        };
        unsafe {
            let imc = ImmGetContext(self.hwnd);
            if imc.is_invalid() {
                return;
            }
            let _ = ImmSetCompositionFontW(imc, &font);
            let _ = ImmSetCompositionWindow(imc, &composition);
            let _ = ImmSetCandidateWindow(imc, &candidates);
            let _ = ImmReleaseContext(self.hwnd, imc);
        }
    }

    /// Where the pane's top is in the stage and how tall the stage is, in
    /// DIPs, for the faceplate's light to run across every pane as one.
    fn place_in_stage(&self) -> (f32, f32) {
        let scale = self.dpi_now() as f32 / 96.0;
        unsafe {
            let Ok(parent) = GetParent(self.hwnd) else {
                return (0.0, 1.0);
            };
            let mut stage = RECT::default();
            let _ = GetClientRect(parent, &mut stage);
            let mut at = POINT::default();
            let _ = ClientToScreen(self.hwnd, &mut at);
            let _ = ScreenToClient(parent, &mut at);
            (at.y as f32 / scale, stage.bottom.max(1) as f32 / scale)
        }
    }

    fn mods() -> Mods {
        let down = |vk: VIRTUAL_KEY| unsafe { GetKeyState(vk.0 as i32) } < 0;
        Mods {
            shift: down(VK_SHIFT),
            ctrl: down(VK_CONTROL),
            alt: down(VK_MENU),
        }
    }

    /// Sends typed bytes: the view jumps back to the live screen and any
    /// selection goes, as in every terminal. The console notes it, since
    /// what was typed may sit in the agent's prompt box as a draft.
    /// It repaints only when the view changed: otherwise the echo repaints,
    /// and a paint now would show the old screen and put the echo a frame
    /// behind.
    fn send(&self, bytes: Vec<u8>) {
        let moved = self.console.screen.lock().is_ok_and(|mut s| {
            let selected = s.term.selection.take().is_some();
            let scrolled = s.term.grid().display_offset() != 0;
            if scrolled {
                s.term.scroll_display(Scroll::Bottom);
            }
            selected || scrolled
        });
        self.console.note_typed();
        self.caret_since.set(Instant::now());
        self.console.write(bytes);
        if moved {
            self.invalidate();
        }
    }

    /// The kitty keyboard flags the program has pushed, if any.
    fn kitty(&self) -> Kitty {
        let mode = self.mode();
        Kitty {
            disambiguate: mode.contains(TermMode::DISAMBIGUATE_ESC_CODES),
            events: mode.contains(TermMode::REPORT_EVENT_TYPES),
            alternates: mode.contains(TermMode::REPORT_ALTERNATE_KEYS),
            all_keys: mode.contains(TermMode::REPORT_ALL_KEYS_AS_ESC),
            text: mode.contains(TermMode::REPORT_ASSOCIATED_TEXT),
        }
    }

    /// The character Windows made of the key press being handled, still in
    /// the queue, and whether it came as `WM_SYSCHAR`.
    fn queued_char(&self) -> Option<(char, bool)> {
        let mut msg = MSG::default();
        let peek = |msg: &mut MSG, first, last| unsafe {
            PeekMessageW(msg, Some(self.hwnd), first, last, PM_NOREMOVE).as_bool()
        };
        let sys = if peek(&mut msg, WM_CHAR, WM_DEADCHAR) {
            false
        } else if peek(&mut msg, WM_SYSCHAR, WM_SYSDEADCHAR) {
            true
        } else {
            return None;
        };
        if msg.message == WM_DEADCHAR || msg.message == WM_SYSDEADCHAR {
            return None;
        }
        char::from_u32(msg.wParam.0 as u32).map(|c| (c, sys))
    }

    /// A character key under the kitty protocol. Returns false when it goes
    /// the xterm way, as the `WM_CHAR` already queued.
    fn kitty_press(&self, vk: u16, mods: Mods, event: KeyEvent, flags: Kitty) -> bool {
        // Alt+Space is the window menu.
        if vk == VK_SPACE.0 && mods.alt && !mods.ctrl {
            return false;
        }
        let mapped = unsafe { MapVirtualKeyW(vk as u32, MAPVK_VK_TO_CHAR) };
        let Some(base) = keys::kitty_base(mapped) else {
            return false;
        };
        let typed = self.queued_char();
        // Paste, copy and a new shell stay the stage's. Ctrl+C with nothing
        // selected is the program's, as its escape code.
        if let Some((c, sys)) = typed {
            let char_mods = Mods { alt: sys, ..mods };
            match keys::char_action(c, char_mods) {
                CharAction::Send(_) => {}
                CharAction::CopyOrInterrupt if !self.has_selection() => {}
                _ => return false,
            }
        }
        let typed = typed.map(|(c, _)| c);
        let Some(bytes) = keys::kitty_text(base, typed, mods, event, flags) else {
            return false;
        };
        self.drop_char();
        self.kitty_sent(vk, bytes);
        true
    }

    fn kitty_sent(&self, vk: u16, bytes: Vec<u8>) {
        let mut down = self.kitty_down.borrow_mut();
        if !down.contains(&vk) {
            down.push(vk);
        }
        drop(down);
        if !bytes.is_empty() {
            self.send(bytes);
        }
    }

    /// A key coming up, which only a program that asked for event types
    /// hears about, and only for a key it heard go down.
    fn on_key_up(&self, vk: u16, mods: Mods) {
        let was_down = {
            let mut down = self.kitty_down.borrow_mut();
            let at = down.iter().position(|&d| d == vk);
            at.map(|i| down.remove(i)).is_some()
        };
        let flags = self.kitty();
        if !was_down || !flags.events || self.console.is_view() {
            return;
        }
        let bytes = match function_key(VIRTUAL_KEY(vk)) {
            Some(key) => keys::kitty_key(key, mods, KeyEvent::Release, flags, false),
            None => {
                let mapped = unsafe { MapVirtualKeyW(vk as u32, MAPVK_VK_TO_CHAR) };
                keys::kitty_base(mapped)
                    .and_then(|base| keys::kitty_text(base, None, mods, KeyEvent::Release, flags))
                    .unwrap_or_default()
            }
        };
        if !bytes.is_empty() {
            self.console.write(bytes);
        }
    }

    fn mode(&self) -> TermMode {
        self.console
            .screen
            .lock()
            .map(|s| *s.term.mode())
            .unwrap_or_default()
    }

    fn paste(&self) {
        let source = paste::choose(
            clipboard::get_text(),
            clipboard::has_image(),
            clipboard::has_files(),
        );
        let text = match source {
            Source::Text(text) => text,
            Source::Image => match clipboard::save_image() {
                Some(path) => paste::quote_paths(&[path.to_string_lossy()]),
                None => return,
            },
            Source::Files => paste::quote_paths(&clipboard::get_files()),
            Source::Nothing => return,
        };
        self.paste_text(&text);
    }

    fn paste_text(&self, text: &str) {
        if text.is_empty() {
            return;
        }
        let bracketed = self.mode().contains(TermMode::BRACKETED_PASTE);
        self.send(keys::paste_bytes(text, bracketed));
    }

    /// Files dropped from Explorer arrive as their paths, as in Windows
    /// Terminal, in the pane they were dropped on. It takes the keyboard so
    /// you can type straight after.
    fn on_drop(&self, hdrop: HDROP) {
        let paths = clipboard::drop_paths(hdrop);
        unsafe {
            DragFinish(hdrop);
        }
        self.paste_text(&paste::quote_paths(&paths));
        self.focus();
    }

    /// Copies the selection, if there is one. Returns whether it did.
    fn copy(&self) -> bool {
        let text = self.console.selection_text();
        if let Ok(mut s) = self.console.screen.lock() {
            s.term.selection = None;
        }
        self.invalidate();
        match text {
            Some(t) if !t.is_empty() => {
                clipboard::set_text(&t);
                true
            }
            _ => false,
        }
    }

    fn has_selection(&self) -> bool {
        self.console
            .screen
            .lock()
            .map(|s| s.term.selection.as_ref().is_some_and(|sel| !sel.is_empty()))
            .unwrap_or(false)
    }

    fn on_char(&self, unit: u16, mods: Mods) {
        let c = if (0xD800..0xDC00).contains(&unit) {
            self.high_surrogate.set(Some(unit));
            return;
        } else if (0xDC00..0xE000).contains(&unit) {
            let Some(high) = self.high_surrogate.take() else {
                return;
            };
            char::decode_utf16([high, unit]).next().and_then(|r| r.ok())
        } else {
            char::from_u32(unit as u32)
        };
        let Some(c) = c else { return };
        if self.search.borrow().is_some() {
            self.search_char(c, mods);
            return;
        }
        if self.console.is_view() {
            match keys::char_action(c, mods) {
                _ if c as u32 == 0x1b => self.close(),
                CharAction::Copy | CharAction::CopyOrInterrupt => {
                    self.copy();
                }
                CharAction::NewShell => app::push(Input::Shell(None)),
                CharAction::Browse => app::push(Input::Browse),
                _ => {}
            }
            return;
        }
        match keys::char_action(c, mods) {
            CharAction::Send(bytes) => self.send(bytes),
            CharAction::Paste => self.paste(),
            CharAction::Copy => {
                self.copy();
            }
            CharAction::CopyOrInterrupt => {
                if !self.has_selection() || !self.copy() {
                    self.send(vec![0x03]);
                }
            }
            CharAction::NewShell => app::push(Input::Shell(None)),
            CharAction::Browse => app::push(Input::Browse),
        }
    }

    /// Keys that make no character. Returns false to let Windows have it.
    fn on_key(&self, vk: u16, mods: Mods, repeat: bool) -> bool {
        if let Some(chord) = keys::chord(vk, mods) {
            self.drop_char();
            match chord {
                Chord::Zoom => self.tell_stage(WM_PANE_ZOOM),
                Chord::Focus(dir) => self.move_focus(dir),
                Chord::Font(step) => app::push(Input::Font(step)),
                Chord::Find => self.open_search(),
            }
            return true;
        }
        let vk = VIRTUAL_KEY(vk);
        // Alt+F4 closes the stage. Windows passes it up from a child.
        if mods.alt && vk == VK_F4 {
            return false;
        }
        if mods.shift && (vk == VK_PRIOR || vk == VK_NEXT) {
            let scroll = if vk == VK_PRIOR {
                Scroll::PageUp
            } else {
                Scroll::PageDown
            };
            if let Ok(mut s) = self.console.screen.lock() {
                s.term.scroll_display(scroll);
            }
            self.invalidate();
            return true;
        }
        // The search bar has the keyboard. Nothing reaches the program.
        if self.search.borrow().is_some() {
            match vk {
                VK_F3 if mods.shift => self.find_next(self.onward().opposite()),
                VK_F3 => self.find_next(self.onward()),
                VK_UP => self.find_next(Direction::Left),
                VK_DOWN => self.find_next(Direction::Right),
                _ => {}
            }
            return true;
        }
        if vk == VK_INSERT && mods.shift {
            self.paste();
            return true;
        }
        if vk == VK_INSERT && mods.ctrl {
            self.copy();
            return true;
        }
        if self.console.is_view() {
            return self.scroll_view(vk, mods);
        }
        let flags = self.kitty();
        let event = if repeat {
            KeyEvent::Repeat
        } else {
            KeyEvent::Press
        };
        let Some(key) = function_key(vk) else {
            return flags.any() && self.kitty_press(vk.0, mods, event, flags);
        };
        let app_cursor = self.mode().contains(TermMode::APP_CURSOR);
        if flags.any() {
            self.kitty_sent(vk.0, keys::kitty_key(key, mods, event, flags, app_cursor));
        } else {
            self.send(keys::key_bytes(key, mods, app_cursor));
        }
        true
    }

    /// A file view scrolls with the keys an editor moves its cursor with.
    fn scroll_view(&self, vk: VIRTUAL_KEY, mods: Mods) -> bool {
        let scroll = match vk {
            VK_UP => Scroll::Delta(1),
            VK_DOWN => Scroll::Delta(-1),
            VK_PRIOR => Scroll::PageUp,
            VK_NEXT => Scroll::PageDown,
            VK_HOME if mods.ctrl => Scroll::Top,
            VK_END if mods.ctrl => Scroll::Bottom,
            VK_LEFT | VK_RIGHT | VK_HOME | VK_END => {
                let cols = match vk {
                    VK_LEFT => -ARROW_COLS,
                    VK_RIGHT => ARROW_COLS,
                    VK_HOME => isize::MIN,
                    _ => isize::MAX,
                };
                if self.console.scroll_sideways(cols) {
                    self.invalidate();
                }
                return true;
            }
            _ => return false,
        };
        if let Ok(mut s) = self.console.screen.lock() {
            s.term.scroll_display(scroll);
        }
        self.invalidate();
        true
    }

    /// The cross: a file view or a browser pane closes, a session ends.
    fn close(&self) {
        match &self.console.web {
            Some(key) => app::push(Input::CloseWeb(key.clone())),
            None if self.console.is_view() => app::push(Input::CloseView(self.serial())),
            None => app::push(Input::PaneAsk(self.session().to_string(), PaneAsk::End)),
        }
    }

    /// A chord was handled on its key press. The character Windows made of
    /// the same press is already queued and must not reach the program.
    fn drop_char(&self) {
        let mut msg = MSG::default();
        unsafe {
            let _ = PeekMessageW(&mut msg, Some(self.hwnd), WM_CHAR, WM_DEADCHAR, PM_REMOVE);
            let _ = PeekMessageW(
                &mut msg,
                Some(self.hwnd),
                WM_SYSCHAR,
                WM_SYSDEADCHAR,
                PM_REMOVE,
            );
        }
    }

    fn move_focus(&self, dir: Dir) {
        let Some(i) = DIRS.iter().position(|d| *d == dir) else {
            return;
        };
        unsafe {
            if let Ok(parent) = GetParent(self.hwnd) {
                SendMessageW(
                    parent,
                    WM_PANE_MOVE,
                    Some(WPARAM(self.serial())),
                    Some(LPARAM(i as isize)),
                );
            }
        }
    }

    fn open_search(&self) {
        let mut search = self.search.borrow_mut();
        if search.is_none() {
            *search = Some(Search {
                query: String::new(),
                regex: None,
                found: None,
                hit: None,
            });
        }
        drop(search);
        self.invalidate();
    }

    fn close_search(&self) {
        self.search.borrow_mut().take();
        self.invalidate();
    }

    /// Typing while the search bar is open edits the query.
    fn search_char(&self, c: char, mods: Mods) {
        match c {
            '\u{1b}' => return self.close_search(),
            '\r' if mods.shift => return self.find_next(self.onward().opposite()),
            '\r' => return self.find_next(self.onward()),
            '\u{3}' => {
                self.copy_found();
                return;
            }
            _ => {}
        }
        let Some(query) = self.search.borrow().as_ref().map(|s| s.query.clone()) else {
            return;
        };
        let query = match c {
            // Backspace, and Ctrl+Backspace for the whole query.
            '\u{8}' => {
                let mut q = query;
                q.pop();
                q
            }
            '\u{7f}' => String::new(),
            '\u{16}' => {
                let pasted = clipboard::get_text().unwrap_or_default();
                query + pasted.lines().next().unwrap_or("")
            }
            c if c.is_control() => return,
            c => query + &c.to_string(),
        };
        self.search_for(query);
    }

    /// Where Enter goes: down a file, as an editor does, and up a
    /// terminal's history, where the newest output is at the bottom.
    fn onward(&self) -> Direction {
        if self.console.is_view() {
            Direction::Right
        } else {
            Direction::Left
        }
    }

    /// A new query. It looks up the history from the match shown, so
    /// typing more of a word stays on the same line. A file view looks
    /// down the file instead.
    fn search_for(&self, query: String) {
        if self.console.is_view() {
            let from = self.search.borrow_mut().as_mut().and_then(|s| {
                s.query = query;
                s.hit.map(|h| (h.line, h.start))
            });
            return self.search_view(from, true);
        }
        let regex = if query.is_empty() {
            None
        } else {
            RegexSearch::new(&find::pattern(&query)).ok()
        };
        let from = self
            .search
            .borrow()
            .as_ref()
            .and_then(|s| s.found.as_ref().map(|m| *m.start()));
        if let Some(s) = self.search.borrow_mut().as_mut() {
            s.query = query;
            s.regex = regex;
            s.found = None;
        }
        self.search_from(from, Direction::Left);
    }

    /// The next match from the one shown, up the history (`Left`) or down
    /// it (`Right`), wrapping round at either end.
    fn find_next(&self, direction: Direction) {
        if self.console.is_view() {
            let forward = direction == Direction::Right;
            let from = self.search.borrow().as_ref().and_then(|s| s.hit).map(|h| {
                // Forward from just past the match, so it is not found again.
                (h.line, h.start + usize::from(forward))
            });
            return self.search_view(from, forward);
        }
        let found = self
            .search
            .borrow()
            .as_ref()
            .and_then(|s| s.found.as_ref().map(|m| *m.start()));
        let from = found.and_then(|start| {
            let s = self.console.screen.lock().ok()?;
            Some(match direction {
                Direction::Left => start.sub(&s.term, Boundary::None, 1),
                Direction::Right => start.add(&s.term, Boundary::None, 1),
            })
        });
        self.search_from(from, direction);
    }

    /// Looks for the query from `from`, or from the bottom of the screen,
    /// and selects what it finds, scrolled into view.
    fn search_from(&self, from: Option<Point>, direction: Direction) {
        {
            let mut search = self.search.borrow_mut();
            let Some(search) = search.as_mut() else {
                return;
            };
            let Ok(mut s) = self.console.screen.lock() else {
                return;
            };
            let found = search.regex.as_mut().and_then(|regex| {
                let bottom =
                    Point::new(Line(s.term.screen_lines() as i32 - 1), s.term.last_column());
                s.term
                    .search_next(regex, from.unwrap_or(bottom), direction, Side::Left, None)
            });
            s.term.selection = found.as_ref().map(|m| {
                let mut sel = Selection::new(SelectionType::Simple, *m.start(), Side::Left);
                sel.update(*m.end(), Side::Right);
                sel
            });
            if let Some(m) = &found {
                s.term.scroll_to_point(*m.start());
            }
            search.found = found;
        }
        self.invalidate();
    }

    /// Looks for the query in a view's file rather than its grid, so the
    /// line numbers never match and a match may cross a wrap.
    fn search_view(&self, from: Option<(usize, usize)>, forward: bool) {
        {
            let mut search = self.search.borrow_mut();
            let Some(search) = search.as_mut() else {
                return;
            };
            let found = self.console.find(&search.query, from, forward);
            let Ok(mut s) = self.console.screen.lock() else {
                return;
            };
            s.term.selection = found.map(|(_, a, b)| {
                let mut sel = Selection::new(SelectionType::Simple, a, Side::Left);
                sel.update(b, Side::Right);
                sel
            });
            if let Some((_, a, _)) = found {
                s.term.scroll_to_point(a);
            }
            search.hit = found.map(|f| f.0);
            search.found = found.map(|(_, a, b)| a..=b);
        }
        self.invalidate();
    }

    fn copy_found(&self) {
        if let Some(t) = self.console.selection_text().filter(|t| !t.is_empty()) {
            clipboard::set_text(&t);
        }
    }

    /// A client point in DIPs.
    fn dip(&self, lparam: LPARAM) -> (f32, f32) {
        let scale = self.dpi_now() as f32 / 96.0;
        let x = (lparam.0 & 0xffff) as i16 as f32 / scale;
        let y = ((lparam.0 >> 16) & 0xffff) as i16 as f32 / scale;
        (x, y)
    }

    fn in_header(&self, lparam: LPARAM) -> bool {
        self.dip(lparam).1 < HEADER_H
    }

    /// Where the header's buttons start: the stash button's, the zoom
    /// button's, then the cross's.
    fn buttons(&self) -> Buttons {
        let mut r = RECT::default();
        unsafe {
            let _ = GetClientRect(self.hwnd, &mut r);
        }
        let width = r.right as f32 * 96.0 / self.dpi_now() as f32;
        glyphs::header_buttons(width, self.stashes(), self.zoom.get().is_some(), true)
    }

    fn on_button(&self, lparam: LPARAM, at: Option<f32>) -> bool {
        let x = self.dip(lparam).0;
        self.in_header(lparam) && at.is_some_and(|a| x >= a && x < a + HEADER_H)
    }

    fn bar_layout(&self) -> BarLayout {
        let mut r = RECT::default();
        unsafe {
            let _ = GetClientRect(self.hwnd, &mut r);
        }
        let width = r.right as f32 * 96.0 / self.dpi_now() as f32;
        glyphs::bar_layout(width, self.zoom.get().is_some(), true)
    }

    /// What in a browser pane's address bar is under a client point.
    fn bar_at(&self, lparam: LPARAM) -> Option<BarHit> {
        if self.console.web.is_none() || !self.in_header(lparam) {
            return None;
        }
        glyphs::bar_hit(&self.bar_layout(), self.dip(lparam).0)
    }

    /// What in a browser pane's tab strip is under a client point.
    fn tab_at(&self, lparam: LPARAM) -> Option<TabHit> {
        let key = self.console.web.as_deref()?;
        let (x, y) = self.dip(lparam);
        let top = glyphs::SCREEN_TOP;
        if !(top..top + glyphs::TABS_H).contains(&y) {
            return None;
        }
        let mut r = RECT::default();
        unsafe {
            let _ = GetClientRect(self.hwnd, &mut r);
        }
        let width = r.right as f32 * 96.0 / self.dpi_now() as f32;
        let count = web::tabs(key).map_or(0, |(t, _)| t.len());
        glyphs::tab_hit(&glyphs::tab_layout(width, count), x)
    }

    /// Whether a client point is in a browser pane's tab strip, on a tab
    /// or not.
    fn in_tabs(&self, lparam: LPARAM) -> bool {
        let y = self.dip(lparam).1;
        self.console.web.is_some()
            && (glyphs::SCREEN_TOP..glyphs::SCREEN_TOP + glyphs::TABS_H).contains(&y)
    }

    /// A press in a browser pane's tab strip: a tab shown, closed with its
    /// cross or the middle button, or a new one. False anywhere else.
    fn on_tabs(&self, lparam: LPARAM, middle: bool) -> bool {
        let Some(key) = &self.console.web else {
            return false;
        };
        if !self.in_tabs(lparam) {
            return false;
        }
        // What was typed is for the tab it was typed in.
        if self.address.take().is_some() {
            self.invalidate();
        }
        let step = match (self.tab_at(lparam), middle) {
            (Some(TabHit::Tab(i) | TabHit::Close(i)), true) => TabStep::Close(Some(i)),
            (Some(TabHit::Tab(i)), false) => TabStep::Select(i),
            (Some(TabHit::Close(i)), false) => TabStep::Close(Some(i)),
            (Some(TabHit::New), false) => TabStep::New,
            _ => return true,
        };
        web::tab(key, step);
        // A new tab puts the keyboard in its address field instead.
        if step != TabStep::New {
            self.focus();
        }
        true
    }

    /// A press on a browser pane's back, forward or reload, or in its
    /// address field. False anywhere else.
    fn on_bar(&self, lparam: LPARAM, double: bool) -> bool {
        let Some(key) = &self.console.web else {
            return false;
        };
        let step = match self.bar_at(lparam) {
            None => return false,
            Some(BarHit::Field) => {
                if self.address.borrow().is_some() {
                    let l = self.bar_layout();
                    self.place_caret(self.dip(lparam).0 - l.field.0 - BAR_INSET, double);
                } else {
                    self.edit_address();
                }
                return true;
            }
            Some(BarHit::Place(side)) => {
                web::toggle_dock(key, side);
                return true;
            }
            Some(BarHit::Size) => {
                app::push(Input::WebAsk(key.clone(), app::WebAsk::Size));
                return true;
            }
            Some(BarHit::Back) => web::Step::Back,
            Some(BarHit::Forward) => web::Step::Forward,
            Some(BarHit::Reload) => web::Step::Reload,
        };
        // What was typed is for where the page was, not where it goes.
        if self.address.take().is_some() {
            self.invalidate();
        }
        web::go(key, step);
        true
    }

    /// A browser's tab key while the pane, not its page, has the keyboard,
    /// as it does while the address is typed.
    fn web_tab_key(&self, vk: VIRTUAL_KEY) -> bool {
        let Some(key) = &self.console.web else {
            return false;
        };
        let m = Self::mods();
        let Some(step) = web::tab_key(vk, m.ctrl, m.shift, m.alt) else {
            return false;
        };
        self.drop_char();
        if step != TabStep::New && self.address.take().is_some() {
            self.invalidate();
        }
        web::tab(key, step);
        true
    }

    /// Gives the address field the keyboard, the address in it selected so
    /// typing replaces it. Again while it has the keyboard selects it all.
    pub fn edit_address(&self) {
        let Some(key) = &self.console.web else {
            return;
        };
        let mut slot = self.address.borrow_mut();
        match slot.as_mut() {
            Some(f) => f.select_all(),
            None => {
                let url = web::label(key)
                    .map(|(_, u)| u)
                    .filter(|u| u != "about:blank")
                    .unwrap_or_default();
                *slot = Some(Field::new(&url, false, MAX_ADDRESS));
            }
        }
        drop(slot);
        unsafe {
            let _ = SetFocus(Some(self.hwnd));
        }
        self.invalidate();
    }

    /// Whether the keyboard went from the address field to this pane's
    /// page with no click to send it there, so the page took it.
    fn page_took(&self, to: HWND) -> bool {
        let down = |vk: VIRTUAL_KEY| unsafe { GetAsyncKeyState(vk.0 as i32) } < 0;
        let clicked = down(VK_LBUTTON) || down(VK_RBUTTON) || down(VK_MBUTTON);
        !to.is_invalid() && unsafe { IsChild(self.hwnd, to) }.as_bool() && !clicked
    }

    /// A click in the field while it has the keyboard: the caret goes where
    /// it landed, `x` DIPs into the text's view, or a double click takes it
    /// all, as an address is copied whole.
    fn place_caret(&self, x: f32, double: bool) {
        let mut slot = self.address.borrow_mut();
        let Some(f) = slot.as_mut() else {
            return;
        };
        if double {
            f.select_all();
        } else if let Ok(l) = glyphs::bar_text(&self.shared.gpu, &f.text) {
            let view = glyphs::bar_view(&self.bar_layout());
            let caret = crate::render::caret_at(&l, field::utf16_at(&f.text, f.caret));
            let scroll = glyphs::bar_scroll(caret.x, view);
            let at = crate::render::offset_at(&l, x + scroll, HEADER_H / 2.0);
            f.set_caret(field::byte_at(&f.text, at), Self::mods().shift);
        }
        drop(slot);
        self.invalidate();
    }

    /// Enter goes to what was typed and Esc leaves the page where it is.
    /// Either way the page has the keyboard again.
    fn end_edit(&self, go: bool) {
        let Some(f) = self.address.take() else {
            return;
        };
        self.invalidate();
        if let (true, Some(key)) = (go, &self.console.web) {
            if let Some(url) = web::address(&f.text) {
                web::open(key, Some(&url));
            }
        }
        self.focus();
    }

    /// A key while the address field has the keyboard. False for what it
    /// leaves to the pane: Alt, and the chords with Ctrl it has no use for.
    fn bar_key(&self, vk: VIRTUAL_KEY, mods: Mods) -> bool {
        if mods.alt {
            return false;
        }
        let mut slot = self.address.borrow_mut();
        let Some(f) = slot.as_mut() else {
            return false;
        };
        let (word, extend) = (mods.ctrl, mods.shift);
        let mut copy = None;
        match vk {
            VK_RETURN | VK_ESCAPE => {
                drop(slot);
                self.drop_char();
                self.end_edit(vk == VK_RETURN);
                return true;
            }
            VK_LEFT => f.left(word, extend),
            VK_RIGHT => f.right(word, extend),
            VK_HOME => f.home(true, extend),
            VK_END => f.end(true, extend),
            VK_BACK => f.backspace(word),
            VK_DELETE => f.delete(word),
            _ if mods.ctrl && !mods.shift => match vk.0 as u8 {
                b'A' | b'L' => f.select_all(),
                b'C' => copy = Some(f.selected().to_string()),
                b'X' => copy = Some(f.cut()),
                b'V' => {
                    if let Some(t) = clipboard::get_text() {
                        f.insert(&t);
                    }
                }
                _ => return false,
            },
            _ if mods.ctrl => return false,
            // A character comes as WM_CHAR.
            _ => return true,
        }
        drop(slot);
        if let Some(t) = copy.filter(|t| !t.is_empty()) {
            clipboard::set_text(&t);
        }
        self.drop_char();
        self.invalidate();
        true
    }

    /// A character typed into the address field.
    fn bar_char(&self, unit: u16) {
        let Some(c) = char::from_u32(unit as u32).filter(|c| !c.is_control()) else {
            return;
        };
        if let Some(f) = self.address.borrow_mut().as_mut() {
            f.insert(c.encode_utf8(&mut [0; 4]));
        }
        self.invalidate();
    }

    fn on_close(&self, lparam: LPARAM) -> bool {
        self.on_button(lparam, self.buttons().close)
    }

    fn on_zoom(&self, lparam: LPARAM) -> bool {
        self.on_button(lparam, self.buttons().zoom)
    }

    fn on_stash(&self, lparam: LPARAM) -> bool {
        self.on_button(lparam, self.buttons().stash)
    }

    /// The cell under a client point, and which half of it.
    fn cell_at(&self, lparam: LPARAM) -> (Point, Side) {
        let (col, row, side) = self.screen_cell(lparam);
        let offset = self
            .console
            .screen
            .lock()
            .map(|s| s.term.grid().display_offset())
            .unwrap_or(0);
        (
            Point::new(Line(row as i32 - offset as i32), Column(col)),
            side,
        )
    }

    /// The column and row on screen under a client point, whatever the
    /// scrollback shows, and which half of the cell.
    fn screen_cell(&self, lparam: LPARAM) -> (usize, usize, Side) {
        let (x, y) = self.dip(lparam);
        let cell = self.cell();
        let size = self.console.size();
        let (ox, oy) = glyphs::grid_origin();
        let colf = ((x - ox) / cell.w).max(0.0);
        let col = (colf as usize).min(size.cols as usize - 1);
        let side = if colf.fract() < 0.5 && (colf as usize) < size.cols as usize {
            Side::Left
        } else {
            Side::Right
        };
        let row = (((y - oy) / cell.h).max(0.0) as usize).min(size.rows as usize - 1);
        (col, row, side)
    }

    /// The link under a client point while Ctrl is held: the cells it
    /// takes and what it opens. A link a program made wins over the text.
    fn link_at(&self, lparam: LPARAM) -> Option<(Vec<Point>, Target)> {
        if !Self::mods().ctrl || self.in_header(lparam) {
            return None;
        }
        let (point, _) = self.cell_at(lparam);
        let probe = |p: &Path| std::fs::metadata(p).ok().map(|m| m.is_dir());
        let (text, points, at, made) = {
            let s = self.console.screen.lock().ok()?;
            let (text, points, at) = links::line_at(&s.term, point)?;
            let grid = s.term.grid();
            let made = grid[points[at]].hyperlink().map(|link| {
                let same = |i: &usize| grid[points[*i]].hyperlink().as_ref() == Some(&link);
                let start = (0..=at).rev().take_while(same).last().unwrap_or(at);
                let end = (at..points.len()).take_while(same).last().unwrap_or(at) + 1;
                (start, end, link.uri().to_string())
            });
            (text, points, at, made)
        };
        if let Some((start, end, uri)) = made {
            let target = links::from_uri(&uri, &probe)?;
            return Some((points[start..end].to_vec(), target));
        }
        let home = std::env::var_os("USERPROFILE").map(PathBuf::from);
        let base = links::Base {
            cwd: self.console.cwd.as_deref(),
            home: home.as_deref(),
        };
        let found = links::find(&text, at, &base, &probe)?;
        Some((points[found.start..found.end].to_vec(), found.target))
    }

    /// Underlines the link under a client point, or nothing when there is
    /// none or Ctrl is up.
    fn hover(&self, lparam: Option<LPARAM>) {
        let cells = lparam.and_then(|at| self.link_at(at)).map(|l| l.0);
        if cells.is_some() {
            self.track();
        }
        if *self.link.borrow() != cells {
            *self.link.borrow_mut() = cells;
            self.invalidate();
        }
    }

    /// Asks for WM_MOUSELEAVE, once until it comes.
    fn track(&self) {
        if self.tracking.replace(true) {
            return;
        }
        let mut track = TRACKMOUSEEVENT {
            cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
            dwFlags: TME_LEAVE,
            hwndTrack: self.hwnd,
            dwHoverTime: 0,
        };
        unsafe {
            let _ = TrackMouseEvent(&mut track);
        }
    }

    /// What the pane shows, for what its cross says.
    fn shows(&self) -> tip::Shows {
        if self.console.web.is_some() {
            tip::Shows::Browser
        } else if self.console.is_view() {
            tip::Shows::File
        } else if self.console.shell {
            tip::Shows::Shell
        } else {
            tip::Shows::Agent
        }
    }

    /// The line for what in the header or the tab strip is under a client
    /// point, in the order a press there is taken.
    fn tip_at(&self, lparam: LPARAM) -> Option<&'static str> {
        if let Some(hit) = self.bar_at(lparam) {
            let docked = self.console.web.as_deref().and_then(web::dock);
            return tip::bar(hit, docked.map(|d| d.side));
        }
        if let Some(hit) = self.tab_at(lparam) {
            return Some(tip::tab(hit));
        }
        let key = if self.on_close(lparam) {
            tip::Key::Close
        } else if self.on_stash(lparam) {
            tip::Key::Stash
        } else if self.on_zoom(lparam) {
            tip::Key::Zoom
        } else {
            return None;
        };
        Some(tip::pane(key, self.shows(), self.zoom.get() == Some(true)))
    }

    /// The mouse's client point, when it is over the pane.
    fn mouse_here(&self) -> Option<LPARAM> {
        let mut p = POINT::default();
        let mut r = RECT::default();
        unsafe {
            let _ = GetCursorPos(&mut p);
            let _ = ScreenToClient(self.hwnd, &mut p);
            let _ = GetClientRect(self.hwnd, &mut r);
        }
        let inside = p.x >= 0 && p.y >= 0 && p.x < r.right && p.y < r.bottom;
        inside.then_some(LPARAM(((p.y as u16 as isize) << 16) | p.x as u16 as isize))
    }

    /// Opens a link. VS Code gets the project, so the file opens in the
    /// window that has it; a view's folder is no project. A web address is
    /// the app's to route, to the project's browser when one is open.
    fn open_link(&self, target: &Target) {
        if let Target::Web(url) = target {
            app::push(Input::Link(url.clone()));
            return;
        }
        let root = self
            .console
            .cwd
            .as_deref()
            .filter(|_| !self.console.is_view());
        watch::open_link(target, root);
    }

    fn start_selection(&self, lparam: LPARAM, ty: SelectionType) {
        let (point, side) = self.cell_at(lparam);
        if let Ok(mut s) = self.console.screen.lock() {
            s.term.selection = Some(Selection::new(ty, point, side));
        }
        self.selecting.set(true);
        unsafe {
            SetCapture(self.hwnd);
        }
        self.invalidate();
    }

    fn encoding(mode: TermMode) -> MouseEncoding {
        if mode.contains(TermMode::SGR_MOUSE) {
            MouseEncoding::Sgr
        } else if mode.contains(TermMode::UTF8_MOUSE) {
            MouseEncoding::Utf8
        } else {
            MouseEncoding::X10
        }
    }

    /// The mode, when clicks go to the program rather than to selecting.
    /// Shift keeps them here, as in xterm, so text can still be selected
    /// and copied from a program that took the mouse.
    fn mouse_mode(&self) -> Option<TermMode> {
        let mode = self.mode();
        (mode.intersects(TermMode::MOUSE_MODE) && !Self::mods().shift && !self.console.is_view())
            .then_some(mode)
    }

    fn report(&self, event: MouseEvent, lparam: LPARAM, mode: TermMode) {
        let (col, row, _) = self.screen_cell(lparam);
        self.report_at(event, (col, row), mode);
    }

    fn report_at(&self, event: MouseEvent, (col, row): (usize, usize), mode: TermMode) {
        self.moved_to.set(Some((col, row)));
        let bytes = keys::mouse_bytes(event, col, row, Self::mods(), Self::encoding(mode));
        self.console.write(bytes);
    }

    /// A button went down over the grid. Returns whether it went to the
    /// program.
    fn report_press(&self, button: Button, lparam: LPARAM) -> bool {
        let Some(mode) = self.mouse_mode() else {
            return false;
        };
        if let Some(held) = self.reported.replace(Some(button)) {
            self.report(MouseEvent::Release(held), lparam, mode);
        }
        self.report(MouseEvent::Press(button), lparam, mode);
        unsafe {
            SetCapture(self.hwnd);
        }
        true
    }

    /// A button came up. Returns whether its press went to the program.
    fn report_release(&self, button: Button, lparam: LPARAM) -> bool {
        if self.reported.get() != Some(button) {
            return false;
        }
        self.reported.set(None);
        // The program asked for the press, so it hears the release even if
        // it has let go of the mouse in between.
        self.report(MouseEvent::Release(button), lparam, self.mode());
        unsafe {
            let _ = ReleaseCapture();
        }
        true
    }

    fn report_move(&self, lparam: LPARAM) {
        let mode = self.mode();
        let held = self.reported.get();
        let wanted = keys::reports_move(
            mode.contains(TermMode::MOUSE_MOTION),
            mode.contains(TermMode::MOUSE_DRAG),
            held.is_some(),
        );
        if !wanted || (held.is_none() && self.mouse_mode().is_none()) || self.in_header(lparam) {
            return;
        }
        let (col, row, _) = self.screen_cell(lparam);
        if self.moved_to.get() != Some((col, row)) {
            self.report(MouseEvent::Move(held), lparam, mode);
        }
    }

    fn on_wheel(&self, wparam: WPARAM, lparam: LPARAM) {
        let raw = ((wparam.0 >> 16) & 0xffff) as i16 as i32;
        if self.console.is_view() && Self::mods().shift {
            // Down goes right, as Shift and the wheel do in an editor.
            self.sideways(-raw);
            return;
        }
        let delta = raw + self.wheel.get();
        let notches = delta / 120;
        self.wheel.set(delta % 120);
        if notches == 0 {
            return;
        }
        // Ctrl and the wheel sizes the font, as in a browser.
        if Self::mods().ctrl {
            let step = if notches > 0 {
                FontStep::Bigger
            } else {
                FontStep::Smaller
            };
            for _ in 0..notches.unsigned_abs() {
                app::push(Input::Font(step));
            }
            return;
        }
        let lines = notches * WHEEL_LINES;
        let mode = self.mode();
        if mode.intersects(TermMode::MOUSE_MODE) {
            // A program that asked for the mouse scrolls itself, by its own
            // measure: Claude Code's full screen mode does. Arrow keys there
            // would walk its prompt history instead.
            let mut p = POINT {
                x: (lparam.0 & 0xffff) as i16 as i32,
                y: ((lparam.0 >> 16) & 0xffff) as i16 as i32,
            };
            unsafe {
                let _ = ScreenToClient(self.hwnd, &mut p);
            }
            let at = LPARAM(((p.y as u16 as isize) << 16) | p.x as u16 as isize);
            let (col, row, _) = self.screen_cell(at);
            let button = if lines > 0 {
                Button::WheelUp
            } else {
                Button::WheelDown
            };
            let event = MouseEvent::Press(button);
            let one = keys::mouse_bytes(event, col, row, Self::mods(), Self::encoding(mode));
            self.console
                .write(one.repeat(notches.unsigned_abs() as usize));
        } else if mode.contains(TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL) {
            // Full screen programs scroll themselves. Give them arrow keys.
            let key = if lines > 0 { Key::Up } else { Key::Down };
            let one = keys::key_bytes(key, Mods::NONE, mode.contains(TermMode::APP_CURSOR));
            self.console
                .write(one.repeat(lines.unsigned_abs() as usize));
        } else if let Ok(mut s) = self.console.screen.lock() {
            s.term.scroll_display(Scroll::Delta(lines));
        }
        self.invalidate();
    }

    /// Scrolls a file view sideways by wheel movement, right when positive,
    /// keeping what is short of a column for the next.
    fn sideways(&self, delta: i32) {
        let delta = delta + self.hwheel.get();
        self.hwheel.set(delta % WHEEL_COL);
        let cols = delta / WHEEL_COL;
        if cols != 0 && self.console.scroll_sideways(cols as isize) {
            self.invalidate();
        }
    }

    /// Tells the console the pane gained or lost the keyboard, for programs
    /// that asked to hear it.
    fn set_focus(&self, focused: bool) {
        self.focused.set(focused);
        self.caret_since.set(Instant::now());
        let mode = self.mode();
        if let Ok(mut s) = self.console.screen.lock() {
            s.term.is_focused = focused;
        }
        if mode.contains(TermMode::FOCUS_IN_OUT) {
            self.console.write(if focused {
                &b"\x1b[I"[..]
            } else {
                &b"\x1b[O"[..]
            });
        }
        self.invalidate();
    }

    fn tell_stage(&self, msg: u32) {
        unsafe {
            if let Ok(parent) = GetParent(self.hwnd) {
                SendMessageW(parent, msg, Some(WPARAM(self.serial())), None);
            }
        }
    }

    fn handle(&self, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
        if matches!(
            msg,
            WM_LBUTTONDOWN | WM_LBUTTONDBLCLK | WM_MBUTTONDOWN | WM_RBUTTONDOWN
        ) {
            tip::press(self.hwnd);
        }
        match msg {
            WM_PAINT => {
                self.paint();
                unsafe {
                    let _ = ValidateRect(Some(self.hwnd), None);
                }
                Some(LRESULT(0))
            }
            WM_ERASEBKGND => Some(LRESULT(1)),
            WM_SIZE => {
                let w = (lparam.0 & 0xffff) as u32;
                let h = ((lparam.0 >> 16) & 0xffff) as u32;
                // A resized target keeps none of its pixels, so a pane back
                // at the size it was drawn at (a stage minimized and
                // restored) must draw again, not show the frame it lost.
                if let Some(t) = self.target.borrow().as_ref() {
                    let _ = t.resize(w, h);
                    *self.drawn.borrow_mut() = None;
                }
                if w > 0 && h > 0 && !self.held.get() {
                    self.fit_grid();
                }
                self.invalidate();
                Some(LRESULT(0))
            }
            WM_DPICHANGED_AFTERPARENT => {
                self.fit_grid();
                self.invalidate();
                Some(LRESULT(0))
            }
            WM_TIMER if wparam.0 == ANIM_TIMER => {
                crate::vsync::took(self.hwnd, ANIM_TIMER);
                self.invalidate();
                Some(LRESULT(0))
            }
            WM_TIMER if wparam.0 == BLINK_TIMER => {
                unsafe {
                    let _ = KillTimer(Some(self.hwnd), BLINK_TIMER);
                }
                self.invalidate();
                Some(LRESULT(0))
            }
            WM_TIMER if wparam.0 == RAIN_TIMER => {
                if theme::rains() {
                    if unsafe { IsWindowVisible(self.hwnd) }.as_bool() {
                        self.invalidate();
                    }
                } else {
                    unsafe {
                        let _ = KillTimer(Some(self.hwnd), RAIN_TIMER);
                    }
                    self.raining.set(false);
                    self.invalidate();
                }
                Some(LRESULT(0))
            }
            WM_TIMER if wparam.0 == SYNC_TIMER => {
                unsafe {
                    let _ = KillTimer(Some(self.hwnd), SYNC_TIMER);
                }
                self.invalidate();
                Some(LRESULT(0))
            }
            WM_SETFOCUS => {
                self.set_focus(true);
                self.tell_stage(WM_PANE_FOCUS);
                Some(LRESULT(0))
            }
            WM_KILLFOCUS => {
                if self.address.borrow().is_some() && self.page_took(HWND(wparam.0 as *mut _)) {
                    // A page takes the keyboard as it first loads, while
                    // the address for it may be being typed.
                    unsafe {
                        let _ = PostMessageW(
                            Some(self.hwnd),
                            web::WM_WEB_EDIT,
                            WPARAM(RETAKE),
                            LPARAM(0),
                        );
                    }
                } else if self.address.take().is_some() {
                    // Anywhere else, a click in the page, a menu, another
                    // window, leaves the address as it was, as a browser
                    // does.
                    self.invalidate();
                }
                self.set_focus(false);
                Some(LRESULT(0))
            }
            WM_IME_STARTCOMPOSITION => {
                // A composition can start before the first paint with the
                // keyboard, or after a font change moved nothing on screen.
                if let Some(rect) = self.ime_at.get() {
                    self.place_ime(rect);
                }
                None
            }
            WM_SETCURSOR if (lparam.0 & 0xffff) as u32 == HTCLIENT => {
                let mut p = POINT::default();
                unsafe {
                    let _ = GetCursorPos(&mut p);
                    let _ = ScreenToClient(self.hwnd, &mut p);
                }
                let at = LPARAM(((p.y as u16 as isize) << 16) | p.x as u16 as isize);
                if self.in_header(at) || self.in_tabs(at) {
                    let cursor = if self.bar_at(at) == Some(BarHit::Field) {
                        IDC_IBEAM
                    } else {
                        IDC_ARROW
                    };
                    unsafe {
                        SetCursor(LoadCursorW(None, cursor).ok());
                    }
                    Some(LRESULT(1))
                } else if let Some(grip) = self.resizing.get().map(|r| r.grip).or(self.grip_at(at))
                {
                    let cursor = match grip {
                        Grip::Right => IDC_SIZEWE,
                        Grip::Bottom => IDC_SIZENS,
                        Grip::Corner => IDC_SIZENWSE,
                    };
                    unsafe {
                        SetCursor(LoadCursorW(None, cursor).ok());
                    }
                    Some(LRESULT(1))
                } else if self.link.borrow().is_some() {
                    unsafe {
                        SetCursor(LoadCursorW(None, IDC_HAND).ok());
                    }
                    Some(LRESULT(1))
                } else {
                    None
                }
            }
            WM_CHAR if self.address.borrow().is_some() => {
                self.bar_char(wparam.0 as u16);
                Some(LRESULT(0))
            }
            WM_CHAR => {
                // AltGr is Ctrl+Alt, so Alt here is never Meta. Meta comes as
                // WM_SYSCHAR.
                let mods = Mods {
                    alt: false,
                    ..Self::mods()
                };
                self.on_char(wparam.0 as u16, mods);
                Some(LRESULT(0))
            }
            WM_SYSCHAR => {
                // Alt+Space is the window menu.
                if wparam.0 == ' ' as usize {
                    return None;
                }
                let mods = Mods {
                    alt: true,
                    ..Self::mods()
                };
                self.on_char(wparam.0 as u16, mods);
                Some(LRESULT(0))
            }
            WM_KEYDOWN | WM_SYSKEYDOWN => {
                // Bit 30 is set when the key was already down.
                let repeat = lparam.0 & (1 << 30) != 0;
                if wparam.0 == VK_CONTROL.0 as usize && !repeat {
                    self.hover(self.mouse_here());
                }
                if self.web_tab_key(VIRTUAL_KEY(wparam.0 as u16))
                    || self.bar_key(VIRTUAL_KEY(wparam.0 as u16), Self::mods())
                    || self.on_key(wparam.0 as u16, Self::mods(), repeat)
                {
                    Some(LRESULT(0))
                } else {
                    None
                }
            }
            WM_KEYUP | WM_SYSKEYUP => {
                if wparam.0 == VK_CONTROL.0 as usize {
                    self.hover(None);
                }
                self.on_key_up(wparam.0 as u16, Self::mods());
                None
            }
            WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => {
                if self.on_bar(lparam, msg == WM_LBUTTONDBLCLK) {
                    return Some(LRESULT(0));
                }
                if self.on_tabs(lparam, false) {
                    return Some(LRESULT(0));
                }
                if let (Some(grip), Some(key)) = (self.grip_at(lparam), &self.console.web) {
                    if let (Some((fit, _)), Some(size)) = (self.page_fit(), web::size(key)) {
                        self.resizing.set(Some(Resize {
                            grip,
                            at: self.dip(lparam),
                            from: size,
                            zoom: fit.zoom,
                        }));
                        unsafe {
                            SetCapture(self.hwnd);
                        }
                        self.invalidate();
                        return Some(LRESULT(0));
                    }
                }
                self.focus();
                if self.on_close(lparam) {
                    self.close();
                } else if self.on_stash(lparam) {
                    app::push(Input::PaneAsk(self.session().to_string(), PaneAsk::Stash));
                } else if self.on_zoom(lparam) {
                    self.tell_stage(WM_PANE_ZOOM);
                } else if self.in_header(lparam) {
                    // A double click zooms, as on a title bar.
                    if msg == WM_LBUTTONDBLCLK && self.zoom.get().is_some() {
                        self.tell_stage(WM_PANE_ZOOM);
                    } else {
                        self.tell_stage(WM_PANE_GRAB);
                    }
                } else if let Some((_, target)) = self.link_at(lparam) {
                    self.open_link(&target);
                } else if self.report_press(Button::Left, lparam) {
                } else if msg == WM_LBUTTONDOWN {
                    self.start_selection(lparam, SelectionType::Simple);
                } else {
                    self.start_selection(lparam, SelectionType::Semantic);
                }
                Some(LRESULT(0))
            }
            WM_MOUSELEAVE => {
                self.tracking.set(false);
                tip::away(self.hwnd);
                self.hover(None);
                Some(LRESULT(0))
            }
            WM_MOUSEMOVE if self.resizing.get().is_some() => {
                self.drag_grip(lparam);
                Some(LRESULT(0))
            }
            WM_MOUSEMOVE => {
                let line = (!self.selecting.get())
                    .then(|| self.tip_at(lparam))
                    .flatten();
                if line.is_some() {
                    self.track();
                }
                tip::over(&self.shared, self.hwnd, line);
                if !self.selecting.get() {
                    self.hover(Some(lparam));
                }
                if self.selecting.get() {
                    let (point, side) = self.cell_at(lparam);
                    if let Ok(mut s) = self.console.screen.lock() {
                        if let Some(sel) = s.term.selection.as_mut() {
                            sel.update(point, side);
                        }
                    }
                    self.invalidate();
                } else {
                    self.report_move(lparam);
                }
                Some(LRESULT(0))
            }
            WM_LBUTTONUP if self.resizing.get().is_some() => {
                self.drag_grip(lparam);
                self.resizing.set(None);
                unsafe {
                    let _ = ReleaseCapture();
                }
                self.invalidate();
                Some(LRESULT(0))
            }
            WM_LBUTTONUP => {
                if self.report_release(Button::Left, lparam) {
                } else if self.selecting.replace(false) {
                    unsafe {
                        let _ = ReleaseCapture();
                    }
                    // Copy on select, like Windows Terminal can. A click
                    // without a drag selects nothing and clears.
                    let empty = self.console.screen.lock().is_ok_and(|mut s| {
                        let empty = s.term.selection.as_ref().is_none_or(|sel| sel.is_empty());
                        if empty {
                            s.term.selection = None;
                        }
                        empty
                    });
                    let text = (!empty).then(|| self.console.selection_text()).flatten();
                    if let Some(t) = text.filter(|t| !t.is_empty()) {
                        clipboard::set_text(&t);
                    }
                    self.invalidate();
                }
                Some(LRESULT(0))
            }
            WM_MBUTTONDOWN if self.on_tabs(lparam, true) => Some(LRESULT(0)),
            WM_RBUTTONDOWN | WM_MBUTTONDOWN if !self.in_header(lparam) => {
                let button = if msg == WM_RBUTTONDOWN {
                    Button::Right
                } else {
                    Button::Middle
                };
                if self.report_press(button, lparam) {
                    self.focus();
                }
                Some(LRESULT(0))
            }
            WM_MBUTTONUP => {
                self.report_release(Button::Middle, lparam);
                Some(LRESULT(0))
            }
            WM_CAPTURECHANGED => {
                if self.resizing.take().is_some() {
                    self.invalidate();
                }
                // Capture taken away mid press, by a menu or Alt+Tab: the
                // program must not think the button is still down.
                if let Some(held) = self.reported.take() {
                    let at = self.moved_to.get().unwrap_or_default();
                    self.report_at(MouseEvent::Release(held), at, self.mode());
                }
                None
            }
            WM_RBUTTONUP if self.console.web.is_some() => {
                if let Some(key) = &self.console.web {
                    app::push(Input::WebAsk(key.clone(), app::WebAsk::Menu));
                }
                Some(LRESULT(0))
            }
            WM_RBUTTONUP => {
                // The console convention, unless the program took the press:
                // right click copies a selection, otherwise pastes.
                if self.report_release(Button::Right, lparam) {
                } else if (!self.has_selection() || !self.copy()) && !self.console.is_view() {
                    self.paste();
                }
                Some(LRESULT(0))
            }
            WM_MOUSEWHEEL => {
                self.on_wheel(wparam, lparam);
                Some(LRESULT(0))
            }
            WM_MOUSEHWHEEL if self.console.is_view() => {
                self.sideways(((wparam.0 >> 16) & 0xffff) as i16 as i32);
                Some(LRESULT(0))
            }
            WM_DROPFILES => {
                self.on_drop(HDROP(wparam.0 as *mut c_void));
                Some(LRESULT(0))
            }
            web::WM_WEB_CHANGED => {
                // The page's size may be what changed.
                self.fit_grid();
                self.invalidate();
                Some(LRESULT(0))
            }
            web::WM_WEB_EDIT if wparam.0 == RETAKE => {
                if self.address.borrow().is_some() {
                    unsafe {
                        let _ = SetFocus(Some(self.hwnd));
                    }
                }
                Some(LRESULT(0))
            }
            web::WM_WEB_EDIT => {
                self.edit_address();
                Some(LRESULT(0))
            }
            // Before the page's own window goes with this one.
            WM_DESTROY => {
                if let Some(key) = &self.console.web {
                    web::detach(key, self.hwnd);
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
    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const Pane;
    if ptr.is_null() {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    if msg == WM_NCDESTROY {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    // The stage owns the Box and destroys the window before dropping it.
    let pane = &*ptr;
    match pane.handle(msg, wparam, lparam) {
        Some(r) => r,
        None => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
