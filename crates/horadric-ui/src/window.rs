//! The cluster window: a frameless, rounded Win32 window that never takes
//! focus. It stacks like any other window: a click brings it forward and an
//! editor can cover it.
//!
//! The rules that make it feel like part of Windows rather than an app:
//! `WS_EX_NOACTIVATE` so clicking it does not steal focus from the editor,
//! `WS_EX_TOOLWINDOW` so it stays out of alt-tab and the taskbar, DWM
//! rounded corners so it matches Windows 11, and every move or resize done
//! with `SWP_NOACTIVATE`. Dragging is handled by hand for the same reason:
//! the system move loop would activate the window.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use horadric_core::tasks::{Mark, Mode};
use horadric_core::{Defaults, Registry, Session, Usage};
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
    GetKeyState, ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT,
    VK_CONTROL,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetCursorPos, GetWindowLongPtrW, GetWindowRect,
    KillTimer, LoadCursorW, PostMessageW, RegisterClassW, SetTimer, SetWindowLongPtrW,
    SetWindowPos, ShowWindow, CREATESTRUCTW, CS_HREDRAW, CS_VREDRAW, GWLP_USERDATA, HWND_NOTOPMOST,
    HWND_TOPMOST, IDC_ARROW, MA_NOACTIVATE, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER,
    SW_SHOWNOACTIVATE, WM_APP, WM_CAPTURECHANGED, WM_DPICHANGED, WM_ERASEBKGND, WM_LBUTTONDOWN,
    WM_LBUTTONUP, WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_NCCREATE, WM_NCDESTROY,
    WM_PAINT, WM_RBUTTONUP, WM_SIZE, WM_TIMER, WNDCLASSW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    WS_POPUP,
};

use crate::anim::{self, TileIn};
use crate::app::{self, Input};
use crate::appear;
use crate::backdrop;
use crate::board::{self, Board, RowState};
use crate::cube;
use crate::files::{Change, Expansion, Row, Tree};
use crate::glyphs::Font;
use crate::layout::{self, ClusterLayout, Hit, Metrics};
use crate::motion;
pub use crate::project::{folder_key, project_key, project_name};
use crate::render::{FilesScene, Flight, Gpu, Scene, Target, TaskRow, TasksScene};
use crate::theme;
use crate::watch::{self, Slot, Watcher};

pub(crate) const CLASS: PCWSTR = w!("HoradricCluster");
const DRAG_THRESHOLD: i32 = 4;
/// The watcher left a fresh file tree in the slot.
const WM_CLUSTER_FILES: u32 = WM_APP + 20;
/// A finished task's row has folded away: the tasks tile lays out again.
const WM_CLUSTER_REFIT: u32 = WM_APP + 21;
/// The windows crate files this under `Win32_UI_Controls`, a large feature
/// to turn on for one number.
const WM_MOUSELEAVE: u32 = 0x02A3;
/// Rows the files tile scrolls per notch of the wheel.
const WHEEL_ROWS: i32 = 3;
/// Asks for the next frame while something moves.
const ANIM_TIMER: usize = 7;

/// What the windows share with the app: GPU objects, the terminal font,
/// the session store, the order of each project's sessions, which are on
/// the stage and which have a browser open, the account's usage and the
/// defaults for new sessions.
pub struct Shared {
    pub gpu: Gpu,
    pub font: Font,
    pub metrics: Metrics,
    pub registry: Arc<Mutex<Registry>>,
    /// Each project's sessions in the order its tiles and the stage's grid
    /// both show them, by project key. Paused ones keep their place.
    pub orders: RefCell<HashMap<String, Vec<String>>>,
    pub staged: RefCell<HashSet<String>>,
    /// The session whose pane has the keyboard, or last had it.
    pub active: RefCell<Option<String>>,
    pub browsing: RefCell<HashSet<String>>,
    /// Written by the feeder thread as status lines arrive.
    pub usage: Arc<Mutex<Option<Usage>>>,
    pub defaults: RefCell<Defaults>,
    /// Each project's task list as last read, by project key.
    pub boards: RefCell<HashMap<String, Board>>,
    /// The cube's window while there is one, for what is carried over it.
    pub cube: Cell<Option<HWND>>,
}

/// One project cluster on screen.
pub struct Cluster {
    pub hwnd: HWND,
    /// Project key: sessions whose project key matches are shown here.
    pub key: String,
    pub name: String,
    pub collapsed: bool,
    /// The project folder, as a session spelled it.
    dir: Option<PathBuf>,
    files: RefCell<Files>,
    tasks: RefCell<Tasks>,
    watcher: Option<Watcher>,
    shared: Rc<Shared>,
    target: RefCell<Option<Target>>,
    drag: RefCell<Option<Drag>>,
    lift: RefCell<Option<Lift>>,
    layout: RefCell<ClusterLayout>,
    /// What the cursor is over, for the buttons to light up.
    hot: Cell<Hit>,
    /// What the left button went down on, until it comes up or a drag
    /// starts.
    pressed: Cell<Option<Hit>>,
    /// A WM_MOUSELEAVE has been asked for and not yet sent.
    tracking: Cell<bool>,
    /// Where each tile has got to on its way somewhere.
    tiles: RefCell<anim::Tiles>,
    /// Task rows done a moment ago, struck through and folding away.
    finishing: RefCell<anim::Finishing>,
    /// A finished row has folded away since the last layout.
    refit_due: Cell<bool>,
    /// Lights flying from a task's row to the session that took it.
    handoffs: RefCell<anim::Handoffs>,
    /// Tiles whose sessions have gone, fading out where they were.
    leaving: RefCell<anim::Leaving<(layout::Rect, Session)>>,
    /// The frame interval the animation timer runs at, if it runs.
    frames: Cell<Option<Duration>>,
    /// Something besides the moving light changed, so the kept layer of
    /// what holds still has to be drawn again.
    dirty: Cell<bool>,
}

/// The files tile. It only shows once git has answered with a tree.
#[derive(Default)]
struct Files {
    slot: Slot,
    tree: Option<Tree>,
    view: Expansion,
    /// Every row, open folders expanded, rebuilt when the tree or the view
    /// changes rather than on every paint.
    rows: Vec<Row>,
    /// Rows above the top of the tile.
    scroll: usize,
    /// Folded by a click on its header.
    collapsed: bool,
    /// How tall its column lets it be below its header, in DIPs. None when
    /// the column had no room for it, which folds it too.
    room: Option<f32>,
    /// When a click last opened it, by [`OPENED`]. The oldest folds first
    /// when a column runs out of room.
    opened: u64,
}

/// Counts clicks that open a files tile, so the one opened last is the
/// last its column folds.
static OPENED: AtomicU64 = AtomicU64::new(1);

impl Files {
    fn rebuild(&mut self) {
        self.rows = self
            .tree
            .as_ref()
            .map(|t| t.rows(&self.view))
            .unwrap_or_default();
    }

    /// How tall the layout should make the tile below its header, none for
    /// no tile.
    fn wanted(&self) -> Option<f32> {
        self.tree.as_ref().map(|_| {
            if self.collapsed {
                0.0
            } else {
                self.room.unwrap_or(0.0)
            }
        })
    }

    /// Showing only its header, by a click or for want of room.
    fn folded(&self) -> bool {
        self.collapsed || self.room.is_none()
    }
}

/// The tasks tile. Every project with a folder has one, empty or not.
#[derive(Default)]
struct Tasks {
    collapsed: bool,
    /// Rows above the top of the tile.
    scroll: usize,
}

/// A row of the tasks tile as the cluster reads it: which item, and how.
struct Item {
    line: usize,
    title: String,
    state: RowState,
    /// Done a moment ago and on its way out of the tile, this far.
    finish: Option<f32>,
    /// The session that has it.
    holder: Option<String>,
}

struct Drag {
    start_cursor: POINT,
    start_window: POINT,
    moved: bool,
}

/// A tile pressed, and once past the drag threshold carried, to a new
/// place among the others.
struct Lift {
    id: String,
    /// Where the button went down and the tile's top then, in DIPs.
    start_y: f32,
    start_top: f32,
    /// Its top now, following the cursor.
    top: f32,
    /// The place it takes if let go now.
    slot: usize,
    moved: bool,
}

pub fn register_class() -> Result<()> {
    unsafe {
        let instance = GetModuleHandleW(None)?;
        let wc = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            lpszClassName: CLASS,
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            ..Default::default()
        };
        // Zero means failure. Registering twice is the only realistic cause
        // and is harmless, so no error is surfaced.
        RegisterClassW(&wc);
        Ok(())
    }
}

impl Cluster {
    /// Creates the window hidden at `(x, y)` in physical pixels, then shows it
    /// without activating it.
    pub fn create(
        shared: Rc<Shared>,
        key: String,
        name: String,
        dir: Option<PathBuf>,
        x: i32,
        y: i32,
    ) -> Result<Box<Self>> {
        let n = shared
            .registry
            .lock()
            .map(|r| r.all().filter(|s| project_key(s) == key).count())
            .unwrap_or(0);
        let initial = layout::cluster(&shared.metrics, n, false, None, None);

        let mut cluster = Box::new(Cluster {
            hwnd: HWND::default(),
            key,
            name,
            collapsed: false,
            dir,
            files: RefCell::new(Files::default()),
            tasks: RefCell::new(Tasks::default()),
            watcher: None,
            shared,
            target: RefCell::new(None),
            drag: RefCell::new(None),
            lift: RefCell::new(None),
            layout: RefCell::new(initial),
            hot: Cell::new(Hit::Nothing),
            pressed: Cell::new(None),
            tracking: Cell::new(false),
            tiles: RefCell::new(anim::Tiles::default()),
            leaving: RefCell::new(anim::Leaving::default()),
            finishing: RefCell::new(anim::Finishing::default()),
            handoffs: RefCell::new(anim::Handoffs::default()),
            refit_due: Cell::new(false),
            frames: Cell::new(None),
            dirty: Cell::new(true),
        });

        unsafe {
            let instance = GetModuleHandleW(None)?;
            // Size is corrected for DPI right after creation, once the
            // window knows which monitor it is on.
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
                Some(instance.into()),
                Some(&*cluster as *const Cluster as *const c_void),
            )?;
            cluster.hwnd = hwnd;
            if let Some(dir) = cluster.dir.clone() {
                let slot = cluster.files.borrow().slot.clone();
                cluster.watcher = watch::start(dir, hwnd, WM_CLUSTER_FILES, slot);
            }

            let pref: DWM_WINDOW_CORNER_PREFERENCE = DWMWCP_ROUND;
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_WINDOW_CORNER_PREFERENCE,
                &pref as *const _ as *const c_void,
                std::mem::size_of::<DWM_WINDOW_CORNER_PREFERENCE>() as u32,
            );
            // No 1px system border: the clay has its own edge.
            backdrop::border(hwnd, None);

            cluster.fit();
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        }
        Ok(cluster)
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

    /// Window size in physical pixels for the current layout.
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

    /// Brings the window above other windows without activating it. The
    /// window never activates, so nothing else would bring it forward.
    /// `HWND_TOP` is not enough: Windows keeps a background process below the
    /// foreground window. Going topmost and straight back is allowed, and
    /// leaves the window first among the normal ones.
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
        self.dirty.set(true);
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    /// The project's sessions in the order the tiles show them, a tile
    /// being carried already in the place it would take.
    fn sessions(&self) -> Vec<Session> {
        let mut v: Vec<Session> = self
            .shared
            .registry
            .lock()
            .map(|r| {
                r.all()
                    .filter(|s| project_key(s) == self.key)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        if let Some(order) = self.shared.orders.borrow().get(&self.key) {
            v.sort_by_key(|s| layout::rank(order, &s.id));
        }
        if let Some(l) = self.lift.borrow().as_ref().filter(|l| l.moved) {
            if let Some(i) = v.iter().position(|s| s.id == l.id) {
                let s = v.remove(i);
                v.insert(l.slot.min(v.len()), s);
            }
        }
        v
    }

    pub fn files_collapsed(&self) -> bool {
        self.files.borrow().collapsed
    }

    pub fn set_files_collapsed(&self, collapsed: bool) {
        self.files.borrow_mut().collapsed = collapsed;
    }

    pub fn tasks_collapsed(&self) -> bool {
        self.tasks.borrow().collapsed
    }

    pub fn set_tasks_collapsed(&self, collapsed: bool) {
        self.tasks.borrow_mut().collapsed = collapsed;
    }

    /// The project's items that get a row, in list order, with how each
    /// reads. None for a project with no folder, which gets no tile.
    fn items(&self) -> Option<Vec<Item>> {
        self.dir.as_ref()?;
        let boards = self.shared.boards.borrow();
        let Some(b) = boards.get(&self.key) else {
            return Some(Vec::new());
        };
        let registry = self.shared.registry.lock().ok();
        let state = |t: &horadric_core::tasks::Task| match &registry {
            Some(r) => board::state_in(t, r),
            None => board::row_state(t, None),
        };
        let mut items: Vec<Item> = b
            .shown()
            .into_iter()
            .map(|i| {
                let t = &b.tasks[i];
                Item {
                    line: t.line,
                    title: t.title.clone(),
                    state: state(t),
                    finish: None,
                    holder: t.holder.clone(),
                }
            })
            .collect();
        let done = |title: &str| {
            b.tasks
                .iter()
                .find(|t| t.title == title && t.mark == Mark::Done)
        };
        let titles = items.iter().map(|i| i.title.clone()).collect();
        let (going, ended) = self
            .finishing
            .borrow_mut()
            .step(Instant::now(), titles, |t| done(t).is_some());
        if ended {
            self.refit_due.set(true);
        }
        for (at, title, p) in going {
            let line = done(&title).map_or(usize::MAX, |t| t.line);
            let item = Item {
                line,
                title,
                state: RowState::Open,
                finish: Some(p),
                holder: None,
            };
            items.insert(at.min(items.len()), item);
        }
        Some(items)
    }

    /// For each row the tasks tile shows, whether it has an approve button,
    /// none for no tile. Folded, it shows none.
    fn task_rows(&self, items: Option<&[Item]>) -> Option<Vec<bool>> {
        let items = items?;
        let t = self.tasks.borrow();
        if t.collapsed {
            return Some(Vec::new());
        }
        let shown = self.shared.metrics.task_rows;
        Some(
            items
                .iter()
                .skip(t.scroll)
                .take(shown)
                .map(|i| i.state == RowState::Review)
                .collect(),
        )
    }

    /// The item on the `i`th row showing, counted from the top.
    fn item_at(&self, i: usize) -> Option<Item> {
        let scroll = self.tasks.borrow().scroll;
        self.items()?.into_iter().nth(scroll + i)
    }

    /// The window's height in physical pixels with the files tile folded:
    /// what it takes of its column before the files tiles share the rest.
    pub fn fixed_px(&self) -> i32 {
        let folded = self.files.borrow().tree.as_ref().map(|_| 0.0);
        let n = self.sessions().len();
        let tasks = self.task_rows(self.items().as_deref());
        let h = layout::cluster(
            &self.shared.metrics,
            n,
            self.collapsed,
            tasks.as_deref(),
            folded,
        )
        .size
        .1;
        (h * self.scale()).round() as i32
    }

    /// The fewest physical pixels of a column the window needs to show its
    /// files tile open. Counted before git has answered for a folder in
    /// git, or a project new in a column would land where its files tile
    /// has no room a moment later.
    pub fn need_px(&self) -> i32 {
        let m = &self.shared.metrics;
        let f = self.files.borrow();
        let git = f.tree.is_some()
            || self
                .dir
                .as_deref()
                .is_some_and(|d| d.ancestors().any(|a| a.join(".git").exists()));
        let body = git.then(|| {
            if f.collapsed {
                0.0
            } else {
                layout::min_files_body(m)
            }
        });
        let tasks = self.task_rows(self.items().as_deref());
        let h = layout::cluster(
            m,
            self.sessions().len(),
            self.collapsed,
            tasks.as_deref(),
            body,
        )
        .size
        .1;
        (h * self.scale()).round() as i32
    }

    /// Whether the files tile asks its column for room, and when it was
    /// opened, which decides who folds first when there is not enough.
    pub fn files_claim(&self) -> Option<u64> {
        let f = self.files.borrow();
        (f.tree.is_some() && !f.collapsed && !self.collapsed).then_some(f.opened)
    }

    /// Gives the files tile the room its column has for it, in physical
    /// pixels below its header, and fits the window to it.
    pub fn set_files_room(&self, px: Option<i32>) {
        let room = px.map(|p| p as f32 / self.scale());
        self.files.borrow_mut().room = room;
        self.fit();
    }

    /// Recomputes layout from the registry and resizes the window to fit.
    /// True when the size changed, so the clusters need arranging again.
    pub fn fit(&self) -> bool {
        let sessions = self.sessions();
        let n = sessions.len();
        let m = &self.shared.metrics;
        let items = self.items();
        {
            // Items can go from under the view: done, or taken out of the
            // file.
            let mut t = self.tasks.borrow_mut();
            let total = items.as_ref().map_or(0, Vec::len);
            t.scroll = t.scroll.min(total.saturating_sub(m.task_rows));
        }
        let wanted = self.files.borrow().wanted();
        let tasks = self.task_rows(items.as_deref());
        let mut l = layout::cluster(m, n, self.collapsed, tasks.as_deref(), wanted);
        let marked: Vec<bool> = {
            let browsing = self.shared.browsing.borrow();
            sessions.iter().map(|s| browsing.contains(&s.id)).collect()
        };
        let coded: Vec<bool> = sessions.iter().map(|s| s.worktree.is_some()).collect();
        layout::mark(&mut l, m, &marked, &coded);
        {
            // Rows can go away under the view: a folder closed, files
            // committed. Never scroll past the last one.
            let mut f = self.files.borrow_mut();
            let shown = l.files.as_ref().map_or(0, |fl| fl.rows.len());
            f.scroll = f.scroll.min(f.rows.len().saturating_sub(shown));
        }
        *self.layout.borrow_mut() = l;
        // The layout can move a button out from under a cursor that has
        // not moved: the plus below the tiles, once a tile is added.
        if self.tracking.get() {
            self.hover(self.cursor_hit());
        }
        // Compare with the real window, not the previous layout: a window is
        // born 10 by 10 and must grow even when its layout never changes.
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

    /// Fits, and has the app arrange the clusters when that changed a size.
    fn refit(&self) {
        if self.fit() {
            app::push(Input::Arrange);
        }
    }

    fn paint(&self) {
        let (w, h) = self.size_px();
        let dpi = self.dpi();
        let mut slot = self.target.borrow_mut();
        if slot.is_none() {
            match Target::new(&self.shared.gpu, self.hwnd, w as u32, h as u32, dpi) {
                Ok(t) => *slot = Some(t),
                Err(e) => {
                    eprintln!("horadric: render target for {}: {e}", self.name);
                    return;
                }
            }
        }
        if std::env::var_os("HORADRIC_DEBUG").is_some() {
            eprintln!("paint {} {}x{} dpi {dpi}", self.name, w, h);
        }
        let sessions = self.sessions();
        let refs: Vec<&Session> = sessions.iter().collect();
        let layout = self.layout.borrow();
        let on_stage = {
            let staged = self.shared.staged.borrow();
            refs.iter().any(|s| staged.contains(&s.id))
        };
        let selected = on_stage
            .then(|| {
                let active = self.shared.active.borrow();
                refs.iter().position(|s| active.as_deref() == Some(&s.id))
            })
            .flatten();
        let files = self.files.borrow();
        let items = self.items();
        let tasks_scene = items.as_ref().map(|items| {
            let t = self.tasks.borrow();
            let boards = self.shared.boards.borrow();
            let b = boards.get(&self.key);
            TasksScene {
                rows: items
                    .iter()
                    .skip(t.scroll)
                    .take(layout.tasks.as_ref().map_or(0, |l| l.rows.len()))
                    .map(|i| TaskRow {
                        title: i.title.clone(),
                        state: i.state,
                        finish: i.finish,
                    })
                    .collect(),
                total: items.len(),
                scroll: t.scroll,
                summary: b.map_or_else(|| "empty".to_string(), Board::summary),
                mode: b.map_or_else(|| Mode::default().label().into(), Board::mode_key),
                collapsed: t.collapsed,
            }
        });
        let hot = self.hot.get();
        let pressed = self.pressed.get();
        let lift = self.lift.borrow();
        let carried = lift.as_ref().filter(|l| l.moved);
        let held = carried.and_then(|l| refs.iter().position(|s| s.id == l.id));
        let inputs: Vec<TileIn> = refs
            .iter()
            .zip(&layout.tiles)
            .enumerate()
            .map(|(i, (s, r))| {
                let is_held = held == Some(i);
                TileIn {
                    id: &s.id,
                    phase: &s.phase,
                    y: carried.filter(|_| is_held).map_or(r.y, |l| l.top),
                    hot: is_held || (pressed.is_none() && hot == Hit::Tile(i)),
                    held: is_held,
                    icon: theme::tile_icon(s),
                    context: s
                        .status
                        .as_ref()
                        .and_then(|st| st.context)
                        .map(|c| c / 100.0),
                }
            })
            .collect();
        let now = Instant::now();
        let looks = self.tiles.borrow_mut().step(now, &inputs);
        let ambient = backdrop::animations_on();
        let drawn = refs
            .iter()
            .zip(&looks)
            .zip(&layout.tiles)
            .map(|((s, l), r)| {
                let at = layout::Rect::new(r.x, l.y, r.w, r.h);
                (s.id.clone(), (at, (*s).clone()))
            })
            .collect();
        let ghosts: Vec<(layout::Rect, Session, f32)> = self
            .leaving
            .borrow_mut()
            .step(now, drawn)
            .into_iter()
            .filter(|_| ambient)
            .map(|((r, s), t)| (r, s, t))
            .collect();
        // A tile on its way somewhere changes what holds still.
        let moving = looks.iter().zip(&inputs).any(|(l, t)| l.moving(t.y));
        let finishing = items
            .as_ref()
            .is_some_and(|items| items.iter().any(|i| i.finish.is_some()));
        // Where each held item's row is, or the tile's header for one
        // scrolled out of view.
        let holders: Vec<(String, (f32, f32))> = match (&items, &layout.tasks) {
            (Some(items), Some(tl)) => {
                let scroll = self.tasks.borrow().scroll;
                items
                    .iter()
                    .enumerate()
                    .filter_map(|(i, it)| {
                        let r = i
                            .checked_sub(scroll)
                            .and_then(|k| tl.rows.get(k))
                            .unwrap_or(&tl.header);
                        let at = (r.x + 16.0, r.y + r.h / 2.0);
                        it.holder.clone().map(|h| (h, at))
                    })
                    .collect()
            }
            _ => Vec::new(),
        };
        let ids: Vec<&str> = refs.iter().map(|s| s.id.as_str()).collect();
        let flights: Vec<Flight> = self
            .handoffs
            .borrow_mut()
            .step(now, &ids, &holders)
            .into_iter()
            .filter(|_| ambient)
            .filter_map(|(id, from, p)| {
                let i = ids.iter().position(|t| *t == id)?;
                let r = layout.tiles.get(i)?;
                let y = looks.get(i).map_or(r.y, |l| l.y);
                Some(Flight {
                    from,
                    to: (r.x + 14.0, y + r.h / 2.0),
                    done: p,
                })
            })
            .collect();
        let finishing = finishing || !flights.is_empty();
        let rebuild = self.dirty.replace(false) || moving || !ghosts.is_empty() || finishing;
        let scene = Scene {
            layout: &layout,
            name: &self.name,
            collapsed: self.collapsed,
            sessions: &refs,
            looks: &looks,
            ghosts: &ghosts,
            flights: &flights,
            held,
            on_stage,
            selected,
            accent: theme::accent(&self.key),
            ambient,
            rebuild,
            now: SystemTime::now(),
            files: files.tree.as_ref().map(|tree| FilesScene {
                tree,
                rows: &files.rows,
                scroll: files.scroll,
                collapsed: files.folded(),
            }),
            tasks: tasks_scene,
            hot: self.hot.get(),
            pressed: self.pressed.get(),
        };
        let result = slot
            .as_ref()
            .map(|t| t.draw(&self.shared.gpu, &self.shared.metrics, &scene));
        // Any EndDraw failure, including D2DERR_RECREATE_TARGET, drops the
        // target. The next paint makes a fresh one.
        if let Some(Err(_)) = result {
            *slot = None;
        }
        let phases: Vec<&horadric_core::Phase> = refs.iter().map(|s| &s.phase).collect();
        let targets: Vec<f32> = inputs.iter().map(|t| t.y).collect();
        let next = anim::Tiles::next_frame(&looks, &phases, &targets, ambient);
        self.schedule(if ghosts.is_empty() && !finishing {
            next
        } else {
            Some(motion::FRAME_FAST)
        });
        if self.refit_due.replace(false) {
            unsafe {
                let _ = PostMessageW(Some(self.hwnd), WM_CLUSTER_REFIT, WPARAM(0), LPARAM(0));
            }
        }
    }

    /// Keeps the animation timer at the rate the next frame needs, or stops
    /// it, so a cluster with nothing moving costs nothing.
    fn schedule(&self, every: Option<Duration>) {
        if self.frames.replace(every) == every {
            return;
        }
        unsafe {
            match every {
                Some(d) => {
                    SetTimer(Some(self.hwnd), ANIM_TIMER, d.as_millis() as u32, None);
                }
                None => {
                    let _ = KillTimer(Some(self.hwnd), ANIM_TIMER);
                }
            }
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
            // Only the light moved: the kept layer stays.
            WM_TIMER if wparam.0 == ANIM_TIMER => {
                unsafe {
                    let _ = InvalidateRect(Some(self.hwnd), None, false);
                }
                Some(LRESULT(0))
            }
            WM_CLUSTER_REFIT => {
                self.refit();
                Some(LRESULT(0))
            }
            WM_CLUSTER_FILES => {
                let fresh = self
                    .files
                    .borrow()
                    .slot
                    .lock()
                    .ok()
                    .and_then(|mut s| s.take());
                if let Some(tree) = fresh {
                    let mut f = self.files.borrow_mut();
                    f.tree = Some(tree);
                    f.rebuild();
                }
                self.refit();
                // A file shown on the stage may be one that changed.
                app::push(Input::FilesChanged(self.key.clone()));
                Some(LRESULT(0))
            }
            WM_MOUSEWHEEL => {
                // Screen coordinates, unlike every other mouse message.
                let mut p = POINT {
                    x: (lparam.0 & 0xffff) as i16 as i32,
                    y: ((lparam.0 >> 16) & 0xffff) as i16 as i32,
                };
                unsafe {
                    let _ = ScreenToClient(self.hwnd, &mut p);
                }
                let s = self.scale();
                let (x, y) = (p.x as f32 / s, p.y as f32 / s);
                let notches = ((wparam.0 >> 16) & 0xffff) as i16 as i32 / 120;
                let over_tasks = match &self.layout.borrow().tasks {
                    Some(tl) if tl.body().contains(x, y) => Some(tl.rows.len()),
                    _ => None,
                };
                if let Some(shown) = over_tasks {
                    let total = self.items().map_or(0, |i| i.len());
                    let scroll = {
                        let mut t = self.tasks.borrow_mut();
                        let max = total.saturating_sub(shown) as i32;
                        let scroll = (t.scroll as i32 - notches).clamp(0, max) as usize;
                        std::mem::replace(&mut t.scroll, scroll) != scroll
                    };
                    if scroll {
                        self.fit();
                    }
                    return Some(LRESULT(0));
                }
                let shown = match &self.layout.borrow().files {
                    Some(fl) if fl.body().contains(x, y) => fl.rows.len(),
                    // Anywhere else scrolls the column, when it holds more
                    // than fits.
                    _ => {
                        app::push(Input::Scroll(self.key.clone(), notches));
                        return Some(LRESULT(0));
                    }
                };
                let mut f = self.files.borrow_mut();
                let max = f.rows.len().saturating_sub(shown) as i32;
                let scroll = (f.scroll as i32 - notches * WHEEL_ROWS).clamp(0, max) as usize;
                if scroll != f.scroll {
                    f.scroll = scroll;
                    self.invalidate();
                }
                Some(LRESULT(0))
            }
            // The style alone does not stop a click from making the cluster
            // the foreground window, seen on Windows 11. Then the terminal
            // it was meant to leave in front is no longer there.
            WM_MOUSEACTIVATE => Some(LRESULT(MA_NOACTIVATE as isize)),
            WM_SIZE => {
                let w = (lparam.0 & 0xffff) as u32;
                let h = ((lparam.0 >> 16) & 0xffff) as u32;
                if let Some(t) = self.target.borrow().as_ref() {
                    let _ = t.resize(w, h);
                }
                self.dirty.set(true);
                Some(LRESULT(0))
            }
            WM_DPICHANGED => {
                let dpi = (wparam.0 & 0xffff) as u32;
                if let Some(t) = self.target.borrow().as_ref() {
                    t.set_dpi(dpi);
                }
                self.dirty.set(true);
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
                // On a screen of another scale its column is another
                // height in pixels.
                app::push(Input::Arrange);
                Some(LRESULT(0))
            }
            WM_LBUTTONDOWN => {
                self.raise();
                let mut cursor = POINT::default();
                unsafe {
                    let _ = GetCursorPos(&mut cursor);
                    SetCapture(self.hwnd);
                }
                let hit = self.hit(lparam);
                self.press(Some(hit));
                if let Hit::Tile(i) = hit {
                    if self.start_lift(i, lparam) {
                        return Some(LRESULT(0));
                    }
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
                if self.lift.borrow().is_some() {
                    self.carry(lparam);
                    if self.lift.borrow().as_ref().is_some_and(|l| l.moved) {
                        cube::lid(&self.shared, cube::under_cursor(&self.shared));
                    }
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
                        // A drag that began on a button moves the window
                        // instead, so the button lets go.
                        self.press(None);
                        self.move_to(d.start_window.x + dx, d.start_window.y + dy);
                        app::push(Input::Carry(self.key.clone(), Some((cursor.x, cursor.y))));
                        let over =
                            self.sole_session().is_some() && cube::under_cursor(&self.shared);
                        cube::lid(&self.shared, over);
                    }
                }
                Some(LRESULT(0))
            }
            WM_LBUTTONUP => {
                // Taken first: letting go of the capture below sends
                // WM_CAPTURECHANGED, which drops a lift or calls off a drag it still finds.
                let lift = self.lift.borrow_mut().take();
                let drag = self.drag.borrow_mut().take();
                unsafe {
                    let _ = ReleaseCapture();
                }
                let into_cube = cube::under_cursor(&self.shared);
                cube::lid(&self.shared, false);
                if let Some(l) = lift {
                    self.press(None);
                    if l.moved && into_cube {
                        app::push(Input::ToCube(l.id));
                        self.fit();
                    } else {
                        self.put_down(l, lparam);
                    }
                    return Some(LRESULT(0));
                }
                self.press(None);
                match drag {
                    // A lone tile drags its window, so the window is what
                    // is dropped in the cube, and it goes back to its place.
                    Some(d) if d.moved && into_cube && self.sole_session().is_some() => {
                        if let Some(id) = self.sole_session() {
                            app::push(Input::ToCube(id));
                        }
                        app::push(Input::Carry(self.key.clone(), None));
                    }
                    Some(d) if d.moved => {
                        let mut cursor = POINT::default();
                        unsafe {
                            let _ = GetCursorPos(&mut cursor);
                        }
                        app::push(Input::Drop(self.key.clone(), cursor.x, cursor.y));
                    }
                    Some(_) => self.click(lparam),
                    None => {}
                }
                Some(LRESULT(0))
            }
            WM_MOUSELEAVE => {
                self.tracking.set(false);
                self.hover(Hit::Nothing);
                Some(LRESULT(0))
            }
            // Capture taken away mid press, by alt tab or a menu: no button
            // up comes, so nothing else would let go of the button.
            WM_CAPTURECHANGED => {
                self.press(None);
                cube::lid(&self.shared, false);
                // Lost mid drag: the window goes back to its place.
                if self.drag.borrow_mut().take().is_some_and(|d| d.moved) {
                    app::push(Input::Carry(self.key.clone(), None));
                }
                // The tile goes back where it was.
                if self.lift.borrow_mut().take().is_some_and(|l| l.moved) {
                    self.fit();
                }
                None
            }
            WM_RBUTTONUP => {
                let s = self.scale();
                let x = (lparam.0 & 0xffff) as i16 as f32 / s;
                let y = ((lparam.0 >> 16) & 0xffff) as i16 as f32 / s;
                match layout::hit(&self.layout.borrow(), x, y) {
                    Hit::Tile(i) | Hit::Browser(i) | Hit::Code(i) => {
                        if let Some(s) = self.sessions().get(i) {
                            app::push(Input::TileMenu(s.id.clone()));
                        }
                    }
                    Hit::Header | Hit::Add | Hit::Shell => {
                        app::push(Input::ProjectMenu(self.key.clone()))
                    }
                    Hit::Task(i) | Hit::TaskApprove(i) => {
                        if let Some(item) = self.item_at(i) {
                            app::push(Input::TaskMenu(self.key.clone(), item.line, item.title));
                        }
                    }
                    Hit::TasksHeader | Hit::TasksMode | Hit::TasksAdd => {
                        app::push(Input::TasksMode(self.key.clone()))
                    }
                    _ => {}
                }
                Some(LRESULT(0))
            }
            _ => None,
        }
    }

    /// What a mouse message's client coordinates land on.
    fn hit(&self, lparam: LPARAM) -> Hit {
        let s = self.scale();
        let x = (lparam.0 & 0xffff) as i16 as f32 / s;
        let y = ((lparam.0 >> 16) & 0xffff) as i16 as f32 / s;
        layout::hit(&self.layout.borrow(), x, y)
    }

    /// What the cursor is over right now, wherever it is.
    fn cursor_hit(&self) -> Hit {
        let mut p = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut p);
            let _ = ScreenToClient(self.hwnd, &mut p);
        }
        let s = self.scale();
        layout::hit(&self.layout.borrow(), p.x as f32 / s, p.y as f32 / s)
    }

    /// Asks for a WM_MOUSELEAVE, which Windows sends once per asking.
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

    /// Notes what the cursor is over, repainting when a button changes.
    fn hover(&self, hot: Hit) {
        let old = self.hot.replace(hot);
        if old != hot && (old.lights() || hot.lights()) {
            self.invalidate();
        }
    }

    fn press(&self, pressed: Option<Hit>) {
        if self.pressed.replace(pressed) != pressed {
            self.invalidate();
        }
    }

    /// The one session a cluster of one tile shows.
    fn sole_session(&self) -> Option<String> {
        let sessions = self.sessions();
        match sessions.as_slice() {
            [s] => Some(s.id.clone()),
            _ => None,
        }
    }

    /// Picks up the `i`th tile, to be carried to a new place. Only where
    /// there is more than one: a lone tile drags its window instead.
    fn start_lift(&self, i: usize, lparam: LPARAM) -> bool {
        let sessions = self.sessions();
        let top = self.layout.borrow().tiles.get(i).map(|r| r.y);
        let (Some(s), Some(top), true) = (sessions.get(i), top, sessions.len() > 1) else {
            return false;
        };
        *self.lift.borrow_mut() = Some(Lift {
            id: s.id.clone(),
            start_y: self.client_y(lparam),
            start_top: top,
            top,
            slot: i,
            moved: false,
        });
        true
    }

    /// Moves the carried tile with the cursor, and the others out of its
    /// way once it is over another's place.
    fn carry(&self, lparam: LPARAM) {
        let y = self.client_y(lparam);
        let scale = self.scale();
        let refit = {
            let layout = self.layout.borrow();
            let mut lift = self.lift.borrow_mut();
            let Some(l) = lift.as_mut() else {
                return;
            };
            let dy = y - l.start_y;
            if !l.moved && (dy * scale).abs() <= DRAG_THRESHOLD as f32 {
                return;
            }
            let (Some(first), Some(last)) = (layout.tiles.first(), layout.tiles.last()) else {
                return;
            };
            let was = l.moved;
            l.moved = true;
            l.top = (l.start_top + dy).clamp(first.y, last.y);
            let slot = layout::tile_slot(&layout.tiles, l.top);
            let refit = !was || slot != l.slot;
            l.slot = slot;
            refit
        };
        self.press(None);
        // The browser buttons and the hit test follow the new order.
        if refit {
            self.fit();
        } else {
            self.invalidate();
        }
    }

    /// Lets go of a tile: in its new place if it was carried, otherwise it
    /// was a click.
    fn put_down(&self, lift: Lift, lparam: LPARAM) {
        if !lift.moved {
            self.click(lparam);
            return;
        }
        let mut shown: Vec<String> = self.sessions().into_iter().map(|s| s.id).collect();
        if let Some(i) = shown.iter().position(|id| *id == lift.id) {
            let id = shown.remove(i);
            shown.insert(lift.slot.min(shown.len()), id);
        }
        app::push(Input::Reorder(self.key.clone(), shown));
    }

    /// A mouse message's client y, in DIPs.
    fn client_y(&self, lparam: LPARAM) -> f32 {
        ((lparam.0 >> 16) & 0xffff) as i16 as f32 / self.scale()
    }

    fn click(&self, lparam: LPARAM) {
        match self.hit(lparam) {
            Hit::New => app::push(Input::New(self.key.clone())),
            Hit::Add => app::push(Input::Add(self.key.clone())),
            Hit::Shell => app::push(Input::Shell(Some(self.key.clone()))),
            Hit::Header => app::push(Input::Toggle(self.hwnd.0 as isize)),
            Hit::FilesHeader => {
                {
                    let mut f = self.files.borrow_mut();
                    if f.folded() {
                        // Folded for want of room, a click still opens it
                        // and its column folds another instead.
                        f.collapsed = false;
                        f.opened = OPENED.fetch_add(1, Ordering::Relaxed);
                    } else {
                        f.collapsed = true;
                    }
                }
                app::push(Input::Arrange);
            }
            Hit::File(i) => self.file_clicked(i),
            Hit::TasksHeader => {
                let collapsed = !self.tasks_collapsed();
                self.set_tasks_collapsed(collapsed);
                app::push(Input::Arrange);
            }
            Hit::TasksMode => app::push(Input::TasksMode(self.key.clone())),
            Hit::TasksAdd => app::push(Input::TaskAdd(self.key.clone())),
            Hit::Task(i) => {
                if let Some(item) = self.item_at(i) {
                    app::push(Input::TaskClick(self.key.clone(), item.line, item.title));
                }
            }
            Hit::TaskApprove(i) => {
                if let Some(item) = self.item_at(i) {
                    app::push(Input::TaskApprove(self.key.clone(), item.line, item.title));
                }
            }
            Hit::Tile(i) => {
                // Tiles are laid out in registry order, the same order
                // `sessions` returns.
                if let Some(s) = self.sessions().get(i) {
                    app::push(Input::Expand(s.id.clone()));
                }
            }
            Hit::Browser(i) => {
                if let Some(s) = self.sessions().get(i) {
                    app::push(Input::Browser(s.id.clone()));
                }
            }
            Hit::Code(i) => {
                if let Some(w) = self.sessions().get(i).and_then(|s| s.worktree.clone()) {
                    watch::open_in_code(Path::new(&w.path));
                }
            }
            Hit::Nothing => {}
        }
    }

    /// A folder opens or closes; a file opens on the stage, or with Ctrl
    /// held in the editor.
    fn file_clicked(&self, i: usize) {
        {
            let mut f = self.files.borrow_mut();
            let f = &mut *f;
            let (Some(tree), Some(row)) = (&f.tree, f.rows.get(f.scroll + i)) else {
                return;
            };
            let node = tree.node(row.node);
            if !node.dir {
                // A deleted file has nothing to open.
                if node.change != Some(Change::Deleted) {
                    if let Some(dir) = &self.dir {
                        let ctrl = unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0;
                        if ctrl {
                            watch::open(dir, &node.path);
                        } else {
                            app::push(Input::View(
                                self.key.clone(),
                                dir.clone(),
                                node.path.clone(),
                            ));
                        }
                    }
                }
                return;
            }
            f.view.toggle(node);
            f.rebuild();
        }
        self.refit();
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
    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const Cluster;
    if ptr.is_null() {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    if msg == WM_NCDESTROY {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    // The Box lives in the app for as long as the window exists, and the app
    // destroys the window before dropping the Box.
    let cluster = &*ptr;
    match cluster.handle(msg, wparam, lparam) {
        Some(r) => r,
        None => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
