//! The UI thread: owns the cluster windows and the terminal windows, the
//! consoles behind them, and keeps the set of windows in sync with the set
//! of sessions.
//!
//! Sessions share one terminal window, the stage, which shows one project
//! at a time: each of its sessions is a pane in a grid. A click on a tile
//! switches the stage to that tile's project and gives its session the
//! keyboard; the sessions of other projects keep running unseen. A global
//! hotkey brings up the session that has waited on you longest.
//!
//! Everything that happens off the UI thread arrives as a message to one
//! hidden window: hook events from the feeder thread, new session requests
//! from the listener, output and exit from the consoles, clicks queued by the
//! window procedures, and the tray icon. A window, not the thread, because
//! thread messages are dropped while Windows runs a modal loop, and resizing
//! a terminal by its edge is a modal loop. A hidden top level window rather
//! than a message-only one, because only top level windows hear Explorer's
//! `TaskbarCreated` broadcast and the shutdown messages. A one second timer
//! on the same window moves the age lines and saves the state.
//!
//! A browser window a session opens is placed beside the stage when it
//! first appears, and follows its project: switching the stage to another
//! project minimises the browsers of the rest and brings back that one's.
//!
//! Every console runs in a session host of its own, so the sessions outlive
//! the app: a start finds the hosts still running and attaches to them,
//! with their screens replayed. What Horadric owned is saved to disk as it
//! changes too. A session whose host is gone comes back as a paused tile,
//! and clicking one resumes its conversation with `claude --resume`.
//!
//! `horadric reload` hands the app over to a new build at once: this one
//! saves, starts the new build's `swap`, and quits. The new app attaches to
//! the same hosts, so an update costs a few seconds and no session notices.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::ffi::c_void;
use std::net::TcpStream;
use std::os::windows::io::AsRawHandle;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use horadric_core::background::Asked;
use horadric_core::diff::{self as changes, Diff, FileDiff, Recount};
use horadric_core::fleet::{self, Device};
use horadric_core::journal::{self, Entry, What};
use horadric_core::release::{self, Manifest};
use horadric_core::ssh;
use horadric_core::usage::has_flag;
use horadric_core::worktree::{self as tree, Worktree};
use horadric_core::{
    session_id, Carry, HookEvent, Phase, Registry, Route, SavedCluster, SavedPanel, SavedSession,
    SavedState, Session, Setting, Usage,
};
use horadric_hooks::listener::{self, Command, Reload, Tagged};
use horadric_hooks::transcript::{self, Past};
use horadric_hooks::{install, TASKS_ENV};
use windows::core::{w, BOOL, PCWSTR};
use windows::Win32::Foundation::{HANDLE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, MonitorFromPoint, MonitorFromRect, HDC, HMONITOR,
    MONITORINFO, MONITORINFOEXW, MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTONULL,
};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::RemoteDesktop::{
    WTSRegisterSessionNotification, NOTIFY_FOR_THIS_SESSION,
};
use windows::Win32::System::SystemInformation::{GetLocalTime, GetTickCount};
use windows::Win32::UI::HiDpi::{
    GetDpiForWindow, SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetLastInputInfo, RegisterHotKey, LASTINPUTINFO, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT,
    VK_HOME, VK_SPACE,
};
use windows::Win32::UI::Shell::{SHQueryUserNotificationState, NIN_BALLOONUSERCLICK};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, GetWindowRect, KillTimer,
    PostMessageW, PostQuitMessage, RegisterClassW, RegisterWindowMessageW, SetTimer, SetWindowPos,
    SystemParametersInfoW, TranslateMessage, MONITORINFOF_PRIMARY, MSG, SPI_GETWORKAREA,
    SPI_SETWORKAREA, SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
    WM_APP, WM_DISPLAYCHANGE, WM_HOTKEY, WM_LBUTTONUP, WM_QUERYENDSESSION, WM_QUIT, WM_RBUTTONUP,
    WM_SETTINGCHANGE, WM_TIMER, WNDCLASSW, WS_EX_TOOLWINDOW, WS_POPUP,
};

use crate::agents;
use crate::appear;
use crate::caption;
use crate::catchup::{self, Away, Catchup};
use crate::columns::{self, Columns};
use crate::console::{self, Console, Launch};
use crate::cube::{self as cube_window, CubeWindow};
use crate::dialog::{self, Dialog, Tone};
use crate::dropdown::{self, Dropdown};
use crate::glide::Glides;
use crate::glyphs::{self, Font};
use crate::keys::{self, FontStep};
use crate::layout::{self, Metrics};
use crate::loot::Loot;
use crate::menu::{self, Item};
use crate::render::{Gpu, StashLook};
use crate::screens::{self, Screen};
use crate::sound;
use crate::start::{self, StartWindow};
use crate::stash::{self, StashWindow};
use crate::terminal::{self, Place, TerminalWindow};
use crate::toast::{self, Kind, Toasts};
use crate::tray::{self, Choice, Tray};
use crate::usage::{self, UsageWindow};
use crate::window::{self, folder_key, project_key, project_name, Cluster, Shared};
use crate::{
    ask, autostart, browsers, history, inbox, paths, picker, recent, shell, snapping, store, theme,
    update, watch, worktree,
};

#[path = "runner.rs"]
mod runner;

pub use runner::ssh_prompt;

/// A hook event changed the registry. `wparam` is 1 when a phase changed.
const WM_HORADRIC_EVENT: u32 = WM_APP + 1;
/// A window procedure queued input with [`push`].
const WM_HORADRIC_INPUT: u32 = WM_APP + 2;
/// A console has new output. `wparam` is its serial.
pub(crate) const WM_HORADRIC_OUTPUT: u32 = WM_APP + 3;
/// A console's process exited. `wparam` is its serial.
pub(crate) const WM_HORADRIC_EXIT: u32 = WM_APP + 4;
/// `horadric new` asked for a session, or `horadric reload` for a new build.
const WM_HORADRIC_NEW: u32 = WM_APP + 5;
/// The tray icon was clicked. `lparam` is the mouse message.
const WM_HORADRIC_TRAY: u32 = WM_APP + 6;
/// Show the folder picker, starting where the app's `pick_from` says.
const WM_HORADRIC_PICK: u32 = WM_APP + 7;
/// Show the menu for the tile the app's `menu_for` names.
const WM_HORADRIC_TILE_MENU: u32 = WM_APP + 8;
/// Show the menu for the project the app's `project_menu_for` names.
const WM_HORADRIC_PROJECT_MENU: u32 = WM_APP + 9;
/// A top level window appeared somewhere. `wparam` is its handle.
const WM_HORADRIC_WINDOW_SHOWN: u32 = WM_APP + 10;
/// A browser window a session opened is gone. `wparam` is its handle.
const WM_HORADRIC_WINDOW_GONE: u32 = WM_APP + 11;
/// Drop the list for the setting the app's `setting_menu_for` names.
const WM_HORADRIC_SETTING_MENU: u32 = WM_APP + 12;
/// Show the menu for the recent project the app's `recent_menu_for` names.
const WM_HORADRIC_RECENT_MENU: u32 = WM_APP + 13;
/// Show the task list menu or dialog the app's `tasks.menu` holds.
const WM_HORADRIC_TASK_MENU: u32 = WM_APP + 14;
/// A worktree's changes were counted, into the app's `counted`.
const WM_HORADRIC_COUNTED: u32 = WM_APP + 15;
/// An update check came back, into the app's `looked`.
const WM_HORADRIC_UPDATE: u32 = WM_APP + 16;
/// An update's download came back, into the app's `downloaded`.
const WM_HORADRIC_DOWNLOADED: u32 = WM_APP + 17;
/// A worktree went but its branch stayed, into the runner's `kept`.
const WM_HORADRIC_KEPT: u32 = WM_APP + 18;
/// Show the menu for the stashed session the app's `stash_menu_for` names.
const WM_HORADRIC_STASH_MENU: u32 = WM_APP + 19;

const APP_CLASS: PCWSTR = w!("HoradricApp");
/// Runs a console program without giving it a console window.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const HOTKEY_NEXT: i32 = 1;
/// The catch-up, on demand.
const HOTKEY_LISTEN: i32 = 2;
/// Not in the `windows` crate's WindowsAndMessaging.
const WM_WTSSESSION_CHANGE: u32 = 0x02B1;
const WTS_SESSION_LOCK: usize = 7;
const WTS_SESSION_UNLOCK: usize = 8;
/// How far back the catch-up on demand looks at least, in seconds: since
/// this morning, or the last eight hours early in the day.
const LISTEN_BACK: u64 = 8 * 3600;
/// The app window's timer that moves the ages on and saves.
const TICK_TIMER: usize = 1;
/// Fires once, a moment after a screen change, to lay the tiles out again
/// once Windows has finished moving windows off a screen that went away.
const SCREEN_TIMER: usize = 2;
/// Runs while a window glides to its place in the columns.
const GLIDE_TIMER: usize = 3;
const ENDED_LINGER: Duration = Duration::from_secs(20);
/// A crash this long after resuming sessions after a crash is a crash of
/// its own, not the same one again, so the next start resumes once more.
const RECOVERED_AFTER: Duration = Duration::from_secs(60);
/// The least time between two sweeps of one project's worktrees. Hook
/// events come many a second while an agent works.
const SWEEP_GAP: Duration = Duration::from_secs(10);
/// Starts the id of a file view, which is no session.
const VIEW: &str = "view:";
pub(crate) const MARGIN_DIP: i32 = 12;
pub(crate) const GAP_DIP: i32 = 12;
/// Narrower than this beside the stage, a new browser window stays where
/// it opened rather than squeeze in.
const BROWSER_MIN_DIP: i32 = 360;

/// Something a window procedure saw that the app has to act on. Window
/// procedures only get `&self`, and the app owns the windows, so they queue
/// it here and the app applies it right after.
pub(crate) enum Input {
    /// Header clicked: collapse or expand the cluster with this window.
    Toggle(isize),
    /// A cluster, or the usage window, by its key in the columns, let go of
    /// after a drag with the cursor here: it takes the place in the columns
    /// under the cursor.
    Drop(String, i32, i32),
    /// A cluster, or the usage window, being dragged with the cursor here:
    /// the others in the column under it make room. None when the drag
    /// was lost without a drop.
    Carry(String, Option<(i32, i32)>),
    /// The wheel turned this many notches over the window with this key,
    /// outside anything that scrolls by itself: its column scrolls.
    Scroll(String, i32),
    /// Tile clicked: show this session's terminal, resume it, or collapse it.
    Expand(String),
    /// Tile right clicked: offer what can be done with this session.
    TileMenu(String),
    /// Header or bottom plus right clicked: offer what can be done with
    /// every session of the project with this key.
    ProjectMenu(String),
    /// Header plus clicked: pick a folder for a new project, starting
    /// beside the project with this key.
    New(String),
    /// Bottom plus clicked: another session in the project with this key,
    /// no questions asked.
    Add(String),
    /// A plain terminal in the project with this key, from the button
    /// beside the bottom plus, or in the project on the stage, from
    /// Ctrl+Shift+T in a pane.
    Shell(Option<String>),
    /// Terminal closed: collapse the terminal window with this handle.
    Close(isize),
    /// A pane was dragged onto another: swap these two sessions.
    Swap(String, String),
    /// A tile was dragged to a new place in the cluster of the project with
    /// this key: its sessions, as the tiles now stand.
    Reorder(String, Vec<String>),
    /// A tile's browser button clicked: bring up this session's browsers.
    Browser(String),
    /// A file in a files tile clicked: show it on the stage beside the
    /// sessions of the project with this key. The folder, then the file's
    /// path inside it.
    View(String, PathBuf, String),
    /// A file view's cross or Esc: close the view with this serial.
    CloseView(usize),
    /// Ctrl and plus, minus, zero or the wheel in a pane: the terminal font
    /// for every pane.
    Font(FontStep),
    /// Files of the project with this key changed on disk.
    FilesChanged(String),
    /// A cluster changed size by itself, a files tile was folded or opened,
    /// the usage window was folded, or a window moved to a screen of
    /// another scale: lay the columns out again.
    Arrange,
    /// Another pane on the stage has the keyboard: its tile latches down.
    Spotlight,
    /// A list setting in the usage window clicked: drop its list under its
    /// row, given in screen pixels.
    SettingMenu(Setting, RECT),
    /// A setting's list closed, with the value picked, if one was.
    Picked(Setting, Option<Option<String>>),
    /// A slider in the usage window let go at a new value.
    SetDefault(Setting, Option<String>),
    /// The catch-up closed, with the session of the line clicked, if one
    /// was.
    Listened(Option<String>),
    /// The start window's tile clicked: pick a folder for the first
    /// project.
    Pick,
    /// A recent project clicked in the start window, or a folder dropped on
    /// it: start a session in this folder.
    StartIn(PathBuf),
    /// A recent project right clicked in the start window: offer a new
    /// session or an old conversation in this folder.
    RecentMenu(PathBuf),
    /// A row of a tasks tile clicked, in the project with this key: the
    /// item on this line of the file, with this title.
    TaskClick(String, usize, String),
    /// The approve button on such a row.
    TaskApprove(String, usize, String),
    /// Such a row right clicked: offer what can be done with the item.
    TaskMenu(String, usize, String),
    /// The mode button in a tasks tile's header: offer the modes.
    TasksMode(String),
    /// The plus in a tasks tile's header: ask for a new item.
    TaskAdd(String),
    /// A stashed session's slot clicked: bring it back.
    Unstash(String),
    /// A stashed session's slot right clicked.
    StashMenu(String),
    /// A tile or a pane let go over the cube, by its session's id.
    ToCube(String),
    /// A session's slot in the cube clicked: it comes out.
    CubeOut(String),
    /// The cube's `main` rune clicked: in, or out again.
    CubeMain,
    /// The cube's button: run the recipe.
    Transmute,
    /// The cube's transmute has played, so it may go if nothing is left
    /// for it to hold.
    CubeSettled,
}

thread_local! {
    static INPUT: RefCell<Vec<Input>> = const { RefCell::new(Vec::new()) };
    static APP_WINDOW: Cell<isize> = const { Cell::new(0) };
    static TASKBAR_CREATED: Cell<u32> = const { Cell::new(0) };
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

/// Queues input for the app and wakes it.
pub(crate) fn push(input: Input) {
    INPUT.with(|q| q.borrow_mut().push(input));
    post(APP_WINDOW.with(Cell::get), WM_HORADRIC_INPUT, 0);
}

fn post(window: isize, msg: u32, wparam: usize) {
    unsafe {
        let _ = PostMessageW(
            Some(HWND(window as *mut c_void)),
            msg,
            WPARAM(wparam),
            LPARAM(0),
        );
    }
}

/// Runs the whole desktop app on the calling thread until quit. With
/// `reload`, or after a Horadric that ended without Quit, the sessions that
/// were running when the last one saved start again by themselves.
pub fn run(port: u16, reload: bool) -> Result<(), String> {
    // A second copy would fail to listen, then show every saved session a
    // second time. Autostart plus a manual start makes that easy to hit.
    if TcpStream::connect_timeout(&([127, 0, 0, 1], port).into(), Duration::from_millis(200))
        .is_ok()
    {
        return Err(format!("Horadric is already running (port {port} answers)"));
    }
    run_app(port, reload).map_err(|e| e.to_string())
}

fn run_app(port: u16, reload: bool) -> windows::core::Result<()> {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        // The folder picker is a COM object.
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        TASKBAR_CREATED.with(|t| t.set(RegisterWindowMessageW(w!("TaskbarCreated"))));
    }
    window::register_class()?;
    usage::register_class()?;
    stash::register_class()?;
    cube_window::register_class()?;
    dropdown::register_class()?;
    ask::register_class()?;
    menu::register_class()?;
    caption::register_class()?;
    dialog::register_class()?;
    toast::register_class()?;
    catchup::register_class()?;
    start::register_class()?;
    terminal::register_class()?;
    let notify = create_app_window()?;
    let notify_id = notify.0 as isize;
    APP_WINDOW.with(|w| w.set(notify_id));
    let hotkey = register_hotkey(notify);
    let listen_key = register_listen_key(notify);
    // Locking the screen is going away, and unlocking it coming back.
    unsafe {
        let _ = WTSRegisterSessionNotification(notify, NOTIFY_FOR_THIS_SESSION);
    }
    store::trim_journal(unix_now());
    browsers::watch(notify, WM_HORADRIC_WINDOW_SHOWN, WM_HORADRIC_WINDOW_GONE);

    let saved = store::load();
    let mut registry = Registry::new();
    let now = SystemTime::now();
    for s in &saved.sessions {
        registry.add(s.to_session(now));
    }
    registry.set_stash(&saved.stash);
    let registry = Arc::new(Mutex::new(registry));
    let usage = Arc::new(Mutex::new(saved.usage.clone()));
    let requests: Arc<Mutex<Vec<Command>>> = Arc::new(Mutex::new(Vec::new()));

    // Listener thread: sockets in, tagged events and commands out.
    let (tx, rx) = mpsc::channel::<Tagged>();
    let (new_tx, new_rx) = mpsc::channel::<Command>();
    thread::spawn(move || {
        if let Err(e) = listener::serve(port, tx, Some(new_tx)) {
            eprintln!("horadric: listener stopped: {e}");
        }
    });

    // Feeder thread: apply to the registry, wake the UI. The limits in a
    // status line are the account's, so the latest from any session wins.
    let feed_registry = Arc::clone(&registry);
    let feed_usage = Arc::clone(&usage);
    thread::spawn(move || {
        // Background sessions already running get their tiles now, not at
        // their next hook, which may be a while for one that is done.
        if agents::list().is_some_and(|list| agents::adopt_all(&feed_registry, &list)) {
            post(notify_id, WM_HORADRIC_EVENT, 1);
        }
        let mut asked = Asked::default();
        for t in rx {
            let limits = t.event.status.as_ref().map(|s| &s.limits);
            if let (Some(limits), Ok(mut u)) = (limits.filter(|l| !l.is_empty()), feed_usage.lock())
            {
                *u = Some(Usage {
                    limits: limits.clone(),
                    at: unix_now(),
                });
            }
            let route = feed_registry
                .lock()
                .map(|r| {
                    let route = r.route(&t.horadric_id, &t.event);
                    // A tag no tile has, on a real conversation, may be a
                    // background session born of a tile long gone.
                    let unknown = matches!(&route, Route::To(id) if r.get(id).is_none());
                    (route, unknown && !t.event.session_id.is_empty())
                })
                .unwrap_or((Route::Stranger, false));
            let target = match route {
                (Route::To(id), false) => Some(id),
                (Route::To(id), true) => {
                    agents::adopt(&feed_registry, &t.event, &mut asked).or(Some(id))
                }
                (Route::Stranger, _) => agents::adopt(&feed_registry, &t.event, &mut asked),
            };
            // Nobody's conversation: a plain `claude` somewhere else.
            let Some(target) = target else {
                continue;
            };
            let changed = feed_registry
                .lock()
                .map(|mut r| r.apply(&target, &t.event, SystemTime::now()))
                .unwrap_or(false);
            post(notify_id, WM_HORADRIC_EVENT, changed as usize);
        }
    });

    // Requests wait in a queue the UI drains when woken.
    let queue = Arc::clone(&requests);
    thread::spawn(move || {
        for n in new_rx {
            if let Ok(mut q) = queue.lock() {
                q.push(n);
            }
            post(notify_id, WM_HORADRIC_NEW, 0);
        }
    });

    // Start with Windows is on unless the user switched it off. Only the
    // first run decides it; later runs respect whatever the menu says. Only
    // the installed copy decides: a dev instance or a build run from target
    // would point it at a build about to be replaced, and a fake install
    // with its own APPDATA has a first run of its own but shares the value.
    let mut autostart_offered = saved.autostart_offered;
    if !autostart_offered && !horadric_hooks::dev() && autostart::running_installed() {
        autostart_offered = autostart::enable();
    }

    // A start after Quit leaves every saved session paused until clicked.
    let how = saved.carry(reload);
    if how == Carry::CrashLoop {
        eprintln!("horadric: ended twice without Quit, resuming nothing this time");
    }
    let carry = if how.resumes() {
        saved.running_ids()
    } else {
        Vec::new()
    };
    let on_stage = saved.on_stage.clone().filter(|_| how.resumes());

    let gpu = Gpu::new()?;
    let font = Font::new(
        &gpu.dw,
        saved.font_family.as_deref(),
        saved.font_size.unwrap_or(keys::FONT_DEFAULT),
    )?;
    let shared = Rc::new(Shared {
        gpu,
        font,
        metrics: Metrics::default(),
        registry,
        orders: RefCell::new(saved.grids.clone().into_iter().collect()),
        staged: RefCell::new(HashSet::new()),
        active: RefCell::new(None),
        browsing: RefCell::new(HashSet::new()),
        usage,
        defaults: RefCell::new(saved.defaults.clone()),
        boards: RefCell::new(HashMap::new()),
        cube: Cell::new(None),
    });
    menu::init(Rc::clone(&shared));
    let toasts = Toasts::new(Rc::clone(&shared), notify, WM_HORADRIC_TRAY);
    // From the hosts' copy, since a session that outlives this app keeps
    // running the status line from wherever it pointed.
    let status_settings = store::write_status_settings(&console::host_program());
    let usage_window = match UsageWindow::create(
        Rc::clone(&shared),
        saved.usage_window.as_ref().is_some_and(|p| p.collapsed),
        -10_000,
        -10_000,
    ) {
        Ok(w) => Some(w),
        Err(e) => {
            eprintln!("horadric: cannot create the usage window: {e}");
            None
        }
    };
    APP.with(|a| {
        let mut app = App {
            shared,
            usage_window,
            stash_window: None,
            cube_window: None,
            cube: Vec::new(),
            cube_main: false,
            start_window: None,
            status_settings,
            setting_menu_for: None,
            dropdown: None,
            switches: HashMap::new(),
            settings_before: None,
            clusters: Vec::new(),
            consoles: HashMap::new(),
            views: HashMap::new(),
            stage: None,
            stage_rect: saved.stage.filter(|r| on_screen(r[0], r[1])),
            stage_key: None,
            hotkey,
            listen_key,
            away: Away::default(),
            catchup: None,
            journaled: HashMap::new(),
            next_serial: 1,
            requests,
            notify,
            tray: Tray::add(notify, WM_HORADRIC_TRAY),
            toasts,
            glides: RefCell::default(),
            glided: Cell::new(None),
            arranged: false,
            carried: None,
            carry_at: None,
            paused: saved
                .sessions
                .iter()
                .map(|s| (s.id.clone(), s.clone()))
                .collect(),
            cluster_places: saved
                .clusters
                .iter()
                .map(|c| (c.key.clone(), c.clone()))
                .collect(),
            columns: Columns::from_keys(&saved.columns),
            recent: saved.recent.clone(),
            autostart_offered,
            quiet: saved.quiet,
            sounds: saved.sounds,
            font_family: saved.font_family.clone(),
            screen: saved.screen.clone(),
            last_saved: Some(saved),
            frozen: false,
            pick_from: None,
            menu_for: None,
            stash_menu_for: None,
            project_menu_for: None,
            recent_menu_for: None,
            reload: None,
            quit: false,
            stop_on_exit: false,
            recovering: (how == Carry::Crash).then(Instant::now),
            browsers: HashMap::new(),
            waiting: HashSet::new(),
            alert_for: None,
            tasks: runner::State::default(),
            new_trees: HashMap::new(),
            recounts: HashMap::new(),
            counted: Arc::new(Mutex::new(Vec::new())),
            sweep_due: HashSet::new(),
            swept_at: HashMap::new(),
            swept_heads: Arc::new(Mutex::new(HashMap::new())),
            update: None,
            last_check: None,
            checking: false,
            looked: Arc::new(Mutex::new(None)),
            downloading: false,
            downloaded: Arc::new(Mutex::new(None)),
            experience: Arc::new(Mutex::new(None)),
            counting_xp: Arc::new(AtomicBool::new(false)),
        };
        app.reconcile(false);
        app.count_experience();
        app.attach_hosts();
        app.carry_on(&carry, on_stage.as_deref());
        // Written before anything resumed can crash it, so that crash is
        // known for what it is.
        app.save();
        *a.borrow_mut() = Some(app);
    });

    unsafe {
        SetTimer(Some(notify), TICK_TIMER, 1000, None);
    }

    let mut msg = MSG::default();
    loop {
        let got = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        if !got.as_bool() || msg.message == WM_QUIT {
            break;
        }
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    APP.with(|a| {
        if let Some(mut app) = a.borrow_mut().take() {
            app.freeze();
            if let Some(stage) = &app.stage {
                stage.destroy();
            }
            for c in &app.clusters {
                c.destroy();
            }
            if let Some(u) = &app.usage_window {
                u.destroy();
            }
            if let Some(w) = &app.stash_window {
                w.destroy();
            }
            if let Some(w) = &app.cube_window {
                w.destroy();
            }
            if let Some(s) = &app.start_window {
                s.destroy();
            }
            // Otherwise the hosts run on and the next start attaches.
            if app.stop_on_exit {
                app.stop_all();
            }
        }
    });
    Ok(())
}

/// The shortcut for the next waiting session, or None when another app
/// holds it. A dev instance adds Shift, so it never fights the installed
/// Horadric for it. AltGr is Ctrl+Alt, and AltGr+Space types nothing on the
/// layouts that matter here.
fn register_hotkey(hwnd: HWND) -> Option<&'static str> {
    let (mods, label) = if horadric_hooks::dev() {
        (MOD_CONTROL | MOD_ALT | MOD_SHIFT, "Ctrl+Alt+Shift+Space")
    } else {
        (MOD_CONTROL | MOD_ALT, "Ctrl+Alt+Space")
    };
    let ok = unsafe {
        RegisterHotKey(
            Some(hwnd),
            HOTKEY_NEXT,
            mods | MOD_NOREPEAT,
            VK_SPACE.0 as u32,
        )
    };
    if let Err(e) = &ok {
        eprintln!("horadric: {label} is taken, no hotkey for the next waiting session: {e}");
    }
    ok.ok().map(|_| label)
}

/// The shortcut for the catch-up, or None when another app holds it. A
/// dev instance adds Shift, as for the next waiting session.
fn register_listen_key(hwnd: HWND) -> Option<&'static str> {
    let (mods, label) = if horadric_hooks::dev() {
        (MOD_CONTROL | MOD_ALT | MOD_SHIFT, "Ctrl+Alt+Shift+Home")
    } else {
        (MOD_CONTROL | MOD_ALT, "Ctrl+Alt+Home")
    };
    let ok = unsafe {
        RegisterHotKey(
            Some(hwnd),
            HOTKEY_LISTEN,
            mods | MOD_NOREPEAT,
            VK_HOME.0 as u32,
        )
    };
    if let Err(e) = &ok {
        eprintln!("horadric: {label} is taken, no hotkey for the catch-up: {e}");
    }
    ok.ok().map(|_| label)
}

/// Seconds since the last real input anywhere in this session.
fn idle_secs() -> u64 {
    let mut info = LASTINPUTINFO {
        cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32,
        dwTime: 0,
    };
    unsafe {
        if !GetLastInputInfo(&mut info).as_bool() {
            return 0;
        }
        // Both wrap after 49 days, together.
        u64::from(GetTickCount().wrapping_sub(info.dwTime) / 1000)
    }
}

/// Seconds since local midnight.
fn local_secs() -> u64 {
    let t = unsafe { GetLocalTime() };
    u64::from(t.wHour) * 3600 + u64::from(t.wMinute) * 60 + u64::from(t.wSecond)
}

fn create_app_window() -> windows::core::Result<HWND> {
    unsafe {
        let instance = GetModuleHandleW(None)?;
        let wc = WNDCLASSW {
            lpfnWndProc: Some(app_proc),
            hInstance: instance.into(),
            lpszClassName: APP_CLASS,
            ..Default::default()
        };
        RegisterClassW(&wc);
        // Never shown. A tool window, so it can never reach alt-tab.
        CreateWindowExW(
            WS_EX_TOOLWINDOW,
            APP_CLASS,
            w!("Horadric"),
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(instance.into()),
            None,
        )
    }
}

unsafe extern "system" fn app_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == TASKBAR_CREATED.with(Cell::get) {
        with_app(|app| app.tray.show());
        return LRESULT(0);
    }
    match msg {
        // Windows is shutting down or logging off. Save now, while every
        // session still runs: once their processes are killed they would
        // look like crashes.
        WM_QUERYENDSESSION => {
            with_app(App::freeze);
            return LRESULT(1);
        }
        // A screen came or went, changed resolution or scale, or the
        // taskbar moved. Laid out now and once more when the timer fires.
        WM_DISPLAYCHANGE | WM_SETTINGCHANGE
            if msg == WM_DISPLAYCHANGE || wparam.0 as u32 == SPI_SETWORKAREA.0 =>
        {
            with_app(App::screen_changed);
            SetTimer(Some(hwnd), SCREEN_TIMER, 1000, None);
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        }
        // Menus and dialogs run modal loops that dispatch messages, including
        // ours. They run here, with the app not borrowed, so those messages
        // are handled rather than bounced.
        WM_HORADRIC_TRAY => {
            let mouse = lparam.0 as u32;
            if mouse == WM_LBUTTONUP || mouse == WM_RBUTTONUP {
                tray_menu(hwnd);
            } else if mouse == NIN_BALLOONUSERCLICK {
                with_app(App::open_alert);
            }
            return LRESULT(0);
        }
        WM_WTSSESSION_CHANGE => {
            match wparam.0 {
                WTS_SESSION_LOCK => {
                    with_app(|app| app.away.lock(unix_now()));
                }
                WTS_SESSION_UNLOCK => {
                    if let Some(since) = with_app(|app| app.away.unlock()).flatten() {
                        with_app(|app| app.welcome_back(since));
                    }
                }
                _ => {}
            }
            return LRESULT(0);
        }
        WM_HORADRIC_PICK => {
            let start = with_app(|app| app.pick_from.take()).flatten();
            pick_and_start(hwnd, start);
            return LRESULT(0);
        }
        WM_HORADRIC_TILE_MENU => {
            if let Some(id) = with_app(|app| app.menu_for.take()).flatten() {
                tile_menu(&id);
            }
            return LRESULT(0);
        }
        WM_HORADRIC_STASH_MENU => {
            if let Some(id) = with_app(|app| app.stash_menu_for.take()).flatten() {
                stash_menu(&id);
            }
            return LRESULT(0);
        }
        WM_HORADRIC_RECENT_MENU => {
            if let Some(dir) = with_app(|app| app.recent_menu_for.take()).flatten() {
                recent_menu(dir);
            }
            return LRESULT(0);
        }
        WM_HORADRIC_PROJECT_MENU => {
            if let Some(key) = with_app(|app| app.project_menu_for.take()).flatten() {
                project_menu(&key);
            }
            return LRESULT(0);
        }
        WM_HORADRIC_SETTING_MENU => {
            // Outside the app's borrow: the list takes the focus as it opens,
            // and the windows losing it are the app's.
            let want = with_app(|app| {
                let (setting, row) = app.setting_menu_for.take()?;
                let current = app
                    .shared
                    .defaults
                    .borrow()
                    .get(setting)
                    .map(str::to_string);
                let dpi = app.usage_window.as_ref().map_or(96, |u| u.dpi());
                Some((Rc::clone(&app.shared), setting, current, row, dpi))
            })
            .flatten();
            if let Some((shared, setting, current, row, dpi)) = want {
                match Dropdown::open(shared, setting, current.as_deref(), row, dpi) {
                    Ok(d) => {
                        with_app(|app| app.dropped(d));
                    }
                    Err(e) => eprintln!("horadric: cannot open a setting's list: {e}"),
                }
            }
            return LRESULT(0);
        }
        WM_HORADRIC_TASK_MENU => {
            if let Some(menu) = with_app(|app| app.tasks.menu.take()).flatten() {
                runner::show_menu(menu);
            }
            return LRESULT(0);
        }
        _ => {}
    }
    let ours = matches!(
        msg,
        WM_HORADRIC_EVENT
            | WM_HORADRIC_INPUT
            | WM_HORADRIC_OUTPUT
            | WM_HORADRIC_EXIT
            | WM_HORADRIC_NEW
            | WM_HORADRIC_WINDOW_SHOWN
            | WM_HORADRIC_WINDOW_GONE
            | WM_HORADRIC_COUNTED
            | WM_HORADRIC_UPDATE
            | WM_HORADRIC_DOWNLOADED
            | WM_HORADRIC_KEPT
            | WM_HOTKEY
            | WM_TIMER
    );
    if !ours {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    // The app is busy when a call it made pumped messages. Try again later
    // rather than touch it twice. A missed timer tick is not worth repeating.
    if with_app(|app| app.on_message(msg, wparam.0)).is_none() && msg != WM_TIMER {
        let _ = PostMessageW(Some(hwnd), msg, wparam, lparam);
    }
    LRESULT(0)
}

/// Runs `f` on the app unless it is already borrowed.
fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|a| a.try_borrow_mut().ok()?.as_mut().map(f))
}

fn tray_menu(hwnd: HWND) {
    let (recent, hotkeys, notify, sounds, terminal, update, xp) = with_app(|app| {
        let xp = app.experience.lock().ok().and_then(|e| *e);
        app.count_experience();
        (
            app.recent.clone(),
            [app.hotkey, app.listen_key],
            !app.quiet,
            app.sounds,
            !app.consoles.is_empty(),
            app.update.as_ref().map(|m| m.version.clone()),
            xp,
        )
    })
    .unwrap_or_default();
    let projects: Vec<String> = recent
        .into_iter()
        .filter(|p| Path::new(p).is_dir())
        .collect();
    let autostart = (!horadric_hooks::dev()).then(autostart::is_enabled);
    let past: Vec<Vec<Past>> = projects
        .iter()
        .map(|p| with_app(|app| app.history(Path::new(p))).unwrap_or_default())
        .collect();
    let lines = past
        .iter()
        .enumerate()
        .map(|(i, p)| history_items(p, tray::HISTORY + i * history::SPAN))
        .collect();
    let (screens, chosen) = (monitors(), with_app(|app| app.screen.clone()).flatten());
    let shown = screens::pick(&screens, chosen.as_deref()).map(|s| s.name.clone());
    let (fonts, font) = with_app(|app| {
        (
            glyphs::monospaced(&app.shared.gpu.dw),
            app.shared.font.family(),
        )
    })
    .unwrap_or_default();
    let menu = tray::menu(
        &projects,
        lines,
        autostart,
        hotkeys,
        notify,
        sounds,
        terminal,
        &screens,
        shown.as_deref(),
        update.as_deref(),
        &fonts,
        &font,
        xp,
    );
    match menu {
        Some(Choice::ToggleNotify) => {
            with_app(|app| {
                app.quiet = !app.quiet;
                app.save();
            });
        }
        Some(Choice::ToggleSounds) => {
            with_app(|app| {
                app.sounds = !app.sounds;
                app.save();
                // So the choice is heard the moment it is made.
                app.sound(Loot::Drop);
            });
        }
        Some(Choice::New) => pick_and_start(hwnd, projects.first().map(PathBuf::from)),
        Some(Choice::Recent(path)) => start_logged(PathBuf::from(path)),
        Some(Choice::History(id)) => {
            if let Some((i, pick)) = history::pick_nested(id, tray::HISTORY) {
                if let (Some(dir), Some(past)) = (projects.get(i), past.get(i)) {
                    with_app(|app| app.reopen(Path::new(dir), past, pick));
                }
            }
        }
        Some(Choice::Tidy) => {
            with_app(App::tidy);
        }
        Some(Choice::Raise) => {
            with_app(App::raise);
        }
        Some(Choice::ShowStage) => {
            with_app(App::show_stage);
        }
        Some(Choice::NextWaiting) => {
            with_app(App::next_waiting);
        }
        Some(Choice::Listen) => {
            with_app(App::listen_on_demand);
        }
        Some(Choice::Arrange) => {
            with_app(App::fit_stage);
        }
        Some(Choice::Screen(i)) => {
            if let Some(screen) = screens.get(i) {
                with_app(|app| app.move_to_screen(screens::choice(screen)));
                // The tiles take the new screen's DPI when they get there,
                // and lay out again for it.
                unsafe { SetTimer(Some(hwnd), SCREEN_TIMER, 1000, None) };
            }
        }
        Some(Choice::Font(i)) => {
            if let Some(family) = fonts.get(i) {
                with_app(|app| app.set_font_family(family));
            }
        }
        Some(Choice::ToggleAutostart) => {
            if autostart::is_enabled() {
                autostart::disable();
            } else {
                autostart::enable();
            }
        }
        Some(Choice::CheckUpdates) => {
            with_app(|app| app.check_update(true));
        }
        Some(Choice::Update) => {
            with_app(|app| app.install_update());
        }
        Some(Choice::EndAll) => {
            if confirm_end(None) {
                with_app(|app| app.end_all(None));
            }
        }
        Some(Choice::Quit) => {
            let (agents, shells) = with_app(|app| app.live_counts()).unwrap_or((0, 0));
            let working = with_app(|app| app.mid_turn_count()).unwrap_or(0);
            let keep = match quit_question(agents, shells) {
                Some(q) => ask(&Dialog {
                    tone: Tone::Question,
                    title: "Quit Horadric",
                    text: &q,
                    buttons: &["Keep running", "Stop them", "Cancel"],
                    default: if working > 0 { 0 } else { 1 },
                })
                .and_then(|b| (b < 2).then_some(b == 0)),
                None => Some(false),
            };
            if let Some(keep) = keep {
                with_app(|app| app.quit(!keep));
                unsafe { PostQuitMessage(0) };
            }
        }
        None => {}
    }
}

/// Where to look for a release, or None for a dev instance that was not
/// pointed anywhere.
fn update_url() -> Option<String> {
    let env = std::env::var("HORADRIC_UPDATE_URL").ok();
    release::manifest_url(
        horadric_hooks::dev(),
        cfg!(debug_assertions),
        env.as_deref(),
    )
}

/// The projects to try showing on the stage, best first: the one it last
/// showed, while its cluster is still there, then the clusters in order.
fn stage_order(last: Option<&str>, keys: &[String]) -> Vec<String> {
    let mut order: Vec<String> = keys
        .iter()
        .filter(|k| Some(k.as_str()) == last)
        .cloned()
        .collect();
    order.extend(keys.iter().filter(|k| Some(k.as_str()) != last).cloned());
    order
}

/// What to ask before quitting with `agents` sessions and `shells` plain
/// terminals running: whether they keep running without Horadric. Nothing
/// when none is.
fn quit_question(agents: usize, shells: usize) -> Option<String> {
    let what = match (agents, shells) {
        (0, 0) => return None,
        (1, 0) => "the running session".to_string(),
        (n, 0) => format!("the {n} running sessions"),
        (0, 1) => "the open terminal".to_string(),
        (0, n) => format!("the {n} open terminals"),
        (1, 1) => "the running session and the open terminal".to_string(),
        (a, s) => format!(
            "{} and {}",
            plural(a, "running session"),
            plural(s, "open terminal")
        ),
    };
    Some(format!(
        "Keep {what} going without Horadric?\n\n\
         Kept running, they carry on, and their tiles come back as they are when \
         Horadric starts again.\n\n\
         Stopped, a session comes back as a paused tile and resumes where it left off. A \
         terminal closes, and whatever runs in it."
    ))
}

fn plural(n: usize, what: &str) -> String {
    match n {
        1 => format!("1 {what}"),
        n => format!("{n} {what}s"),
    }
}

/// What a tile's menu can offer for its session.
enum TileKind {
    /// Horadric runs it right now.
    Live,
    /// Horadric can resume it.
    Paused,
    /// Started with `horadric run` in someone else's terminal.
    Elsewhere,
    /// A Claude Code background session no pane is attached to.
    Background,
}

fn tile_menu(id: &str) {
    const OPEN: usize = 1;
    const END: usize = 2;
    const RENAME: usize = 3;
    const STASH: usize = 4;
    const PICK: usize = 5;
    const STOP_RUNEWORD: usize = 6;
    let Some((kind, shell)) = with_app(|app| app.tile_kind(id)).flatten() else {
        return;
    };
    let rename = Item::action(RENAME, "Rename\u{2026}");
    let full = with_app(|app| app.shared.registry.lock().is_ok_and(|r| r.stash_full()));
    let stash = match full {
        Some(false) => Item::action(STASH, "Stash"),
        _ => Item::Disabled("Stash is full".into()),
    };
    let restful = matches!(kind, TileKind::Live | TileKind::Paused);
    let items = match (kind, shell) {
        (TileKind::Live, false) => vec![
            Item::action(OPEN, "Show terminal"),
            rename,
            stash,
            Item::Separator,
            Item::action(END, "End session"),
        ],
        (TileKind::Live, true) => vec![
            Item::action(OPEN, "Show terminal"),
            rename,
            Item::Separator,
            Item::action(END, "Close terminal"),
        ],
        (TileKind::Paused, false) => vec![
            Item::action(OPEN, "Resume"),
            rename,
            stash,
            Item::Separator,
            Item::action(END, "End session"),
        ],
        (TileKind::Paused, true) => vec![
            Item::action(OPEN, "Open again"),
            rename,
            Item::Separator,
            Item::action(END, "Remove terminal"),
        ],
        (TileKind::Elsewhere, _) => vec![rename, Item::action(END, "Remove tile")],
        (TileKind::Background, _) => vec![
            Item::action(OPEN, "Attach"),
            rename,
            Item::Separator,
            Item::action(END, "End session"),
        ],
    };
    let mut items = items;
    let runewords = with_app(|app| app.runewords_of(id)).flatten();
    let offered = match &runewords {
        Some((offered, _)) if restful => offered.clone(),
        _ => Vec::new(),
    };
    if let Some((_, word)) = &runewords {
        let entry = match word {
            Some(w) => Some(Item::action(
                STOP_RUNEWORD,
                format!("Stop {} ({})", w.name, w.progress()),
            )),
            None if !offered.is_empty() => Some(Item::Submenu(
                "Runeword".into(),
                offered
                    .iter()
                    .enumerate()
                    .map(|(i, (name, _))| Item::action(RUNEWORD + i, name.clone()))
                    .collect(),
            )),
            None => None,
        };
        // With the other things done to the session, above ending it.
        if let (Some(entry), Some(at)) = (
            entry,
            items.iter().rposition(|i| matches!(i, Item::Separator)),
        ) {
            items.insert(at, entry);
        }
    }
    let tree = with_app(|app| app.diff_of(id)).flatten();
    if let Some((w, diff)) = &tree {
        items.insert(0, changes_menu(w, diff.as_ref()));
        items.insert(1, Item::Separator);
    }
    if with_app(|app| app.can_pick(id)) == Some(true) {
        items.insert(0, Item::action(PICK, "Pick this tomb\u{2026}"));
        items.insert(1, Item::Separator);
    }
    let picked = menu::popup(&items);
    if let (Some(i), Some((w, diff))) = (picked, &tree) {
        let file = diff.as_ref().and_then(|d| match i {
            _ if (UNCOMMITTED..COMMITTED).contains(&i) => d.uncommitted.get(i - UNCOMMITTED),
            _ if (COMMITTED..COMMITTED_END).contains(&i) => d.committed.get(i - COMMITTED),
            _ => None,
        });
        if let Some(f) = file {
            let dir = PathBuf::from(&w.path);
            with_app(|app| {
                if let Some(key) = app.project_of(id) {
                    app.open_view(&key, &dir, &f.path);
                }
            });
            return;
        }
        if i == CODE {
            watch::open_in_code(Path::new(&w.path));
            return;
        }
    }
    match picked {
        Some(OPEN) => {
            with_app(|app| app.reveal(id, false));
        }
        Some(END) => {
            with_app(|app| app.end(id));
        }
        Some(RENAME) => rename_session(id),
        Some(PICK) => runner::tomb::ask_pick(id),
        Some(STOP_RUNEWORD) => {
            with_app(|app| app.stop_runeword(id));
        }
        Some(i) if (RUNEWORD..RUNEWORD + offered.len()).contains(&i) => {
            let (name, runes) = offered[i - RUNEWORD].clone();
            with_app(|app| app.give_runeword(id, &name, runes));
        }
        Some(STASH) if confirm_stash(id) => {
            with_app(|app| app.stash(id));
        }
        _ => {}
    }
}

/// Asks before stashing a session mid turn, which stops the turn. One
/// waiting or at its prompt loses nothing: it resumes to the same place.
fn confirm_stash(id: &str) -> bool {
    let busy = with_app(|app| {
        app.consoles
            .get(id)
            .is_some_and(|c| c.exit_code().is_none())
            && app
                .shared
                .registry
                .lock()
                .is_ok_and(|r| r.get(id).is_some_and(|s| s.phase.mid_turn()))
    });
    if busy != Some(true) {
        return true;
    }
    let pressed = ask(&Dialog {
        tone: Tone::Warning,
        title: "Stash session",
        text: "It is mid turn. Stashing stops it now, and the turn is cut short. A click \
               on it in the stash resumes the conversation.",
        buttons: &["Stash", "Cancel"],
        default: 1,
    });
    pressed == Some(0)
}

/// The menu for a stashed session: bring it back running or paused, or
/// end it for good.
fn stash_menu(id: &str) {
    const BACK: usize = 1;
    const PAUSED: usize = 2;
    const END: usize = 3;
    let items = vec![
        Item::action(BACK, "Bring back"),
        Item::action(PAUSED, "Bring back paused"),
        Item::Separator,
        Item::action(END, "End session"),
    ];
    match menu::popup(&items) {
        Some(BACK) => {
            with_app(|app| app.unstash(id, true));
        }
        Some(PAUSED) => {
            with_app(|app| app.unstash(id, false));
        }
        Some(END) => {
            with_app(|app| app.end_stashed(id));
        }
        _ => {}
    }
}

/// Where the tile menu's changed files start: those not committed, then
/// those committed on the branch, then VS Code.
const UNCOMMITTED: usize = 100;
const COMMITTED: usize = 400;
const COMMITTED_END: usize = 700;
/// Where the runewords a session can be given start in its tile menu.
const RUNEWORD: usize = 800;
const CODE: usize = 700;
/// The most files a half of the changes lists. A menu taller than the
/// screen scrolls by the pixel, which is no way to look at a change.
const CHANGES_LISTED: usize = 40;

/// What a session's worktree changed, as a submenu: each file with its
/// counts, the ones not committed first, and the worktree in VS Code.
fn changes_menu(w: &Worktree, diff: Option<&Diff>) -> Item {
    let empty = Diff::default();
    let d = diff.unwrap_or(&empty);
    let mut items = vec![Item::Disabled("Not committed".into())];
    let list = |items: &mut Vec<Item>, files: &[FileDiff], first: usize| {
        for (i, f) in files.iter().take(CHANGES_LISTED).enumerate() {
            items.push(Item::action(first + i, changes::menu_label(f)));
        }
        if files.len() > CHANGES_LISTED {
            let more = files.len() - CHANGES_LISTED;
            items.push(Item::Disabled(format!("and {more} more")));
        }
        if files.is_empty() {
            items.push(Item::Disabled("Nothing".into()));
        }
    };
    list(&mut items, &d.uncommitted, UNCOMMITTED);
    items.extend([
        Item::Separator,
        Item::Disabled(format!("Committed on {}", w.branch)),
    ]);
    list(&mut items, &d.committed, COMMITTED);
    items.extend([
        Item::Separator,
        if watch::vs_code().is_some() {
            Item::action(CODE, "Open worktree in VS Code")
        } else {
            Item::Disabled("Open worktree in VS Code".into())
        },
    ]);
    let label = match diff {
        None => "Changes".to_string(),
        Some(d) if d.is_empty() => "No changes".to_string(),
        Some(d) => format!("Changes\t{}", changes::glance(d.totals())),
    };
    Item::Submenu(label, items)
}

/// Asks for a session's new name. An empty one gives the naming back to
/// Claude's own title.
fn rename_session(id: &str) {
    let Some((label, key)) = with_app(|app| {
        let r = app.shared.registry.lock().ok()?;
        let s = r.get(id)?;
        Some((s.label().to_string(), project_key(s)))
    })
    .flatten() else {
        return;
    };
    let question = ask::Ask {
        title: "Rename session",
        prompt: "Leave it empty and Claude names it again.",
        initial: &label,
        placeholder: "Claude's own title",
        verb: "rename",
        notes: false,
        pick: None,
    };
    if let Some(a) = ask_beside(Some(&key), &question) {
        with_app(|app| app.rename(id, &a.text));
    }
}

/// Asks with buttons, outside the app's borrow: the dialog runs a modal
/// loop that dispatches the app's messages. The button pressed, if one was.
pub(crate) fn ask(d: &Dialog) -> Option<usize> {
    let shared = with_app(|app| Rc::clone(&app.shared))?;
    dialog::show(&shared.gpu, &shared.metrics, d)
}

/// Asks beside the cluster of project `key`, outside the app's borrow: the
/// question runs a modal loop that dispatches the app's messages.
fn ask_beside(key: Option<&str>, question: &ask::Ask) -> Option<ask::Answer> {
    let (shared, beside) = with_app(|app| {
        let beside = key
            .and_then(|k| app.clusters.iter().find(|c| c.key == k))
            .map(|c| c.hwnd);
        (Rc::clone(&app.shared), beside)
    })?;
    ask::ask(shared, beside, question)
}

/// How long Claude Code may take to save a switched model or effort as the
/// default. It gives itself three seconds.
const SAVED_WITHIN: Duration = Duration::from_secs(5);

/// How many sessions a batch starts: enough to work on several things at
/// once, few enough to keep an eye on.
const BATCH: usize = 4;

/// How many past conversations the History menu lists. The last line
/// opens Claude Code's own picker for the rest.
const HISTORY: usize = 10;

/// How many hosts the project menu lists one by one. Past that they are
/// found by name instead.
const MENU_HOSTS: usize = 8;

/// What a session's console runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Run {
    Agent,
    Shell,
    /// `ssh` to this host, a shell on another machine.
    Ssh(String),
    /// `claude attach` to the background session with this short id. The
    /// daemon runs the agent; the pane only shows it.
    Attach(String),
}

fn project_menu(key: &str) {
    const ADD: usize = 1;
    const START_BATCH: usize = 2;
    const START_OVER: usize = 3;
    const END_ALL: usize = 4;
    const SHELL: usize = 5;
    const CODE: usize = 7;
    const EXPLORE: usize = 8;
    const OTHER_HOST: usize = 9;
    const WORKTREES: usize = 10;
    const SSH_FIND: usize = 11;
    const SSH: usize = 20;
    const PAST: usize = 100;
    const SUGGEST: usize = 200;
    const SUGGEST_END: usize = 300;
    const MERGE: usize = 300;
    const MERGE_END: usize = 400;
    let dir = with_app(|app| app.project_dir(key)).flatten();
    let past = match &dir {
        Some(d) => with_app(|app| app.history(d)).unwrap_or_default(),
        None => Vec::new(),
    };
    let hosts = dir
        .as_deref()
        .map(horadric_hooks::tasks::hosts)
        .unwrap_or_default();
    let inventory = dir.as_deref().and_then(horadric_hooks::tasks::fleet);
    // A fleet, or more hosts than a menu reads well, is found by name.
    let finding = inventory.is_some() || hosts.len() > MENU_HOSTS;
    let devices = fleet::with_hosts(&hosts, inventory.map(|(_, d)| d).unwrap_or_default());
    let listed: &[String] = if finding { &[] } else { &hosts };
    let mut suggested = horadric_hooks::tasks::ssh_config_hosts();
    suggested.retain(|h| !hosts.contains(h));
    let offering = suggested.len() <= MENU_HOSTS;
    let mut merges = match &dir {
        Some(d) => with_app(|app| app.merges(d)).unwrap_or_default(),
        None => Vec::new(),
    };
    merges.truncate(MERGE_END - MERGE);
    let mut items = vec![
        Item::action(ADD, "New session"),
        Item::Submenu("History".into(), history_items(&past, PAST)),
        Item::action(START_BATCH, format!("Start {BATCH} sessions")),
        Item::action(START_OVER, format!("Start over with {BATCH} sessions")),
        Item::Separator,
        Item::action(SHELL, "New terminal\tCtrl+Shift+T"),
    ];
    for (i, host) in listed.iter().enumerate() {
        items.push(Item::action(SSH + i, format!("SSH to {host}")));
    }
    if finding {
        items.push(Item::action(SSH_FIND, "SSH to..."));
    }
    if dir.is_some() {
        items.push(if suggested.is_empty() || !offering {
            Item::action(OTHER_HOST, "Add host")
        } else {
            let mut offer: Vec<Item> = suggested
                .iter()
                .enumerate()
                .map(|(i, h)| Item::action(SUGGEST + i, h.as_str()))
                .collect();
            offer.extend([Item::Separator, Item::action(OTHER_HOST, "Other host")]);
            Item::Submenu("Add host".into(), offer)
        });
    }
    // Only a repository's main tree can add worktrees.
    let own_trees = dir
        .as_deref()
        .filter(|d| worktree::main_tree(d).is_some())
        .map(|d| horadric_hooks::tasks::worktrees(d).enabled);
    if let Some(on) = own_trees {
        items.extend([
            Item::Separator,
            Item::Action {
                id: WORKTREES,
                label: "A worktree for each new session".into(),
                checked: on,
            },
        ]);
    }
    if !merges.is_empty() {
        let into = merges
            .first()
            .and_then(|m| worktree::checked_out(&m.main))
            .unwrap_or_else(|| "main".into());
        items.push(Item::Separator);
        for (i, m) in merges.iter().enumerate() {
            items.push(Item::action(
                MERGE + i,
                format!("Merge {} into {into}", m.branch),
            ));
        }
    }
    items.extend([
        Item::Separator,
        if watch::vs_code().is_some() {
            Item::action(CODE, "Open in VS Code")
        } else {
            Item::Disabled("Open in VS Code".into())
        },
        Item::action(EXPLORE, "Open in Explorer"),
        Item::Separator,
        Item::action(END_ALL, "End all sessions"),
    ]);
    let picked = menu::popup(&items);
    match (picked, &dir) {
        (Some(i), Some(dir)) if (SUGGEST..SUGGEST_END).contains(&i) => {
            return add_host(dir, &suggested[i - SUGGEST]);
        }
        (Some(OTHER_HOST), Some(dir)) => {
            return ask_host(key, dir, if offering { &[] } else { &suggested });
        }
        (Some(SSH_FIND), Some(_)) => return ssh_to(key, &hosts, &devices),
        (Some(WORKTREES), Some(dir)) => {
            let on = own_trees.unwrap_or(false);
            if let Err(e) = horadric_hooks::tasks::set_worktrees(dir, !on) {
                eprintln!("horadric: cannot write the project's config: {e}");
            }
            return;
        }
        _ => {}
    }
    let ending = matches!(picked, Some(START_OVER | END_ALL));
    if ending && !confirm_end(Some(key)) {
        return;
    }
    with_app(|app| match picked {
        Some(ADD) => app.add_sessions(key, 1),
        Some(START_BATCH) => app.add_sessions(key, BATCH),
        Some(START_OVER) => app.start_over(key, BATCH),
        Some(END_ALL) => app.end_all(Some(key)),
        Some(SHELL) => app.open_shell(key),
        Some(i) if (SSH..PAST).contains(&i) => app.open_ssh(key, &listed[i - SSH], None),
        Some(i) if (MERGE..MERGE_END).contains(&i) => {
            app.merge(&merges[i - MERGE]);
        }
        Some(CODE) => {
            if let Some(dir) = app.project_dir(key) {
                watch::open_in_code(&dir);
            }
        }
        Some(EXPLORE) => {
            if let Some(dir) = app.project_dir(key) {
                watch::explore(&dir);
            }
        }
        Some(i) => {
            if let (Some(pick), Some(dir)) = (history::pick(i, PAST), &dir) {
                app.reopen(dir, &past, pick);
            }
        }
        _ => {}
    });
}

/// Asks which of the project's hosts and devices to open a terminal on,
/// found as it is typed by name, group or address: a fleet is too many
/// for a menu. A device from the inventory names its terminal, since "SSH
/// 7" says nothing among dozens. Anything else `ssh` takes connects too.
fn ssh_to(key: &str, hosts: &[String], devices: &[Device]) {
    let suggest = |text: &str| {
        fleet::find(devices, text, ask::LIST_ROWS)
            .into_iter()
            .map(|d| ask::Suggestion {
                label: d.name.clone(),
                detail: match &d.group {
                    Some(g) => g.clone(),
                    None if d.destination != d.name => d.destination.clone(),
                    None => String::new(),
                },
                value: d.name.clone(),
            })
            .collect()
    };
    let check = |text: &str| ssh::refusal(text, devices.iter().any(|d| d.name == text.trim()));
    let pick = ask::Pick {
        suggest: &suggest,
        check: &check,
        browse: false,
        glyph: crate::theme::SSH_ICON,
        paths: false,
    };
    let prompt = format!(
        "{} to pick from. Type part of a name, a group or an address.",
        devices.len()
    );
    let question = ask::Ask {
        title: "SSH to",
        prompt: &prompt,
        initial: "",
        placeholder: "name, group or address",
        verb: "connect",
        notes: false,
        pick: Some(&pick),
    };
    let Some(a) = ask_beside(Some(key), &question) else {
        return;
    };
    let text = a.text.trim();
    let device = devices.iter().find(|d| d.name == text);
    with_app(|app| match device {
        Some(d) if !hosts.contains(&d.name) => app.open_ssh(key, &d.destination, Some(&d.name)),
        Some(d) => app.open_ssh(key, &d.destination, None),
        None => app.open_ssh(key, text, None),
    });
}

/// Asks for a host to add to the project, anything `ssh` takes. With
/// `offer`, the `~/.ssh/config` names too many for a submenu, those are
/// found as it is typed.
fn ask_host(key: &str, dir: &Path, offer: &[String]) {
    let names = fleet::with_hosts(offer, Vec::new());
    let suggest = |text: &str| {
        fleet::find(&names, text, ask::LIST_ROWS)
            .into_iter()
            .map(|d| ask::Suggestion {
                label: d.name.clone(),
                detail: String::new(),
                value: d.name.clone(),
            })
            .collect()
    };
    let check = |text: &str| ssh::refusal(text, false);
    let pick = ask::Pick {
        suggest: &suggest,
        check: &check,
        browse: false,
        glyph: crate::theme::SSH_ICON,
        paths: false,
    };
    let question = ask::Ask {
        title: "Add host",
        prompt: "Anything ssh takes: an alias from ~/.ssh/config, or user@address.",
        initial: "",
        placeholder: "user@address",
        verb: "add it",
        notes: false,
        pick: (!offer.is_empty()).then_some(&pick),
    };
    if let Some(a) = ask_beside(Some(key), &question) {
        let host = a.text.trim();
        if !host.is_empty() {
            add_host(dir, host);
        }
    }
}

/// Writes `host` into the project's config. The menu reads the hosts each
/// time it opens and sessions at their next start, so nothing else is
/// told.
fn add_host(dir: &Path, host: &str) {
    if let Err(e) = horadric_hooks::tasks::add_host(dir, host) {
        eprintln!("horadric: cannot add host {host}: {e}");
    }
}

/// The lines of a History menu: each past conversation, `first` on, then
/// Claude Code's own picker for the rest.
fn history_items(past: &[Past], first: usize) -> Vec<Item> {
    let now = SystemTime::now();
    let mut items: Vec<Item> = past
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let ago = now.duration_since(p.modified).unwrap_or_default();
            Item::action(first + i, history::label(&p.title.text, ago))
        })
        .collect();
    if items.is_empty() {
        items.push(Item::Disabled("No earlier conversations".into()));
    }
    items.push(Item::Separator);
    items.push(Item::action(history::all(first), "All conversations..."));
    items
}

/// A recent project right clicked in the start window, which has no
/// cluster and so no project menu: a new session, or an old one back.
fn recent_menu(dir: PathBuf) {
    const ADD: usize = 1;
    const PAST: usize = 100;
    let past = with_app(|app| app.history(&dir)).unwrap_or_default();
    let mut items = vec![Item::action(ADD, "New session"), Item::Separator];
    items.extend(history_items(&past, PAST));
    match menu::popup(&items) {
        Some(ADD) => start_logged(dir),
        Some(i) => {
            if let Some(pick) = history::pick(i, PAST) {
                with_app(|app| app.reopen(&dir, &past, pick));
            }
        }
        None => {}
    }
}

/// Asks before ending running sessions. Paused ones cost nothing to lose:
/// their conversations stay on disk for `claude --resume`.
fn confirm_end(key: Option<&str>) -> bool {
    let (live, working) = with_app(|app| app.running_in(key)).unwrap_or((0, 0));
    let project = key.map(project_name);
    match end_question(project.as_deref(), live, working) {
        Some(q) => {
            let title = match live {
                1 => "End session",
                _ => "End sessions",
            };
            let pressed = ask(&Dialog {
                tone: Tone::Warning,
                title,
                text: &q,
                buttons: &["End", "Cancel"],
                default: 0,
            });
            pressed == Some(0)
        }
        None => true,
    }
}

/// What to ask before ending `live` running sessions, `working` of them mid
/// turn, in one project or in all of them. Nothing when none is running.
fn end_question(project: Option<&str>, live: usize, working: usize) -> Option<String> {
    if live == 0 {
        return None;
    }
    let what = match live {
        1 => "the running session".to_string(),
        n => format!("all {n} running sessions"),
    };
    let place = match project {
        Some(p) => format!(" in {p}"),
        None => String::new(),
    };
    let busy = match working {
        0 => String::new(),
        1 if live == 1 => " It is in the middle of a turn.".to_string(),
        1 => " One of them is in the middle of a turn.".to_string(),
        n => format!(" {n} of them are in the middle of a turn."),
    };
    Some(format!("End {what}{place}?{busy}"))
}

fn pick_and_start(hwnd: HWND, start: Option<PathBuf>) {
    if let Some(dir) = pick_folder(hwnd, start) {
        start_logged(dir);
    }
}

/// Asks where to start a session: a path typed with its folders offered
/// as it goes, the recent projects while it is empty, and Explorer's
/// picker behind Browse for a folder easier found by looking. Browse
/// starts in what was typed if it is a folder, else in `start`.
fn pick_folder(hwnd: HWND, start: Option<PathBuf>) -> Option<PathBuf> {
    let (shared, recent) = with_app(|app| (Rc::clone(&app.shared), app.recent.clone()))?;
    let home = std::env::var("USERPROFILE").ok();
    let suggest = |text: &str| {
        paths::offers(text, &recent, home.as_deref(), folders_in, ask::LIST_ROWS)
            .into_iter()
            .map(|o| ask::Suggestion {
                label: o.label,
                detail: o.detail,
                value: o.value,
            })
            .collect()
    };
    let check = |text: &str| {
        let dir = paths::expand(text.trim(), home.as_deref());
        if dir.is_empty() {
            Some("Type a folder, or pick one from the list".to_string())
        } else if !Path::new(&dir).is_dir() {
            Some(format!("There is no folder {dir}"))
        } else {
            None
        }
    };
    let pick = ask::Pick {
        suggest: &suggest,
        check: &check,
        browse: true,
        glyph: '\u{E8B7}',
        paths: true,
    };
    let question = ask::Ask {
        title: "Start a session",
        prompt: "In which folder? Type a path, or pick a recent project.",
        initial: "",
        placeholder: "C:\\path\\to\\project",
        verb: "start",
        notes: false,
        pick: Some(&pick),
    };
    let a = ask::ask(shared, None, &question)?;
    let typed = PathBuf::from(paths::expand(a.text.trim(), home.as_deref()));
    if a.browse {
        let from = Some(typed).filter(|p| p.is_dir()).or(start);
        return picker::pick_folder(hwnd, from.as_deref());
    }
    Some(typed)
}

/// The folders in `dir` by name, for the picker, leaving out those Windows
/// hides.
fn folders_in(dir: &str) -> Vec<String> {
    use std::os::windows::fs::MetadataExt;
    const HIDDEN: u32 = 0x2;
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter(|e| {
            e.metadata()
                .is_ok_and(|m| m.is_dir() && m.file_attributes() & HIDDEN == 0)
        })
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    names.sort_by_key(|n| n.to_lowercase());
    names
}

fn start_logged(dir: PathBuf) {
    if let Some(Err(e)) = with_app(|app| app.start(None, dir, Vec::new())) {
        eprintln!("horadric: cannot start session: {e}");
    }
}

struct App {
    shared: Rc<Shared>,
    /// The account's usage and the defaults for new sessions.
    usage_window: Option<Box<UsageWindow>>,
    /// The sessions put away for later, there while it holds any.
    stash_window: Option<Box<StashWindow>>,
    /// The cube, there while there is anything to drop in it.
    cube_window: Option<Box<CubeWindow>>,
    /// The sessions in the cube, by id, in the order they went in.
    cube: Vec<String>,
    /// `main` is in the cube beside them.
    cube_main: bool,
    /// Stands where the first project will go while none is open.
    start_window: Option<Box<StartWindow>>,
    /// The settings file that gives a session `horadric status` as its status
    /// line. None when it could not be written, and then sessions go without.
    status_settings: Option<PathBuf>,
    /// The setting whose list is about to drop, and its row on screen.
    setting_menu_for: Option<(Setting, RECT)>,
    /// A setting's list, while it is dropped down.
    dropdown: Option<Box<Dropdown>>,
    /// Slash commands waiting to be typed into running sessions, by session
    /// id, for settings picked since they started: each goes in once the
    /// session is free for it, see [`Session::free_for_command`].
    switches: HashMap<String, Vec<(Setting, String)>>,
    /// The user's own default model and effort, from before the first of
    /// those commands went in, and when the last one did. Claude Code saves
    /// what they pick as the default for every new session, even outside
    /// Horadric, so these go back once it has.
    settings_before: Option<(Vec<Option<serde_json::Value>>, Instant)>,
    // Boxed on purpose: the window procedures hold a raw pointer to each
    // window struct, so it must not move when the Vec grows.
    #[allow(clippy::vec_box)]
    clusters: Vec<Box<Cluster>>,
    /// Sessions Horadric started itself, by session id. A `horadric run`
    /// session has a tile but no console: its terminal is somewhere else.
    consoles: HashMap<String, Arc<Console>>,
    /// Files shown on the stage, at most one per project, by project key.
    /// A click on another file replaces it, like VS Code's preview tab, so
    /// browsing a project never piles up panes.
    views: HashMap<String, Arc<Console>>,
    /// The terminal window, showing one project's sessions, while open.
    stage: Option<Box<TerminalWindow>>,
    /// Where the stage was when it last closed.
    stage_rect: Option<[i32; 4]>,
    /// The project the stage showed when it last closed, which "Show
    /// terminal" in the tray brings back.
    stage_key: Option<String>,
    /// The next waiting session's shortcut, as the tray menu shows it.
    hotkey: Option<&'static str>,
    /// The catch-up's shortcut, likewise.
    listen_key: Option<&'static str>,
    /// Whether you are away, for the catch-up when you come back.
    away: Away,
    /// The catch-up, while it is open.
    catchup: Option<Box<Catchup>>,
    /// Each session's phase as last journaled, with its project and name,
    /// by session id, so only a change is written, and a session that
    /// vanishes can still be named.
    journaled: HashMap<String, (Phase, String, String)>,
    next_serial: usize,
    requests: Arc<Mutex<Vec<Command>>>,
    notify: HWND,
    tray: Tray,
    /// Says what needs saying, over the tray.
    toasts: Toasts,
    /// Windows on their way to their place in the columns.
    glides: RefCell<Glides>,
    /// When the glides last moved on, while they move.
    glided: Cell<Option<Instant>>,
    /// The columns have been laid out with windows in them once.
    arranged: bool,
    /// The key of the window being dragged, which the layout leaves where
    /// the cursor holds it, and the column and place the others make room
    /// for.
    carried: Option<String>,
    carry_at: Option<(usize, usize)>,
    /// Sessions with no process that a click resumes: restored from disk,
    /// or left behind by a crash.
    paused: HashMap<String, SavedSession>,
    /// How each project's cluster was folded, applied when it appears.
    cluster_places: HashMap<String, SavedCluster>,
    /// Which column each cluster and the usage window stand in.
    columns: Columns,
    /// Projects sessions were started in, newest first, for the tray menu.
    recent: Vec<String>,
    autostart_offered: bool,
    last_saved: Option<SavedState>,
    /// Set once Horadric is quitting or Windows is shutting down. The state
    /// on disk is final then: sessions dying on the way out must not be
    /// saved as gone.
    frozen: bool,
    /// Where the next folder picker opens. Set by the plus button.
    pick_from: Option<PathBuf>,
    /// The tile whose menu is about to show.
    menu_for: Option<String>,
    /// The stashed session whose menu is about to show.
    stash_menu_for: Option<String>,
    /// The project whose menu is about to show.
    project_menu_for: Option<String>,
    /// The recent project whose menu is about to show.
    recent_menu_for: Option<PathBuf>,
    /// A new build to hand over to.
    reload: Option<Reload>,
    /// Quit from the tray: the next start resumes no session whose host
    /// is gone. Anything else that ends the app, a reload, a logoff or a
    /// crash, resumes the sessions that were running.
    quit: bool,
    /// Quit chose to stop the sessions rather than leave their hosts
    /// running.
    stop_on_exit: bool,
    /// When this start resumed sessions after a crash, until it has run
    /// long enough not to count as the same crash again.
    recovering: Option<Instant>,
    /// Browser windows sessions opened, by window handle.
    browsers: HashMap<isize, Browser>,
    /// The sessions waiting on you as last seen, so only one that starts
    /// waiting is announced.
    waiting: HashSet<String>,
    /// The session the last notification was about, which a click on it
    /// shows. None when it was about several.
    alert_for: Option<String>,
    /// No notifications, from the tray menu.
    quiet: bool,
    /// Loot sounds, from the tray menu.
    sounds: bool,
    /// The terminal font picked from the tray menu. Kept as picked, so a
    /// family that is uninstalled for a while comes back when it is not.
    font_family: Option<String>,
    /// The screen the columns stand on, by device name. None follows the
    /// primary one.
    screen: Option<String>,
    /// The task lists: what was read, and what the runner is up to.
    tasks: runner::State,
    /// Worktrees just added for sessions about to start, by session id,
    /// with the setup commands to run in them first. Taken by the launch.
    new_trees: HashMap<String, (Worktree, Vec<String>)>,
    /// When each session's worktree is counted again, by session id.
    recounts: HashMap<String, Recount>,
    /// Counts back from their threads.
    counted: Arc<Mutex<Vec<Counted>>>,
    /// Projects whose worktrees are to be swept, by project key: every
    /// project with a session at startup, and one whose session was heard
    /// from since, since that may have been a merge.
    sweep_due: HashSet<String>,
    /// When each project was last swept, by project key.
    swept_at: HashMap<String, Instant>,
    /// The main tree's HEAD at each project's last sweep, by project key.
    /// Nothing is merged while it stays put, so the sweep stops there.
    swept_heads: Arc<Mutex<HashMap<String, String>>>,
    /// A newer release, verified, that the tray offers.
    update: Option<Manifest>,
    /// When the last update check started. The first tick checks.
    last_check: Option<Instant>,
    /// An update check is on its thread.
    checking: bool,
    /// An update check back from its thread, and whether the tray asked
    /// for it.
    looked: Arc<Mutex<Option<(bool, Looked)>>>,
    /// An update is downloading on its thread.
    downloading: bool,
    /// A download back from its thread: the `horadric.exe` it put in
    /// place, verified, or why it failed.
    downloaded: Arc<Mutex<Option<Result<PathBuf, String>>>>,
    /// Your commits that landed in the recent projects, as last counted.
    /// None until the first count is back.
    experience: Arc<Mutex<Option<u64>>>,
    /// A count of `experience` is on its thread.
    counting_xp: Arc<AtomicBool>,
}

/// What an update check found: a newer release, none, or why it failed.
type Looked = Result<Option<Manifest>, String>;

/// A worktree's count back from its thread: the session's id, and what
/// changed, None for a worktree that is gone.
/// A session's worktree changes, and whether its work landed when asked.
type Counted = (String, Option<Diff>, Option<bool>);

/// A browser window a session opened.
struct Browser {
    session: String,
    /// Minimised by a project switch, so switching back restores it. One
    /// the user minimised stays minimised.
    hidden: bool,
}

impl App {
    fn on_message(&mut self, msg: u32, wparam: usize) {
        match msg {
            WM_HORADRIC_EVENT => {
                self.reconcile(wparam != 0);
                self.switch_free();
                self.run_tasks();
                self.recount();
                self.mark_sweeps();
            }
            WM_HORADRIC_COUNTED => self.take_counts(),
            WM_HORADRIC_UPDATE => self.take_update(),
            WM_HORADRIC_DOWNLOADED => self.take_download(),
            WM_HORADRIC_KEPT => self.offer_merges(),
            WM_HORADRIC_INPUT => self.apply_input(),
            WM_HORADRIC_OUTPUT => self.output(wparam),
            WM_HORADRIC_WINDOW_SHOWN => self.window_shown(wparam as isize),
            WM_HORADRIC_WINDOW_GONE => self.window_gone(wparam as isize),
            WM_HORADRIC_EXIT => self.exited(wparam),
            WM_HORADRIC_NEW => {
                let pending = self
                    .requests
                    .lock()
                    .map(|mut q| std::mem::take(&mut *q))
                    .unwrap_or_default();
                for command in pending {
                    match command {
                        Command::New(n) => {
                            if let Err(e) = self.start(n.name, PathBuf::from(n.cwd), n.args) {
                                eprintln!("horadric: cannot start session: {e}");
                            }
                        }
                        Command::Reload(r) => self.begin_reload(r),
                        Command::Tasks(t) => {
                            if let Some(tomb) = &t.tomb {
                                self.tomb_reported(tomb, t.why.as_deref());
                            }
                            self.refresh_boards(true);
                            self.run_tasks();
                        }
                    }
                }
            }
            WM_HOTKEY if wparam as i32 == HOTKEY_NEXT => self.next_waiting(),
            WM_HOTKEY if wparam as i32 == HOTKEY_LISTEN => self.listen_on_demand(),
            WM_TIMER if wparam == GLIDE_TIMER => self.glide(),
            WM_TIMER if wparam == SCREEN_TIMER => {
                unsafe {
                    let _ = KillTimer(Some(self.notify), SCREEN_TIMER);
                }
                self.screen_changed();
            }
            WM_TIMER => {
                self.tick();
                // Activity heard while a count was due too soon.
                self.recount();
                if self.swept_at.is_empty() {
                    self.mark_sweeps();
                }
                self.sweep();
                self.switch_free();
                self.tick_tasks();
                self.reload_when_ready();
                if self
                    .last_check
                    .is_none_or(|t| t.elapsed() >= release::CHECK_EVERY)
                {
                    self.check_update(false);
                }
            }
            _ => {}
        }
    }

    fn console_by_serial(&self, serial: usize) -> Option<&Arc<Console>> {
        self.consoles
            .values()
            .chain(self.views.values())
            .find(|c| c.serial == serial)
    }

    /// A session's console, or a file view's, by its id.
    fn console_of(&self, id: &str) -> Option<&Arc<Console>> {
        self.consoles
            .get(id)
            .or_else(|| self.views.values().find(|v| v.id == id))
    }

    /// The stage, when it shows this console.
    fn stage_showing(&self, serial: usize) -> Option<&TerminalWindow> {
        self.stage.as_deref().filter(|s| s.shows(serial))
    }

    /// Running agents and running plain terminals.
    fn live_counts(&self) -> (usize, usize) {
        let live = self.consoles.values().filter(|c| c.exit_code().is_none());
        let shells = live.clone().filter(|c| c.shell).count();
        (live.count() - shells, shells)
    }

    /// Sessions in a Horadric terminal that are in the middle of a turn.
    fn mid_turn_count(&self) -> usize {
        let Ok(r) = self.shared.registry.lock() else {
            return 0;
        };
        self.consoles
            .iter()
            .filter(|(id, c)| {
                c.exit_code().is_none() && r.get(id).is_some_and(|s| s.phase.mid_turn())
            })
            .count()
    }

    fn begin_reload(&mut self, reload: Reload) {
        self.reload = Some(reload);
        self.reconcile(false);
        self.reload_when_ready();
    }

    /// Hands over to the new build: saves, starts the new build's `swap`,
    /// and quits. `swap` waits for this process to exit, installs the
    /// build, and starts it with `--reload`. The sessions' hosts run on and
    /// the new build attaches to them, so nothing waits for a turn to end.
    fn reload_when_ready(&mut self) {
        let Some(reload) = &self.reload else {
            return;
        };
        let exe = PathBuf::from(&reload.exe);
        self.freeze();
        let started = std::process::Command::new(&exe)
            .args(["swap", "--pid", &std::process::id().to_string()])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn();
        match started {
            Ok(_) => unsafe { PostQuitMessage(0) },
            Err(e) => {
                eprintln!(
                    "horadric: cannot reload, {} did not start: {e}",
                    exe.display()
                );
                self.reload = None;
                self.frozen = false;
                self.reconcile(false);
            }
        }
    }

    /// Connects to every session host still running for this instance,
    /// after a crash, a reload or a Quit that kept them. Each session
    /// carries on where it was, its screen replayed, and its phase read
    /// from its transcript, since its hooks went nowhere while no app ran.
    fn attach_hosts(&mut self) {
        for id in console::running_hosts() {
            let Some(saved) = self.paused.get(&id).cloned() else {
                eprintln!("horadric: a session host runs for {id}, which is not a saved session");
                continue;
            };
            let serial = self.next_serial;
            let console =
                match Console::attach(&id, serial, saved.args.clone(), saved.shell, self.notify) {
                    Ok(c) => c,
                    Err(e) => {
                        eprintln!("horadric: cannot attach to {id}: {e}");
                        continue;
                    }
                };
            self.next_serial += 1;
            self.paused.remove(&id);
            let busy = saved
                .claude_session_id
                .as_deref()
                .filter(|_| !saved.shell)
                .and_then(|claude| transcript::mid_turn(&saved.cwd, claude))
                .unwrap_or(false);
            if let Ok(mut r) = self.shared.registry.lock() {
                let now = SystemTime::now();
                r.apply(&id, &HookEvent::synthetic(HookEvent::REGISTER), now);
                if busy {
                    r.apply(&id, &HookEvent::synthetic("PreToolUse"), now);
                }
            }
            self.consoles.insert(id, console);
        }
        self.reconcile(true);
    }

    /// Ends every session and waits a moment for their hosts to say so,
    /// since the kills go out on threads that end with this process.
    fn stop_all(&self) {
        for c in self.consoles.values() {
            if c.exit_code().is_none() {
                c.kill();
            }
        }
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline
            && self
                .consoles
                .values()
                .any(|c| !c.is_view() && c.exit_code().is_none())
        {
            thread::sleep(Duration::from_millis(20));
        }
    }

    /// After a reload or a crash: starts again the sessions that were running
    /// and have no host any more, without opening a window for each, and
    /// puts back the one that was on stage.
    fn carry_on(&mut self, ids: &[String], on_stage: Option<&str>) {
        for id in ids {
            // A hook heard already means its agent outlived the last
            // Horadric, and a second on the same conversation would fight it.
            let heard = self
                .shared
                .registry
                .lock()
                .map(|r| r.get(id).is_some_and(|s| s.phase != Phase::Paused))
                .unwrap_or(true);
            if heard {
                continue;
            }
            if let Err(e) = self.resume(id, false) {
                eprintln!("horadric: cannot resume {id}: {e}");
            }
        }
        if let Some(id) = on_stage.filter(|id| self.consoles.contains_key(*id)) {
            self.reveal(id, false);
        }
    }

    /// What a tile's menu offers, and whether it is a plain terminal.
    fn tile_kind(&self, id: &str) -> Option<(TileKind, bool)> {
        let (shell, background) = {
            let r = self.shared.registry.lock().ok()?;
            let s = r.get(id)?;
            (s.shell, s.background.is_some())
        };
        let kind = if self.paused.contains_key(id) {
            TileKind::Paused
        } else if self
            .consoles
            .get(id)
            .is_some_and(|c| c.exit_code().is_none())
        {
            TileKind::Live
        } else if background {
            TileKind::Background
        } else {
            TileKind::Elsewhere
        };
        Some((kind, shell))
    }

    fn output(&mut self, serial: usize) {
        let Some(console) = self.console_by_serial(serial) else {
            return;
        };
        if !console.take_dirty() {
            return;
        }
        if let Some(t) = self.stage_showing(serial) {
            t.refresh(serial);
        }
        // No hook reports on a shell, so its tile tells what it runs from
        // the terminal's title and how busy it is from its output. The
        // tiles redraw on the next tick.
        if console.shell {
            let title = console.title();
            if let Ok(mut r) = self.shared.registry.lock() {
                if let Some(s) = r.get_mut(&console.id).filter(|s| s.background.is_none()) {
                    s.touch(SystemTime::now());
                    s.last_line = title.or_else(|| s.ssh.clone()).unwrap_or_default();
                }
            }
        }
    }

    /// The agent exited. A clean exit, as from `/exit`, ends the session.
    /// Anything else, a crash or a kill, leaves it paused, so a click
    /// resumes it instead of losing it.
    fn exited(&mut self, serial: usize) {
        if self.frozen {
            return;
        }
        let Some(console) = self.console_by_serial(serial).cloned() else {
            return;
        };
        // Detaching closes the pane and nothing more: the daemon runs the
        // session on. Unless it was the session that ended.
        if let Some((short, _, _)) = self.background_of(&console.id) {
            self.consoles.remove(&console.id);
            self.check_background(console.id.clone(), short);
            self.reconcile(true);
            return;
        }
        // A shell has nothing to resume, and `exit` means done with it
        // whatever code the last command left behind. `ssh` failing to
        // connect or dropping is the exception: it pauses below, and a
        // click reconnects.
        let ssh_failed = console.exit_code().is_some_and(ssh::keeps)
            && self
                .shared
                .registry
                .lock()
                .is_ok_and(|r| r.get(&console.id).is_some_and(|s| s.ssh.is_some()));
        if console.shell && !ssh_failed {
            self.forget(&console.id);
            self.reconcile(false);
            return;
        }
        if console.exit_code() == Some(0) {
            if let Ok(mut r) = self.shared.registry.lock() {
                if r.get(&console.id).is_some() {
                    r.apply(
                        &console.id,
                        &HookEvent::synthetic("SessionEnd"),
                        SystemTime::now(),
                    );
                }
            }
        } else {
            if let Ok(mut r) = self.shared.registry.lock() {
                if let Some(s) = r.get(&console.id) {
                    let saved = SavedSession::from_session(s, console.args.clone(), false);
                    self.paused.insert(console.id.clone(), saved);
                    r.apply(
                        &console.id,
                        &HookEvent::synthetic(HookEvent::PAUSE),
                        SystemTime::now(),
                    );
                }
            }
            // The pane stays, so whatever went wrong can be read.
            if let Some(t) = self.stage_showing(serial) {
                t.refresh(serial);
            }
        }
        // A clean exit leaves the stage here.
        self.reconcile(true);
    }

    /// Starts an agent in a new session and opens its terminal. Returns the
    /// session's id.
    fn start(
        &mut self,
        name: Option<String>,
        cwd: PathBuf,
        args: Vec<String>,
    ) -> Result<String, String> {
        if !cwd.is_dir() {
            return Err(format!("{} is not a directory", cwd.display()));
        }
        let folder = folder_name(&cwd);
        let base = name.clone().unwrap_or_else(|| folder.clone());
        let id = self.unique_id(&base);
        let branch = name.as_deref().unwrap_or("session").to_string();
        let shown = name.unwrap_or(folder);
        let cwd = self.own_tree(&id, &branch, cwd, &args);
        if let Err(e) = self.launch(&id, &shown, cwd, args, Run::Agent, false) {
            // A worktree the session never started in holds nothing.
            if let Some((w, _)) = self.new_trees.remove(&id) {
                worktree::remove(w);
            }
            return Err(e);
        }
        // A new session is where the eye already is: against the tiles, not
        // wherever the stage was left.
        if let Some(key) = self.project_of(&id) {
            if self.fill_stage(&key, true) {
                if let Some(stage) = &self.stage {
                    stage.focus_session(&id);
                }
            }
        }
        Ok(id)
    }

    /// Where a new session starts: a worktree of its own added from `cwd`,
    /// or `cwd` itself when the project keeps one shared tree, is not in a
    /// repository, or `args` carry on a conversation, which Claude Code
    /// keeps by the folder it was held in.
    fn own_tree(&mut self, id: &str, branch: &str, cwd: PathBuf, args: &[String]) -> PathBuf {
        let carries_on = ["--resume", "-r", "--continue", "-c"]
            .iter()
            .any(|f| has_flag(args, f));
        if carries_on {
            return cwd;
        }
        match worktree::add(&cwd, branch, &self.ports_taken()) {
            Ok(Some(fresh)) => {
                // The project is where it was asked for, not the worktree.
                recent::remember(&mut self.recent, &cwd.to_string_lossy());
                self.new_trees
                    .insert(id.to_string(), (fresh.worktree, fresh.setup));
                fresh.cwd
            }
            Ok(None) => cwd,
            Err(e) => {
                eprintln!("horadric: no worktree for {id}, it shares the main tree: {e}");
                cwd
            }
        }
    }

    /// Counts again what each session's worktree has changed, where the
    /// agent did something since the last count. On threads of their own,
    /// since each count starts git three times.
    /// A session that committed is asked the same way whether its work
    /// landed, worktree or not.
    fn recount(&mut self) {
        type Due = (
            String,
            Option<Worktree>,
            Option<PathBuf>,
            Option<SystemTime>,
        );
        let trees: Vec<Due> = match self.shared.registry.lock() {
            Ok(r) => {
                r.all()
                    .filter_map(|s| {
                        let land = s.loot.may_land().then(|| {
                            PathBuf::from(s.worktree.as_ref().map_or(&s.cwd, |w| &w.path))
                        });
                        if s.worktree.is_none() && land.is_none() {
                            return None;
                        }
                        Some((
                            s.id.clone(),
                            s.worktree.clone(),
                            land,
                            s.activity.last().copied(),
                        ))
                    })
                    .collect()
            }
            Err(_) => return,
        };
        self.recounts
            .retain(|id, _| trees.iter().any(|(t, ..)| t == id));
        let now = Instant::now();
        for (id, w, land, activity) in trees {
            let r = self.recounts.entry(id.clone()).or_default();
            r.heard(activity);
            if !r.start(now) {
                continue;
            }
            let counted = Arc::clone(&self.counted);
            let notify = self.notify.0 as isize;
            std::thread::spawn(move || {
                let diff = w.as_ref().and_then(worktree::count);
                let landed = land.as_deref().and_then(worktree::landed);
                if let Ok(mut c) = counted.lock() {
                    c.push((id, diff, landed));
                }
                post(notify, WM_HORADRIC_COUNTED, 0);
            });
        }
    }

    /// Marks every project with a session as due for a sweep.
    fn mark_sweeps(&mut self) {
        if let Ok(r) = self.shared.registry.lock() {
            self.sweep_due.extend(r.all().map(project_key));
        }
    }

    /// Sweeps the worktrees that are done with out of each due project, on
    /// a thread each, at most every [`SWEEP_GAP`] a project.
    fn sweep(&mut self) {
        let now = Instant::now();
        let ready: Vec<String> = self
            .sweep_due
            .iter()
            .filter(|k| self.swept_at.get(*k).is_none_or(|t| now - *t >= SWEEP_GAP))
            .cloned()
            .collect();
        if ready.is_empty() {
            return;
        }
        // Every session's folder, paused ones too, and the worktrees of
        // sessions about to start.
        let mut busy: Vec<String> = match self.shared.registry.lock() {
            Ok(r) => r
                .all()
                .flat_map(|s| {
                    std::iter::once(s.cwd.clone())
                        .chain(s.worktree.as_ref().map(|w| w.path.clone()))
                })
                .chain(r.stashed().iter().flat_map(|s| {
                    std::iter::once(s.cwd.clone())
                        .chain(s.worktree.as_ref().map(|w| w.path.clone()))
                }))
                .collect(),
            Err(_) => return,
        };
        busy.extend(self.new_trees.values().map(|(w, _)| w.path.clone()));
        for key in ready {
            self.sweep_due.remove(&key);
            self.swept_at.insert(key.clone(), now);
            let heads = Arc::clone(&self.swept_heads);
            let busy = busy.clone();
            std::thread::spawn(move || {
                let dir = Path::new(&key);
                let Some(head) = worktree::head(dir) else {
                    return;
                };
                if heads.lock().is_ok_and(|h| h.get(&key) == Some(&head)) {
                    return;
                }
                for path in worktree::sweep(dir, &busy) {
                    eprintln!("horadric: removed the merged worktree {path}");
                }
                if let Ok(mut h) = heads.lock() {
                    h.insert(key, head);
                }
            });
        }
    }

    /// Counts `experience` again on a thread, from git in every recent
    /// project, so opening the tray menu never waits on git. The menu shows
    /// the count before, which is at most one menu behind.
    fn count_experience(&self) {
        if self.counting_xp.swap(true, Ordering::SeqCst) {
            return;
        }
        let projects: Vec<PathBuf> = self.recent.iter().map(PathBuf::from).collect();
        let (experience, counting) = (Arc::clone(&self.experience), Arc::clone(&self.counting_xp));
        thread::spawn(move || {
            let hashes: Vec<String> = projects
                .iter()
                .filter(|p| p.is_dir())
                .flat_map(|p| worktree::mine(p))
                .collect();
            let xp = horadric_core::experience::total(hashes.iter().map(String::as_str));
            if let Ok(mut e) = experience.lock() {
                *e = Some(xp);
            }
            counting.store(false, Ordering::SeqCst);
        });
    }

    /// Looks for a newer release on a thread. `asked` when the tray asked,
    /// which is the only time the outcome is said out loud.
    fn check_update(&mut self, asked: bool) {
        self.last_check = Some(Instant::now());
        let Some(url) = update_url() else {
            if asked {
                self.toasts.show(
                    Kind::Info,
                    "No updates to check",
                    "A dev instance checks HORADRIC_UPDATE_URL only, and it is not set.",
                );
            }
            return;
        };
        if self.checking {
            return;
        }
        self.checking = true;
        let looked = Arc::clone(&self.looked);
        let notify = self.notify.0 as isize;
        std::thread::spawn(move || {
            let found = update::look(&url, env!("CARGO_PKG_VERSION"));
            if let Ok(mut l) = looked.lock() {
                *l = Some((asked, found));
            }
            post(notify, WM_HORADRIC_UPDATE, 0);
        });
    }

    /// What the update check found. A newer release only ever shows in the
    /// tray menu, unless the tray asked.
    fn take_update(&mut self) {
        let Some((asked, found)) = self.looked.lock().ok().and_then(|mut l| l.take()) else {
            return;
        };
        self.checking = false;
        match found {
            Ok(Some(m)) => {
                eprintln!("horadric: Horadric {} is out", m.version);
                if asked {
                    self.toasts.show(
                        Kind::Done,
                        &format!("Horadric {} is out", m.version),
                        &format!("Pick \"Update to {}\" in the tray menu.", m.version),
                    );
                }
                self.update = Some(m);
            }
            Ok(None) => {
                self.update = None;
                if asked {
                    self.toasts.show(
                        Kind::Done,
                        "Horadric is up to date",
                        &format!("{} is the newest release.", env!("CARGO_PKG_VERSION")),
                    );
                }
            }
            Err(e) => {
                eprintln!("horadric: update check failed: {e}");
                if asked {
                    self.toasts.show(Kind::Failed, "Update check failed", &e);
                }
            }
        }
    }

    /// Downloads the release the tray offered on a thread. Its hashes are
    /// checked there against the manifest whose signature the check
    /// verified, and only then does [`Self::take_download`] reload into it.
    fn install_update(&mut self) {
        let (Some(manifest), Some(url)) = (self.update.clone(), update_url()) else {
            return;
        };
        if self.downloading {
            return;
        }
        self.downloading = true;
        self.toasts.show(
            Kind::Info,
            &format!("Updating to Horadric {}", manifest.version),
            "Downloading. The sessions carry on through the update.",
        );
        let downloaded = Arc::clone(&self.downloaded);
        let notify = self.notify.0 as isize;
        std::thread::spawn(move || {
            let got = update::download(&url, &manifest);
            if let Ok(mut d) = downloaded.lock() {
                *d = Some(got);
            }
            post(notify, WM_HORADRIC_DOWNLOADED, 0);
        });
    }

    /// Hands over to a verified download the way `horadric reload` would.
    /// A dev instance stops short: its `swap` restarts from the folder it
    /// was started from, so it would run the download in place and never
    /// install anything.
    fn take_download(&mut self) {
        let Some(got) = self.downloaded.lock().ok().and_then(|mut d| d.take()) else {
            return;
        };
        self.downloading = false;
        match got {
            Ok(exe) if horadric_hooks::dev() => {
                eprintln!("horadric: update downloaded to {}", exe.display());
                self.toasts.show(
                    Kind::Done,
                    "Update downloaded",
                    "A dev instance does not install it. It is verified, in the updates folder.",
                );
            }
            Ok(exe) => {
                eprintln!(
                    "horadric: update downloaded, reloading into {}",
                    exe.display()
                );
                self.begin_reload(Reload {
                    exe: exe.to_string_lossy().into_owned(),
                    now: true,
                });
            }
            Err(e) => {
                eprintln!("horadric: update failed: {e}");
                self.toasts.show(Kind::Failed, "Update failed", &e);
            }
        }
    }

    /// Puts the counts that came back on their sessions' tiles.
    fn take_counts(&mut self) {
        let counts = self
            .counted
            .lock()
            .map(|mut c| std::mem::take(&mut *c))
            .unwrap_or_default();
        if counts.is_empty() {
            return;
        }
        let mut rune = false;
        if let Ok(mut r) = self.shared.registry.lock() {
            for (id, diff, landed) in counts {
                if let Some(rc) = self.recounts.get_mut(&id) {
                    rc.done();
                }
                if let Some(s) = r.get_mut(&id) {
                    s.diff = diff;
                    // A worktree swept after its merge can no longer say.
                    if let Some(landed) = landed {
                        rune |= landed && !s.loot.landed;
                        s.loot.landed = landed;
                    }
                }
            }
        }
        if rune {
            self.sound(Loot::Rune);
        }
        for c in &self.clusters {
            c.invalidate();
        }
    }

    /// The worktree of a session, counted now, for its menu to list what
    /// changed as it is this moment.
    fn diff_of(&mut self, id: &str) -> Option<(Worktree, Option<Diff>)> {
        let w = self
            .shared
            .registry
            .lock()
            .ok()?
            .get(id)?
            .worktree
            .clone()?;
        let diff = worktree::count(&w);
        if let Ok(mut r) = self.shared.registry.lock() {
            if let Some(s) = r.get_mut(id) {
                s.diff = diff.clone();
            }
        }
        for c in &self.clusters {
            c.invalidate();
        }
        Some((w, diff))
    }

    /// The ports of every session's worktree, running or paused.
    fn ports_taken(&self) -> Vec<tree::Ports> {
        self.shared
            .registry
            .lock()
            .map(|r| r.all().filter_map(|s| s.worktree.as_ref()?.ports).collect())
            .unwrap_or_default()
    }

    /// The past conversations held in `dir` that no tile holds, newest
    /// first. Claude Code keeps every one, ended or not, so this is the way
    /// back to a session closed for good.
    fn history(&self, dir: &Path) -> Vec<Past> {
        let open: Vec<String> = match self.shared.registry.lock() {
            Ok(r) => r
                .all()
                .filter_map(|s| s.claude_session_id.clone())
                .chain(
                    r.stashed()
                        .iter()
                        .filter_map(|s| s.claude_session_id.clone()),
                )
                .collect(),
            Err(_) => return Vec::new(),
        };
        transcript::history(&dir.to_string_lossy(), &open, HISTORY)
    }

    /// Carries on a past conversation from `past`, the History menu's list
    /// for `dir`, in a new tile. Or the tile opens Claude Code's own picker
    /// of every conversation in the folder.
    fn reopen(&mut self, dir: &Path, past: &[Past], pick: history::Pick) {
        let past = match pick {
            history::Pick::Past(i) => match past.get(i) {
                Some(p) => Some(p),
                None => return,
            },
            history::Pick::All => None,
        };
        let mut args = vec!["--resume".to_string()];
        args.extend(past.map(|p| p.id.clone()));
        let id = match self.start(None, dir.to_path_buf(), args) {
            Ok(id) => id,
            Err(e) => {
                eprintln!("horadric: cannot resume a conversation: {e}");
                return;
            }
        };
        // Known before any hook says so. A pause before the next prompt
        // would otherwise resume nothing and start afresh.
        if let (Some(p), Ok(mut r)) = (past, self.shared.registry.lock()) {
            if let Some(s) = r.get_mut(&id) {
                s.claude_session_id = Some(p.id.clone());
                s.prompted = true;
                s.title = Some(p.title.clone());
            }
        }
    }

    /// Resumes a paused session in the same tile, with its conversation.
    /// With `show`, its terminal opens too.
    fn resume(&mut self, id: &str, show: bool) -> Result<(), String> {
        // Out of `paused` before launching, not after: launching opens the
        // window through `expand`, and a session still marked paused there
        // resumes again, and again, one `claude` each time.
        let Some(saved) = self.paused.remove(id) else {
            return Ok(());
        };
        // A console that crashed may still show its last screen. The resumed
        // one takes its pane's place.
        self.consoles.remove(id);
        let cwd = PathBuf::from(&saved.cwd);
        let result = if cwd.is_dir() {
            let args = saved.launch_args();
            let run = match (&saved.ssh, saved.shell) {
                (Some(host), _) => Run::Ssh(host.clone()),
                (None, true) => Run::Shell,
                (None, false) => Run::Agent,
            };
            self.launch(id, &saved.name, cwd, args, run, show)
        } else {
            Err(format!("{} no longer exists", cwd.display()))
        };
        if result.is_err() {
            self.paused.insert(id.to_string(), saved);
        }
        result
    }

    /// Opens a plain terminal in the project with this key, on the stage
    /// beside its sessions, with the keyboard.
    fn open_shell(&mut self, key: &str) {
        let Some(dir) = self.project_dir(key) else {
            return;
        };
        let n = self
            .shared
            .registry
            .lock()
            .map(|r| r.all().filter(|s| s.shell && project_key(s) == key).count())
            .unwrap_or(0);
        let id = self.unique_id("terminal");
        if let Err(e) = self.launch(&id, &shell::name(n), dir, Vec::new(), Run::Shell, false) {
            eprintln!("horadric: cannot open a terminal: {e}");
            return;
        }
        if self.fill_stage(key, false) {
            if let Some(stage) = &self.stage {
                stage.focus_session(&id);
            }
        }
    }

    /// Opens an SSH terminal on `host` in the project with this key, the
    /// way [`App::open_shell`] opens a plain one. Without a `name` it is
    /// numbered among the project's SSH terminals.
    fn open_ssh(&mut self, key: &str, host: &str, name: Option<&str>) {
        let Some(dir) = self.project_dir(key) else {
            return;
        };
        let n = self
            .shared
            .registry
            .lock()
            .map(|r| {
                r.all()
                    .filter(|s| s.ssh.is_some() && project_key(s) == key)
                    .count()
            })
            .unwrap_or(0);
        let id = self.unique_id("ssh");
        let run = Run::Ssh(host.to_string());
        let name = name.map_or_else(|| ssh::name(n), String::from);
        if let Err(e) = self.launch(&id, &name, dir, Vec::new(), run, false) {
            eprintln!("horadric: cannot open ssh to {host}: {e}");
            return;
        }
        if self.fill_stage(key, false) {
            if let Some(stage) = &self.stage {
                stage.focus_session(&id);
            }
        }
    }

    /// Registers a session, starts its console and, with `show`, opens its
    /// terminal. `run` says whether the console runs the agent, a plain
    /// shell or `ssh`.
    pub(crate) fn launch(
        &mut self,
        id: &str,
        name: &str,
        cwd: PathBuf,
        args: Vec<String>,
        run: Run,
        show: bool,
    ) -> Result<(), String> {
        // One agent per session, whatever path led here. A second would be
        // a runaway, not a feature.
        if self
            .consoles
            .get(id)
            .is_some_and(|c| c.exit_code().is_none())
        {
            return Err(format!("{id} is already running"));
        }
        // An attached pane is started like a shell, untagged: the session's
        // hooks come from the daemon, not from this console.
        let shell = run != Run::Agent;
        let attach = matches!(run, Run::Attach(_));
        let host = match &run {
            Run::Ssh(h) => Some(h.clone()),
            _ => None,
        };
        let (program, args) = match &run {
            Run::Agent => (
                console::agent_program()
                    .ok_or("claude not found on PATH (or set HORADRIC_AGENT)")?,
                args,
            ),
            Run::Shell => (
                console::shell_program().ok_or("no shell found (set HORADRIC_SHELL)")?,
                args,
            ),
            Run::Ssh(h) => (
                console::ssh_program().ok_or("no ssh found (install OpenSSH Client)")?,
                ssh::args(h),
            ),
            Run::Attach(short) => (
                console::claude_program().ok_or("claude not found on PATH")?,
                vec!["attach".to_string(), short.clone()],
            ),
        };
        let fresh = self.new_trees.remove(id);
        if fresh.is_none() {
            recent::remember(&mut self.recent, &cwd.to_string_lossy());
        }

        let register = HookEvent {
            cwd: cwd.to_string_lossy().to_string(),
            name: Some(name.to_string()),
            ..HookEvent::synthetic(HookEvent::REGISTER)
        };
        let mut own_tree = None;
        let was_known = self
            .shared
            .registry
            .lock()
            .map(|mut r| {
                let known = r.get(id).is_some();
                // A background tile keeps the phase its hooks gave it.
                if !attach {
                    r.apply(id, &register, SystemTime::now());
                }
                if let Some(s) = r.get_mut(id) {
                    if let Some((w, _)) = &fresh {
                        s.worktree = Some(w.clone());
                    }
                    own_tree = s.worktree.clone();
                    s.shell = shell && !attach;
                    // Until the remote shell sets a title, the tile says
                    // where it is.
                    if let Some(h) = &host {
                        s.last_line = h.clone();
                    }
                    s.ssh = host;
                }
                known
            })
            .unwrap_or(false);

        let extra = if attach {
            Vec::new()
        } else {
            self.extra_args(id, &program, &args, &cwd)
        };
        let env = own_tree
            .as_ref()
            .map(|w| {
                let mut env = w.ports.map(tree::env).unwrap_or_default();
                // `horadric quest` finds the list in the main tree.
                env.push((TASKS_ENV.into(), folder_key(&cwd.to_string_lossy())));
                env
            })
            .unwrap_or_default();
        let serial = self.next_serial;
        self.next_serial += 1;
        let console = Console::spawn(
            Launch {
                id: id.to_string(),
                serial,
                program,
                args,
                extra,
                cwd,
                shell,
                env,
                setup: fresh.map(|(_, setup)| setup).unwrap_or_default(),
            },
            self.notify,
        )
        .map_err(|e| {
            if let Ok(mut r) = self.shared.registry.lock() {
                if attach {
                    // The session runs on in the daemon whatever the pane did.
                } else if was_known {
                    r.apply(
                        id,
                        &HookEvent::synthetic(HookEvent::PAUSE),
                        SystemTime::now(),
                    );
                } else if let Some(w) = r.remove(id).and_then(|s| s.worktree) {
                    worktree::remove(w);
                }
            }
            format!("could not start the agent: {e}")
        })?;
        self.consoles.insert(id.to_string(), console);
        self.reconcile(true);
        if show {
            self.reveal(id, false);
        }
        Ok(())
    }

    /// What goes before a session's own arguments this time: the defaults
    /// from the usage window, the status line that feeds it, and what it
    /// is told about the task list and the project's hosts. Only for Claude
    /// Code, not for a shell put in its place with `HORADRIC_AGENT`, and not
    /// over settings the session brought itself.
    fn extra_args(&mut self, id: &str, program: &Path, args: &[String], cwd: &Path) -> Vec<String> {
        if !console::is_claude(program) {
            return Vec::new();
        }
        let mut extra = self.shared.defaults.borrow().flags(args);
        if let (Some(path), false) = (&self.status_settings, has_flag(args, "--settings")) {
            extra.push("--settings".into());
            extra.push(path.to_string_lossy().into_owned());
        }
        extra.extend(self.task_args(id, program, cwd));
        extra
    }

    /// A setting picked in the usage window. Sessions take it as they start
    /// or resume. Running ones switch too where Claude Code has a command
    /// for it, typed in once each is free, unless it chose the setting with
    /// its own arguments.
    fn set_default(&mut self, setting: Setting, value: Option<String>) {
        if let Some(command) = setting.command(value.as_deref()) {
            for (id, c) in &self.consoles {
                if !c.claude || c.exit_code().is_some() || setting.chosen_by(&c.args) {
                    continue;
                }
                let queue = self.switches.entry(id.clone()).or_default();
                queue.retain(|(s, _)| *s != setting);
                queue.push((setting, command.clone()));
            }
        }
        self.shared.defaults.borrow_mut().set(setting, value);
        if let Some(u) = &self.usage_window {
            u.invalidate();
        }
        self.save();
        self.switch_free();
    }

    /// Types the next waiting command into each session free for it. One
    /// at a time, so the agent has taken one before the next comes. Once
    /// all are in and Claude Code has had time to save them, the user's own
    /// defaults go back.
    fn switch_free(&mut self) {
        let settings = install::settings_path();
        if self.switches.is_empty() {
            if let (Some((was, last)), Some(path)) = (&self.settings_before, &settings) {
                if last.elapsed() >= SAVED_WITHIN {
                    if let Err(e) = install::restore(path, &install::SWITCHED, was) {
                        eprintln!("horadric: cannot put back the default model: {e}");
                    }
                    self.settings_before = None;
                }
            }
            return;
        }
        let Ok(registry) = self.shared.registry.lock() else {
            return;
        };
        let consoles = &self.consoles;
        let before = &mut self.settings_before;
        self.switches.retain(|id, queue| {
            let (Some(c), Some(s)) = (consoles.get(id), registry.get(id)) else {
                return false;
            };
            if c.exit_code().is_some() || queue.is_empty() {
                return false;
            }
            if s.free_for_command(c.typed_at()) {
                let was = match (before.take(), &settings) {
                    (Some((was, _)), _) => Some(was),
                    (None, Some(path)) => install::snapshot(path, &install::SWITCHED).ok(),
                    (None, None) => None,
                };
                *before = was.map(|w| (w, Instant::now()));
                let (_, command) = queue.remove(0);
                c.write(format!("{command}\r").into_bytes());
            }
            !queue.is_empty()
        });
    }

    /// A setting's list opened. Its row in the usage window stays lit
    /// while it is.
    fn dropped(&mut self, d: Box<Dropdown>) {
        if let Some(old) = self.dropdown.replace(d) {
            old.destroy();
        }
        let i = self
            .dropdown
            .as_ref()
            .and_then(|d| Setting::ALL.iter().position(|s| *s == d.setting));
        if let Some(u) = &self.usage_window {
            u.open.set(i);
            u.invalidate();
        }
    }

    /// Puts a session in the stash: its process stops, its tile and pane
    /// go, and its conversation, arguments and worktree are kept for a
    /// click to bring back.
    fn stash(&mut self, id: &str) {
        let args = match (self.paused.get(id), self.consoles.get(id)) {
            (Some(p), _) => p.args.clone(),
            (None, Some(c)) => c.args.clone(),
            (None, None) => return,
        };
        let stashed = match self.shared.registry.lock() {
            Ok(mut r) => {
                let Some(s) = r.get(id).filter(|s| !s.shell && s.background.is_none()) else {
                    return;
                };
                let saved = SavedSession::from_session(s, args, false);
                r.stash(saved)
            }
            Err(_) => false,
        };
        if !stashed {
            return;
        }
        // Out of the registry before the process goes, so its exit finds no
        // session to pause and no console to report on.
        if let Some(c) = self.consoles.remove(id) {
            if c.exit_code().is_none() {
                c.kill();
            }
        }
        self.paused.remove(id);
        self.journaled.remove(id);
        self.browsers.retain(|&hwnd, b| {
            let keep = b.session != id;
            if !keep {
                browsers::untrack(hwnd_of(hwnd));
            }
            keep
        });
        for order in self.shared.orders.borrow_mut().values_mut() {
            order.retain(|s| s != id);
        }
        self.reconcile(false);
    }

    /// Takes a session out of the stash into its project's tiles, paused,
    /// and with `run` resumes it on the stage as well.
    fn unstash(&mut self, id: &str, run: bool) {
        let saved = match self.shared.registry.lock() {
            Ok(mut r) => r.unstash(id, SystemTime::now()),
            Err(_) => None,
        };
        let Some(saved) = saved else {
            return;
        };
        self.paused.insert(id.to_string(), saved);
        self.reconcile(false);
        if run {
            if let Err(e) = self.resume(id, true) {
                eprintln!("horadric: cannot resume {id}: {e}");
            }
        }
    }

    /// Ends a stashed session for good, its worktree with it.
    fn end_stashed(&mut self, id: &str) {
        let saved = match self.shared.registry.lock() {
            Ok(mut r) => r.discard(id),
            Err(_) => None,
        };
        let Some(saved) = saved else {
            return;
        };
        if let Some(w) = saved.worktree.clone() {
            let key = project_key(&saved.to_session(SystemTime::now()));
            self.remove_tree(key, w);
        }
        self.reconcile(false);
    }

    /// Makes the stash window match the stash: there while it holds any,
    /// under the usage window when it first comes, each slot's look.
    fn sync_stash(&mut self) {
        let now = SystemTime::now();
        let items: Vec<(String, StashLook)> = match self.shared.registry.lock() {
            Ok(r) => r
                .stashed()
                .iter()
                .map(|saved| {
                    let s = saved.to_session(now);
                    let key = project_key(&s);
                    let look = StashLook {
                        name: s.label().to_string(),
                        project: project_name(&key),
                        accent: theme::accent(&key),
                        ink: theme::rarity_color(s.rarity()),
                    };
                    (s.id.clone(), look)
                })
                .collect(),
            Err(_) => return,
        };
        if items.is_empty() {
            if let Some(w) = self.stash_window.take() {
                self.glides.borrow_mut().forget(w.hwnd.0 as isize);
                w.destroy();
            }
            return;
        }
        if self.stash_window.is_none() {
            match StashWindow::create(Rc::clone(&self.shared), -10_000, -10_000) {
                // Born under whatever else stands there.
                Ok(w) => {
                    w.raise();
                    self.stash_window = Some(w);
                }
                Err(e) => {
                    eprintln!("horadric: cannot create the stash: {e}");
                    return;
                }
            }
        }
        if !self.columns.contains(columns::STASH) {
            self.columns.add_under(columns::STASH, columns::USAGE);
        }
        if let Some(w) = &self.stash_window {
            w.set_items(items);
        }
    }

    /// Ends a session for good: the process, the window, the tile, and its
    /// place in the saved state.
    fn end(&mut self, id: &str) {
        // Forgetting alone would leave it running, and back at its next hook.
        if let Some((short, _, _)) = self.background_of(id) {
            agents::stop(&short);
        }
        self.forget(id);
        self.reconcile(false);
    }

    /// Ends every session of the project with this key, or of every
    /// project.
    fn end_all(&mut self, key: Option<&str>) {
        let ids: Vec<String> = match self.shared.registry.lock() {
            Ok(r) => r
                .all()
                .filter(|s| key.is_none_or(|k| project_key(s) == k))
                .map(|s| s.id.clone())
                .collect(),
            Err(_) => return,
        };
        for id in &ids {
            self.forget(id);
        }
        self.reconcile(false);
    }

    /// Starts `n` more sessions in the project with this key.
    fn add_sessions(&mut self, key: &str, n: usize) {
        let Some(dir) = self.project_dir(key) else {
            return;
        };
        for _ in 0..n {
            if let Err(e) = self.start(None, dir.clone(), Vec::new()) {
                eprintln!("horadric: cannot start session: {e}");
                return;
            }
        }
    }

    /// Replaces the project's sessions with `n` new ones. The new ones
    /// start first, so the cluster never empties and keeps its place. Its
    /// plain terminals stay: a dev server has nothing to do with starting
    /// over.
    fn start_over(&mut self, key: &str, n: usize) {
        let old: Vec<String> = match self.shared.registry.lock() {
            Ok(r) => r
                .all()
                .filter(|s| !s.shell && project_key(s) == key)
                .map(|s| s.id.clone())
                .collect(),
            Err(_) => return,
        };
        self.add_sessions(key, n);
        for id in &old {
            self.forget(id);
        }
        self.reconcile(false);
    }

    /// Running sessions in the project with this key, or in every project,
    /// and how many of them are mid turn.
    fn running_in(&self, key: Option<&str>) -> (usize, usize) {
        let Ok(r) = self.shared.registry.lock() else {
            return (0, 0);
        };
        let running: Vec<&Session> = self
            .consoles
            .iter()
            .filter(|(_, c)| c.exit_code().is_none())
            .filter_map(|(id, _)| r.get(id))
            .filter(|s| key.is_none_or(|k| project_key(s) == k))
            .collect();
        let working = running.iter().filter(|s| s.phase.mid_turn()).count();
        (running.len(), working)
    }

    /// Kills a session's process and drops every trace of it, without
    /// updating the windows.
    fn forget(&mut self, id: &str) {
        if let Some(c) = self.consoles.remove(id) {
            if c.exit_code().is_none() {
                c.kill();
            }
        }
        self.paused.remove(id);
        self.browsers.retain(|&hwnd, b| {
            let keep = b.session != id;
            if !keep {
                browsers::untrack(hwnd_of(hwnd));
            }
            keep
        });
        for order in self.shared.orders.borrow_mut().values_mut() {
            order.retain(|s| s != id);
        }
        let own_tree = match self.shared.registry.lock() {
            Ok(mut r) => r
                .remove(id)
                .and_then(|s| Some((project_key(&s), s.worktree?))),
            Err(_) => None,
        };
        if let Some((key, w)) = own_tree {
            self.remove_tree(key, w);
        }
    }

    fn unique_id(&self, base: &str) -> String {
        let id = session_id(base, SystemTime::now());
        let taken = |candidate: &str| {
            self.consoles.contains_key(candidate)
                || self.paused.contains_key(candidate)
                || self
                    .shared
                    .registry
                    .lock()
                    .map(|r| r.get(candidate).is_some() || r.is_stashed(candidate))
                    .unwrap_or(false)
        };
        if !taken(&id) {
            return id;
        }
        (2..)
            .map(|n| format!("{id}-{n}"))
            .find(|c| !taken(c))
            .expect("an unbounded range always finds a free id")
    }

    /// Shows a session's terminal: resumes it when paused, otherwise puts
    /// its project on the stage and gives it the keyboard. With `toggle`,
    /// a session that already has the keyboard in front collapses the
    /// stage instead, which is what a second click on its tile means.
    fn reveal(&mut self, id: &str, toggle: bool) {
        if self.paused.contains_key(id) {
            if let Err(e) = self.resume(id, true) {
                eprintln!("horadric: cannot resume {id}: {e}");
            }
            return;
        }
        let Some(serial) = self.consoles.get(id).map(|c| c.serial) else {
            if let Some((short, name, cwd)) = self.background_of(id) {
                if let Err(e) = self.launch(id, &name, cwd, Vec::new(), Run::Attach(short), true) {
                    eprintln!("horadric: cannot attach to {id}: {e}");
                }
            }
            // Otherwise started with `horadric run`: its terminal is the
            // one it was run in.
            return;
        };
        if let Some(stage) = self.stage_showing(serial) {
            if toggle && stage.is_foreground() && stage.active().as_deref() == Some(id) {
                self.close_stage();
            } else {
                stage.focus_session(id);
            }
            return;
        }
        let Some(key) = self.project_of(id) else {
            return;
        };
        if self.fill_stage(&key, false) {
            if let Some(stage) = &self.stage {
                stage.focus_session(id);
            }
        }
    }

    /// A background tile's short id, name and folder.
    fn background_of(&self, id: &str) -> Option<(String, String, PathBuf)> {
        let r = self.shared.registry.lock().ok()?;
        let s = r.get(id)?;
        Some((s.background.clone()?, s.name.clone(), PathBuf::from(&s.cwd)))
    }

    /// Ends a background tile whose session no longer runs. Asked off the
    /// UI thread, since `claude agents` takes a moment.
    fn check_background(&self, id: String, short: String) {
        let registry = Arc::clone(&self.shared.registry);
        let notify = self.notify.0 as isize;
        thread::spawn(move || {
            if agents::running(&short) != Some(false) {
                return;
            }
            if let Ok(mut r) = registry.lock() {
                if r.get(&id).is_some() {
                    r.apply(&id, &HookEvent::synthetic("SessionEnd"), SystemTime::now());
                }
            }
            post(notify, WM_HORADRIC_EVENT, 1);
        });
    }

    fn project_of(&self, id: &str) -> Option<String> {
        self.shared.registry.lock().ok()?.get(id).map(project_key)
    }

    /// The sessions of a project that have a console to show, in grid
    /// order. A clean exit has nothing left worth reading; a crash keeps its
    /// pane until resumed.
    fn grid_of(&mut self, key: &str) -> Vec<String> {
        let mut live: Vec<(usize, String)> = {
            let Ok(r) = self.shared.registry.lock() else {
                return Vec::new();
            };
            self.consoles
                .iter()
                .filter(|(id, c)| {
                    c.exit_code() != Some(0) && r.get(id).is_some_and(|s| project_key(s) == key)
                })
                .map(|(id, c)| (c.serial, id.clone()))
                .collect()
        };
        // New ones join in the order they started.
        live.sort();
        let live: Vec<String> = live.into_iter().map(|(_, id)| id).collect();
        let mut ids = layout::grid_order(
            self.shared
                .orders
                .borrow_mut()
                .entry(key.to_string())
                .or_default(),
            &live,
        );
        // A file view comes last and is never saved in the order.
        if let Some(view) = self.views.get(key) {
            ids.push(view.id.clone());
        }
        ids
    }

    /// Puts a project's sessions on the stage, opening it when closed.
    /// With `dock`, the stage moves against the tiles as a square filling
    /// top to bottom. False when the project has nothing to show.
    fn fill_stage(&mut self, key: &str, dock: bool) -> bool {
        let ids = self.grid_of(key);
        let sessions: Vec<(Arc<Console>, String)> = ids
            .iter()
            .filter_map(|id| Some((Arc::clone(self.console_of(id)?), self.name_of(id))))
            .collect();
        if sessions.is_empty() {
            return false;
        }
        let switched = self.stage.as_ref().map(|s| s.project()).as_deref() != Some(key);
        let (area, tiles_left) = self.stage_area();
        let docked = layout::square(area, tiles_left);
        if let Some(stage) = self.stage.as_ref().filter(|_| dock) {
            stage.set_visible_rect(docked);
        }
        if self.stage.is_none() {
            let place = match self.stage_rect {
                Some(r) if !dock => Place::Rect(r),
                _ => Place::Visible(docked),
            };
            match TerminalWindow::open(Rc::clone(&self.shared), place) {
                Ok(t) => self.stage = Some(t),
                Err(e) => {
                    eprintln!("horadric: cannot open the stage: {e}");
                    return false;
                }
            }
        }
        if let Some(stage) = &self.stage {
            stage.show(key, &project_name(key), sessions);
        }
        self.mark_staged();
        if switched {
            self.browsers_follow(key);
        }
        true
    }

    /// A window appeared. When it is a browser that one of our sessions
    /// started, however far down, the session takes it on and it moves
    /// beside the stage.
    fn window_shown(&mut self, id: isize) {
        if self.browsers.contains_key(&id) {
            return;
        }
        let hwnd = hwnd_of(id);
        let Some(process) = browsers::browser_process(hwnd) else {
            return;
        };
        let process = HANDLE(process.as_raw_handle());
        let Some(session) = self
            .consoles
            .iter()
            .find(|(_, c)| c.contains(process))
            .map(|(id, _)| id.clone())
        else {
            return;
        };
        browsers::track(hwnd);
        self.browsers.insert(
            id,
            Browser {
                session,
                hidden: false,
            },
        );
        self.place_browser(hwnd);
        self.mark_browsers();
    }

    fn window_gone(&mut self, id: isize) {
        browsers::untrack(hwnd_of(id));
        if self.browsers.remove(&id).is_some() {
            self.mark_browsers();
        }
    }

    /// Moves a new browser window into the space beside the stage, or
    /// beside where a new session would dock the stage when it is closed.
    fn place_browser(&self, hwnd: HWND) {
        let Some(size) = browsers::size(hwnd) else {
            return;
        };
        let (area, tiles_left) = self.stage_area();
        let stage = self
            .stage
            .as_ref()
            .and_then(|s| snapping::visible_rect(s.hwnd).ok())
            .map(|r| [r.left, r.top, r.right, r.bottom])
            .unwrap_or_else(|| layout::square(area, tiles_left));
        let place = layout::beside_stage(
            work_area(self.screen.as_deref()),
            &self.tile_rects(),
            stage,
            size,
            self.px(MARGIN_DIP),
            self.px(GAP_DIP),
            self.px(BROWSER_MIN_DIP),
        );
        if let Some(rect) = place {
            browsers::place(hwnd, rect);
        }
    }

    /// The stage switched to the project with this key: its sessions'
    /// browsers come back, just below the stage, and every other project's
    /// get out of the way.
    fn browsers_follow(&mut self, key: &str) {
        let Some(stage) = self.stage.as_ref().map(|s| s.hwnd) else {
            return;
        };
        let projects: HashMap<String, String> = match self.shared.registry.lock() {
            Ok(r) => self
                .browsers
                .values()
                .filter_map(|b| Some((b.session.clone(), project_key(r.get(&b.session)?))))
                .collect(),
            Err(_) => return,
        };
        for (&id, b) in self.browsers.iter_mut() {
            let hwnd = hwnd_of(id);
            match projects.get(&b.session) {
                // One the user minimised stays down.
                Some(p) if p == key => {
                    if b.hidden || !browsers::minimised(hwnd) {
                        browsers::show_below(hwnd, stage);
                    }
                    b.hidden = false;
                }
                Some(_) => b.hidden |= browsers::hide(hwnd),
                None => {}
            }
        }
    }

    /// Brings up a session's browser windows, from a click on its tile.
    fn show_browsers(&mut self, id: &str) {
        for (&hwnd, b) in self.browsers.iter_mut() {
            if b.session == id {
                browsers::bring_to_front(hwnd_of(hwnd));
                b.hidden = false;
            }
        }
    }

    /// Tells the tiles which sessions have a browser open.
    fn mark_browsers(&mut self) {
        self.browsers.retain(|&id, _| browsers::exists(hwnd_of(id)));
        let now: HashSet<String> = self.browsers.values().map(|b| b.session.clone()).collect();
        if *self.shared.browsing.borrow() != now {
            *self.shared.browsing.borrow_mut() = now;
            for c in &self.clusters {
                c.fit();
            }
        }
    }

    /// Makes the stage match its project: sessions that started or resumed
    /// there join, ended ones leave, and with none left it closes.
    fn sync_stage(&mut self) {
        let Some(key) = self.stage.as_ref().map(|s| s.project()) else {
            return;
        };
        if !self.fill_stage(&key, false) {
            self.close_stage();
        }
    }

    /// Swaps two sessions' places in the stage's grid, and so their tiles'.
    fn swap(&mut self, a: &str, b: &str) {
        let Some(key) = self.stage.as_ref().map(|s| s.project()) else {
            return;
        };
        if let Some(order) = self.shared.orders.borrow_mut().get_mut(&key) {
            let i = order.iter().position(|s| s == a);
            let j = order.iter().position(|s| s == b);
            if let (Some(i), Some(j)) = (i, j) {
                order.swap(i, j);
            }
        }
        self.sync_stage();
        self.fit_cluster(&key);
    }

    /// A tile was dragged to a new place: the project's order becomes its
    /// tiles' as they now stand, and the stage's grid follows.
    fn reorder(&mut self, key: &str, shown: &[String]) {
        {
            let mut orders = self.shared.orders.borrow_mut();
            let order = orders.entry(key.to_string()).or_default();
            *order = layout::reordered(order, shown);
        }
        self.fit_cluster(key);
        self.sync_stage();
    }

    fn fit_cluster(&self, key: &str) {
        for c in self.clusters.iter().filter(|c| c.key == key) {
            c.fit();
        }
    }

    /// Gives every session a place in its project's order, new ones after
    /// the rest in the order they started, so a tile and its pane take the
    /// same place from the first.
    fn order_sessions(&self) {
        let Ok(r) = self.shared.registry.lock() else {
            return;
        };
        let mut all: Vec<_> = r.all().collect();
        all.sort_by(|a, b| (a.created, &a.id).cmp(&(b.created, &b.id)));
        let mut orders = self.shared.orders.borrow_mut();
        for s in all {
            let order = orders.entry(project_key(s)).or_default();
            if !order.contains(&s.id) {
                order.push(s.id.clone());
            }
        }
    }

    /// Says when a session starts waiting on you, unless you are looking at
    /// it: the stage in front with that session in it. The tiles light up
    /// too, but an editor may be covering them.
    fn announce(&mut self) {
        let Ok(r) = self.shared.registry.lock() else {
            return;
        };
        let now: HashSet<String> = r.waiting().iter().map(|s| s.id.clone()).collect();
        let seen = self.stage.as_ref().filter(|s| s.is_foreground());
        let new: Vec<&Session> = r
            .waiting()
            .into_iter()
            .filter(|s| !self.waiting.contains(&s.id))
            .filter(|s| {
                let serial = self.consoles.get(&s.id).map(|c| c.serial);
                !seen.is_some_and(|stage| serial.is_some_and(|n| stage.shows(n)))
            })
            .collect();
        let alert = (!self.quiet)
            .then(|| {
                let waiting: Vec<inbox::Waiting> = new
                    .iter()
                    .map(|s| inbox::Waiting {
                        name: s.label(),
                        phase: &s.phase,
                        line: &s.last_line,
                    })
                    .collect();
                inbox::alert(&waiting)
            })
            .flatten();
        let about = match new.as_slice() {
            [one] => Some(one.id.clone()),
            _ => None,
        };
        drop(r);
        self.waiting = now;
        if let Some(a) = alert {
            self.alert_for = about;
            self.tasks.merge_for = None;
            self.toasts.show(Kind::Waiting, &a.title, &a.text);
        }
    }

    /// The notification was clicked: show the session it was about, or,
    /// for several, the one that has waited longest.
    fn open_alert(&mut self) {
        if let Some(m) = self.tasks.merge_for.take() {
            runner::ask_for(self, runner::Menu::Merge(m));
            return;
        }
        match self.alert_for.take() {
            Some(id)
                if self
                    .shared
                    .registry
                    .lock()
                    .is_ok_and(|r| r.get(&id).is_some()) =>
            {
                self.reveal(&id, false)
            }
            _ => self.next_waiting(),
        }
    }

    /// A step of the terminal font, for every pane at once.
    fn set_font(&mut self, step: FontStep) {
        let font = &self.shared.font;
        let size = keys::font_size(font.size(), step);
        if size == font.size() {
            return;
        }
        if let Err(e) = font.set_size(size) {
            eprintln!("horadric: cannot size the terminal font: {e}");
            return;
        }
        if let Some(stage) = &self.stage {
            stage.refont();
        }
        self.save();
    }

    /// The terminal font's family, for every pane at once.
    fn set_font_family(&mut self, family: &str) {
        if let Err(e) = self.shared.font.set_family(family) {
            eprintln!("horadric: cannot load the font {family}: {e}");
            return;
        }
        self.font_family = Some(family.to_string());
        if let Some(stage) = &self.stage {
            stage.refont();
        }
        self.save();
    }

    /// Names a session from its tile's menu. Its tile, pane and the stage's
    /// title follow.
    fn rename(&mut self, id: &str, name: &str) {
        if let Ok(mut r) = self.shared.registry.lock() {
            if let Some(s) = r.get_mut(id) {
                s.rename(Some(name));
            }
        }
        self.reconcile(false);
        self.save();
    }

    /// A session's name, what its pane's header says.
    fn name_of(&self, id: &str) -> String {
        self.shared
            .registry
            .lock()
            .ok()
            .and_then(|r| r.get(id).map(|s| s.label().to_string()))
            .or_else(|| {
                let path = self.console_of(id)?.path()?;
                Some(path.file_name()?.to_string_lossy().into_owned())
            })
            .unwrap_or_else(|| id.to_string())
    }

    /// Shows a file of a project on the stage, beside its sessions, with
    /// the keyboard. It takes the place of the file shown there before.
    fn open_view(&mut self, key: &str, dir: &Path, rel: &str) {
        let path = dir.join(rel.replace('/', "\\"));
        let id = format!("{VIEW}{key}");
        match self.views.get(key) {
            Some(v) if v.path().as_deref() == Some(path.as_path()) => {
                v.reload(self.notify);
            }
            _ => {
                let serial = self.next_serial;
                self.next_serial += 1;
                // The header names the file, then the folder it is in.
                let folder = rel.rsplit_once('/').map_or("", |(f, _)| f).to_string();
                let view = Console::view(id.clone(), serial, path, folder, self.notify);
                self.views.insert(key.to_string(), view);
            }
        }
        if self.fill_stage(key, false) {
            if let Some(stage) = &self.stage {
                stage.focus_session(&id);
            }
        }
    }

    fn close_view(&mut self, serial: usize) {
        self.views.retain(|_, v| v.serial != serial);
        self.sync_stage();
    }

    /// A project's files changed: its view reads its file again, if that
    /// was one of them.
    fn files_changed(&self, key: &str) {
        let Some(view) = self.views.get(key) else {
            return;
        };
        if view.reload(self.notify) {
            if let Some(stage) = self.stage_showing(view.serial) {
                stage.refresh(view.serial);
            }
        }
    }

    /// Tells the tiles which sessions are on the stage.
    fn mark_staged(&self) {
        let now: HashSet<String> = self
            .stage
            .as_ref()
            .map(|s| s.sessions().into_iter().collect())
            .unwrap_or_default();
        if *self.shared.staged.borrow() != now {
            *self.shared.staged.borrow_mut() = now;
            for c in &self.clusters {
                c.invalidate();
            }
        }
    }

    /// Closing the stage is done with looking, so file views go with it.
    fn close_stage(&mut self) {
        self.views.clear();
        if let Some(stage) = self.stage.take() {
            if let Some(r) = stage.rect() {
                self.stage_rect = Some(r);
            }
            self.stage_key = Some(stage.project());
            stage.destroy();
        }
        self.mark_staged();
    }

    /// Shows the session that has waited on you longest. When the one in
    /// front is waiting too, the next one after it, so pressing again moves
    /// on past a session you are not ready to answer.
    fn next_waiting(&mut self) {
        let waiting: Vec<String> = self
            .shared
            .registry
            .lock()
            .map(|r| {
                r.waiting()
                    .iter()
                    .filter(|s| self.consoles.contains_key(&s.id))
                    .map(|s| s.id.clone())
                    .collect()
            })
            .unwrap_or_default();
        let front = self
            .stage
            .as_ref()
            .filter(|s| s.is_foreground())
            .and_then(|s| s.active());
        match inbox::next(&waiting, front.as_deref()).map(str::to_string) {
            Some(id) => self.reveal(&id, false),
            None => {
                if let Some(stage) = &self.stage {
                    stage.bring_to_front();
                }
            }
        }
    }

    /// The space beside the clusters, as visible edges, and whether they
    /// are on its left: where a new session docks the stage, and what "Fit
    /// terminal beside tiles" fills. Measured from where the clusters are,
    /// so one moved to the right puts the stage on the left.
    fn stage_area(&self) -> ([i32; 4], bool) {
        layout::beside(
            work_area(self.screen.as_deref()),
            &self.tile_rects(),
            self.px(MARGIN_DIP),
            self.px(GAP_DIP),
        )
    }

    /// Where each cluster and the usage window are, as (left, top, right,
    /// bottom).
    fn tile_rects(&self) -> Vec<[i32; 4]> {
        let usage = self
            .usage_window
            .iter()
            .map(|u| (u.position(), u.size_px()));
        let stash = self
            .stash_window
            .iter()
            .map(|w| (w.position(), w.size_px()));
        let cube = self.cube_window.iter().map(|w| (w.position(), w.size_px()));
        self.clusters
            .iter()
            .map(|c| (c.position(), c.size_px()))
            .chain(usage)
            .chain(stash)
            .chain(cube)
            .map(|((x, y), (w, h))| [x, y, x + w, y + h])
            .collect()
    }

    /// DIPs in physical pixels, at the DPI the clusters are drawn at.
    fn px(&self, dip: i32) -> i32 {
        let dpi = self.clusters.first().map_or(96, |c| c.dpi());
        (dip as f32 * dpi as f32 / 96.0).round() as i32
    }

    /// Fills the space beside the clusters with the stage.
    /// Brings the stage back after it was closed, showing the project it
    /// showed then, or else the first cluster with a terminal. In front
    /// when it is already open.
    fn show_stage(&mut self) {
        if let Some(stage) = &self.stage {
            stage.bring_to_front();
            return;
        }
        let keys: Vec<String> = self.clusters.iter().map(|c| c.key.clone()).collect();
        let order = stage_order(self.stage_key.as_deref(), &keys);
        for key in order {
            if self.fill_stage(&key, false) {
                if let Some(stage) = &self.stage {
                    stage.bring_to_front();
                }
                return;
            }
        }
    }

    fn fit_stage(&mut self) {
        let (area, _) = self.stage_area();
        if let Some(stage) = &self.stage {
            stage.set_visible_rect(area);
        }
    }

    /// Once a second: drop long ended sessions and their consoles, redraw
    /// ages, save what changed.
    fn tick(&mut self) {
        if self
            .recovering
            .is_some_and(|since| since.elapsed() > RECOVERED_AFTER)
        {
            self.recovering = None;
        }
        let running: Vec<&String> = self
            .consoles
            .iter()
            .filter(|(_, c)| c.exit_code().is_none())
            .map(|(id, _)| id)
            .collect();
        let pruned = self
            .shared
            .registry
            .lock()
            .map(|mut r| {
                // A hook can say a session ended while its process runs on.
                // Every `claude` started in its terminal inherits its tag,
                // so a `claude -p` there ends the tile when it quits. Only
                // the process exiting ends a session Horadric runs.
                for id in running {
                    if r.get(id).is_some_and(|s| s.phase == Phase::Ended) {
                        r.apply(
                            id,
                            &HookEvent::synthetic(HookEvent::REGISTER),
                            SystemTime::now(),
                        );
                    }
                }
                r.prune_ended(ENDED_LINGER, SystemTime::now())
            })
            .unwrap_or(0);
        if pruned > 0 {
            // A console goes with its tile once its process is gone. One
            // still running keeps going: it will be adopted again by its
            // next hook.
            // The stage follows in `reconcile`.
            let gone: Vec<String> = self
                .consoles
                .iter()
                .filter(|(id, c)| {
                    c.exit_code().is_some()
                        && self
                            .shared
                            .registry
                            .lock()
                            .map(|r| r.get(id).is_none())
                            .unwrap_or(false)
                })
                .map(|(id, _)| id.clone())
                .collect();
            for id in gone {
                self.consoles.remove(&id);
            }
            self.reconcile(true);
        } else {
            for c in &self.clusters {
                c.invalidate();
            }
        }
        // Bringing the stage to the front says nothing to the app, so a
        // look is noticed here, within a second.
        self.identify();
        if let Some(since) = self.away.idle(idle_secs(), unix_now()) {
            self.welcome_back(since);
        }
        self.save();
    }

    /// Everything worth bringing back, as it is right now.
    fn snapshot(&self) -> SavedState {
        let mut sessions = Vec::new();
        if let Ok(r) = self.shared.registry.lock() {
            // A background session is the daemon's to keep. Found again at
            // the next start, never resumed into a second copy.
            for s in r.all().filter(|s| s.background.is_none()) {
                if let Some(p) = self.paused.get(&s.id) {
                    sessions.push(SavedSession::from_session(s, p.args.clone(), false));
                } else if let Some(c) = self.consoles.get(&s.id) {
                    if c.exit_code().is_none() && s.phase != Phase::Ended {
                        sessions.push(SavedSession::from_session(s, c.args.clone(), true));
                    }
                }
            }
        }
        let clusters = self
            .clusters
            .iter()
            .map(|c| SavedCluster {
                key: c.key.clone(),
                collapsed: c.collapsed,
                files_collapsed: c.files_collapsed(),
                tasks_collapsed: c.tasks_collapsed(),
            })
            .collect();
        // A project closed for good does not keep a place forever: only
        // one still in the recent list does.
        let mut columns = self.columns.clone();
        let recent: HashSet<String> = self.recent.iter().map(|p| folder_key(p)).collect();
        columns.forget(|k| {
            k == columns::USAGE
                || k == columns::STASH
                || k == columns::CUBE
                || recent.contains(k)
                || self.clusters.iter().any(|c| c.key == k)
        });
        SavedState {
            clusters,
            columns: columns.keys(),
            recent: self.recent.clone(),
            autostart_offered: self.autostart_offered,
            stage: self
                .stage
                .as_ref()
                .and_then(|s| s.rect())
                .or(self.stage_rect),
            on_stage: self
                .stage
                .as_ref()
                .and_then(|s| s.active())
                .filter(|id| !id.starts_with(VIEW)),
            grids: self
                .shared
                .orders
                .borrow()
                .iter()
                .map(|(key, order)| {
                    let kept: Vec<String> = order
                        .iter()
                        .filter(|id| sessions.iter().any(|s| &s.id == *id))
                        .cloned()
                        .collect();
                    (key.clone(), kept)
                })
                .filter(|(_, order)| !order.is_empty())
                .collect(),
            sessions,
            stash: self
                .shared
                .registry
                .lock()
                .map(|r| r.stashed().to_vec())
                .unwrap_or_default(),
            defaults: self.shared.defaults.borrow().clone(),
            usage: self.shared.usage.lock().ok().and_then(|u| u.clone()),
            usage_window: self.usage_window.as_ref().map(|u| SavedPanel {
                collapsed: u.collapsed.get(),
            }),
            font_size: Some(self.shared.font.size()).filter(|&s| s != keys::FONT_DEFAULT),
            quiet: self.quiet,
            sounds: self.sounds,
            font_family: self.font_family.clone(),
            screen: self.screen.clone(),
            live: !self.quit,
            recovering: self.recovering.is_some(),
            ..Default::default()
        }
    }

    /// Writes the state when it changed since the last write.
    fn save(&mut self) {
        if self.frozen {
            return;
        }
        let now = self.snapshot();
        if self.last_saved.as_ref() != Some(&now) {
            store::save(&now);
            self.last_saved = Some(now);
        }
    }

    /// Saves for the last time as quit, so the next start resumes nothing
    /// whose host is gone. With `stop`, the sessions end with the app.
    fn quit(&mut self, stop: bool) {
        self.quit = true;
        self.stop_on_exit = stop;
        self.freeze();
    }

    /// Saves one last time and stops saving.
    fn freeze(&mut self) {
        self.save();
        // Nothing brings an attached pane back, so its host must not
        // outlive this app. The session itself runs on in the daemon.
        if let Ok(r) = self.shared.registry.lock() {
            for (id, c) in &self.consoles {
                if r.get(id).is_some_and(|s| s.background.is_some()) {
                    c.kill();
                }
            }
        }
        self.frozen = true;
    }

    /// Makes the windows match the projects in the registry.
    fn reconcile(&mut self, phase_changed: bool) {
        self.read_boards(false);
        let mut projects: HashSet<String> = self
            .shared
            .registry
            .lock()
            .map(|r| r.all().map(project_key).collect())
            .unwrap_or_default();
        projects.extend(self.listed());

        // Remove clusters whose project is gone.
        let mut i = 0;
        while i < self.clusters.len() {
            if projects.contains(&self.clusters[i].key) {
                i += 1;
            } else {
                let c = self.clusters.remove(i);
                self.glides.borrow_mut().forget(c.hwnd.0 as isize);
                c.destroy();
            }
        }

        // Add clusters for new projects, off screen until laid out, or where
        // the user last put them.
        for key in &projects {
            if self.clusters.iter().any(|c| &c.key == key) {
                continue;
            }
            match Cluster::create(
                Rc::clone(&self.shared),
                key.clone(),
                project_name(key),
                self.project_dir(key),
                -10_000,
                -10_000,
            ) {
                Ok(mut c) => {
                    if let Some(place) = self.cluster_places.get(key) {
                        c.collapsed = place.collapsed;
                        c.set_files_collapsed(place.files_collapsed);
                        c.set_tasks_collapsed(place.tasks_collapsed);
                    }
                    self.clusters.push(c);
                }
                Err(e) => eprintln!("horadric: cannot create window: {e}"),
            }
        }

        self.order_sessions();
        for c in &self.clusters {
            c.fit();
        }
        self.sync_stash();
        self.sync_cube();
        self.sync_start();
        // A status line can bring the first limits, which adds rows.
        if let Some(u) = &self.usage_window {
            u.fit();
        }
        self.arrange();
        if std::env::var_os("HORADRIC_DEBUG").is_some() {
            for c in &self.clusters {
                eprintln!(
                    "cluster {} at {:?} size {:?} column {:?}",
                    c.name,
                    c.position(),
                    c.size_px(),
                    self.columns.find(&c.key)
                );
            }
        }

        // Pane headers show each session's phase.
        if phase_changed {
            if let Some(stage) = &self.stage {
                stage.invalidate();
            }
        }
        self.sync_stage();

        let (total, waiting) = self
            .shared
            .registry
            .lock()
            .map(|r| (r.len(), r.waiting().len()))
            .unwrap_or((0, 0));
        let app = if horadric_hooks::dev() {
            "Horadric dev"
        } else {
            "Horadric"
        };
        let tip = match (total, waiting) {
            _ if self.reload.is_some() => format!("{app}: reloading"),
            (0, _) => format!("{app}: no sessions"),
            (t, 0) => format!("{app}: {t} session{}", if t == 1 { "" } else { "s" }),
            (t, w) => format!("{app}: {t} sessions, {w} waiting"),
        };
        self.tray.set_tip(&tip);
        self.identify();
        self.announce();
        self.journal_phases();
    }

    /// Writes a line for each session whose phase became one worth telling:
    /// waiting, done, or ended, which a session that vanishes did too.
    fn journal_phases(&mut self) {
        let now = unix_now();
        let mut lines = Vec::new();
        let mut seen = HashSet::new();
        let mut dropped = false;
        if let Ok(r) = self.shared.registry.lock() {
            for s in r.all() {
                seen.insert(s.id.clone());
                let (project, name) = (project_key(s), s.label().to_string());
                let before = self.journaled.insert(
                    s.id.clone(),
                    (s.phase.clone(), project.clone(), name.clone()),
                );
                // A session first seen here, at a start or a reload, has
                // told nothing new yet.
                let Some((before, _, _)) = before else {
                    continue;
                };
                if before == s.phase {
                    continue;
                }
                let what = match &s.phase {
                    Phase::Waiting(_) => What::Waiting {
                        line: s.last_line.clone(),
                    },
                    Phase::Done => {
                        dropped = true;
                        What::Done {
                            line: s.last_line.clone(),
                        }
                    }
                    Phase::Ended => What::Ended,
                    _ => continue,
                };
                lines.push(Entry {
                    at: now,
                    session: s.id.clone(),
                    name,
                    project,
                    what,
                });
            }
        }
        let gone: Vec<String> = self
            .journaled
            .keys()
            .filter(|id| !seen.contains(*id))
            .cloned()
            .collect();
        for id in gone {
            let Some((phase, project, name)) = self.journaled.remove(&id) else {
                continue;
            };
            if phase != Phase::Ended && !self.frozen {
                lines.push(Entry {
                    at: now,
                    session: id,
                    name,
                    project,
                    what: What::Ended,
                });
            }
        }
        for e in &lines {
            store::journal(e);
        }
        // With the beam, which rises the moment the tile turns done.
        if dropped {
            self.sound(Loot::Drop);
        }
    }

    /// Plays `loot` when the tray says loot is heard.
    fn sound(&self, loot: Loot) {
        if self.sounds {
            sound::play(loot);
        }
    }

    /// You came back after being away since `since`: say what happened, if
    /// anything did.
    fn welcome_back(&mut self, since: u64) {
        self.listen(since, Some(since), false);
    }

    /// The catch-up from the tray or its hotkey: since this morning.
    fn listen_on_demand(&mut self) {
        let now = unix_now();
        let since = now
            .saturating_sub(local_secs())
            .min(now.saturating_sub(LISTEN_BACK));
        self.listen(since, None, true);
    }

    /// Opens the catch-up with what happened since `since`, `away` being
    /// when you left if that is why. Asked for, it opens even with nothing
    /// to tell, to say so; on coming back it stays shut.
    fn listen(&mut self, since: u64, away: Option<u64>, asked: bool) {
        if let Some(c) = self.catchup.take() {
            c.destroy();
        }
        let now = unix_now();
        let entries = store::journal_since(since);
        let groups = {
            let Ok(r) = self.shared.registry.lock() else {
                return;
            };
            let boards = self.shared.boards.borrow();
            let item_is = |e: &Entry, mark: horadric_core::tasks::Mark, title: &str| {
                boards
                    .get(&e.project)
                    .is_some_and(|b| b.tasks.iter().any(|t| t.mark == mark && t.title == title))
            };
            journal::summary(&entries, since, now, |e| match &e.what {
                What::Waiting { .. } => r.get(&e.session).is_some_and(|s| s.phase.is_waiting()),
                What::Done { .. } => r.get(&e.session).is_some_and(Session::unread),
                What::Review { title } => item_is(e, horadric_core::tasks::Mark::Review, title),
                What::Blocked { title, .. } => {
                    item_is(e, horadric_core::tasks::Mark::Blocked, title)
                }
                _ => false,
            })
        };
        if groups.is_empty() && !asked {
            return;
        }
        // Coming back to a full screen game or a presentation is not the
        // moment; the tray and the hotkey still open it.
        let state = unsafe { SHQueryUserNotificationState() }.map_or(5, |s| s.0);
        if !asked && toast::hold_back(state) {
            return;
        }
        let mut rows = catchup::rows(&groups, now, project_name);
        if rows.is_empty() {
            rows.push(catchup::Row {
                kind: layout::CatchupKind::Line { detail: false },
                text: "Nothing happened".to_string(),
                detail: String::new(),
                age: String::new(),
                section: None,
                session: String::new(),
                background: false,
            });
        }
        let clock = catchup::clock(local_secs(), now, since);
        let sub = catchup::covers(away, since, now, &clock);
        match Catchup::open(Rc::clone(&self.shared), sub, rows) {
            Ok(c) => self.catchup = Some(c),
            Err(e) => eprintln!("horadric: cannot open the catch-up: {e}"),
        }
    }

    /// The session whose pane has the keyboard, with the stage in front,
    /// has been looked at, so a turn it finished is no longer unread.
    fn identify(&mut self) {
        let Some(id) = self
            .stage
            .as_ref()
            .filter(|s| s.is_foreground())
            .and_then(|s| s.active())
        else {
            return;
        };
        let changed = self
            .shared
            .registry
            .lock()
            .is_ok_and(|mut r| r.get_mut(&id).is_some_and(Session::identify));
        if changed {
            for c in &self.clusters {
                c.invalidate();
            }
        }
    }

    /// Clicks and drags collected by the window procedures.
    fn apply_input(&mut self) {
        let inputs = INPUT.with(|q| std::mem::take(&mut *q.borrow_mut()));
        let mut relayout = false;
        for input in inputs {
            match input {
                Input::Toggle(hwnd) => {
                    if let Some(c) = self.clusters.iter_mut().find(|c| c.hwnd.0 as isize == hwnd) {
                        c.collapsed = !c.collapsed;
                        c.fit();
                        relayout = true;
                    }
                }
                Input::Drop(key, x, y) => {
                    self.carried = None;
                    self.carry_at = None;
                    self.drop_window(&key, x, y);
                }
                Input::Carry(key, Some((x, y))) => self.carry_window(&key, x, y),
                Input::Carry(_, None) => {
                    self.carried = None;
                    self.carry_at = None;
                    self.arrange();
                }
                Input::Scroll(key, notches) => {
                    if self.scroll_column(&key, notches) {
                        relayout = true;
                    }
                }
                Input::Expand(id) => self.reveal(&id, true),
                Input::Unstash(id) => self.unstash(&id, true),
                Input::ToCube(id) => {
                    self.put_in_cube(&id);
                    relayout |= self.sync_cube();
                }
                Input::CubeOut(id) => {
                    self.out_of_cube(&id);
                    relayout |= self.sync_cube();
                }
                Input::CubeMain => {
                    self.cube_main = !self.cube_main;
                    relayout |= self.sync_cube();
                }
                Input::Transmute => self.transmute(),
                Input::CubeSettled => relayout |= self.sync_cube(),
                Input::StashMenu(id) => {
                    self.stash_menu_for = Some(id);
                    post(self.notify.0 as isize, WM_HORADRIC_STASH_MENU, 0);
                }
                Input::TileMenu(id) => {
                    self.menu_for = Some(id);
                    post(self.notify.0 as isize, WM_HORADRIC_TILE_MENU, 0);
                }
                Input::ProjectMenu(key) => {
                    self.project_menu_for = Some(key);
                    post(self.notify.0 as isize, WM_HORADRIC_PROJECT_MENU, 0);
                }
                Input::New(key) => {
                    // Projects tend to sit side by side, so the picker opens
                    // in the folder that holds this one.
                    self.pick_from = self
                        .project_dir(&key)
                        .map(|d| d.parent().map(Path::to_path_buf).unwrap_or(d));
                    post(self.notify.0 as isize, WM_HORADRIC_PICK, 0);
                }
                Input::Add(key) => {
                    if let Some(dir) = self.project_dir(&key) {
                        if let Err(e) = self.start(None, dir, Vec::new()) {
                            eprintln!("horadric: cannot start session: {e}");
                        }
                    }
                }
                Input::Shell(key) => {
                    let key = key.or_else(|| self.stage.as_ref().map(|s| s.project()));
                    if let Some(key) = key {
                        self.open_shell(&key);
                    }
                }
                Input::Close(hwnd) => {
                    if self
                        .stage
                        .as_ref()
                        .is_some_and(|s| s.hwnd.0 as isize == hwnd)
                    {
                        self.close_stage();
                    }
                }
                Input::Swap(a, b) => self.swap(&a, &b),
                Input::Reorder(key, shown) => self.reorder(&key, &shown),
                Input::Browser(id) => self.show_browsers(&id),
                Input::View(key, dir, rel) => self.open_view(&key, &dir, &rel),
                Input::CloseView(serial) => self.close_view(serial),
                Input::Font(step) => self.set_font(step),
                Input::FilesChanged(key) => self.files_changed(&key),
                Input::Arrange => relayout = true,
                Input::Spotlight => {
                    self.identify();
                    for c in &self.clusters {
                        c.invalidate();
                    }
                }
                Input::SettingMenu(s, row) => {
                    self.setting_menu_for = Some((s, row));
                    post(self.notify.0 as isize, WM_HORADRIC_SETTING_MENU, 0);
                }
                Input::Picked(setting, pick) => {
                    if let Some(d) = self.dropdown.take() {
                        d.destroy();
                    }
                    if let Some(u) = &self.usage_window {
                        u.open.set(None);
                        u.invalidate();
                    }
                    if let Some(value) = pick {
                        self.set_default(setting, value);
                    }
                }
                Input::SetDefault(setting, value) => self.set_default(setting, value),
                Input::Listened(session) => {
                    if let Some(c) = self.catchup.take() {
                        c.destroy();
                    }
                    if let Some(id) = session {
                        let known = self
                            .shared
                            .registry
                            .lock()
                            .is_ok_and(|r| r.get(&id).is_some());
                        if known {
                            self.reveal(&id, false);
                        }
                    }
                }
                Input::Pick => {
                    // A new project most likely sits beside the last one.
                    self.pick_from = self
                        .recent
                        .iter()
                        .map(PathBuf::from)
                        .find(|p| p.is_dir())
                        .map(|d| d.parent().map(Path::to_path_buf).unwrap_or(d));
                    post(self.notify.0 as isize, WM_HORADRIC_PICK, 0);
                }
                Input::RecentMenu(dir) => {
                    self.recent_menu_for = Some(dir);
                    post(self.notify.0 as isize, WM_HORADRIC_RECENT_MENU, 0);
                }
                Input::StartIn(dir) => {
                    if let Err(e) = self.start(None, dir, Vec::new()) {
                        eprintln!("horadric: cannot start session: {e}");
                    }
                }
                Input::TaskClick(key, line, title) => self.task_clicked(&key, line, &title),
                Input::TaskApprove(key, line, title) => {
                    self.set_task(&key, line, &title, horadric_core::tasks::Mark::Done)
                }
                Input::TaskMenu(key, line, title) => {
                    runner::ask_for(self, runner::Menu::Item(key, line, title))
                }
                Input::TasksMode(key) => runner::ask_for(self, runner::Menu::Mode(key)),
                Input::TaskAdd(key) => runner::ask_for(self, runner::Menu::Add(key)),
            }
        }
        if relayout {
            self.arrange();
        }
    }

    /// Shows the start window while no project is open, and only then.
    fn sync_start(&mut self) {
        if !self.clusters.is_empty() {
            if let Some(s) = self.start_window.take() {
                s.destroy();
            }
            return;
        }
        match &self.start_window {
            Some(s) => {
                s.set_recent(&self.recent);
            }
            None => {
                match StartWindow::create(Rc::clone(&self.shared), &self.recent, -10_000, -10_000) {
                    Ok(s) => self.start_window = Some(s),
                    Err(e) => eprintln!("horadric: cannot create the start window: {e}"),
                }
            }
        }
    }

    /// The folder the project with this key lives in, as a session in it
    /// spelled it. A session in a worktree has the key too, but the
    /// project's own folder is the main working tree, so a session there
    /// comes first and the key itself stands in when none is. With no
    /// session, as the recent list spells it, for a project up for its
    /// task list alone.
    fn project_dir(&self, key: &str) -> Option<PathBuf> {
        let from_session = self.shared.registry.lock().ok().and_then(|r| {
            let mut dirs = r
                .all()
                .filter(|s| project_key(s) == key && !s.cwd.is_empty())
                .map(|s| s.cwd.clone());
            let first = dirs.next()?;
            let spelled = |d: &String| {
                d.replace('\\', "/")
                    .trim_end_matches('/')
                    .eq_ignore_ascii_case(key)
            };
            Some(if spelled(&first) {
                first
            } else {
                dirs.find(spelled).unwrap_or_else(|| key.to_string())
            })
        });
        from_session.map(PathBuf::from).or_else(|| {
            self.recent
                .iter()
                .find(|p| folder_key(p) == key)
                .map(PathBuf::from)
        })
    }

    /// Brings every tile window above the other windows.
    fn raise(&mut self) {
        if let Some(u) = &self.usage_window {
            u.raise();
        }
        if let Some(w) = &self.stash_window {
            w.raise();
        }
        if let Some(w) = &self.cube_window {
            w.raise();
        }
        if let Some(s) = &self.start_window {
            s.raise();
        }
        for c in &self.clusters {
            c.raise();
        }
    }

    /// Scrolls every column back to its top and lays them out again.
    fn tidy(&mut self) {
        for c in &mut self.columns.cols {
            c.scroll = 0;
        }
        self.arrange();
    }

    /// How the columns sit on the primary screen at the DPI the tiles are
    /// drawn at. None while there is no window to measure.
    fn grid(&self) -> Option<Grid> {
        let dpi = self
            .usage_window
            .as_deref()
            .map(UsageWindow::dpi)
            .or_else(|| self.start_window.as_deref().map(StartWindow::dpi))
            .or_else(|| self.clusters.first().map(|c| c.dpi()))?;
        let work = work_area(self.screen.as_deref());
        let scale = dpi as f32 / 96.0;
        let px = |dip: f32| (dip * scale).round() as i32;
        let width = px(self.shared.metrics.width);
        let margin = px(MARGIN_DIP as f32);
        let gap = px(GAP_DIP as f32);
        Some(Grid {
            left: work.0 + margin,
            right: work.2 - margin,
            top: work.1 + margin,
            height: work.3 - work.1 - 2 * margin,
            width,
            gap,
            min_files: px(layout::min_files_body(&self.shared.metrics)),
            fits: ((work.2 - work.0 - 2 * margin + gap) / (width + gap)).max(1) as usize,
            step: px(self.shared.metrics.tile_h + self.shared.metrics.gap),
        })
    }

    /// The keys that have a window right now.
    fn present(&self) -> HashSet<String> {
        self.clusters
            .iter()
            .map(|c| c.key.clone())
            .chain(self.usage_window.iter().map(|_| columns::USAGE.to_string()))
            .chain(self.stash_window.iter().map(|_| columns::STASH.to_string()))
            .chain(self.cube_window.iter().map(|_| columns::CUBE.to_string()))
            .collect()
    }

    /// The windows standing in a column with these keys, top to bottom.
    /// The start window stands in for the first project, so it goes below
    /// the usage window in the first column.
    fn column_windows(&self, keys: &[String], first: bool) -> Vec<Tile<'_>> {
        let mut out: Vec<Tile> = keys
            .iter()
            .filter_map(|k| {
                if k == columns::USAGE {
                    return self.usage_window.as_deref().map(Tile::Usage);
                }
                if k == columns::STASH {
                    return self.stash_window.as_deref().map(Tile::Stash);
                }
                if k == columns::CUBE {
                    return self.cube_window.as_deref().map(Tile::Cube);
                }
                self.clusters
                    .iter()
                    .find(|c| &c.key == k)
                    .map(|c| Tile::Cluster(c))
            })
            .collect();
        if let Some(s) = self.start_window.as_deref().filter(|_| first) {
            let at = out
                .iter()
                .position(|t| matches!(t, Tile::Usage(_)))
                .map_or(0, |i| i + 1);
            out.insert(at, Tile::Start(s));
        }
        out
    }

    /// Lays the tiles out in their columns down the left of the primary
    /// screen, a column as tall as the work area. A project not yet in a
    /// column gets the one with the most room, or a new one when none has
    /// enough and another fits. See `columns`.
    fn arrange(&mut self) {
        self.arrange_with(crate::backdrop::animations_on());
    }

    /// Lays the tiles out, gliding the windows that move when `animate`.
    fn arrange_with(&mut self, animate: bool) {
        let Some(g) = self.grid() else {
            return;
        };
        let present = self.present();
        let is_present = |k: &str| present.contains(k);
        self.columns.prune(is_present);
        if self.usage_window.is_some() && !self.columns.contains(columns::USAGE) {
            self.columns.add_first(columns::USAGE);
        }
        let mut fresh: Vec<(String, String, i32)> = self
            .clusters
            .iter()
            .filter(|c| !self.columns.contains(&c.key))
            .map(|c| (c.name.to_lowercase(), c.key.clone(), c.need_px()))
            .collect();
        // Several at once, as on the first start, go in an order that does
        // not change from one start to the next.
        fresh.sort();
        for (_, key, need) in fresh {
            let shown = self.columns.shown(g.fits, is_present);
            let rooms: Vec<i32> = shown
                .iter()
                .enumerate()
                .map(|(i, (_, keys))| {
                    let used: i32 = self
                        .column_windows(keys, i == 0)
                        .iter()
                        .map(|t| {
                            let s = t.stacked();
                            s.fixed + g.gap + s.files.map_or(0, |_| g.min_files)
                        })
                        .sum();
                    g.height - used
                })
                .collect();
            let col = columns::place_new(&rooms, need, shown.len() < g.fits);
            self.columns.add(&key, col, is_present);
        }

        let shown = self.columns.shown(g.fits, is_present);
        let mut scrolls = Vec::new();
        for (i, (model, keys)) in shown.iter().enumerate() {
            let x = g.x(i);
            let tiles = self.column_windows(keys, i == 0);
            let items: Vec<columns::Stacked> = tiles.iter().map(Tile::stacked).collect();
            let scroll = self.columns.cols[*model].scroll;
            let (filled, room) = columns::fill(&items, g.top, g.height, g.gap, g.min_files, scroll);
            scrolls.push((*model, scroll.clamp(0, room)));
            for (t, f) in tiles.iter().zip(filled) {
                // The one being dragged is where the cursor holds it.
                if t.key().is_some() && t.key() == self.carried.as_deref() {
                    continue;
                }
                t.place(x, f, &mut self.glides.borrow_mut(), animate, self.arranged);
            }
        }
        if shown.is_empty() {
            if let Some(s) = self.start_window.as_deref() {
                Tile::Start(s).place(
                    g.x(0),
                    columns::Filled {
                        y: g.top,
                        files: None,
                    },
                    &mut self.glides.borrow_mut(),
                    animate,
                    self.arranged,
                );
            }
        }
        for (model, scroll) in scrolls {
            self.columns.cols[model].scroll = scroll;
        }
        // The windows there at the start stand there at once; the ones
        // that come after arrive.
        self.arranged = !self.clusters.is_empty() || self.start_window.is_some();
        if self.glides.borrow().moving() && self.glided.get().is_none() {
            self.glided.set(Some(Instant::now()));
            unsafe {
                SetTimer(
                    Some(self.notify),
                    GLIDE_TIMER,
                    crate::motion::FRAME_FAST.as_millis() as u32,
                    None,
                );
            }
        }
    }

    /// Moves every gliding window on a frame, and stops the timer once
    /// they are all in place.
    fn glide(&mut self) {
        let now = Instant::now();
        let dt = self.glided.get().map_or(Duration::ZERO, |t| now - t);
        self.glided.set(Some(now));
        let mut glides = self.glides.borrow_mut();
        let moves = glides.step(dt, |id| window_at(HWND(id as *mut c_void)));
        unsafe {
            for (id, (x, y)) in moves {
                let _ = SetWindowPos(
                    HWND(id as *mut c_void),
                    None,
                    x,
                    y,
                    0,
                    0,
                    SWP_NOACTIVATE | SWP_NOZORDER | SWP_NOSIZE,
                );
            }
            if !glides.moving() {
                self.glided.set(None);
                let _ = KillTimer(Some(self.notify), GLIDE_TIMER);
            }
        }
    }

    /// Where a window that stands in the columns is, or is gliding to.
    fn resting(&self, t: &Tile) -> (i32, i32) {
        self.glides
            .borrow()
            .target(t.hwnd().0 as isize)
            .unwrap_or_else(|| window_at(t.hwnd()))
    }

    /// A window let go of after a drag takes the place in the columns under
    /// the cursor, or a new column right of the last when there is room.
    fn drop_window(&mut self, key: &str, x: i32, y: i32) {
        let mut cols = self.columns.clone();
        let Some((col, slot, _)) = self.drop_place(&mut cols, key, x, y) else {
            return;
        };
        let present = self.present();
        cols.move_to(key, col, slot, |k| present.contains(k));
        self.columns = cols;
        self.arrange();
        self.save();
    }

    /// A window being dragged is over (x, y): the others lay out as if it
    /// were let go there, and glide to make room. Only in a column that is
    /// already there, and not for a window alone in its own, since those
    /// change which column is where and the columns would move under the
    /// cursor. Nothing is saved until the drop.
    fn carry_window(&mut self, key: &str, x: i32, y: i32) {
        self.carried = Some(key.to_string());
        let mut cols = self.columns.clone();
        let Some((col, slot, room)) = self.drop_place(&mut cols, key, x, y) else {
            return;
        };
        let want = room.then_some((col, slot));
        if want == self.carry_at {
            return;
        }
        self.carry_at = want;
        let saved = self.columns.clone();
        if let Some((col, slot)) = want {
            let present = self.present();
            cols.move_to(key, col, slot, |k| present.contains(k));
            self.columns = cols;
        }
        self.arrange();
        self.columns = saved;
    }

    /// Where a window let go of at (x, y) goes in `cols`: its column and
    /// its place in it, and whether that column is one already there that
    /// it does not stand in alone.
    fn drop_place(
        &self,
        cols: &mut Columns,
        key: &str,
        x: i32,
        y: i32,
    ) -> Option<(usize, usize, bool)> {
        let g = self.grid()?;
        let present = self.present();
        let is_present = |k: &str| present.contains(k);
        // Moved against what the screen shows, so extra columns folded into
        // the last one on a narrow screen become part of it.
        cols.merge_past(g.fits, is_present);
        let shown = cols.shown(g.fits, is_present);
        let lefts: Vec<i32> = (0..shown.len()).map(|i| g.x(i)).collect();
        let alone = shown
            .iter()
            .any(|(_, keys)| keys.len() == 1 && keys[0] == key);
        let col = columns::drop_column(&lefts, g.width, g.gap, x, shown.len() < g.fits || alone);
        let others: Vec<(i32, i32)> = shown
            .get(col)
            .map(|(_, keys)| {
                let keys: Vec<String> = keys.iter().filter(|k| *k != key).cloned().collect();
                self.column_windows(&keys, false)
                    .iter()
                    .map(|t| {
                        let y = self.resting(t).1;
                        (y, y + t.height())
                    })
                    .collect()
            })
            .unwrap_or_default();
        let slot = columns::drop_slot(&others, y);
        Some((col, slot, col < shown.len() && !alone))
    }

    /// Scrolls the column holding the window with this key by the wheel's
    /// notches. False when that changed nothing.
    fn scroll_column(&mut self, key: &str, notches: i32) -> bool {
        let Some(g) = self.grid() else {
            return false;
        };
        let present = self.present();
        let shown = self.columns.shown(g.fits, |k| present.contains(k));
        let Some(&(model, _)) = shown.iter().find(|(_, keys)| keys.iter().any(|k| k == key)) else {
            return false;
        };
        let c = &mut self.columns.cols[model];
        let was = c.scroll;
        c.scroll = (c.scroll - notches * g.step).max(0);
        // Past the end is clamped when laid out, so a turn the other way
        // answers at once.
        c.scroll != was
    }

    /// Stands the columns on another screen, None for the primary one. The
    /// stage stays where it is, since tiles on a small screen beside a
    /// terminal on the big one is a reason to move them, unless the tiles
    /// now lie over it.
    fn move_to_screen(&mut self, screen: Option<String>) {
        if self.screen == screen {
            return;
        }
        self.screen = screen;
        self.save();
        self.screen_changed();
    }

    /// A screen came or went, or the taskbar moved: the columns fit the new
    /// work area, and a stage left off every screen or over the tiles docks
    /// beside them again.
    fn screen_changed(&mut self) {
        self.arrange_with(false);
        self.stage_rect = self.stage_rect.filter(|r| on_screen(r[0], r[1]));
        let Some(stage) = &self.stage else {
            return;
        };
        let Ok(r) = snapping::visible_rect(stage.hwnd) else {
            return;
        };
        let rect = [r.left, r.top, r.right, r.bottom];
        let covers =
            |t: &[i32; 4]| rect[0] < t[2] && t[0] < rect[2] && rect[1] < t[3] && t[1] < rect[3];
        if on_one_screen(rect) && !self.tile_rects().iter().any(covers) {
            return;
        }
        let (area, tiles_left) = self.stage_area();
        if let Some(stage) = &self.stage {
            stage.set_visible_rect(layout::square(area, tiles_left));
        }
    }
}

/// How the columns sit on the screen, in physical pixels.
struct Grid {
    left: i32,
    right: i32,
    top: i32,
    height: i32,
    width: i32,
    gap: i32,
    /// The shortest a files tile gets before its column folds it.
    min_files: i32,
    /// How many columns side by side fit on the screen.
    fits: usize,
    /// How far a notch of the wheel scrolls a column: one tile.
    step: i32,
}

impl Grid {
    /// The left edge of the `i`th column. Never off the screen to the
    /// right: overlap is better than lost.
    fn x(&self, i: usize) -> i32 {
        (self.left + i as i32 * (self.width + self.gap)).min(self.right - self.width)
    }
}

/// A window that stands in the columns.
enum Tile<'a> {
    Usage(&'a UsageWindow),
    Stash(&'a StashWindow),
    Cube(&'a CubeWindow),
    Start(&'a StartWindow),
    Cluster(&'a Cluster),
}

impl Tile<'_> {
    fn stacked(&self) -> columns::Stacked {
        match self {
            Tile::Usage(u) => columns::Stacked {
                fixed: u.size_px().1,
                files: None,
            },
            Tile::Stash(w) => columns::Stacked {
                fixed: w.size_px().1,
                files: None,
            },
            Tile::Cube(w) => columns::Stacked {
                fixed: w.size_px().1,
                files: None,
            },
            Tile::Start(s) => columns::Stacked {
                fixed: s.size_px().1,
                files: None,
            },
            Tile::Cluster(c) => columns::Stacked {
                fixed: c.fixed_px(),
                files: c.files_claim(),
            },
        }
    }

    /// Its key in the columns. The start window has none.
    fn key(&self) -> Option<&str> {
        match self {
            Tile::Usage(_) => Some(columns::USAGE),
            Tile::Stash(_) => Some(columns::STASH),
            Tile::Cube(_) => Some(columns::CUBE),
            Tile::Start(_) => None,
            Tile::Cluster(c) => Some(&c.key),
        }
    }

    fn hwnd(&self) -> HWND {
        match self {
            Tile::Usage(u) => u.hwnd,
            Tile::Stash(w) => w.hwnd,
            Tile::Cube(w) => w.hwnd,
            Tile::Start(s) => s.hwnd,
            Tile::Cluster(c) => c.hwnd,
        }
    }

    fn height(&self) -> i32 {
        match self {
            Tile::Usage(u) => u.size_px().1,
            Tile::Stash(w) => w.size_px().1,
            Tile::Cube(w) => w.size_px().1,
            Tile::Start(s) => s.size_px().1,
            Tile::Cluster(c) => c.size_px().1,
        }
    }

    /// Sends it to its place in the column at `x`, a cluster's files tile
    /// sized first. A window already on screen glides there.
    /// With `arrive`, a window coming onto the screen for the first time
    /// rises into its place and fades in.
    fn place(&self, x: i32, f: columns::Filled, glides: &mut Glides, animate: bool, arrive: bool) {
        if let Tile::Cluster(c) = self {
            c.set_files_room(f.files);
        }
        let hwnd = self.hwnd();
        let at = window_at(hwnd);
        let fresh = at.0 <= -5000 || at.1 <= -5000;
        let Some((x, y)) = glides.aim(hwnd.0 as isize, at, (x, f.y), animate) else {
            return;
        };
        // A window that was created off screen has never painted. Moving
        // it into view does not always ask it to, hence the invalidate.
        match self {
            Tile::Usage(u) => {
                u.move_to(x, y);
                u.invalidate();
            }
            Tile::Stash(w) => {
                w.move_to(x, y);
                w.invalidate();
            }
            Tile::Cube(w) => {
                w.move_to(x, y);
                w.invalidate();
            }
            Tile::Start(s) => {
                s.move_to(x, y);
                s.invalidate();
            }
            Tile::Cluster(c) => {
                c.move_to(x, y);
                c.invalidate();
            }
        }
        if fresh && arrive && !matches!(self, Tile::Usage(_)) {
            let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96) as f32;
            let rise = (appear::CLUSTER_RISE * dpi / 96.0).round() as i32;
            appear::begin(hwnd, appear::CLUSTER, rise);
        }
    }
}

/// A window's top left corner on screen.
fn window_at(hwnd: HWND) -> (i32, i32) {
    let mut r = RECT::default();
    unsafe {
        let _ = GetWindowRect(hwnd, &mut r);
    }
    (r.left, r.top)
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn hwnd_of(id: isize) -> HWND {
    HWND(id as *mut c_void)
}

/// Whether a window's top left corner, nudged inside, lands on a monitor.
fn on_screen(x: i32, y: i32) -> bool {
    let p = POINT {
        x: x + 20,
        y: y + 10,
    };
    !unsafe { MonitorFromPoint(p, MONITOR_DEFAULTTONULL) }.is_invalid()
}

/// Whether a rect lies inside the work area of one screen, a pixel either
/// way allowed.
fn on_one_screen(r: [i32; 4]) -> bool {
    let rect = RECT {
        left: r[0],
        top: r[1],
        right: r[2],
        bottom: r[3],
    };
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    let found =
        unsafe { GetMonitorInfoW(MonitorFromRect(&rect, MONITOR_DEFAULTTONEAREST), &mut info) };
    let w = info.rcWork;
    found.as_bool()
        && r[0] >= w.left - 1
        && r[1] >= w.top - 1
        && r[2] <= w.right + 1
        && r[3] <= w.bottom + 1
}

fn folder_name(cwd: &Path) -> String {
    cwd.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "session".into())
}

/// Every screen plugged in, in the order `screens::in_order` gives.
fn monitors() -> Vec<Screen> {
    unsafe extern "system" fn each(m: HMONITOR, _: HDC, _: *mut RECT, out: LPARAM) -> BOOL {
        let out = unsafe { &mut *(out.0 as *mut Vec<Screen>) };
        let mut info = MONITORINFOEXW::default();
        info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        if unsafe { GetMonitorInfoW(m, &mut info as *mut MONITORINFOEXW as *mut MONITORINFO) }
            .as_bool()
        {
            let i = info.monitorInfo;
            let len = info.szDevice.iter().position(|&c| c == 0).unwrap_or(32);
            let rect = |r: RECT| [r.left, r.top, r.right, r.bottom];
            out.push(Screen {
                name: String::from_utf16_lossy(&info.szDevice[..len]),
                bounds: rect(i.rcMonitor),
                work: rect(i.rcWork),
                primary: i.dwFlags & MONITORINFOF_PRIMARY != 0,
            });
        }
        true.into()
    }
    let mut found: Vec<Screen> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(
            None,
            None,
            Some(each),
            LPARAM(&mut found as *mut Vec<Screen> as isize),
        );
    }
    screens::in_order(found)
}

/// The work area of the screen the columns stand on, `chosen` or else the
/// primary one, as (left, top, right, bottom).
pub(crate) fn work_area(chosen: Option<&str>) -> (i32, i32, i32, i32) {
    if let Some(s) = screens::pick(&monitors(), chosen) {
        return (s.work[0], s.work[1], s.work[2], s.work[3]);
    }
    let mut r = RECT::default();
    unsafe {
        let _ = SystemParametersInfoW(
            SPI_GETWORKAREA,
            0,
            Some(&mut r as *mut RECT as *mut c_void),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        );
    }
    if r.right <= r.left {
        return (0, 0, 1920, 1080);
    }
    (r.left, r.top, r.right, r.bottom)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_stage_comes_back_with_the_project_it_showed() {
        let keys = ["a".to_string(), "b".to_string(), "c".to_string()];
        assert_eq!(stage_order(Some("b"), &keys), ["b", "a", "c"]);
        assert_eq!(stage_order(None, &keys), ["a", "b", "c"]);
        assert_eq!(stage_order(Some("gone"), &keys), ["a", "b", "c"]);
        assert!(stage_order(Some("b"), &[]).is_empty());
    }

    #[test]
    fn ending_asks_only_when_something_runs() {
        assert_eq!(end_question(Some("app"), 0, 0), None);
        assert_eq!(
            end_question(Some("app"), 1, 1).as_deref(),
            Some("End the running session in app? It is in the middle of a turn.")
        );
        assert_eq!(
            end_question(None, 4, 0).as_deref(),
            Some("End all 4 running sessions?")
        );
        assert_eq!(
            end_question(Some("app"), 4, 1).as_deref(),
            Some("End all 4 running sessions in app? One of them is in the middle of a turn.")
        );
        assert_eq!(
            end_question(Some("app"), 3, 2).as_deref(),
            Some("End all 3 running sessions in app? 2 of them are in the middle of a turn.")
        );
    }

    #[test]
    fn quitting_asks_whether_what_runs_keeps_running() {
        assert_eq!(quit_question(0, 0), None);
        let first = |a, s| {
            quit_question(a, s)
                .unwrap()
                .lines()
                .next()
                .unwrap()
                .to_string()
        };
        assert_eq!(
            first(1, 0),
            "Keep the running session going without Horadric?"
        );
        assert_eq!(
            first(2, 0),
            "Keep the 2 running sessions going without Horadric?"
        );
        assert_eq!(
            first(0, 1),
            "Keep the open terminal going without Horadric?"
        );
        assert_eq!(
            first(1, 1),
            "Keep the running session and the open terminal going without Horadric?"
        );
        assert_eq!(
            first(1, 3),
            "Keep 1 running session and 3 open terminals going without Horadric?"
        );
        assert!(quit_question(2, 0)
            .unwrap()
            .contains("\n\nStopped, a session"));
    }
}
