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

use std::borrow::Cow;
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use horadric_core::tasks::{Mark, Mode};
use horadric_core::{format_age, Agent, Defaults, Registry, Session, Usage};
use windows::core::{w, Result, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DwmGetWindowAttribute, DwmSetWindowAttribute, DWMWA_CLOAKED, DWMWA_WINDOW_CORNER_PREFERENCE,
    DWMWCP_ROUND, DWM_WINDOW_CORNER_PREFERENCE,
};
use windows::Win32::Graphics::Gdi::{
    InvalidateRect, MonitorFromRect, ScreenToClient, ValidateRect, MONITOR_DEFAULTTONULL,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT,
    VK_CONTROL,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetCursorPos, GetWindow, GetWindowLongPtrW,
    GetWindowLongW, GetWindowRect, IsIconic, IsWindowVisible, KillTimer, LoadCursorW, PostMessageW,
    RegisterClassW, SetCursor, SetTimer, SetWindowLongPtrW, SetWindowPos, ShowWindow,
    CREATESTRUCTW, CS_HREDRAW, CS_VREDRAW, GWLP_USERDATA, GWL_EXSTYLE, GW_HWNDPREV, HWND_NOTOPMOST,
    HWND_TOPMOST, IDC_ARROW, IDC_HAND, MA_NOACTIVATE, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
    SWP_NOZORDER, SW_SHOWNOACTIVATE, WM_APP, WM_CAPTURECHANGED, WM_DPICHANGED, WM_ERASEBKGND,
    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_NCCREATE,
    WM_NCDESTROY, WM_PAINT, WM_RBUTTONUP, WM_SIZE, WM_TIMER, WNDCLASSW, WS_EX_LAYERED,
    WS_EX_NOACTIVATE, WS_EX_NOREDIRECTIONBITMAP, WS_EX_TOOLWINDOW, WS_EX_TRANSPARENT, WS_POPUP,
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
use crate::render::{
    FilesScene, Flight, Gpu, Scene, Target, TaskRow, TasksScene, TomeScene, TomeStone, TRACE_BARS,
};
use crate::theme;
use crate::tip;
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
    /// Claude's limits, written by the feeder thread as status lines
    /// arrive. They go with the account, see `accounts`.
    pub usage: Arc<Mutex<Option<Usage>>>,
    /// The other agents' limits, written by the feeder thread as their
    /// events bring them.
    pub agent_usage: Arc<Mutex<BTreeMap<Agent, Usage>>>,
    /// The account each agent uses, as the usage window names it.
    pub account: RefCell<BTreeMap<Agent, String>>,
    /// What each agent's sessions start with, from its own lists.
    pub defaults: RefCell<BTreeMap<Agent, Defaults>>,
    /// Each project's task list as last read, by project key.
    pub boards: RefCell<HashMap<String, Board>>,
    /// The cube's window while there is one, for what is carried over it.
    pub cube: Cell<Option<HWND>>,
    /// Each project's Runetome as last read, by project key, the empty
    /// stone last.
    pub tomes: RefCell<HashMap<String, Vec<TomeStone>>>,
}

impl Shared {
    /// What `agent`'s sessions start with.
    pub fn defaults_of(&self, agent: Agent) -> Defaults {
        self.defaults
            .borrow()
            .get(&agent)
            .cloned()
            .unwrap_or_default()
    }

    /// `agent`'s limits as last heard.
    pub fn usage_of(&self, agent: Agent) -> Option<Usage> {
        match agent {
            Agent::Claude => self.usage.lock().ok()?.clone(),
            _ => self.agent_usage.lock().ok()?.get(&agent).cloned(),
        }
    }
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
    tome: RefCell<Tome>,
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
    leaving: RefCell<anim::Leaving<(layout::Rect, Rc<Session>)>>,
    /// The frame interval the animation timer runs at, if it runs.
    frames: Cell<Option<Duration>>,
    /// Something besides the moving light changed, so the kept layer of
    /// what holds still has to be drawn again.
    dirty: Cell<bool>,
    /// What the kept layer was last drawn from, so an event for another
    /// project, or one that changed nothing here, draws nothing.
    drawn: RefCell<Option<Still>>,
    /// The tiles' clock readings at the last tick, so the tick redraws only
    /// when an age or a trace has moved.
    clock: RefCell<Vec<Clock>>,
}

/// What the kept layer shows, besides the clock and the cursor.
#[derive(PartialEq)]
struct Still {
    layout: ClusterLayout,
    sessions: Vec<Rc<Session>>,
    items: Option<Vec<Item>>,
    stones: Option<Vec<TomeStone>>,
    board: Option<(String, String)>,
    staged: Vec<bool>,
    active: Option<String>,
}

/// What a tile shows that moves with time alone: its age, and its trace
/// of the last minutes, the trace's slide rounded to a quarter bar.
#[derive(PartialEq)]
struct Clock {
    age: String,
    trace: Vec<u8>,
    slide: u8,
}

impl Clock {
    fn of(s: &Session, now: SystemTime) -> Clock {
        let (bars, scrolled) = s.trace(now, TRACE_BARS);
        let busy = bars.iter().any(|&b| b > 0.0);
        Clock {
            age: format_age(now.duration_since(s.since).unwrap_or_default()),
            trace: bars.iter().map(|b| (b * 32.0) as u8).collect(),
            slide: if busy { (scrolled * 4.0) as u8 } else { 0 },
        }
    }
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

/// The Runetome. Every project with a folder has one.
#[derive(Default)]
struct Tome {
    collapsed: bool,
    /// A stone pressed, and once past the drag threshold carried.
    carry: Option<Carry>,
}

/// A stone held down: which, where the button went down on the screen,
/// and where the cursor is in the window, in DIPs.
struct Carry {
    i: usize,
    start: POINT,
    at: (f32, f32),
    moved: bool,
}

/// A row of the tasks tile as the cluster reads it: which item, and how.
#[derive(Clone, PartialEq)]
struct Item {
    line: usize,
    title: String,
    state: RowState,
    /// What the row says in place of its state's word.
    note: Option<String>,
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
        let initial = layout::cluster(&shared.metrics, n, false, None, None, None);

        let mut cluster = Box::new(Cluster {
            hwnd: HWND::default(),
            key,
            name,
            collapsed: false,
            dir,
            files: RefCell::new(Files::default()),
            tasks: RefCell::new(Tasks::default()),
            tome: RefCell::new(Tome::default()),
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
            drawn: RefCell::new(None),
            clock: RefCell::new(Vec::new()),
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

    pub fn tome_collapsed(&self) -> bool {
        self.tome.borrow().collapsed
    }

    pub fn set_tome_collapsed(&self, collapsed: bool) {
        self.tome.borrow_mut().collapsed = collapsed;
    }

    /// The project's stones, none for a project with no folder, which has
    /// no Runetome.
    fn stones(&self) -> Option<Vec<TomeStone>> {
        self.dir.as_ref()?;
        Some(
            self.shared
                .tomes
                .borrow()
                .get(&self.key)
                .cloned()
                .unwrap_or_default(),
        )
    }

    /// How many stones the Runetome lays out, none for no tome. Folded, it
    /// lays out none.
    fn stone_count(&self, stones: Option<&[TomeStone]>) -> Option<usize> {
        let n = stones?.len();
        Some(if self.tome.borrow().collapsed { 0 } else { n })
    }

    /// The session whose tile is at this point on the screen, if this
    /// window is the one there.
    pub fn session_at(&self, at: POINT) -> Option<String> {
        if !crate::app::window_under(at, self.hwnd) {
            return None;
        }
        let mut p = at;
        unsafe {
            let _ = ScreenToClient(self.hwnd, &mut p);
        }
        let s = self.scale();
        match layout::hit(&self.layout.borrow(), p.x as f32 / s, p.y as f32 / s) {
            Hit::Tile(i) | Hit::Browser(i) | Hit::Code(i) => {
                self.sessions().get(i).map(|s| s.id.clone())
            }
            _ => None,
        }
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
        let now = crate::app::unix_now();
        let mut items: Vec<Item> = b
            .shown()
            .into_iter()
            .map(|i| {
                let t = &b.tasks[i];
                Item {
                    line: t.line,
                    title: t.title.clone(),
                    state: state(t),
                    note: board::note(t, now),
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
                note: None,
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
        let tome = self.stone_count(self.stones().as_deref());
        let h = layout::cluster(
            &self.shared.metrics,
            n,
            self.collapsed,
            tasks.as_deref(),
            tome,
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
        let tome = self.stone_count(self.stones().as_deref());
        let h = layout::cluster(
            m,
            self.sessions().len(),
            self.collapsed,
            tasks.as_deref(),
            tome,
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
    /// pixels below its header, and fits the window to it. Every arrange
    /// gives every cluster its room, so it draws again only if that
    /// changed anything.
    pub fn set_files_room(&self, px: Option<i32>) {
        let room = px.map(|p| p as f32 / self.scale());
        self.files.borrow_mut().room = room;
        self.update();
    }

    /// Recomputes layout from the registry, resizes the window to fit and
    /// draws it again. True when the size changed, so the clusters need
    /// arranging again.
    pub fn fit(&self) -> bool {
        let changed = self.lay_out();
        self.invalidate();
        changed
    }

    /// As [`Cluster::fit`], for when the registry or the boards changed:
    /// draws again only if that changed what this cluster shows. Every hook
    /// event lands here for every cluster, and a full redraw of each is
    /// the dearest thing the app does.
    pub fn update(&self) -> bool {
        let changed = self.lay_out();
        self.refresh();
        changed
    }

    /// Draws again if what it shows changed since it was last drawn.
    pub fn refresh(&self) {
        if self.drawn.borrow().as_ref() != Some(&self.still()) {
            self.invalidate();
        }
    }

    /// Once a second: draws again only if an age or a trace moved.
    pub fn tick(&self) {
        let now = SystemTime::now();
        let clock: Vec<Clock> = self.sessions().iter().map(|s| Clock::of(s, now)).collect();
        if *self.clock.borrow() != clock {
            *self.clock.borrow_mut() = clock;
            self.invalidate();
        }
    }

    fn still(&self) -> Still {
        let sessions = self.sessions().into_iter().map(Rc::new).collect();
        self.still_of(sessions, self.items())
    }

    /// What a paint drew from `sessions` and `items` holds still.
    fn still_of(&self, sessions: Vec<Rc<Session>>, items: Option<Vec<Item>>) -> Still {
        let staged = {
            let staged = self.shared.staged.borrow();
            sessions.iter().map(|s| staged.contains(&s.id)).collect()
        };
        let board = self
            .shared
            .boards
            .borrow()
            .get(&self.key)
            .map(|b| (b.summary(), b.mode_key()));
        Still {
            layout: self.layout.borrow().clone(),
            sessions,
            items,
            stones: self.stones(),
            board,
            staged,
            active: self.shared.active.borrow().clone(),
        }
    }

    fn lay_out(&self) -> bool {
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
        let tome = self.stone_count(self.stones().as_deref());
        let mut l = layout::cluster(m, n, self.collapsed, tasks.as_deref(), tome, wanted);
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
        // Shared, so the tiles leaving and what holds still keep them
        // without a copy each.
        let sessions: Vec<Rc<Session>> = self.sessions().into_iter().map(Rc::new).collect();
        let refs: Vec<&Session> = sessions.iter().map(|s| &**s).collect();
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
                        note: i.note.clone(),
                        finish: i.finish,
                    })
                    .collect(),
                total: items.len(),
                scroll: t.scroll,
                summary: b.map_or_else(String::new, Board::summary),
                mode: b.map_or_else(|| Mode::default().label().into(), Board::mode_key),
                collapsed: t.collapsed,
            }
        });
        let stones = self.stones();
        let tome_scene = stones.as_ref().map(|stones| {
            let t = self.tome.borrow();
            TomeScene {
                stones,
                collapsed: t.collapsed,
                carried: t.carry.as_ref().filter(|c| c.moved).map(|c| (c.i, c.at)),
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
        let drawn = sessions
            .iter()
            .zip(&looks)
            .zip(&layout.tiles)
            .map(|((s, l), r)| {
                let at = layout::Rect::new(r.x, l.y, r.w, r.h);
                (s.id.clone(), (at, Rc::clone(s)))
            })
            .collect();
        let ghosts: Vec<(layout::Rect, Session, f32)> = self
            .leaving
            .borrow_mut()
            .step(now, drawn)
            .into_iter()
            .filter(|_| ambient)
            .map(|((r, s), t)| (r, Session::clone(&s), t))
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
        if rebuild {
            *self.drawn.borrow_mut() = Some(self.still_of(sessions.clone(), items.clone()));
        }
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
            tome: tome_scene,
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
        // Fast frames beat with the display. The slow ones, a light going
        // round or a breath, are cheaper on a timer and look no different.
        crate::vsync::stop(self.hwnd, ANIM_TIMER);
        unsafe {
            let _ = KillTimer(Some(self.hwnd), ANIM_TIMER);
            match every {
                Some(d) if d <= motion::FRAME_FAST => crate::vsync::start(self.hwnd, ANIM_TIMER),
                Some(d) => {
                    SetTimer(Some(self.hwnd), ANIM_TIMER, d.as_millis() as u32, None);
                }
                None => {}
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
            // Only the light moved: the kept layer stays. Nobody sees the
            // light of a cluster under another window, so it waits there,
            // and moves on within a frame of coming back into view.
            WM_TIMER if wparam.0 == ANIM_TIMER => {
                crate::vsync::took(self.hwnd, ANIM_TIMER);
                if !out_of_sight(self.hwnd) {
                    unsafe {
                        let _ = InvalidateRect(Some(self.hwnd), None, false);
                    }
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
                tip::press(self.hwnd);
                self.raise();
                let mut cursor = POINT::default();
                unsafe {
                    let _ = GetCursorPos(&mut cursor);
                    SetCapture(self.hwnd);
                }
                let hit = self.hit(lparam);
                self.press(Some(hit));
                if let Hit::Stone(i) = hit {
                    self.tome.borrow_mut().carry = Some(Carry {
                        i,
                        start: cursor,
                        at: self.client(lparam),
                        moved: false,
                    });
                    return Some(LRESULT(0));
                }
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
                if self.carry_stone(lparam) {
                    return Some(LRESULT(0));
                }
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
                let carry = self.tome.borrow_mut().carry.take();
                unsafe {
                    let _ = ReleaseCapture();
                }
                if let Some(c) = carry {
                    self.press(None);
                    if c.moved {
                        self.drop_stone(c.i);
                        self.invalidate();
                    } else {
                        self.click(lparam);
                    }
                    return Some(LRESULT(0));
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
                tip::away(self.hwnd);
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
                if self.tome.borrow_mut().carry.take().is_some_and(|c| c.moved) {
                    self.invalidate();
                }
                None
            }
            WM_RBUTTONUP => {
                tip::press(self.hwnd);
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
                    Hit::TasksHeader | Hit::TasksMode | Hit::TasksAdd | Hit::TasksGive => {
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
        // Nothing to read while a tile or the window is carried.
        let carried = self.drag.borrow().is_some()
            || self.lift.borrow().is_some()
            || self.tome.borrow().carry.as_ref().is_some_and(|c| c.moved);
        let line = match hot {
            _ if carried => None,
            Hit::Stone(i) => self
                .stones()
                .and_then(|s| s.into_iter().nth(i))
                .map(|s| Cow::Owned(s.tip)),
            _ => tip::cluster(hot).map(Cow::Borrowed),
        };
        tip::over_line(&self.shared, self.hwnd, line);
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

    /// Follows the cursor with a stone held down, once it has gone past
    /// the drag threshold. False when no stone is held.
    fn carry_stone(&self, lparam: LPARAM) -> bool {
        let at = self.client(lparam);
        let empty = |i: usize| {
            self.stones()
                .and_then(|s| s.into_iter().nth(i))
                .is_none_or(|s| s.label.is_none())
        };
        let moved = {
            let mut t = self.tome.borrow_mut();
            let Some(c) = t.carry.as_mut() else {
                return false;
            };
            c.at = at;
            if !c.moved {
                let mut cursor = POINT::default();
                unsafe {
                    let _ = GetCursorPos(&mut cursor);
                }
                let far = (cursor.x - c.start.x).abs() > DRAG_THRESHOLD
                    || (cursor.y - c.start.y).abs() > DRAG_THRESHOLD;
                // The empty stone makes stones, it is not one to cast.
                c.moved = far && !empty(c.i);
            }
            c.moved
        };
        if moved {
            self.press(None);
            unsafe {
                SetCursor(LoadCursorW(None, IDC_HAND).ok());
            }
            self.invalidate();
        }
        true
    }

    /// A stone let go of after a drag: cast on the tile or pane under the
    /// cursor, which the app finds.
    fn drop_stone(&self, i: usize) {
        let Some(label) = self.stones().and_then(|s| s.into_iter().nth(i)?.label) else {
            return;
        };
        let mut cursor = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut cursor);
        }
        app::push(Input::StoneDrop(self.key.clone(), label, cursor));
    }

    /// A mouse message's client point, in DIPs.
    fn client(&self, lparam: LPARAM) -> (f32, f32) {
        let s = self.scale();
        (
            (lparam.0 & 0xffff) as i16 as f32 / s,
            ((lparam.0 >> 16) & 0xffff) as i16 as f32 / s,
        )
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
            Hit::TomeHeader => {
                let collapsed = !self.tome_collapsed();
                self.set_tome_collapsed(collapsed);
                app::push(Input::Arrange);
            }
            Hit::Stone(i) => {
                if let Some(s) = self.stones().and_then(|s| s.into_iter().nth(i)) {
                    app::push(Input::Stone(self.key.clone(), s.label));
                }
            }
            Hit::TasksMode => app::push(Input::TasksMode(self.key.clone())),
            Hit::TasksAdd => app::push(Input::TaskAdd(self.key.clone())),
            Hit::TasksGive => app::push(Input::GiveQuests(self.key.clone())),
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

/// Whether nothing of `hwnd` can be seen: minimised, hidden, off every
/// screen, or under one window that covers it whole, as a maximised one
/// does. A window that may be see-through never counts as covering.
fn out_of_sight(hwnd: HWND) -> bool {
    unsafe {
        if !IsWindowVisible(hwnd).as_bool() || IsIconic(hwnd).as_bool() {
            return true;
        }
        let mut mine = RECT::default();
        if GetWindowRect(hwnd, &mut mine).is_err() {
            return false;
        }
        if MonitorFromRect(&mine, MONITOR_DEFAULTTONULL).is_invalid() {
            return true;
        }
        // Browsers and Electron apps draw without a redirection bitmap,
        // and so do overlays, which are tool windows.
        let overlay = WS_EX_NOREDIRECTIONBITMAP.0 | WS_EX_TOOLWINDOW.0;
        let see_through =
            |ex: u32| ex & (WS_EX_LAYERED | WS_EX_TRANSPARENT).0 != 0 || ex & overlay == overlay;
        let mut above = GetWindow(hwnd, GW_HWNDPREV);
        while let Ok(w) = above {
            let covers = IsWindowVisible(w).as_bool()
                && !IsIconic(w).as_bool()
                && !see_through(GetWindowLongW(w, GWL_EXSTYLE) as u32)
                && !cloaked(w)
                && {
                    let mut r = RECT::default();
                    GetWindowRect(w, &mut r).is_ok()
                        && r.left <= mine.left
                        && r.top <= mine.top
                        && r.right >= mine.right
                        && r.bottom >= mine.bottom
                };
            if covers {
                return true;
            }
            above = GetWindow(w, GW_HWNDPREV);
        }
        false
    }
}

/// A window on another virtual desktop, or hidden by the shell, still says
/// it is visible.
fn cloaked(hwnd: HWND) -> bool {
    let mut cloak = 0u32;
    unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            &mut cloak as *mut u32 as *mut c_void,
            std::mem::size_of::<u32>() as u32,
        )
        .is_ok()
            && cloak != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use horadric_core::session::ACTIVITY_SPAN;

    #[test]
    fn an_idle_tile_reads_the_same_until_its_age_moves() {
        let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        let mut s = Session::new("t", "t", "C:/repo");
        s.since = t0;
        let at = |secs| Clock::of(&s, t0 + Duration::from_secs(secs));
        assert!(at(300) == at(301));
        assert!(at(300) != at(360));
    }

    #[test]
    fn a_busy_trace_moves_a_quarter_bar_at_a_time() {
        let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        let mut s = Session::new("t", "t", "C:/repo");
        s.since = t0;
        s.activity = vec![t0 + Duration::from_secs(290)];
        let bar = ACTIVITY_SPAN.as_secs() / TRACE_BARS as u64;
        let slides: HashSet<u8> = (0..bar)
            .map(|i| Clock::of(&s, t0 + Duration::from_secs(300 + i)).slide)
            .collect();
        assert_eq!(slides.len(), 4);
    }
}
