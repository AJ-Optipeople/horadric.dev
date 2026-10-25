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
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::ffi::c_void;
use std::net::TcpStream;
use std::os::windows::io::AsRawHandle;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use horadric_core::accounts::{Account, Accounts};
use horadric_core::agent::Agent;
use horadric_core::background::Asked;
use horadric_core::checkpoint;
use horadric_core::chronicle;
use horadric_core::diff::{self as changes, Diff, FileDiff, Recount};
use horadric_core::fleet::{self, Device};
use horadric_core::journal::{self, Entry, What};
use horadric_core::overlap::Overlap;
use horadric_core::presence;
use horadric_core::release::{self, Manifest};
use horadric_core::saved::{Discord, Side};
use horadric_core::ssh;
use horadric_core::usage::has_flag;
use horadric_core::worktree::{self as tree, Worktree};
use horadric_core::{
    session_id, Carry, HookEvent, Phase, Registry, Route, SavedCluster, SavedPanel, SavedSession,
    SavedState, Session, Setting, Usage,
};
use horadric_hooks::listener::{self, Command, KnowCall, Reload, Tagged};
use horadric_hooks::transcript::{self, Past};
use horadric_hooks::{install, TASKS_ENV};
use windows::core::{w, BOOL, PCWSTR};
use windows::Win32::Foundation::{HANDLE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateRectRgn, EnumDisplayMonitors, GetMonitorInfoW, MonitorFromPoint, MonitorFromRect,
    RedrawWindow, SetWindowRgn, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW,
    MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTONULL, RDW_ALLCHILDREN, RDW_ERASE, RDW_FRAME,
    RDW_INVALIDATE,
};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::RemoteDesktop::{
    WTSRegisterSessionNotification, NOTIFY_FOR_THIS_SESSION,
};
use windows::Win32::System::SystemInformation::{GetLocalTime, GetTickCount};
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::UI::HiDpi::{
    GetDpiForWindow, SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetLastInputInfo, RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS, LASTINPUTINFO,
    MOD_NOREPEAT,
};
use windows::Win32::UI::Shell::{SHQueryUserNotificationState, NIN_BALLOONUSERCLICK};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, EnumWindows, GetMessageW, GetWindowRect,
    GetWindowThreadProcessId, IsChild, KillTimer, PostMessageW, PostQuitMessage, RegisterClassW,
    RegisterWindowMessageW, SetTimer, SetWindowPos, SystemParametersInfoW, TranslateMessage,
    WindowFromPoint, MONITORINFOF_PRIMARY, MSG, SPI_GETWORKAREA, SPI_SETWORKAREA, SWP_NOACTIVATE,
    SWP_NOSIZE, SWP_NOZORDER, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, WM_APP, WM_DISPLAYCHANGE,
    WM_HOTKEY, WM_LBUTTONUP, WM_QUERYENDSESSION, WM_QUIT, WM_RBUTTONUP, WM_SETTINGCHANGE, WM_TIMER,
    WNDCLASSW, WS_EX_TOOLWINDOW, WS_POPUP,
};

use crate::accounts;
use crate::agents;
use crate::appear;
use crate::away::{self, AwayCard};
use crate::caption;
use crate::catchup::{self, Away, Catchup};
use crate::columns::{self, Columns};
use crate::console::{self, Console, Launch};
use crate::cube::{self as cube_window, CubeWindow};
use crate::dialog::{self, Dialog, Tone};
use crate::dropdown::{self, Dropdown, Whose};
use crate::glide::Glides;
use crate::glyphs::{self, Font};
use crate::hotkey::{self, Action, Chord, Press};
use crate::keys::{self, FontStep};
use crate::layout::{self, Metrics};
use crate::loot::Loot;
use crate::menu::{self, Item};
use crate::pick;
use crate::portal_key;
use crate::questlog::{self, QuestLog};
use crate::render::{Gpu, StashLook};
use crate::sanctuary;
use crate::screens::{self, Screen};
use crate::settings::{self, Field, SettingsWindow};
use crate::sound;
use crate::start::{self, StartWindow};
use crate::stash::{self, StashWindow};
use crate::terminal::{self, Place, TerminalWindow};
use crate::theme::Theme;
use crate::toast::{self, Kind, Toasts};
use crate::tray::{self, Choice, Tray};
use crate::usage::{self, UsageWindow};
use crate::window::{self, folder_key, project_key, project_name, Cluster, Shared};
use crate::{
    ask, autostart, browsers, explorer, history, inbox, paths, picker, recent, shell, snapping,
    store, theme, town, update, viewport, watch, web, worktree,
};

#[path = "runner.rs"]
mod runner;

#[path = "drive.rs"]
mod drive;

#[path = "chronicler.rs"]
mod chronicler;

#[path = "spectating.rs"]
mod spectating;

#[path = "portaling.rs"]
mod portaling;

#[path = "far.rs"]
mod far;

#[path = "handing.rs"]
mod handing;

#[path = "node_asks.rs"]
mod node_asks;

#[path = "keeping.rs"]
mod keeping;
pub(crate) use handing::{leaving_menu, leaving_pick};
pub(crate) use keeping::{set_auto as set_auto_takeover, set_keeper, tray_state as keeping_tray};

pub use runner::ssh_prompt;

/// Hook events changed the registry. What they did waits in [`EVENTS`].
const WM_HORADRIC_EVENT: u32 = WM_APP + 1;
/// Events the UI has not taken yet: [`EVENTS_DUE`] once a
/// `WM_HORADRIC_EVENT` is on its way, [`EVENTS_PHASE`] if one of them
/// changed a phase. A burst of events costs the UI one pass, not one each.
static EVENTS: AtomicUsize = AtomicUsize::new(0);
const EVENTS_DUE: usize = 1;
const EVENTS_PHASE: usize = 2;
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
/// Show the usage window's own menu.
const WM_HORADRIC_USAGE_MENU: u32 = WM_APP + 23;
/// Show the list of an agent's accounts, from the usage window's Account
/// row. The agent is the message's `wparam`, its place in `Agent::ALL`.
const WM_HORADRIC_ACCOUNT_MENU: u32 = WM_APP + 24;
/// Ask what the app's `web_ask` says, for a browser pane.
const WM_HORADRIC_WEB: u32 = WM_APP + 25;
/// Do what the app's `pane_ask` says, for a session's pane.
const WM_HORADRIC_PANE: u32 = WM_APP + 26;
/// The usage window's Version row clicked, handled outside the app's
/// borrow since it may ask.
const WM_HORADRIC_VERSION: u32 = WM_APP + 27;
/// A session did not start, and the app's `start_failed` says why.
const WM_HORADRIC_START_FAILED: u32 = WM_APP + 28;
/// Cast the stone the app's `stone_for` names, or offer to stop it,
/// outside the app's borrow since it may ask which session.
const WM_HORADRIC_STONE: u32 = WM_APP + 29;
/// Offer what can be done with the stone the app's `stone_menu_for`
/// names, outside the app's borrow since a menu runs a loop of its own.
const WM_HORADRIC_STONE_MENU: u32 = WM_APP + 30;
/// Drop the list for the row of the Settings window the app's
/// `settings_list_for` names.
const WM_HORADRIC_SETTINGS_LIST: u32 = WM_APP + 31;
/// Ask what a row of the Settings window needs asked first, the row the
/// app's `settings_ask` names.
const WM_HORADRIC_SETTINGS_ASK: u32 = WM_APP + 36;
/// A backup job on its thread has finished.
const WM_HORADRIC_BACKUP: u32 = WM_APP + 42;
/// Town Portal's link heard something from the relay.
const WM_HORADRIC_PORTAL: u32 = WM_APP + 40;
/// A brother's link heard something, or one of its panes asked.
const WM_HORADRIC_FAR: u32 = WM_APP + 50;
/// A brother hands a session here whose project this PC has no checkout
/// of: ask where it goes, outside the app's borrow.
const WM_HORADRIC_PLACE: u32 = WM_APP + 51;
/// Town Portal's key on the stage clicked: open it, or offer what can be
/// done with it, outside the app's borrow since a menu runs a loop of its
/// own.
const WM_HORADRIC_PORTAL_KEY: u32 = WM_APP + 41;
/// Undoing a session's last turn has finished on its thread, into the
/// app's `undone`.
const WM_HORADRIC_UNDONE: u32 = WM_APP + 47;

/// What the list of a row of the Settings window offers.
struct SettingsList {
    /// Said on top: when the pick takes hold.
    note: &'static str,
    labels: Vec<String>,
    /// The line in use.
    current: usize,
    /// The line that sits apart, so it is never picked by a slip.
    apart: Option<usize>,
}

/// The lines of the list for `agent`'s default `setting`, Default first,
/// and the place of `now` among them.
fn default_choices(agent: Agent, setting: Setting, now: Option<&str>) -> (Vec<String>, usize) {
    let choices = setting.choices(agent);
    let labels = std::iter::once("Default")
        .chain(choices.iter().map(|(_, name)| *name))
        .map(str::to_string)
        .collect();
    let current = match now {
        None => 0,
        Some(v) => choices
            .iter()
            .position(|(c, _)| *c == v)
            .map_or(0, |i| i + 1),
    };
    (labels, current)
}

/// The value line `i` of the list from [`default_choices`] stands for:
/// None for Default, which leaves it to the agent.
fn default_picked(agent: Agent, setting: Setting, i: usize) -> Option<Option<String>> {
    match i {
        0 => Some(None),
        i => setting
            .choices(agent)
            .get(i - 1)
            .map(|(v, _)| Some(v.to_string())),
    }
}

/// A button in a session pane's header, handled outside the app's borrow
/// since it may ask first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneAsk {
    /// The cross: end the session.
    End,
    /// Put the session in the stash.
    Stash,
    /// The pane's selection right clicked: copy it or hand it to an agent.
    Hand,
}

/// What is asked for a browser pane, outside the app's borrow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebAsk {
    /// Where to go.
    Address,
    /// The header's menu.
    Menu,
    /// The size the page lays out at.
    Size,
    /// The list of downloads.
    Downloads,
}

const APP_CLASS: PCWSTR = w!("HoradricApp");
/// Runs a console program without giving it a console window.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
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
/// Runs while a session works, to breathe the tray icon's light.
const BREATH_TIMER: usize = 4;
/// Runs while the stage follows the work, to give it back at the first
/// input.
const SPECTATE_TIMER: usize = 5;
/// Runs while Town Portal is on, to send a watched screen as it changes.
const PORTAL_TIMER: usize = 6;
const ENDED_LINGER: Duration = Duration::from_secs(20);
/// A crash this long after resuming sessions after a crash is a crash of
/// its own, not the same one again, so the next start resumes once more.
const RECOVERED_AFTER: Duration = Duration::from_secs(60);
/// The least time between two sweeps of one project's worktrees. Hook
/// events come many a second while an agent works.
const SWEEP_GAP: Duration = Duration::from_secs(10);
/// Starts the id of a file view, which is no session.
const VIEW: &str = "view:";
/// The start of a browser pane's id, before its project key.
pub(crate) const WEB: &str = "web:";
/// The start of an Arcane Sanctuary's id, before its project key.
const SKY: &str = "sky:";
pub(crate) const MARGIN_DIP: i32 = 12;
pub(crate) const GAP_DIP: i32 = 12;
/// The narrowest the stage gets when it gives way to a new column.
const STAGE_MIN_W_DIP: i32 = 480;
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
    /// A tile's lamp clicked: park this session's pane off the stage, or
    /// bring it back.
    Park(String),
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
    /// Ctrl+Shift+B in a pane: the browser pane of the project on the
    /// stage, opened or given the keyboard.
    Browse,
    /// Ctrl+Shift+G in a pane: the Arcane Sanctuary of the project on the
    /// stage, opened or given the keyboard.
    Sanctuary,
    /// A sanctuary's cross: close the one of the project with this key.
    CloseSky(String),
    /// A web address Ctrl+clicked in a pane: to the browser pane of the
    /// project on the stage when it has one, otherwise to the user's own
    /// browser.
    Link(String),
    /// A browser pane's cross: close the page of the project with this key.
    CloseWeb(String),
    /// The town's browser tabs changed, and with them its tiles.
    Pages(String),
    /// A browser tab's tile clicked: show that tab of the project's
    /// browser on the stage.
    Page(String, usize),
    /// A tab key in a browser pane's page, or its page closing itself.
    WebTab(String, web::TabStep),
    /// Something picked on the stage, as its reference: paste it into the
    /// prompt of the session with this id.
    Hand(String, String),
    /// A browser pane's header right clicked, or Ctrl+L in its page: what
    /// can be done with the page of the project with this key.
    WebAsk(String, WebAsk),
    /// A file view's cross or Esc: close the view with this serial.
    CloseView(usize),
    /// A session pane's cross or stash button, for the session with this
    /// id.
    PaneAsk(String, PaneAsk),
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
    SettingMenu(Agent, Setting, RECT),
    /// The usage window right clicked: its own menu.
    UsageMenu,
    /// Town Portal's key on the stage's caption clicked.
    PortalKey,
    /// The usage window's Account row clicked: the agent's accounts to
    /// switch to.
    AccountMenu(Agent),
    /// The usage window's Version row clicked: the release it names, or the
    /// notes of this one.
    Version,
    /// A setting's list closed, with the value picked, if one was.
    Picked(Agent, Setting, Option<Option<String>>),
    /// A row of the Settings window clicked, its place on screen given for
    /// a list to drop from.
    SettingsClick(Field, RECT),
    /// A list of the Settings window closed, with the place of the line
    /// picked, if one was.
    SettingsPicked(Field, Option<usize>),
    /// The Settings window's cross, Esc or Alt+F4.
    SettingsClosed,
    /// A key pressed in the Settings window while it listens for a
    /// shortcut's new chord.
    SettingsPress(Press),
    /// A slider in the usage window let go at a new value.
    SetDefault(Agent, Setting, Option<String>),
    /// The catch-up closed, with the session of the line clicked, if one
    /// was.
    Listened(Option<String>),
    /// An answer typed on the away card, for the quest `title` in the
    /// project at `dir`: to its question, or against what was `assumed`.
    Answered {
        dir: String,
        title: String,
        assumed: Option<String>,
        text: String,
    },
    /// The away card closed.
    AwayClosed,
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
    /// The gold ! beside it: start an agent that suggests quests.
    GiveQuests(String),
    /// The quest log asks, or is asked for.
    QuestLog(questlog::Ask),
    /// A stone of the Runetome of the project with this key clicked, by
    /// its label: cast it, or stop it while it is cast. None is the empty
    /// stone, which starts the Runesmith.
    Stone(String, Option<String>),
    /// A stone of that project's tome let go of here, on the screen: cast
    /// on the tile or pane under it.
    StoneDrop(String, String, POINT),
    /// A stone of that project's tome, at this place, dragged to another
    /// place in the same tome.
    StoneMove(String, usize, usize),
    /// A stone of that project's tome right clicked, by its label, None
    /// for the empty stone or the tome's header: offer what can be done
    /// with it.
    StoneMenu(String, Option<String>),
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
    /// The stage came to the front, from the taskbar, alt-tab or a click:
    /// the tiles come with it, so Horadric shows as one app.
    StageActive,
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

/// Tells the UI the registry changed, unless it has yet to hear of the
/// last change, when it will see this one with it.
/// Hands a session's transcript to the memory when a hook says it is a
/// good moment: the turn ended, the context is about to be compacted, or
/// the session is over. A subagent's hooks are its own, not the session's.
fn keep_transcript(t: &Tagged) {
    crate::memory::keep_hooked(t);
}

fn post_event(window: isize, phase: bool) {
    let flags = EVENTS_DUE | if phase { EVENTS_PHASE } else { 0 };
    if EVENTS.fetch_or(flags, Ordering::AcqRel) & EVENTS_DUE == 0 {
        post(window, WM_HORADRIC_EVENT, 0);
    }
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
    crate::suggest::register_class()?;
    settings::register_class()?;
    ask::register_class()?;
    menu::register_class()?;
    caption::register_class()?;
    dialog::register_class()?;
    toast::register_class()?;
    catchup::register_class()?;
    away::register_class()?;
    questlog::register_class()?;
    start::register_class()?;
    terminal::register_class()?;
    let notify = create_app_window()?;
    let notify_id = notify.0 as isize;
    APP_WINDOW.with(|w| w.set(notify_id));
    web::init(notify);
    // Locking the screen is going away, and unlocking it coming back.
    unsafe {
        let _ = WTSRegisterSessionNotification(notify, NOTIFY_FOR_THIS_SESSION);
    }
    store::trim_journal(unix_now());
    browsers::watch(notify, WM_HORADRIC_WINDOW_SHOWN, WM_HORADRIC_WINDOW_GONE);

    let saved = store::load();
    let keys = chords(&saved.hotkeys);
    let keys_live = Action::ALL.map(|a| register_key(notify, a, keys[a.index()]));
    web::set_sizes(&saved.page_sizes);
    theme::set_accents(&saved.accents);
    web::set_docks(&saved.page_docks);
    sanctuary::set_docks(&saved.sky_docks);
    let mut registry = Registry::new();
    let now = SystemTime::now();
    for s in &saved.sessions {
        registry.add(s.to_session(now));
    }
    registry.set_stash(&saved.stash);
    let registry = Arc::new(Mutex::new(registry));
    let usage = Arc::new(Mutex::new(saved.usage.clone()));
    let agent_usage = Arc::new(Mutex::new(saved.agent_usage.clone()));
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
    // status line are the account's, so any session's reading counts, though
    // an idle one's stale numbers never pull the bars back.
    let feed_registry = Arc::clone(&registry);
    let feed_usage = Arc::clone(&usage);
    let feed_agent_usage = Arc::clone(&agent_usage);
    thread::spawn(move || {
        // Background sessions already running get their tiles now, not at
        // their next hook, which may be a while for one that is done.
        if agents::list().is_some_and(|list| agents::adopt_all(&feed_registry, &list)) {
            post_event(notify_id, true);
        }
        let mut asked = Asked::default();
        for t in rx {
            // What a lore tool showed a session lights the sanctuary and
            // is no news of the session itself.
            if t.event.hook_event_name == HookEvent::GLOW {
                if crate::glows::hear(&feed_registry, &t.horadric_id, &t.event) {
                    post_event(notify_id, false);
                }
                continue;
            }
            keep_transcript(&t);
            if let Some(limits) = t.limits.filter(|l| !l.is_empty()) {
                let now = unix_now();
                let heard = |held: Option<&Usage>| Usage {
                    limits: held.map_or(limits.clone(), |h| h.limits.merge(&limits, now)),
                    at: now,
                };
                if t.agent == Agent::Claude {
                    if let Ok(mut u) = feed_usage.lock() {
                        *u = Some(heard(u.as_ref()));
                    }
                } else if let Ok(mut u) = feed_agent_usage.lock() {
                    let merged = heard(u.get(&t.agent));
                    u.insert(t.agent, merged);
                }
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
            if std::env::var_os("HORADRIC_DEBUG").is_some()
                && matches!(
                    t.event.hook_event_name.as_str(),
                    HookEvent::AGENTS | HookEvent::SPAWN
                )
            {
                eprintln!(
                    "mod {} for {target}: {:?} {:?} {:?}",
                    t.event.hook_event_name, t.event.agent_id, t.event.agent_type, t.event.agents
                );
            }
            let changed = feed_registry
                .lock()
                .map(|mut r| r.apply(&target, &t.event, SystemTime::now()))
                .unwrap_or(false);
            // Woken by the post below whether or not it lit anything.
            crate::glows::hear(&feed_registry, &target, &t.event);
            // A turn's end is when its keeper is sent the snapshot.
            if t.agent == Agent::Claude
                && t.event.hook_event_name == "Stop"
                && t.event.agent_id.is_none()
            {
                keeping::turn_ended(&target, &t.event.transcript_path);
            }
            post_event(notify_id, changed);
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
    crate::memory::set_fetch(crate::net::get);
    crate::memory::set_across(saved.recall_across);
    crate::memory::warm();
    let mut autostart_offered = saved.autostart_offered;
    if !autostart_offered && !horadric_hooks::dev() && autostart::running_installed() {
        autostart_offered = autostart::enable();
    }
    let mut explorer_offered = saved.explorer_offered;
    if !explorer_offered && !horadric_hooks::dev() && autostart::running_installed() {
        explorer_offered = explorer::enable();
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
    let pages = if how.resumes() {
        saved.pages.clone()
    } else {
        BTreeMap::new()
    };

    theme::set(Theme::from_key(saved.theme.as_deref()));
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
        parked: RefCell::new(HashSet::new()),
        active: RefCell::new(None),
        browsing: RefCell::new(HashSet::new()),
        usage,
        agent_usage,
        account: RefCell::new(BTreeMap::new()),
        defaults: RefCell::new({
            let mut d = saved.agent_defaults.clone();
            d.insert(Agent::Claude, saved.defaults.clone());
            d
        }),
        boards: RefCell::new(HashMap::new()),
        cube: Cell::new(None),
        tomes: RefCell::new(HashMap::new()),
        warriv: RefCell::new(HashMap::new()),
        astir: RefCell::new(HashSet::new()),
        warriv_line: RefCell::new(HashMap::new()),
    });
    menu::init(Rc::clone(&shared));
    let toasts = Toasts::new(Rc::clone(&shared), notify, WM_HORADRIC_TRAY);
    // From the hosts' copy, since a session that outlives this app keeps
    // running the status line from wherever it pointed.
    let status_settings = store::write_status_settings(&console::host_program());
    let mcp_config = store::write_mcp_config(&console::host_program());
    town::make();
    crate::backdrop::set_still(saved.still);
    let usage_window = match UsageWindow::create(
        Rc::clone(&shared),
        saved.usage_window.as_ref().is_some_and(|p| p.collapsed),
        saved.usage_window.as_ref().is_some_and(|p| p.locked),
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
            agents_found: [Agent::Codex, Agent::Grok]
                .into_iter()
                .filter(|a| console::agent_program(*a).is_some())
                .collect(),
            stash_window: None,
            cube_window: None,
            cube: Vec::new(),
            cube_main: false,
            start_window: None,
            status_settings,
            mcp_config,
            setting_menu_for: None,
            dropdown: None,
            settings_window: None,
            settings_list_for: None,
            settings_agent: Agent::Claude,
            agents_off: saved.agents_off.clone(),
            settings_ask: None,
            backup_cfg: horadric_hooks::state_dir()
                .map(|d| crate::backup::Config::load(&d))
                .unwrap_or_default(),
            backup_busy: None,
            backup_done: Arc::new(Mutex::new(Vec::new())),
            backup_checked: None,
            backup_step: None,
            backup_note: backup_started(),
            settings_project: None,
            switches: HashMap::new(),
            settings_before: None,
            accounts: accounts::load(),
            logins: Agent::ALL.map(|a| (a, Login::default())).into(),
            clusters: Vec::new(),
            consoles: HashMap::new(),
            portal: None,
            portal_checked: Default::default(),
            far: far::Far::new(notify),
            views: HashMap::new(),
            webs: HashMap::new(),
            skies: HashMap::new(),
            glow_stamp: 0,
            web_ask: None,
            pane_ask: None,
            stage: None,
            stage_rect: saved.stage.filter(|r| on_screen(r[0], r[1])),
            stage_key: None,
            tiles_edge: None,
            keys,
            keys_live,
            key_listening: None,
            keys_note: None,
            drives: saved.drives.clone(),
            tasks: runner::State::after(&saved.warriv_heard),
            stopped: saved.stopped.iter().cloned().collect(),
            away: Away::default(),
            spectating: Default::default(),
            catchup: None,
            away_card: None,
            quest_log: None,
            journaled: HashMap::new(),
            next_serial: 1,
            requests,
            notify,
            tray: Tray::add(notify, WM_HORADRIC_TRAY),
            toasts,
            glides: RefCell::default(),
            glided: Cell::new(None),
            column_bounds: HashMap::new(),
            clipped: RefCell::default(),
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
            open: saved.clusters.iter().map(|c| c.key.clone()).collect(),
            closed: saved.closed.iter().cloned().collect(),
            columns: Columns::from_keys(&saved.columns),
            recent: saved.recent.clone(),
            town_listed: None,
            autostart_offered,
            explorer_offered,
            quiet: saved.quiet,
            sounds: saved.sounds,
            town_hidden: saved.town_hidden,
            still: saved.still,
            switch_accounts: saved.switch_accounts,
            out_seen: HashMap::new(),
            all_out_told: false,
            discord: saved.discord,
            recall_across: saved.recall_across,
            rich: None,
            run: presence::Run::from_saved(saved.run),
            tome: runner::runeword::Tome::new(&saved, reload),
            cube_on: saved.cube,
            font_family: saved.font_family.clone(),
            screen: saved.screen.clone(),
            update_told: saved.update_told.clone(),
            last_saved: Some(saved),
            frozen: false,
            pick_from: None,
            start_failed: None,
            menu_for: None,
            stone_for: None,
            stone_menu_for: None,
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
            new_trees: HashMap::new(),
            recounts: HashMap::new(),
            counted: Arc::new(Mutex::new(Vec::new())),
            undone: Arc::default(),
            sweep_due: HashSet::new(),
            swept_at: HashMap::new(),
            swept_heads: Arc::new(Mutex::new(HashMap::new())),
            update: None,
            update_click: false,
            stop_agents: HashMap::new(),
            last_check: None,
            checking: false,
            looked: Arc::new(Mutex::new(None)),
            downloading: false,
            downloaded: Arc::new(Mutex::new(None)),
            experience: Arc::new(Mutex::new(None)),
            counting_xp: Arc::new(AtomicBool::new(false)),
        };
        for agent in Agent::ALL {
            app.read_login(agent);
        }
        app.reconcile(false);
        app.count_experience();
        app.attach_hosts();
        app.restore_webs(&pages);
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
    // What these hold would otherwise go with the thread's locals, once
    // the process has begun to unload its DLLs, and DXGI raises an
    // exception for a factory released that late: every quit and every
    // reload ended as a crash in the event log.
    crate::web::shutdown();
    crate::tip::release();
    crate::menu::release();
    Ok(())
}

/// The global shortcuts as state.json sets them, each one it does not
/// set at its default.
fn chords(saved: &BTreeMap<String, String>) -> [Chord; hotkey::COUNT] {
    Action::ALL.map(|a| {
        saved
            .get(a.key())
            .and_then(|s| Chord::parse(s))
            .unwrap_or_else(|| a.default_chord(horadric_hooks::dev()))
    })
}

/// Registers `action`'s shortcut on `chord`. False when another program
/// holds the chord.
fn register_key(hwnd: HWND, action: Action, chord: Chord) -> bool {
    let mods = HOT_KEY_MODIFIERS(chord.mods.flags()) | MOD_NOREPEAT;
    let ok = unsafe { RegisterHotKey(Some(hwnd), action.id(), mods, u32::from(chord.key)) };
    if let Err(e) = &ok {
        eprintln!(
            "horadric: {} is taken, no hotkey for {}: {e}",
            chord.name(),
            action.label()
        );
    }
    ok.is_ok()
}

fn unregister_key(hwnd: HWND, action: Action) {
    unsafe {
        let _ = UnregisterHotKey(Some(hwnd), action.id());
    }
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
            with_app(|app| app.portal_power("leaving"));
            with_app(App::freeze);
            return LRESULT(1);
        }
        // Brothers hear it before Windows sleeps or after a shutdown was
        // called off, rather than waiting out the silence to know.
        windows::Win32::UI::WindowsAndMessaging::WM_ENDSESSION if wparam.0 == 0 => {
            with_app(|app| app.portal_power("awake"));
            return LRESULT(0);
        }
        windows::Win32::UI::WindowsAndMessaging::WM_POWERBROADCAST => {
            match wparam.0 as u32 {
                windows::Win32::UI::WindowsAndMessaging::PBT_APMSUSPEND => {
                    with_app(|app| app.portal_power("sleeping"))
                }
                windows::Win32::UI::WindowsAndMessaging::PBT_APMRESUMESUSPEND
                | windows::Win32::UI::WindowsAndMessaging::PBT_APMRESUMEAUTOMATIC => {
                    with_app(|app| app.portal_power("awake"))
                }
                _ => None,
            };
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
                match with_app(App::take_update_click).flatten() {
                    Some(m) => offer_update(&m),
                    None => {
                        with_app(App::open_alert);
                    }
                }
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
        WM_HORADRIC_START_FAILED => {
            if let Some(why) = with_app(|app| app.start_failed.take()).flatten() {
                ask(&Dialog {
                    tone: Tone::Error,
                    title: "Cannot start the session",
                    text: &why,
                    buttons: &["OK"],
                    default: 0,
                    check: None,
                });
            }
            return LRESULT(0);
        }
        WM_HORADRIC_TILE_MENU => {
            if let Some(id) = with_app(|app| app.menu_for.take()).flatten() {
                tile_menu(&id);
            }
            return LRESULT(0);
        }
        WM_HORADRIC_STONE => {
            if let Some((key, label)) = with_app(|app| app.stone_for.take()).flatten() {
                runner::runeword::stone_clicked(&key, &label);
            }
            return LRESULT(0);
        }
        WM_HORADRIC_STONE_MENU => {
            if let Some((key, label)) = with_app(|app| app.stone_menu_for.take()).flatten() {
                runner::runeword::stone_menu(&key, label.as_deref());
            }
            return LRESULT(0);
        }
        WM_HORADRIC_STASH_MENU => {
            if let Some(id) = with_app(|app| app.stash_menu_for.take()).flatten() {
                stash_menu(&id);
            }
            return LRESULT(0);
        }
        WM_HORADRIC_USAGE_MENU => {
            usage_menu();
            return LRESULT(0);
        }
        WM_HORADRIC_PLACE => {
            handing::place_ask(hwnd);
            return LRESULT(0);
        }
        WM_HORADRIC_PORTAL_KEY => {
            portal_key();
            return LRESULT(0);
        }
        WM_HORADRIC_WEB => {
            if let Some((key, what)) = with_app(|app| app.web_ask.take()).flatten() {
                match what {
                    WebAsk::Address => go_to(&key),
                    WebAsk::Menu => web_menu(&key),
                    WebAsk::Size => size_menu(&key),
                    WebAsk::Downloads => web::downloads(&key),
                }
            }
            return LRESULT(0);
        }
        WM_HORADRIC_ACCOUNT_MENU => {
            account_menu(Agent::ALL.get(wparam.0).copied().unwrap_or_default());
            return LRESULT(0);
        }
        WM_HORADRIC_VERSION => {
            // A waiting update shows its own notes, so the notes are one
            // click away either way. The tray still looks for an update.
            match with_app(|app| app.update.clone()).flatten() {
                Some(m) => offer_update(&m),
                None => watch::open_link(
                    &crate::links::Target::Web(release::notes_url(env!("CARGO_PKG_VERSION"))),
                    None,
                ),
            }
            return LRESULT(0);
        }
        WM_HORADRIC_PANE => {
            if let Some((id, what)) = with_app(|app| app.pane_ask.take()).flatten() {
                pane_ask(&id, what);
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
                let (agent, setting, row) = app.setting_menu_for.take()?;
                let current = app
                    .shared
                    .defaults_of(agent)
                    .get(setting)
                    .map(str::to_string);
                let dpi = app.usage_window.as_ref().map_or(96, |u| u.dpi());
                Some((Rc::clone(&app.shared), agent, setting, current, row, dpi))
            })
            .flatten();
            if let Some((shared, agent, setting, current, row, dpi)) = want {
                match Dropdown::open(shared, agent, setting, current.as_deref(), row, dpi) {
                    Ok(d) => {
                        with_app(|app| app.dropped(d));
                    }
                    Err(e) => eprintln!("horadric: cannot open a setting's list: {e}"),
                }
            }
            return LRESULT(0);
        }
        WM_HORADRIC_SETTINGS_LIST => {
            // Outside the app's borrow, as a setting's list is opened.
            let want = with_app(|app| {
                let (field, row) = app.settings_list_for.take()?;
                let list = app.settings_list(field)?;
                let dpi = app.settings_window.as_ref().map_or(96, |w| w.dpi());
                Some((Rc::clone(&app.shared), field, list, row, dpi))
            })
            .flatten();
            if let Some((shared, field, list, row, dpi)) = want {
                let SettingsList {
                    note,
                    labels,
                    current,
                    apart,
                } = list;
                match Dropdown::settings(shared, field, note, labels, current, apart, row, dpi) {
                    Ok(d) => {
                        with_app(|app| app.dropped(d));
                    }
                    Err(e) => eprintln!("horadric: cannot open a setting's list: {e}"),
                }
            }
            return LRESULT(0);
        }
        WM_HORADRIC_SETTINGS_ASK => {
            settings_ask();
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
            | WM_HORADRIC_BACKUP
            | WM_HORADRIC_UNDONE
            | WM_HORADRIC_DOWNLOADED
            | WM_HORADRIC_KEPT
            | WM_HORADRIC_PORTAL
            | WM_HORADRIC_FAR
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
    let (recent, hotkeys, terminal, update, xp) = with_app(|app| {
        let xp = app.experience.lock().ok().and_then(|e| *e);
        app.count_experience();
        (
            app.recent.clone(),
            app.key_names(),
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
    let driven = with_app(|app| app.driven()).unwrap_or_default();
    let logins = with_app(App::logins).unwrap_or_default();
    let hotkeys = hotkeys.each_ref().map(Option::as_deref);
    let menu = tray::menu(
        &projects,
        hotkeys,
        &driven,
        &logins,
        terminal,
        update.as_deref(),
        xp,
    );
    match menu {
        Some(Choice::Settings) => open_settings(None),
        Some(Choice::New) => pick_and_start(hwnd, projects.first().map(PathBuf::from)),
        Some(Choice::Recent(path)) => start_logged(PathBuf::from(path)),
        Some(Choice::QuestLog(path)) => {
            push(Input::QuestLog(questlog::Ask::Open(folder_key(&path))))
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
        Some(Choice::Update) => {
            if let Some(m) = with_app(|app| app.update.clone()).flatten() {
                offer_update(&m);
            }
        }
        Some(Choice::Drive(key, on)) => {
            with_app(|app| {
                app.set_drive(&key, on);
                app.refresh_settings();
            });
        }
        Some(Choice::ShipsPublic(key, on)) => {
            if !on || runner::ships_public(&key) {
                with_app(|app| {
                    app.set_ships_public(&key, on);
                    app.refresh_settings();
                });
            }
        }
        Some(Choice::StopWarriv) => {
            with_app(App::stop_warriv);
        }
        Some(Choice::Account(agent, i)) => {
            let picked = logins
                .iter()
                .find(|l| l.agent == agent)
                .and_then(|l| l.accounts.get(i));
            if let Some(to) = picked.map(|a| a.id.clone()) {
                with_app(|app| app.switch_account(agent, to, false));
            }
        }
        Some(Choice::AddAccount(agent)) => {
            with_app(|app| app.add_account(agent));
        }
        Some(Choice::SignIn) => crate::workos::sign_in_in_background(),
        Some(Choice::SignOut) => crate::workos::sign_out(),
        Some(Choice::Keeper(keeper)) => set_keeper(keeper),
        Some(Choice::AutoTakeover(on)) => set_auto_takeover(on),
        Some(Choice::Control(id, on)) => set_control(&id, on),
        Some(Choice::CastOut(id, name)) => {
            let (line, mine) = crate::brothers::line_of(&id);
            if confirm_cast_out(&name, &line, mine) {
                cast_out(&id);
            }
        }
        Some(Choice::Leave(to)) => {
            with_app(|app| app.leave_for(to));
        }
        Some(Choice::AddNode) => match far::ask_node() {
            Some(Ok(name)) => {
                with_app(|app| {
                    app.toasts.show(
                        Kind::Done,
                        &format!("Joined {name}"),
                        "Dialling it over ssh. Its sessions show here once it answers; \
                         the tray's Linux nodes menu says how the link stands.",
                    )
                });
            }
            Some(Err(why)) => {
                with_app(|app| app.toasts.show(Kind::Failed, "Cannot join that node", &why));
            }
            None => {}
        },
        Some(Choice::ForgetNode(name)) => far::forget_node(&name),
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
                    check: None,
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

/// Opens the Settings window, or brings it back to the front when it is
/// open, at the project with key `at` when given. Outside the app's
/// borrow: it takes the focus as it opens, and the windows losing it are
/// the app's.
fn open_settings(at: Option<String>) {
    let section = match at {
        Some(_) => settings::Section::Projects,
        None => settings::Section::default(),
    };
    let want = with_app(|app| {
        if at.is_some() {
            app.settings_project = at;
            app.refresh_settings();
        }
        match &app.settings_window {
            Some(w) => {
                if section == settings::Section::Projects {
                    w.show(section);
                }
                Err(w.hwnd)
            }
            None => Ok((
                Rc::clone(&app.shared),
                app.settings_values(),
                app.tray.taskbar_icon(),
            )),
        }
    });
    match want {
        Some(Err(hwnd)) => settings::bring_back(hwnd),
        Some(Ok((shared, values, icon))) => {
            match SettingsWindow::create(shared, values, section, icon) {
                Ok(w) => {
                    with_app(|app| app.settings_window = Some(w));
                }
                Err(e) => eprintln!("horadric: cannot open the Settings window: {e}"),
            }
        }
        None => {}
    }
}

/// Asks every window of the app to paint, its children too, for a change
/// that touches all of them, such as the theme.
fn repaint_all() {
    unsafe extern "system" fn each(hwnd: HWND, _: LPARAM) -> BOOL {
        let mut pid = 0;
        unsafe {
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            if pid == GetCurrentProcessId() {
                let _ = RedrawWindow(
                    Some(hwnd),
                    None,
                    None,
                    RDW_INVALIDATE | RDW_ERASE | RDW_FRAME | RDW_ALLCHILDREN,
                );
            }
        }
        BOOL(1)
    }
    unsafe {
        let _ = EnumWindows(Some(each), LPARAM(0));
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
    /// A brother runs it: only what the brother serves.
    Far,
}

fn tile_menu(id: &str) {
    const OPEN: usize = 1;
    const END: usize = 2;
    const RENAME: usize = 3;
    const STASH: usize = 4;
    const PICK: usize = 5;
    const STOP_RUNEWORD: usize = 6;
    const FOUND: usize = 7;
    const TAKE_OVER: usize = 8;
    const UNDO: usize = 50;
    let Some((kind, shell)) = with_app(|app| app.tile_kind(id)).flatten() else {
        return;
    };
    let rename = Item::action(RENAME, "Rename\u{2026}");
    let full = with_app(|app| app.shared.registry.lock().is_ok_and(|r| r.stash_full()));
    let stash = match full {
        Some(false) => Item::action(STASH, "Stash"),
        _ => Item::Disabled("Stash is full".into()),
    };
    let live = matches!(kind, TileKind::Live);
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
        (TileKind::Far, _) => with_app(|app| app.far_menu(id, OPEN, RENAME))
            .flatten()
            .unwrap_or_default(),
        (TileKind::Background, _) => vec![
            Item::action(OPEN, "Attach"),
            rename,
            Item::Separator,
            Item::action(END, "End session"),
        ],
    };
    let mut items = items;
    // Runewords are given from the Runetome; the tile stops the one its
    // session has, with the other things done to it, above ending it.
    let word = with_app(|app| app.runeword_of(id)).flatten();
    if let (Some(w), Some(at)) = (
        word,
        items.iter().rposition(|i| matches!(i, Item::Separator)),
    ) {
        let stop = format!("Stop {} ({})", w.name, w.progress());
        items.insert(at, Item::action(STOP_RUNEWORD, stop));
    }
    if with_app(|app| app.can_undo(id)) == Some(true) {
        if let Some(at) = items.iter().rposition(|i| matches!(i, Item::Separator)) {
            items.insert(at, Item::action(UNDO, "Undo last turn"));
        }
    }
    let subagents = if live && !shell {
        with_app(|app| app.stoppable_subagents(id)).unwrap_or_default()
    } else {
        Vec::new()
    };
    if let (false, Some(at)) = (
        subagents.is_empty(),
        items.iter().rposition(|i| matches!(i, Item::Separator)),
    ) {
        items.insert(at, subagents_menu(&subagents));
    }
    let tree = with_app(|app| app.diff_of(id)).flatten();
    if let Some((w, diff)) = &tree {
        items.insert(0, changes_menu(w, diff.as_ref()));
        items.insert(1, Item::Separator);
    }
    let in_town = with_app(|app| app.project_of(id).is_some_and(|k| town::is(&k)));
    if in_town == Some(true) && live && !shell {
        items.insert(0, Item::action(FOUND, "Found a project\u{2026}"));
        items.insert(1, Item::Separator);
    }
    if with_app(|app| app.can_pick(id)) == Some(true) {
        items.insert(0, Item::action(PICK, "Pick this tomb\u{2026}"));
        items.insert(1, Item::Separator);
    }
    if let (Some(hand), Some(at)) = (
        with_app(|app| app.handoff_menu(id)).flatten(),
        items.iter().rposition(|i| matches!(i, Item::Separator)),
    ) {
        items.insert(at, hand);
    }
    if let Some(hand) = with_app(|app| app.handoff_far_menu(id)).flatten() {
        items.push(Item::Separator);
        items.push(hand);
    }
    let here = with_app(|app| app.keeping_offer(id)).flatten();
    let elsewhere = with_app(|app| app.keeping_offer_elsewhere(id)).flatten();
    if let Some(label) = here.clone().or(elsewhere.map(|(_, l)| l)) {
        items.insert(0, Item::action(TAKE_OVER, label));
        items.insert(1, Item::Separator);
    }
    let picked = menu::popup(&items);
    if picked == Some(TAKE_OVER) {
        if here.is_some() {
            with_app(|app| app.keeping_take_over(id));
        } else {
            with_app(|app| app.keeping_ask_take_over(id));
        }
        return;
    }
    if let Some(i) = picked {
        if with_app(|app| app.handoff_picked(id, i)) == Some(true) {
            return;
        }
        if with_app(|app| app.far_picked(id, i)) == Some(true) {
            return;
        }
    }
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
    if let Some((agent, _)) = picked
        .and_then(|i| i.checked_sub(STOP_SUBAGENT))
        .and_then(|i| subagents.get(i))
    {
        with_app(|app| app.stop_subagent(id, agent));
        return;
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
        Some(FOUND) => found_project(id),
        Some(STOP_RUNEWORD) => {
            with_app(|app| app.stop_runeword(id));
        }
        Some(UNDO) => {
            with_app(|app| app.undo_turn(id));
        }
        Some(STASH) if confirm_stash(id) => {
            with_app(|app| app.stash(id));
        }
        _ => {}
    }
}

/// The first of a tile menu's ids for stopping a subagent, one for each
/// after it, past every other id the menu uses.
const STOP_SUBAGENT: usize = 900;

/// The subagents a session runs, each a pick that stops it, by
/// [`STOP_SUBAGENT`] and its place in `agents`.
fn subagents_menu(agents: &[(String, String)]) -> Item {
    let items = agents
        .iter()
        .enumerate()
        .map(|(i, (_, label))| Item::action(STOP_SUBAGENT + i, format!("Stop {label}")))
        .collect();
    Item::Submenu(format!("Subagents ({})", agents.len()), items)
}

/// Asks for the new project's name, then asks the town session to found it:
/// only the session has the conversation to summarise, so it runs
/// `horadric found` with what it settled.
fn found_project(id: &str) {
    let Some(key) = with_app(|app| app.project_of(id)).flatten() else {
        return;
    };
    let prompt = match town::default_parent() {
        Some(p) => format!("It is made in {}, beside your latest project.", p.display()),
        None => "What is it called?".to_string(),
    };
    let question = ask::Ask {
        title: "Found a project",
        prompt: &prompt,
        initial: "",
        placeholder: "Project name",
        verb: "found",
        notes: false,
        secret: false,
        pick: None,
    };
    let Some(a) = ask_beside(Some(&key), &question) else {
        return;
    };
    let name = a.text.trim().to_string();
    if let Some(why) = town::check_name(&name) {
        ask(&Dialog {
            tone: Tone::Warning,
            title: "Found a project",
            text: &why,
            buttons: &["OK"],
            default: 0,
            check: None,
        });
        return;
    }
    let text = format!(
        "Found the project \"{name}\": run `horadric found \"{name}\" --summary \"<what we \
         settled, in a short paragraph>\"`, then tell me it is made.\r"
    );
    with_app(|app| {
        if let Some(c) = app.consoles.get(id) {
            c.write(text.into_bytes());
        }
    });
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
        check: None,
    });
    pressed == Some(0)
}

/// A session pane's cross or stash button. The cross ends at once, as the
/// tile menu's End does; only a stash mid turn is asked about.
fn pane_ask(id: &str, what: PaneAsk) {
    match what {
        PaneAsk::End => {
            with_app(|app| app.end(id));
        }
        PaneAsk::Stash if stash_has_room() && confirm_stash(id) => {
            with_app(|app| app.stash(id));
        }
        PaneAsk::Hand => hand_menu(id),
        _ => {}
    }
}

/// The agents on the stage, when it shows the project `key`, or whatever
/// project it shows for None.
fn stage_agents(key: Option<&str>) -> Vec<(String, String)> {
    with_app(|app| {
        app.stage
            .as_ref()
            .filter(|s| key.is_none_or(|k| s.project() == k))
            .map(|s| s.agents())
    })
    .flatten()
    .unwrap_or_default()
}

/// A line per agent to hand a pick to, numbered from `base`, the one that
/// last had the keyboard first.
fn hand_items(agents: &[(String, String)], verb: &str, base: usize) -> Vec<Item> {
    if agents.is_empty() {
        return vec![Item::Disabled(format!(
            "{verb} an agent: none on the stage"
        ))];
    }
    agents
        .iter()
        .enumerate()
        .map(|(i, (_, name))| Item::action(base + i, format!("{verb} {name}")))
        .collect()
}

/// A pane's selection right clicked: copy it, or hand it to an agent's
/// prompt as a reference to the whole of it.
fn hand_menu(id: &str) {
    const COPY: usize = 1;
    const HAND: usize = 100;
    let Some((console, name)) = with_app(|app| app.stage.as_ref()?.console_of(id)).flatten() else {
        return;
    };
    let agents: Vec<_> = stage_agents(None)
        .into_iter()
        .filter(|(a, _)| a != id)
        .collect();
    let mut items = vec![Item::action(COPY, "Copy"), Item::Separator];
    items.extend(hand_items(&agents, "Hand to", HAND));
    let picked = menu::popup(&items);
    let text = console.selection_text().unwrap_or_default();
    match picked {
        Some(COPY) if !text.is_empty() => crate::clipboard::set_text(&text),
        Some(n) if n >= HAND && !text.is_empty() => {
            let Some((to, _)) = agents.get(n - HAND) else {
                return;
            };
            let pick = match (console.path(), console.selection_lines()) {
                (Some(path), Some((first, last))) => pick::Pick::File { path, first, last },
                _ => pick::Pick::Terminal { from: name, text },
            };
            match pick::save(&pick) {
                Some(reference) => push(Input::Hand(to.clone(), reference)),
                None => return,
            }
        }
        _ => return,
    }
    if let Ok(mut s) = console.screen.lock() {
        s.term.selection = None;
    }
    with_app(|app| app.stage.as_ref().map(|s| s.refresh(console.serial)));
}

/// Lets the human pick an element in the project's page, then hands it,
/// with a picture of it, to the agent `to`.
fn pick_in_page(key: &str, to: String) {
    let Some(tab) = web::shown_tab(key) else {
        return;
    };
    web::focus(key);
    let params = serde_json::json!({
        "expression": pick::SCRIPT,
        "awaitPromise": true,
        "returnByValue": true,
    })
    .to_string();
    let owned = key.to_string();
    web::devtools(key, tab, "Runtime.evaluate", &params, move |answer| {
        // Given up, or the page went away under the pick.
        let Some(mut element) = answer.ok().as_deref().and_then(pick::element_of) else {
            web::forget_call(&owned, tab);
            return;
        };
        let crop = pick::crop_params(element.rect);
        let key = owned.clone();
        web::devtools(&owned, tab, "Page.captureScreenshot", &crop, move |shot| {
            web::forget_call(&key, tab);
            element.crop = shot.ok().and_then(|s| save_crop(&s));
            if let Some(reference) = pick::save(&pick::Pick::Element(element)) {
                push(Input::Hand(to, reference));
            }
        });
    });
}

/// Writes the picture in a `Page.captureScreenshot` answer to a pick's
/// file.
fn save_crop(answer: &str) -> Option<PathBuf> {
    let v: serde_json::Value = serde_json::from_str(answer).ok()?;
    let png = horadric_core::release::base64_decode(v.get("data")?.as_str()?)?;
    std::fs::create_dir_all(pick::dir()).ok()?;
    let path = pick::new_path("png");
    std::fs::write(&path, png).ok()?;
    Some(path)
}

/// Says so when the stash is full, as the tile menu's greyed Stash does.
fn stash_has_room() -> bool {
    let full = with_app(|app| app.shared.registry.lock().is_ok_and(|r| r.stash_full()));
    if full != Some(true) {
        return true;
    }
    ask(&Dialog {
        tone: Tone::Warning,
        title: "Stash is full",
        text: "End a stashed session or bring one back to make room.",
        buttons: &["OK"],
        default: 0,
        check: None,
    });
    false
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
        secret: false,
        pick: None,
    };
    if let Some(a) = ask_beside(Some(&key), &question) {
        with_app(|app| app.rename(id, &a.text));
    }
}

/// Shows a release's notes and installs it if asked to, outside the app's
/// borrow. The notes are the reason to update or wait, so they come
/// before the download, not after.
fn offer_update(m: &Manifest) {
    let notes = m.notes.trim();
    let text = if notes.is_empty() {
        "This release came without notes."
    } else {
        notes
    };
    let pressed = ask(&Dialog {
        tone: Tone::Question,
        title: &format!("Horadric {} is out", m.version),
        text,
        buttons: &["Update now", "Not now"],
        default: 0,
        check: None,
    });
    if pressed == Some(0) {
        with_app(|app| app.install_update());
    }
}

/// Asks with buttons, outside the app's borrow: the dialog runs a modal
/// loop that dispatches the app's messages. The button pressed, if one was.
pub(crate) fn ask(d: &Dialog) -> Option<usize> {
    ask_opened(d, |_| {})
}

/// [`ask`], telling `opened` the dialog's window once it shows.
pub(crate) fn ask_opened(d: &Dialog, opened: impl FnOnce(HWND)) -> Option<usize> {
    let shared = with_app(|app| Rc::clone(&app.shared))?;
    dialog::show_opened(&shared.gpu, &shared.metrics, d, opened)
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

/// How many past conversations a History menu lists. The last line opens
/// Claude Code's own picker for the rest.
const HISTORY: usize = 10;

/// How many hosts the project menu lists one by one. Past that they are
/// found by name instead.
const MENU_HOSTS: usize = 8;

/// The work modes the Settings window offers, worktrees off then on.
const WORK_MODES: [&str; 2] = [
    "Trunk: sessions share the tree and commit on it",
    "Branch per session: each in a worktree of its own",
];

/// What a session's console runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Run {
    Agent(Agent),
    Shell,
    /// `ssh` to this host, a shell on another machine.
    Ssh(String),
    /// `claude attach` to the background session with this short id. The
    /// daemon runs the agent; the pane only shows it.
    Attach(String),
    /// This program with the launch's arguments: `horadric runestep`
    /// running a runeword's command where it can be watched.
    Program(PathBuf),
}

/// What to say when `agent` is not installed, or was installed after
/// Horadric started and is not on the `PATH` it has.
fn not_found(agent: Agent) -> String {
    let get = match agent {
        Agent::Claude => "Install it from https://claude.com/claude-code",
        Agent::Codex => "Install it with `npm install -g @openai/codex`",
        Agent::Grok => "Install Grok Build from https://x.ai/cli",
    };
    format!(
        "Horadric cannot find {}. {get}, then try again. If it is installed, \
         quit Horadric from the tray and start it again, so it sees the new PATH.",
        agent.label()
    )
}

/// Puts the keyboard in the address bar of the project's browser pane,
/// or asks in a prompt when the pane is not on the stage.
fn go_to(key: &str) {
    if !web::edit(key) {
        ask_address(key);
    }
}

/// Asks where the project's browser pane should go.
fn ask_address(key: &str) {
    let current = web::label(key).map(|(_, url)| url).unwrap_or_default();
    let current = if current == "about:blank" {
        String::new()
    } else {
        current
    };
    let question = ask::Ask {
        title: "Go to",
        prompt: "An address, a local server such as localhost:3000, or words to search for.",
        initial: &current,
        placeholder: "localhost:3000",
        verb: "go",
        notes: false,
        secret: false,
        pick: None,
    };
    let Some(a) = ask_beside(Some(key), &question) else {
        return;
    };
    if let Some(url) = web::address(&a.text) {
        with_app(|app| app.open_web(key, Some(&url)));
    }
}

/// Asks what to find in the project's page, then finds it there with the
/// page's own find bar.
fn ask_find(key: &str) {
    let question = ask::Ask {
        title: "Find in page",
        prompt: "Words to find in the page. Ctrl+F in the page finds too.",
        initial: "",
        placeholder: "",
        verb: "find",
        notes: false,
        secret: false,
        pick: None,
    };
    if let Some(a) = ask_beside(Some(key), &question).filter(|a| !a.text.trim().is_empty()) {
        web::find(key, a.text.trim());
    }
}

/// The menu lines for the page's size, numbered from `base`: fit, each
/// preset, resize by hand, and a size typed in.
fn size_items(key: &str, base: usize) -> Vec<Item> {
    let now = web::size(key);
    let check = |id: usize, label: String, on: bool| Item::Action {
        id,
        label,
        checked: on,
    };
    let mut items = vec![check(base, "Fit to pane".into(), now.is_none())];
    for (i, (name, w, h)) in viewport::PRESETS.iter().enumerate() {
        let label = format!("{name}\t{w} × {h}");
        items.push(check(base + 1 + i, label, now == Some((*w, *h))));
    }
    let custom = now.filter(|s| !viewport::PRESETS.iter().any(|(_, w, h)| (*w, *h) == *s));
    items.push(Item::Separator);
    items.push(check(base + 10, "Resize by hand".into(), custom.is_some()));
    items.push(Item::action(base + 11, "Size..."));
    items.push(Item::Separator);
    let dock = web::dock(key).map(|d| d.side);
    items.push(check(base + 12, "In the grid".into(), dock.is_none()));
    for (i, (side, label)) in PLACES.iter().enumerate() {
        items.push(check(base + 13 + i, (*label).into(), dock == Some(*side)));
    }
    items
}

/// Where the browser pane can stand beside the grid, as the menu names
/// them.
const PLACES: [(Side, &str); 3] = [
    (Side::Left, "On the left"),
    (Side::Top, "On top"),
    (Side::Right, "On the right"),
];

/// Does what a line from [`size_items`] says. False for another line.
fn pick_size(key: &str, base: usize, picked: usize) -> bool {
    let Some(i) = picked.checked_sub(base).filter(|i| *i <= 15) else {
        return false;
    };
    match i {
        0 => web::set_size(key, None),
        10 => {
            // From the size it shows at now, so the grips start where the
            // page already is.
            if web::size(key).is_none() {
                web::set_size(key, Some(web::room(key).unwrap_or((1280, 800))));
            }
        }
        11 => ask_size(key),
        12 => web::set_dock(key, None),
        13..=15 => {
            let (side, _) = PLACES[i - 13];
            if web::dock(key).map(|d| d.side) != Some(side) {
                web::toggle_dock(key, side);
            }
        }
        n => {
            if let Some((_, w, h)) = viewport::PRESETS.get(n - 1) {
                web::set_size(key, Some((*w, *h)));
            }
        }
    }
    true
}

/// The size button's menu.
fn size_menu(key: &str) {
    if let Some(picked) = menu::popup(&size_items(key, 1)) {
        pick_size(key, 1, picked);
    }
}

/// Asks for a size to lay the page out at.
fn ask_size(key: &str) {
    let current = web::size(key).map_or(String::new(), |(w, h)| format!("{w} × {h}"));
    let question = ask::Ask {
        title: "Page size",
        prompt: "The width and height to lay the page out at, in CSS pixels.",
        initial: &current,
        placeholder: "1024 × 768",
        verb: "resize",
        notes: false,
        secret: false,
        pick: None,
    };
    let Some(a) = ask_beside(Some(key), &question) else {
        return;
    };
    if let Some(size) = viewport::parse(&a.text) {
        web::set_size(key, Some(size));
    }
}

/// What can be done with the project's browser pane.
fn web_menu(key: &str) {
    const ADDRESS: usize = 1;
    const BACK: usize = 2;
    const FORWARD: usize = 3;
    const RELOAD: usize = 4;
    const OUTSIDE: usize = 5;
    const CLOSE: usize = 6;
    const NEW_TAB: usize = 7;
    const CLOSE_TAB: usize = 8;
    const FIND: usize = 9;
    const DOWNLOADS: usize = 10;
    const SIZE: usize = 100;
    const PICK: usize = 200;
    let agents = stage_agents(Some(key));
    let items = vec![
        Item::action(NEW_TAB, "New tab\tCtrl+T"),
        Item::action(CLOSE_TAB, "Close tab\tCtrl+W"),
        Item::Separator,
        Item::action(ADDRESS, "Go to...\tCtrl+L"),
        Item::action(BACK, "Back\tAlt+Left"),
        Item::action(FORWARD, "Forward\tAlt+Right"),
        Item::action(RELOAD, "Reload\tF5"),
        Item::Separator,
        Item::action(FIND, "Find in page...\tCtrl+F"),
        Item::action(DOWNLOADS, "Downloads\tCtrl+J"),
        Item::Submenu("Size and place".into(), size_items(key, SIZE)),
        Item::action(OUTSIDE, "Open in your browser"),
        Item::Separator,
    ];
    let mut items = items;
    items.extend(hand_items(&agents, "Pick an element for", PICK));
    items.extend([Item::Separator, Item::action(CLOSE, "Close")]);
    match menu::popup(&items) {
        Some(ADDRESS) => go_to(key),
        Some(NEW_TAB) => web::tab(key, web::TabStep::New),
        Some(CLOSE_TAB) => web::tab(key, web::TabStep::Close(None)),
        Some(BACK) => web::go(key, web::Step::Back),
        Some(FORWARD) => web::go(key, web::Step::Forward),
        Some(RELOAD) => web::go(key, web::Step::Reload),
        Some(FIND) => ask_find(key),
        Some(DOWNLOADS) => web::downloads(key),
        Some(OUTSIDE) => {
            if let Some((_, url)) = web::label(key).filter(|(_, u)| u.starts_with("http")) {
                watch::open_link(&crate::links::Target::Web(url), None);
            }
        }
        Some(CLOSE) => {
            with_app(|app| app.close_web(key));
        }
        Some(n) if n >= PICK => {
            if let Some((to, _)) = agents.get(n - PICK) {
                pick_in_page(key, to.clone());
            }
        }
        Some(picked) => {
            pick_size(key, SIZE, picked);
        }
        None => {}
    }
}

fn project_menu(key: &str) {
    const ADD: usize = 1;
    const START_BATCH: usize = 2;
    const START_OVER: usize = 3;
    const END_ALL: usize = 4;
    const SHELL: usize = 5;
    const BROWSE: usize = 14;
    const SANCTUARY: usize = 16;
    const QUESTS: usize = 15;
    const CODE: usize = 7;
    const EXPLORE: usize = 8;
    const SETTINGS: usize = 9;
    const ADD_TREE: usize = 6;
    const SSH_FIND: usize = 11;
    const CLOSE: usize = 12;
    const HIDE_TOWN: usize = 13;
    const SSH: usize = 20;
    const SSH_END: usize = 200;
    const MERGE: usize = 300;
    const MERGE_END: usize = 400;
    const ADD_AGENT: usize = 500;
    const PAST: usize = 600;
    let dir = with_app(|app| app.project_dir(key)).flatten();
    // Reading transcripts and asking git are the slow part of opening the
    // menu, so they are done side by side.
    let held = with_app(|app| app.held_conversations()).unwrap_or_default();
    let (past, mut merges, own_trees) = match &dir {
        Some(d) => std::thread::scope(|scope| {
            let past = scope.spawn(|| past_in(d, &held, HISTORY));
            let merges = scope.spawn(|| runner::merges(d));
            // Only a repository's main tree can add worktrees.
            let own_trees = worktree::main_tree(d)
                .is_some()
                .then(|| horadric_hooks::tasks::worktrees(d).enabled);
            (
                past.join().unwrap_or_default(),
                merges.join().unwrap_or_default(),
                own_trees,
            )
        }),
        None => (Vec::new(), Vec::new(), None),
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
    merges.truncate(MERGE_END - MERGE);
    let mut items = vec![Item::action(ADD, "New session")];
    // In trunk mode a worktree is had by asking for one.
    if own_trees == Some(false) {
        items.push(Item::action(ADD_TREE, "New session in its own worktree"));
    }
    // Claude Code is what the plus starts. Another agent is offered when
    // it is installed and not turned off in the Settings window.
    let off = with_app(|app| app.agents_off.clone()).unwrap_or_default();
    let others: Vec<Agent> = [Agent::Codex, Agent::Grok]
        .into_iter()
        .filter(|a| !off.contains(a) && console::agent_program(*a).is_some())
        .collect();
    for (i, a) in others.iter().enumerate() {
        items.push(Item::action(
            ADD_AGENT + i,
            format!("New {} session", a.label()),
        ));
    }
    items.extend([
        Item::Submenu("History".into(), history_items(&past, PAST)),
        Item::action(QUESTS, "Quest log..."),
        Item::action(START_BATCH, format!("Start {BATCH} sessions")),
        Item::action(START_OVER, format!("Start over with {BATCH} sessions")),
        Item::Separator,
        Item::action(SHELL, "New terminal\tCtrl+Shift+T"),
        Item::action(BROWSE, "Browser\tCtrl+Shift+B"),
        Item::action(SANCTUARY, "Arcane Sanctuary\tCtrl+Shift+G"),
    ]);
    for (i, host) in listed.iter().enumerate() {
        items.push(Item::action(SSH + i, format!("SSH to {host}")));
    }
    if finding {
        items.push(Item::action(SSH_FIND, "SSH to..."));
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
        Item::action(SETTINGS, "Project settings\u{2026}"),
        Item::Separator,
        Item::action(END_ALL, "End all sessions"),
    ]);
    // The town is not closed but hidden, and only once no session runs
    // there, or the hide would not show.
    if !town::is(key) {
        items.push(Item::action(CLOSE, "Close project"));
    } else if with_app(|app| app.sessions_in(key)) == Some(0) {
        items.push(Item::action(HIDE_TOWN, "Hide Tristram"));
    } else {
        items.push(Item::Disabled(
            "Hide Tristram (end its sessions first)".into(),
        ));
    }
    let picked = menu::popup(&items);
    match (picked, &dir) {
        (Some(SSH_FIND), Some(_)) => return ssh_to(key, &hosts, &devices),
        (Some(QUESTS), _) => return push(Input::QuestLog(questlog::Ask::Open(key.into()))),
        (Some(SETTINGS), _) => return open_settings(Some(key.to_string())),
        _ => {}
    }
    let ending = matches!(picked, Some(START_OVER | END_ALL | CLOSE));
    if ending && !confirm_end(Some(key)) {
        return;
    }
    with_app(|app| match picked {
        Some(ADD) => app.add_sessions(key, 1),
        Some(ADD_TREE) => {
            if let Some(dir) = app.project_dir(key) {
                if let Err(e) = app.start_in(None, dir, Vec::new(), Agent::Claude, true) {
                    eprintln!("horadric: cannot start session: {e}");
                }
            }
        }
        Some(i) if (ADD_AGENT..ADD_AGENT + others.len()).contains(&i) => {
            if let Some(dir) = app.project_dir(key) {
                if let Err(e) = app.start_in(None, dir, Vec::new(), others[i - ADD_AGENT], false) {
                    eprintln!("horadric: cannot start session: {e}");
                }
            }
        }
        Some(START_BATCH) => app.add_sessions(key, BATCH),
        Some(START_OVER) => app.start_over(key, BATCH),
        Some(END_ALL) => app.end_all(Some(key)),
        Some(CLOSE) => app.close_project(key),
        Some(HIDE_TOWN) => app.hide_town(true),
        Some(SHELL) => app.open_shell(key),
        Some(BROWSE) => app.open_web(key, None),
        Some(SANCTUARY) => app.open_sky(key),
        Some(i) if (SSH..SSH_END).contains(&i) => app.open_ssh(key, &listed[i - SSH], None),
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
        secret: false,
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

/// A restore's next question, once the backups at the target are known.
enum RestoreStep {
    /// Which machine's backup, of these.
    Machines(Vec<String>),
    /// The passphrase of this machine's backup, the one kept not opening it.
    Passphrase(String),
}

/// Swaps in a restore staged before the last quit, now, while nothing has
/// opened the memory. What to tell of it, once there is a screen to tell
/// it on.
fn backup_started() -> Option<(bool, String)> {
    let state = horadric_hooks::state_dir()?;
    match crate::backup::run::apply_pending(&state)? {
        Ok(kept) => {
            let beside = kept
                .first()
                .and_then(|p| p.parent())
                .map_or(String::new(), |p| {
                    format!(" The old ones are kept in {}.", p.display())
                });
            Some((
                false,
                format!("The memory and the lore are back from the backup.{beside}"),
            ))
        }
        Err(e) => Some((
            true,
            format!("The restored memory could not be put in place: {e}"),
        )),
    }
}

/// A size in the Backup section's words.
fn backup_size(bytes: u64) -> String {
    match bytes {
        b if b >= 1 << 20 => format!("{:.1} MB", b as f64 / (1 << 20) as f64),
        b if b >= 1 << 10 => format!("{} KB", b >> 10),
        b => format!("{b} bytes"),
    }
}

/// What a row of the Settings window asks before it changes anything:
/// a host to add, or whether Warriv may ship public. Outside the app's
/// borrow, since a question waits for its answer.
fn settings_ask() {
    if let Some(field) = with_app(|app| app.settings_ask.take_if(|f| f.asks_backup())).flatten() {
        backup_ask(field);
        with_app(|app| app.refresh_settings());
        return;
    }
    let Some((field, key, dir, on)) = with_app(|app| {
        let field = app.settings_ask.take()?;
        let key = app.settings_key()?;
        let dir = app.project_dir(&key)?;
        let on = app.drive_of(&key).is_some_and(|d| d.ships_public);
        Some((field, key, dir, on))
    })
    .flatten() else {
        return;
    };
    match field {
        Field::AddHost => {
            let hosts = horadric_hooks::tasks::hosts(&dir);
            let mut offer = horadric_hooks::tasks::ssh_config_hosts();
            offer.retain(|h| !hosts.contains(h));
            ask_host(&key, &dir, &offer);
        }
        Field::ShipsPublic if on => {
            with_app(|app| app.set_ships_public(&key, false));
        }
        Field::ShipsPublic if runner::ships_public(&key) => {
            with_app(|app| app.set_ships_public(&key, true));
        }
        _ => {}
    }
    with_app(|app| app.refresh_settings());
}

/// A secret asked of the human, kept out of the app's borrow while it
/// waits.
fn ask_secret(title: &str, prompt: &str, verb: &str) -> Option<String> {
    let question = ask::Ask {
        title,
        prompt,
        initial: "",
        placeholder: "",
        verb,
        notes: false,
        secret: true,
        pick: None,
    };
    ask_beside(None, &question).map(|a| a.text)
}

/// A new backup passphrase, typed twice. A slip is asked about again
/// rather than told in a toast, which is easy to miss and keeps nothing.
fn ask_new_passphrase() -> Option<String> {
    const FIRST: &str = "Eight characters or more. It is kept in Windows Credential Manager so backups run by themselves. Without it the backup cannot be read, by you or anyone.";
    let mut prompt = FIRST;
    loop {
        let first = ask_secret("Backup passphrase", prompt, "go on")?;
        if first.chars().count() < 8 {
            prompt = "That was shorter than eight characters. Choose one of eight or more.";
            continue;
        }
        let again = ask_secret(
            "Backup passphrase",
            "Type the same passphrase again, to be sure it is the one you meant.",
            "keep it",
        )?;
        if first == again {
            return Some(first);
        }
        prompt = "The two did not match, so nothing was kept. Choose the passphrase again.";
    }
}

/// The questions of the Backup section: the folder, the passphrase, and
/// the restore's machine and passphrase.
fn backup_ask(field: Field) {
    match field {
        Field::BackupFolder => {
            let Some((owner, start)) = with_app(|app| {
                let owner = app.settings_window.as_ref().map(|w| w.hwnd);
                (owner.unwrap_or_default(), app.backup_cfg.folder.clone())
            }) else {
                return;
            };
            if let Some(dir) = picker::pick_folder(owner, start.as_deref()) {
                with_app(|app| app.backup_folder(dir));
            }
        }
        Field::BackupPassphrase => {
            if let Some(chosen) = ask_new_passphrase() {
                with_app(|app| app.backup_passphrase(chosen));
            }
        }
        Field::BackupRestore => {
            let Some(step) = with_app(|app| app.backup_step.take()).flatten() else {
                return;
            };
            match step {
                RestoreStep::Machines(list) => {
                    let Some(machine) = pick_machine(list) else {
                        return;
                    };
                    with_app(|app| app.backup_restore(machine, None));
                }
                RestoreStep::Passphrase(machine) => {
                    let prompt = format!("The passphrase {machine}'s backup was made with.");
                    if let Some(p) = ask_secret("Restore", &prompt, "restore") {
                        with_app(|app| app.backup_restore(machine, Some(p)));
                    }
                }
            }
        }
        _ => {}
    }
}

/// Which machine's backup to restore: this one's when it has one and it
/// is the only one, otherwise the human picks.
fn pick_machine(list: Vec<String>) -> Option<String> {
    if let [only] = list.as_slice() {
        return Some(only.clone());
    }
    let here = crate::backup::config::machine();
    let initial = if list.contains(&here) {
        here
    } else {
        String::new()
    };
    let suggest = |text: &str| {
        let text = text.trim().to_lowercase();
        list.iter()
            .filter(|m| m.to_lowercase().contains(&text))
            .map(|m| ask::Suggestion {
                label: m.clone(),
                detail: String::new(),
                value: m.clone(),
            })
            .collect()
    };
    let check = |text: &str| {
        (!list.iter().any(|m| m == text.trim())).then(|| "Pick one of the machines".to_string())
    };
    let pick = ask::Pick {
        suggest: &suggest,
        check: &check,
        browse: false,
        glyph: '\u{E7F8}',
        paths: false,
    };
    let question = ask::Ask {
        title: "Restore",
        prompt:
            "Whose backup? It replaces the memory and the lore here; the ones it replaces are kept.",
        initial: &initial,
        placeholder: "A computer's name",
        verb: "restore",
        notes: false,
        secret: false,
        pick: Some(&pick),
    };
    ask_beside(None, &question).map(|a| a.text.trim().to_string())
}

/// Asks for a host to add to the project, anything `ssh` takes, the
/// `~/.ssh/config` names in `offer` found as it is typed.
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
        secret: false,
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

/// Every agent's past conversations in `dir` together, the newest
/// `limit`, newest first, leaving out the ones in `held`.
fn past_in(dir: &Path, held: &[String], limit: usize) -> Vec<Past> {
    let dir = dir.to_string_lossy();
    let mut past = transcript::history(&dir, held, limit);
    past.extend(horadric_hooks::codex::history(&dir, held, limit));
    past.extend(horadric_hooks::grok::history(&dir, held, limit));
    past.sort_by_key(|p| std::cmp::Reverse(p.modified));
    past.truncate(limit);
    past
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

/// Town Portal's key clicked. Closed, it opens the portal and shows the
/// code a phone scans; open, it offers another code or closing it.
fn portal_key() {
    const PAIR: usize = 1;
    const CLOSE: usize = 2;
    let Some(dir) = horadric_hooks::state_dir() else {
        return;
    };
    let config = horadric_core::portal::Config::load(&dir).filter(|c| c.enabled);
    let Some(config) = config else {
        portal_pair(&dir);
        return;
    };
    let host = config
        .relay
        .trim_start_matches("wss://")
        .trim_start_matches("ws://")
        .to_string();
    let items = [
        Item::Disabled(format!("Town Portal, through {host}")),
        Item::Separator,
        Item::action(PAIR, "Pair a phone..."),
        Item::action(CLOSE, "Close the portal"),
    ];
    match menu::popup(&items) {
        Some(PAIR) => portal_pair(&dir),
        Some(CLOSE) => {
            if let Some(mut c) = horadric_core::portal::Config::load(&dir) {
                c.enabled = false;
                c.ticket = None;
                if let Err(e) = c.save(&dir) {
                    eprintln!("horadric: cannot close the portal: {e}");
                }
            }
            with_app(|app| app.portal_soon());
        }
        _ => {}
    }
}

/// Opens the portal if it is closed and shows a new code to pair with.
fn portal_pair(dir: &std::path::Path) {
    if let Err(e) = crate::portal_pair::pair_a_phone(dir) {
        ask(&Dialog {
            tone: Tone::Error,
            title: "Town Portal did not open",
            text: &e,
            buttons: &["OK"],
            default: 0,
            check: None,
        });
    }
    with_app(|app| app.portal_soon());
}

/// The usage window's menu, for what the settings rows do not hold.
fn usage_menu() {
    const CUBE: usize = 1;
    let on = with_app(|app| app.cube_on).unwrap_or_default();
    let items = [Item::Action {
        id: CUBE,
        label: "Horadric Cube".into(),
        checked: on,
    }];
    if menu::popup(&items) == Some(CUBE) {
        with_app(|app| {
            app.cube_on = !on;
            app.save();
            if app.sync_cube() {
                app.arrange();
            }
        });
    }
}

/// `agent`'s accounts: a pick switches to it, and Add account logs in to
/// another.
fn account_menu(agent: Agent) {
    const ADD: usize = 1;
    const NOW: usize = 2;
    const PICK: usize = 100;
    const FORGET: usize = 200;
    let Some((all, live, switching)) = with_app(|app| app.account_choices(agent)) else {
        return;
    };
    let all: Vec<Account> = all.of(agent).cloned().collect();
    let now = unix_now();
    let mut items: Vec<Item> = all
        .iter()
        .enumerate()
        .map(|(i, a)| Item::Action {
            id: PICK + i,
            label: a.label_at(now),
            checked: live.as_deref() == Some(a.id.as_str()),
        })
        .collect();
    if items.is_empty() {
        items.push(Item::Disabled(format!(
            "Not logged in to {}",
            agent.provider()
        )));
    }
    if let Some((to, waiting)) = &switching {
        let email = all
            .iter()
            .find(|a| &a.id == to)
            .map_or("", |a| a.email.as_str());
        let turns = if *waiting == 1 { "turn" } else { "turns" };
        items.push(Item::Separator);
        items.push(Item::Disabled(format!(
            "Switching to {email} after {waiting} {turns}"
        )));
        items.push(Item::action(NOW, "Switch now"));
    }
    items.push(Item::Separator);
    items.push(Item::action(ADD, "Add account"));
    let others: Vec<Item> = all
        .iter()
        .enumerate()
        .filter(|(_, a)| live.as_deref() != Some(a.id.as_str()))
        .map(|(i, a)| Item::action(FORGET + i, a.email.clone()))
        .collect();
    if !others.is_empty() {
        items.push(Item::Submenu("Forget".into(), others));
    }
    let picked = menu::popup(&items);
    with_app(|app| match picked {
        Some(ADD) => app.add_account(agent),
        Some(NOW) => {
            if let Some((to, _)) = switching {
                app.switch_account(agent, to, true);
            }
        }
        Some(i) if (FORGET..FORGET + all.len()).contains(&i) => {
            app.forget_account(agent, &all[i - FORGET].id);
        }
        Some(i) if (PICK..PICK + all.len()).contains(&i) => {
            app.switch_account(agent, all[i - PICK].id.clone(), false);
        }
        _ => {}
    });
}

/// A recent project right clicked in the start window, which has no
/// cluster and so no project menu: a new session, an old one back, or its
/// quest log.
fn recent_menu(dir: PathBuf) {
    const ADD: usize = 1;
    const QUESTS: usize = 2;
    const PAST: usize = 100;
    let held = with_app(|app| app.held_conversations()).unwrap_or_default();
    let past = past_in(&dir, &held, HISTORY);
    let items = [
        Item::action(ADD, "New session"),
        Item::Submenu("History".into(), history_items(&past, PAST)),
        Item::action(QUESTS, "Quest log..."),
    ];
    match menu::popup(&items) {
        Some(ADD) => start_logged(dir),
        Some(QUESTS) => push(Input::QuestLog(questlog::Ask::Open(folder_key(
            &dir.to_string_lossy(),
        )))),
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
/// The tray's "Can control this PC" switch for a brother. The roster this
/// PC serves reads it, and the links already up are told within a round.
fn set_control(id: &str, on: bool) {
    let Some(dir) = horadric_hooks::state_dir() else {
        return;
    };
    let mut standing = crate::standing::Standing::load(&dir);
    standing.set_control(id, on);
    if let Err(e) = standing.save(&dir) {
        eprintln!("horadric: cannot keep who controls this PC: {e}");
    }
}

/// Casting out cannot be undone, so it is asked first, Cancel the default.
/// `behind` are the brothers only `name` stands behind, which a stolen
/// machine could have let in: offered too, ticked, since a thief's
/// brothers are its own. Gives whether to cast those out with it.
fn confirm_cast_out(name: &str, line: &[(String, String)], mine: bool) -> bool {
    let text = cast_out_text(name, line, mine);
    let pressed = ask(&Dialog {
        tone: Tone::Warning,
        title: &format!("Cast out {name}"),
        text: &text,
        buttons: &["Cast out", "Cancel"],
        default: 1,
        check: None,
    });
    pressed == Some(0)
}

/// What the cast out asks, naming the brothers that go with `name` (its
/// line: those that came in only through it), and saying so when this PC
/// is one of them.
fn cast_out_text(name: &str, line: &[(String, String)], mine: bool) -> String {
    let mut text = format!(
        "{name} will be refused from now on, by this PC and by every brother: \
         it can no longer see or control any of them, and asking to join again \
         is refused without a prompt.\n\n\
         This cannot be undone. Only a brother asking {name} to join again \
         lets it back in, with a new key."
    );
    if !line.is_empty() {
        let names: Vec<&str> = line.iter().map(|(_, n)| n.as_str()).collect();
        text.push_str(&format!(
            "\n\n{} came in only through {name}, so they go with it: if {name} was \
             stolen, so may they be. Join any of them again by a code to keep it.",
            names.join(", ")
        ));
    }
    if mine {
        text.push_str(&format!(
            "\n\nThis PC came in through {name}: once {name} is out, this PC goes \
             with it, whoever decides it. Join this PC again by a code from a brother \
             that did not come in through {name} to keep it."
        ));
    }
    text
}

/// Casts the brother out: the brothers' state writes it where it lasts
/// (the outcasts and the roster) before its key goes, so a link that
/// races the drop is refused too.
fn cast_out(id: &str) {
    crate::brothers::cast_out(id);
}

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
                check: None,
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
        secret: false,
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
    /// The agents besides Claude Code found installed when the app
    /// started, each with a screen in the usage window.
    agents_found: Vec<Agent>,
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
    /// The MCP config that gives a session `horadric mcp`, the tools for its
    /// project's browser pane. None when it could not be written.
    mcp_config: Option<PathBuf>,
    /// The setting whose list is about to drop, and its row on screen.
    setting_menu_for: Option<(Agent, Setting, RECT)>,
    /// A setting's list, while it is dropped down.
    dropdown: Option<Box<Dropdown>>,
    /// The Settings window, while it is open.
    settings_window: Option<Box<SettingsWindow>>,
    /// The agent whose defaults the Settings window's Sessions section
    /// shows.
    settings_agent: Agent,
    /// The installed agents a project's menu does not offer.
    agents_off: BTreeSet<Agent>,
    /// The row of the Settings window whose list is about to drop, and its
    /// place on screen.
    settings_list_for: Option<(Field, RECT)>,
    /// The row of the Settings window that asks before it changes
    /// anything, for its question to open outside the app's borrow.
    settings_ask: Option<Field>,
    /// The memory's backup: its settings as the file has them, the job on
    /// its thread (named for the Settings section), what the jobs have
    /// finished, and when the schedule last looked.
    backup_cfg: crate::backup::Config,
    backup_busy: Option<String>,
    backup_done: Arc<Mutex<Vec<crate::backup::run::Done>>>,
    backup_checked: Option<Instant>,
    /// Where a restore is, between its questions.
    backup_step: Option<RestoreStep>,
    /// What start found of a restore staged last time: whether it failed,
    /// and what to tell.
    backup_note: Option<(bool, String)>,
    /// The project the Settings window's Projects section shows, by key.
    settings_project: Option<String>,
    /// Slash commands waiting to be typed into running sessions, by session
    /// id, for settings picked since they started: each goes in once the
    /// session is free for it, see [`Session::free_for_command`].
    switches: HashMap<String, Vec<(Setting, String)>>,
    /// The user's own default model and effort, from before the first of
    /// those commands went in, and when the last one did. Claude Code saves
    /// what they pick as the default for every new session, even outside
    /// Horadric, so these go back once it has.
    settings_before: Option<(Vec<Option<serde_json::Value>>, Instant)>,
    /// Every account logged in to, each agent's, kept for switching
    /// between.
    accounts: Accounts,
    /// Each agent's login as the app follows it.
    logins: BTreeMap<Agent, Login>,
    // Boxed on purpose: the window procedures hold a raw pointer to each
    // window struct, so it must not move when the Vec grows.
    #[allow(clippy::vec_box)]
    clusters: Vec<Box<Cluster>>,
    /// Sessions Horadric started itself, by session id. A `horadric run`
    /// session has a tile but no console: its terminal is somewhere else.
    consoles: HashMap<String, Arc<Console>>,
    /// Town Portal's link to the relay, while `portal.json` turns it on.
    portal: Option<portaling::Portal>,
    portal_checked: portaling::Checked,
    /// The brothers this PC is a client of, and their panes.
    far: far::Far,
    /// Files shown on the stage, at most one per project, by project key.
    /// A click on another file replaces it, like VS Code's preview tab, so
    /// browsing a project never piles up panes.
    views: HashMap<String, Arc<Console>>,
    /// Browser panes, at most one per project, by project key. The page
    /// itself is `web`'s and outlives its pane.
    webs: HashMap<String, Arc<Console>>,
    /// Arcane Sanctuaries, at most one per project, by project key. What
    /// they show is `sanctuary`'s and outlives the pane.
    skies: HashMap<String, Arc<Console>>,
    /// The glows' stamp the sanctuaries were last drawn at.
    glow_stamp: u64,
    /// What to ask for which project's browser pane, once out of the borrow.
    web_ask: Option<(String, WebAsk)>,
    /// Which session pane's header button to act on, once out of the
    /// borrow.
    pane_ask: Option<(String, PaneAsk)>,
    /// The terminal window, showing one project's sessions, while open.
    stage: Option<Box<TerminalWindow>>,
    /// Where the stage was when it last closed.
    stage_rect: Option<[i32; 4]>,
    /// The first pixel right of the tiles when the columns were last
    /// arranged, which tells a stage standing against them from one you
    /// put somewhere else.
    tiles_edge: Option<i32>,
    /// The project the stage showed when it last closed, which "Show
    /// terminal" in the tray brings back.
    stage_key: Option<String>,
    /// The global shortcuts' chords, in [`Action::ALL`]'s order.
    keys: [Chord; hotkey::COUNT],
    /// Which of them are registered. Another program may hold a chord.
    keys_live: [bool; hotkey::COUNT],
    /// The shortcut the Settings window waits for a new chord for. Every
    /// shortcut is let go meanwhile, so pressing one reaches the window.
    key_listening: Option<Action>,
    /// Why the last chord pressed did not take, for the Settings window.
    keys_note: Option<String>,
    /// The projects Warriv drives, by project key, from the tray and the
    /// quests tile's mode menu. Changed through [`App::set_drive`] and
    /// [`App::stop_warriv`].
    drives: BTreeMap<String, horadric_core::warriv::Drive>,
    /// The projects whose drive was stopped: their runner starts nothing
    /// until the human picks a mode or lets Warriv drive again.
    stopped: HashSet<String>,
    /// Whether you are away, for the catch-up when you come back.
    away: Away,
    /// Spectator mode: the stage following the work while you are away.
    spectating: spectating::Spectating,
    /// The catch-up, while it is open.
    catchup: Option<Box<Catchup>>,
    /// What happened while you were away, while it is open.
    away_card: Option<Box<AwayCard>>,
    /// The quest log, while it is open.
    quest_log: Option<Box<QuestLog>>,
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
    /// The top and bottom each window in the columns is cut to, by its
    /// handle, as last arranged: the column's, or for the windows below a
    /// pinned one, from the pin's bottom down.
    column_bounds: HashMap<isize, (i32, i32)>,
    /// The cut each window in the columns has now, by its handle, with the
    /// width it was made for, so a frame that changes neither sets none.
    clipped: RefCell<Clipped>,
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
    /// Projects whose cluster stays up with no session left, until closed
    /// from the project menu. Ending every session is often a fresh start
    /// in the same project, not leaving it.
    open: HashSet<String>,
    /// Projects closed from the project menu. Their unfinished tasks no
    /// longer keep the cluster up, and the runner leaves them alone, until
    /// a session starts in them again.
    closed: HashSet<String>,
    /// Which column each cluster and the usage window stand in.
    columns: Columns,
    /// Projects sessions were started in, newest first, for the tray menu.
    recent: Vec<String>,
    /// The projects the town's `CLAUDE.md` was last written for.
    town_listed: Option<Vec<town::Project>>,
    autostart_offered: bool,
    explorer_offered: bool,
    last_saved: Option<SavedState>,
    /// Set once Horadric is quitting or Windows is shutting down. The state
    /// on disk is final then: sessions dying on the way out must not be
    /// saved as gone.
    frozen: bool,
    /// Where the next folder picker opens. Set by the plus button.
    pick_from: Option<PathBuf>,
    /// Why the last session asked for did not start, until it is said.
    start_failed: Option<String>,
    /// The tile whose menu is about to show.
    menu_for: Option<String>,
    /// The stone about to be cast, by project key and label.
    stone_for: Option<(String, String)>,
    /// The stone whose menu is about to show, by project key and label,
    /// None for the empty stone.
    stone_menu_for: Option<(String, Option<String>)>,
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
    /// No notifications, from the Settings window.
    quiet: bool,
    /// Loot sounds, from the Settings window.
    sounds: bool,
    /// Tristram's tile is hidden while no session runs in the town.
    town_hidden: bool,
    /// Horadric's own animations are off, from the Settings window.
    still: bool,
    /// A usage limit switches Claude Code to the account with the most
    /// left, from the Settings window.
    switch_accounts: bool,
    /// The sessions a usage limit stopped that were seen to, each with
    /// when it stopped, so one stop switches once.
    out_seen: HashMap<String, SystemTime>,
    /// "Every account has run out" was said, until an account is taken.
    all_out_told: bool,
    /// What the Discord profile may show, from the Settings window.
    /// Changed only through [`App::set_discord`].
    discord: Discord,
    /// Recall may look in other projects, told to the memory thread.
    recall_across: bool,
    /// The Rich Presence client, kept while [`App::discord`] is on.
    rich: Option<crate::discord::Discord>,
    /// When the current run of work began, which the profile counts from.
    run: presence::Run,
    /// The cube is shown, from the usage window's menu.
    cube_on: bool,
    /// The terminal font picked in the Settings window. Kept as picked, so a
    /// family that is uninstalled for a while comes back when it is not.
    font_family: Option<String>,
    /// The screen the columns stand on, by device name. None follows the
    /// primary one.
    screen: Option<String>,
    /// The task lists: what was read, and what the runner is up to.
    tasks: runner::State,
    /// Runewords cast on projects, and what casting keeps on the way.
    tome: runner::runeword::Tome,
    /// Worktrees just added for sessions about to start, by session id,
    /// with the setup commands to run in them first. Taken by the launch.
    new_trees: HashMap<String, (Worktree, Vec<String>)>,
    /// When each session's worktree is counted again, by session id.
    recounts: HashMap<String, Recount>,
    /// Counts back from their threads.
    counted: Arc<Mutex<Vec<Counted>>>,
    /// Undone turns back from their threads: the session's name and how
    /// it went.
    undone: Arc<Mutex<Vec<(String, checkpoint::Outcome)>>>,
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
    /// The newest release a notification told of, kept in the saved state.
    update_told: Option<String>,
    /// The last notification said a release is out, so a click on it
    /// shows that release's notes.
    update_click: bool,
    /// Subagents the human asked to stop, by session id, until that
    /// session's mod next asks what Horadric knows and is told to.
    stop_agents: HashMap<String, Vec<String>>,
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

/// One agent's login as the app follows it. Each agent's is its own: a
/// switch of one stops only that agent's sessions.
#[derive(Default)]
struct Login {
    /// Who it is logged in as, by account id, as last read.
    account: Option<String>,
    /// When the login files last changed, as last seen and as last read.
    /// A change is read once it has held still for a tick, since Claude
    /// Code's login writes two files one after the other.
    seen: Option<accounts::Stamp>,
    read: Option<accounts::Stamp>,
    /// An account switch waiting for sessions to finish their turns.
    switch: Option<AccountSwitch>,
    /// A login opened by Add account, with the account in use before it,
    /// which goes back once the new login is kept.
    adding: Option<(Option<String>, Instant)>,
    /// A login just put in, and when. A `claude` that read `.claude.json`
    /// before can write it back after, with the old profile, so for a
    /// while the login is put in again if it is found changed.
    settling: Option<(String, Instant)>,
}

/// Switching an agent's accounts. Every running agent holds its login in
/// memory, so each stops once it is free and all resume on the new
/// login together. None may run on while the files change: one still on
/// the old login would write its token back when it refreshes it, and
/// Codex takes another account on disk as a permanent error.
struct AccountSwitch {
    to: String,
    /// Still running, to stop once free.
    running: Vec<String>,
    /// Stopped, to resume once the login is in.
    stopped: Vec<String>,
    /// Of those, the ones a usage limit had stopped, told to go on.
    limited: Vec<String>,
    /// Stop them now, mid turn or not.
    now: bool,
}

/// How long an account a limit stopped is passed over when the limits
/// heard show none full, so no reset is known.
const OUT_FOR: u64 = 60 * 60;

/// How long a login just put in is watched for being written over.
const SETTLES_IN: Duration = Duration::from_secs(15);

/// How long a login opened by Add account is waited for.
const ADDING_FOR: Duration = Duration::from_secs(15 * 60);

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
                self.light_skies();
                let events = EVENTS.swap(0, Ordering::AcqRel);
                if events & EVENTS_DUE == 0 {
                    return;
                }
                self.reconcile(events & EVENTS_PHASE != 0);
                self.switch_free();
                self.switch_when_out();
                self.switch_step();
                self.run_tasks();
                self.recount();
                self.mark_sweeps();
            }
            WM_HORADRIC_COUNTED => self.take_counts(),
            WM_HORADRIC_UPDATE => {
                self.take_update();
                self.refresh_settings();
            }
            WM_HORADRIC_BACKUP => self.take_backup(),
            WM_HORADRIC_UNDONE => self.take_undone(),
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
                            let cwd = PathBuf::from(n.cwd);
                            if let Err(e) = self.start_in(n.name, cwd, n.args, n.agent, false) {
                                eprintln!("horadric: cannot start session: {e}");
                            }
                        }
                        Command::Reload(r) => self.begin_reload(r),
                        Command::Tasks(t) => {
                            if let Some(tomb) = &t.tomb {
                                self.tomb_reported(tomb, t.why.as_deref());
                            }
                            if let (Some(quest), Some(tell)) = (&t.quest, &t.tell) {
                                self.hear_tell(&t.dir, quest, tell, t.by.as_deref());
                            }
                            if let (Some(quest), Some(fix)) = (&t.quest, &t.fix) {
                                self.hear_fix(&t.dir, quest, fix);
                            }
                            if let Some(label) = &t.cast {
                                self.cast_from_shell(&t.dir, label, t.by.as_deref());
                            }
                            self.refresh_boards(true);
                            self.run_tasks();
                        }
                        Command::Overlap(o) => self.overlapped(&o),
                        Command::Browser(call) => self.browser_call(call),
                        Command::Know(call) => self.know_call(call),
                        Command::Recall(call) => {
                            if let Some(call) = self.handing_recall(call) {
                                crate::memory::ask(call);
                            }
                        }
                    }
                }
            }
            WM_HOTKEY => match Action::from_id(wparam as i32) {
                Some(Action::Next) => self.next_waiting(),
                Some(Action::Listen) => self.listen_on_demand(),
                Some(Action::Stop) => self.stop_warriv(),
                Some(Action::Account) => post(self.notify.0 as isize, WM_HORADRIC_ACCOUNT_MENU, 0),
                None => {}
            },
            WM_TIMER if wparam == GLIDE_TIMER => {
                crate::vsync::took(self.notify, GLIDE_TIMER);
                self.glide();
            }
            WM_TIMER if wparam == SPECTATE_TIMER => self.watch_return(),
            WM_TIMER if wparam == PORTAL_TIMER => self.portal_push(),
            WM_HORADRIC_PORTAL => {
                self.portal_drain();
                self.portal_show();
            }
            WM_HORADRIC_FAR => self.far_drain(),
            WM_TIMER if wparam == BREATH_TIMER => {
                self.tray.step();
                self.breathe_stage();
            }
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
                self.watch_login();
                self.switch_when_out();
                self.switch_step();
                self.tick_tasks();
                self.reload_when_ready();
                self.portal_check();
                self.portal_show();
                self.far_check();
                self.handing_tick();
                self.keeping_tick();
                self.backup_tick();
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

    /// Shows Town Portal as it stands on the stage's key.
    fn portal_show(&self) {
        let Some(stage) = &self.stage else {
            return;
        };
        let (connected, phones) = self.portal.as_ref().map_or((false, 0), |p| p.look());
        stage.set_portal(portal_key::look(self.portal.is_some(), connected, phones));
    }

    /// Reads `portal.json` again at the next tick rather than in two
    /// seconds, so the key answers a click at once.
    fn portal_soon(&mut self) {
        self.portal_checked = Default::default();
        self.portal_check();
        self.portal_show();
    }

    fn console_by_serial(&self, serial: usize) -> Option<&Arc<Console>> {
        self.consoles
            .values()
            .chain(self.views.values())
            .chain(self.far.consoles.values())
            .find(|c| c.serial == serial)
    }

    /// A session's console, or a file view's, by its id.
    fn console_of(&self, id: &str) -> Option<&Arc<Console>> {
        self.consoles
            .get(id)
            .or_else(|| self.views.values().find(|v| v.id == id))
            .or_else(|| self.webs.values().find(|v| v.id == id))
            .or_else(|| self.skies.values().find(|v| v.id == id))
            .or_else(|| self.far.consoles.get(id))
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
            let console = match Console::attach(
                &id,
                serial,
                saved.args.clone(),
                saved.shell,
                saved.agent,
                PathBuf::from(&saved.cwd),
                self.notify,
            ) {
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
            // Its keeper may have taken it over meanwhile: it waits to
            // hear.
            if self.keeping_holds(id) {
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
        let (shell, background, far) = {
            let r = self.shared.registry.lock().ok()?;
            let s = r.get(id)?;
            (s.shell, s.background.is_some(), s.brother.is_some())
        };
        let kind = if far {
            TileKind::Far
        } else if self.paused.contains_key(id) {
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
        let said = console.take_program();
        let mut changed = false;
        if let Ok(mut r) = self.shared.registry.lock() {
            if let Some(s) = r.get_mut(&console.id).filter(|s| s.background.is_none()) {
                let now = SystemTime::now();
                if let Some(said) = said {
                    if std::env::var_os("HORADRIC_DEBUG").is_some() {
                        eprintln!("horadric: {} says {said:?} (OSC 7501)", console.id);
                    }
                    changed = s.hear_program(said, now);
                }
                // No hook reports on a shell, so its tile tells what it runs
                // from the terminal's title and how busy it is from its
                // output, unless the program says itself. The tiles redraw
                // on the next tick.
                if console.shell {
                    s.touch(now);
                    let said = s.program.as_ref().and_then(|h| h.line.clone());
                    s.last_line = said
                        .or_else(|| console.title())
                        .or_else(|| s.ssh.clone())
                        .unwrap_or_default();
                }
            }
        }
        if changed {
            post_event(APP_WINDOW.with(Cell::get), true);
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
        self.start_in(name, cwd, args, Agent::Claude, false)
    }

    /// [`App::start`], in a worktree of its own when `own_tree` even where
    /// the project works in one shared tree.
    fn start_in(
        &mut self,
        name: Option<String>,
        cwd: PathBuf,
        args: Vec<String>,
        agent: Agent,
        own_tree: bool,
    ) -> Result<String, String> {
        if !cwd.is_dir() {
            return Err(format!("{} is not a directory", cwd.display()));
        }
        let folder = folder_name(&cwd);
        let base = name.clone().unwrap_or_else(|| folder.clone());
        let id = self.unique_id(&base);
        let branch = name.as_deref().unwrap_or("session").to_string();
        let shown = name.unwrap_or(folder);
        let cwd = self.own_tree(&id, &branch, cwd, &args, agent, own_tree);
        if let Err(e) = self.launch(&id, &shown, cwd, args, Run::Agent(agent), false) {
            // A worktree the session never started in holds nothing.
            if let Some((w, _)) = self.new_trees.remove(&id) {
                worktree::remove(w);
            }
            // Said outside the borrow. Otherwise nothing shows it failed:
            // the folder picker only closes, and no tile appears.
            self.start_failed = Some(e.clone());
            post(self.notify.0 as isize, WM_HORADRIC_START_FAILED, 0);
            return Err(e);
        }
        // A new session joins the stage where it stands, at the size it was
        // given. Docking it again each time threw away that size.
        if let Some(key) = self.project_of(&id) {
            if self.fill_stage(&key) {
                if let Some(stage) = &self.stage {
                    stage.focus_session(&id);
                }
            }
        }
        Ok(id)
    }

    /// Where a new session starts: a worktree of its own added from `cwd`,
    /// or `cwd` itself when the project is in trunk mode and none was
    /// `asked` for, is not in a repository, or `args` carry on a
    /// conversation, which the agent keeps by the folder it was held in.
    fn own_tree(
        &mut self,
        id: &str,
        branch: &str,
        cwd: PathBuf,
        args: &[String],
        agent: Agent,
        asked: bool,
    ) -> PathBuf {
        if agent.carries_on(args) {
            return cwd;
        }
        match worktree::add(&cwd, branch, &self.ports_taken(), asked) {
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

    /// What the update check found. A newer release is told of once, with
    /// the start of its notes, and a click on that shows them all. After
    /// that it waits in the tray menu, unless the tray asked again.
    fn take_update(&mut self) {
        let Some((asked, found)) = self.looked.lock().ok().and_then(|mut l| l.take()) else {
            return;
        };
        self.checking = false;
        match found {
            Ok(Some(m)) => {
                eprintln!("horadric: Horadric {} is out", m.version);
                if asked || self.update_told.as_deref() != Some(m.version.as_str()) {
                    let teaser = release::teaser(&m.notes, 120);
                    let text = if teaser.is_empty() {
                        "Click to update.".to_string()
                    } else {
                        format!("{teaser} Click to see what is new.")
                    };
                    self.alert_for = None;
                    self.tasks.merge_for = None;
                    self.tasks.ship_for = None;
                    self.update_click = true;
                    self.toasts
                        .show(Kind::Done, &format!("Horadric {} is out", m.version), &text);
                    self.update_told = Some(m.version.clone());
                }
                self.offer_in_usage(Some(&m.version));
                self.update = Some(m);
            }
            Ok(None) => {
                self.update = None;
                self.offer_in_usage(None);
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

    /// Puts the release to update to, if any, on the usage window's
    /// Version row.
    fn offer_in_usage(&self, version: Option<&str>) {
        if let Some(u) = &self.usage_window {
            *u.update.borrow_mut() = version.map(str::to_string);
            u.invalidate();
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
        self.update_click = false;
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
            c.refresh();
        }
    }

    /// The worktree of a session and what it changed, for its menu. The
    /// count kept on the tile is fresh within seconds of the agent's last
    /// move, and counting again takes several git runs the menu would
    /// wait for, so it is counted now only when it never was.
    fn diff_of(&mut self, id: &str) -> Option<(Worktree, Option<Diff>)> {
        let (w, kept) = {
            let r = self.shared.registry.lock().ok()?;
            let s = r.get(id)?;
            (s.worktree.clone()?, s.diff.clone())
        };
        if kept.is_some() {
            return Some((w, kept));
        }
        let diff = worktree::count(&w);
        if let Ok(mut r) = self.shared.registry.lock() {
            if let Some(s) = r.get_mut(id) {
                s.diff = diff.clone();
            }
        }
        for c in &self.clusters {
            c.refresh();
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

    /// The conversations the stash holds, which come back from there.
    fn stashed_conversations(&self) -> Vec<String> {
        self.shared
            .registry
            .lock()
            .map(|r| {
                r.stashed()
                    .iter()
                    .filter_map(|s| s.claude_session_id.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The conversations tiles hold, running, paused or stashed, which a
    /// History menu leaves out: two agents on one would write over each
    /// other.
    fn held_conversations(&self) -> Vec<String> {
        self.shared
            .registry
            .lock()
            .map(|r| {
                r.all()
                    .filter_map(|s| s.claude_session_id.clone())
                    .chain(
                        r.stashed()
                            .iter()
                            .filter_map(|s| s.claude_session_id.clone()),
                    )
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Carries on a past conversation from `past`, a History menu's list
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
        let agent = past.map_or(Agent::Claude, |p| p.agent);
        let args = agent.resume_args(past.map(|p| p.id.as_str()));
        let id = match self.start_in(None, dir.to_path_buf(), args, agent, false) {
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
        if let Some(line) = self.keeping_fenced(id) {
            return Err(line);
        }
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
                (None, false) => Run::Agent(saved.agent),
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
        if self.fill_stage(key) {
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
        if self.fill_stage(key) {
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
        let shell = !matches!(run, Run::Agent(_));
        let attach = matches!(run, Run::Attach(_));
        let host = match &run {
            Run::Ssh(h) => Some(h.clone()),
            _ => None,
        };
        let (program, args) = match &run {
            Run::Agent(agent) => (
                console::agent_program(*agent).ok_or_else(|| not_found(*agent))?,
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
            Run::Program(program) => (program.clone(), args),
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
                    if let Run::Agent(agent) = run {
                        // A shell put in with `HORADRIC_AGENT` stays Claude's.
                        s.agent = console::agent_of(&program).unwrap_or(agent);
                    }
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
            // Empty, so one this Horadric was started with, as a dev
            // instance from a quest's worktree is, never sends `horadric
            // quest` to another project's list. Warriv works on its own
            // project's list from wherever its shell has wandered.
            .unwrap_or_else(|| {
                let main = match horadric_core::warriv::is_warriv(id) {
                    true => folder_key(&cwd.to_string_lossy()),
                    false => String::new(),
                };
                vec![(TASKS_ENV.into(), main)]
            });
        let mut env = env;
        if !attach && !shell && console::is_claude(&program) {
            env.extend(self.shared.defaults_of(Agent::Claude).env(Agent::Claude));
        }
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
    /// from the usage window (prompts bypassed for a quest in its own
    /// worktree), the status line that feeds it, the tools for
    /// the project's browser pane, and what it is told about the task list
    /// and the project's hosts. Only for Claude
    /// Code, not for a shell put in its place with `HORADRIC_AGENT`, and not
    /// over settings the session brought itself.
    fn extra_args(&mut self, id: &str, program: &Path, args: &[String], cwd: &Path) -> Vec<String> {
        let bypass = self.bypasses_prompts(id);
        match console::agent_of(program) {
            Some(Agent::Claude) => {}
            Some(other) => self.record_agent_launch(id, other.program()),
            // A shell put in its place with `HORADRIC_AGENT`.
            None => self.record_agent_launch(id, "other"),
        }
        if console::agent_of(program) == Some(Agent::Codex) {
            let hook = store::exe_command(&console::host_program(), "hook codex");
            let mut extra = Agent::Codex.hook_args(&hook);
            extra.extend(Agent::Codex.login_args());
            let exe = console::host_program();
            extra.extend(Agent::Codex.mcp_args(&exe.to_string_lossy(), None));
            extra.extend(self.shared.defaults_of(Agent::Codex).flags_for(
                Agent::Codex,
                args,
                bypass,
            ));
            return extra;
        }
        // Grok's hook is in its home, written by `install`.
        if console::agent_of(program) == Some(Agent::Grok) {
            return self
                .shared
                .defaults_of(Agent::Grok)
                .flags_for(Agent::Grok, args, bypass);
        }
        if !console::is_claude(program) {
            return Vec::new();
        }
        // The quest's model counts as the session's own choice, so the
        // default model stays out.
        let model = self.quest_model_args(id, args);
        let own: Vec<String> = args.iter().chain(&model).cloned().collect();
        let mut extra =
            self.shared
                .defaults_of(Agent::Claude)
                .flags_for(Agent::Claude, &own, bypass);
        extra.extend(model);
        if let (Some(path), false) = (&self.status_settings, has_flag(args, "--settings")) {
            extra.push("--settings".into());
            extra.push(path.to_string_lossy().into_owned());
        }
        let config = self.mcp_config.as_ref().map(|p| p.to_string_lossy());
        extra.extend(Agent::Claude.mcp_args("", config.as_deref()));
        extra.extend(self.task_args(id, program, cwd));
        if town::is(&folder_key(&cwd.to_string_lossy())) {
            extra.extend(town::add_dir_args(&self.town_projects()));
        }
        self.record_launch(id, args, &extra);
        extra
    }

    /// The agents with a screen in the usage window: Claude Code, those
    /// installed, and any with a session, which may have been started
    /// from a program installed since.
    fn agents_in_use(&self) -> Vec<Agent> {
        let running: HashSet<Agent> = self
            .shared
            .registry
            .lock()
            .map(|r| r.all().map(|s| s.agent).collect())
            .unwrap_or_default();
        Agent::ALL
            .into_iter()
            .filter(|a| *a == Agent::Claude || self.agents_found.contains(a) || running.contains(a))
            .collect()
    }

    /// A setting picked in the usage window for `agent`'s sessions. They
    /// take it as they start or resume. Running Claude Code sessions switch
    /// too where it has a command for it, typed in once each is free, unless
    /// one chose the setting with its own arguments.
    fn set_default(&mut self, agent: Agent, setting: Setting, value: Option<String>) {
        let command = setting
            .command(value.as_deref())
            .filter(|_| agent == Agent::Claude);
        if let Some(command) = command {
            for (id, c) in &self.consoles {
                if !c.claude || c.exit_code().is_some() || setting.chosen_by(&c.args) {
                    continue;
                }
                let queue = self.switches.entry(id.clone()).or_default();
                queue.retain(|(s, _)| *s != setting);
                queue.push((setting, command.clone()));
            }
        }
        self.shared
            .defaults
            .borrow_mut()
            .entry(agent)
            .or_default()
            .set(setting, value);
        if let Some(u) = &self.usage_window {
            u.invalidate();
        }
        self.refresh_settings();
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

    fn login(&mut self, agent: Agent) -> &mut Login {
        self.logins.entry(agent).or_default()
    }

    /// Reads who `agent` is logged in as, keeps that login, and shows it.
    /// At start and whenever its login files change: a refreshed token
    /// replaces the kept one, and a login typed into any session adds an
    /// account.
    fn read_login(&mut self, agent: Agent) {
        let stamp = accounts::stamp(agent);
        let login = self.login(agent);
        login.read = stamp.clone();
        login.seen = stamp;
        if let Some((to, at)) = login.settling.clone() {
            if at.elapsed() > SETTLES_IN {
                login.settling = None;
            } else if accounts::live_id(agent).as_ref() != Some(&to) {
                eprintln!("horadric: the {agent:?} login was written over, putting it back");
                if let Some(a) = self.accounts.get(&to).cloned() {
                    if let Err(e) = self.put_file(&a) {
                        eprintln!("horadric: cannot put the login back: {e}");
                    }
                }
                // From the first put, so a writer that never stops is not
                // fought for ever.
                self.login(agent).settling = Some((to, at));
                return;
            }
        }
        let Some(live) = accounts::live(agent) else {
            // Mid write, or not logged in. Who it was stays shown then.
            if accounts::live_id(agent).is_none() {
                self.login(agent).account = None;
                self.show_account(agent, None);
            }
            return;
        };
        let id = live.id.clone();
        let changed = self.accounts.remember(live);
        let added = self
            .login(agent)
            .adding
            .as_ref()
            .is_some_and(|(before, _)| before.as_ref() != Some(&id));
        if added {
            let before = self.login(agent).adding.take().and_then(|(b, _)| b);
            let email = self.accounts.get(&id).map(|a| a.email.clone());
            self.save_accounts();
            // Back to the account the sessions run on, so adding one stops
            // nothing. The new one is a pick away.
            match before.and_then(|b| self.accounts.get(&b).cloned()) {
                Some(back) => {
                    if let Err(e) = self.put_file(&back) {
                        eprintln!("horadric: cannot put the login back: {e}");
                    }
                }
                None => self.adopt(agent, id),
            }
            self.toasts.show(
                Kind::Done,
                &format!("Added {}", email.unwrap_or_default()),
                "Pick it under Account in the usage window to switch to it.",
            );
            return;
        }
        if changed {
            self.save_accounts();
        }
        if self.login(agent).account.as_ref() != Some(&id) {
            self.adopt(agent, id);
        }
    }

    /// Reads each agent's login again once its files changed and held
    /// still.
    fn watch_login(&mut self) {
        for agent in Agent::ALL {
            let login = self.login(agent);
            if login
                .adding
                .as_ref()
                .is_some_and(|(_, at)| at.elapsed() > ADDING_FOR)
            {
                login.adding = None;
            }
            let stamp = accounts::stamp(agent);
            if stamp != login.seen {
                login.seen = stamp;
                continue;
            }
            if stamp != login.read {
                self.read_login(agent);
            }
        }
    }

    /// Account `id` is the one `agent` uses now: its limits come back and
    /// the usage window names it. The limits heard until now were the
    /// account's before it.
    fn adopt(&mut self, agent: Agent, id: String) {
        if let Some(before) = self.login(agent).account.clone() {
            let arriving = self.accounts.get(&id).and_then(|a| a.usage.clone());
            let leaving = match agent {
                Agent::Claude => self
                    .shared
                    .usage
                    .lock()
                    .ok()
                    .map(|mut u| std::mem::replace(&mut *u, arriving)),
                _ => self
                    .shared
                    .agent_usage
                    .lock()
                    .ok()
                    .map(|mut all| match arriving {
                        Some(u) => all.insert(agent, u),
                        None => all.remove(&agent),
                    }),
            };
            if let Some(leaving) = leaving {
                self.accounts.set_usage(&before, leaving);
            }
        }
        let email = self.accounts.get(&id).map(|a| a.email.clone());
        // Taken by hand or logged in to again, it is worth trying.
        self.accounts.set_out(&id, None);
        self.all_out_told = false;
        self.login(agent).account = Some(id);
        self.save_accounts();
        self.show_account(agent, email);
    }

    fn show_account(&mut self, agent: Agent, email: Option<String>) {
        if self.shared.account.borrow().get(&agent) == email.as_ref() {
            return;
        }
        match email {
            Some(e) => self.shared.account.borrow_mut().insert(agent, e),
            None => self.shared.account.borrow_mut().remove(&agent),
        };
        if let Some(u) = &self.usage_window {
            if u.fit() {
                self.arrange();
            }
        }
    }

    fn save_accounts(&self) {
        if let Err(e) = accounts::save(&self.accounts) {
            eprintln!("horadric: cannot save the accounts: {e}");
        }
    }

    /// The accounts for `agent`'s Account menu, the one in use, and the
    /// one a switch is on its way to with how many sessions it waits for.
    /// The one in use carries the limits heard now, not those it left with.
    fn account_choices(
        &mut self,
        agent: Agent,
    ) -> (Accounts, Option<String>, Option<(String, usize)>) {
        let login = self.login(agent);
        let switching = login
            .switch
            .as_ref()
            .map(|s| (s.to.clone(), s.running.len()));
        let live = login.account.clone();
        let mut all = self.accounts.clone();
        if let Some(id) = &live {
            all.set_usage(id, self.usage_of(agent));
        }
        (all, live, switching)
    }

    /// The limits heard for `agent`'s account in use.
    fn usage_of(&self, agent: Agent) -> Option<Usage> {
        match agent {
            Agent::Claude => self.shared.usage.lock().ok()?.clone(),
            _ => self.shared.agent_usage.lock().ok()?.get(&agent).cloned(),
        }
    }

    /// Each agent's kept accounts for the tray's Account menu, with what
    /// each has left. Only agents logged in to some.
    fn logins(&mut self) -> Vec<tray::Logins> {
        let now = unix_now();
        Agent::ALL
            .into_iter()
            .filter_map(|agent| {
                let (all, live, _) = self.account_choices(agent);
                let accounts: Vec<tray::Pick> = all
                    .of(agent)
                    .map(|a| tray::Pick {
                        id: a.id.clone(),
                        label: a.label_at(now),
                        live: live.as_deref() == Some(a.id.as_str()),
                    })
                    .collect();
                (!accounts.is_empty()).then_some(tray::Logins { agent, accounts })
            })
            .collect()
    }

    /// Opens a login for another of `agent`'s accounts. The sessions keep
    /// running on this one: the new login is kept and this one put back.
    fn add_account(&mut self, agent: Agent) {
        if let Some(why) = accounts::refused(agent) {
            self.toasts
                .show(Kind::Failed, "Cannot add an account", &why);
            return;
        }
        self.read_login(agent);
        match accounts::log_in(agent) {
            Ok(()) => {
                let login = self.login(agent);
                login.adding = Some((login.account.clone(), Instant::now()));
                self.toasts.show(
                    Kind::Info,
                    &format!("Log in to {}", agent.provider()),
                    "Log in with the other account in the window that opened. \
                     The sessions carry on with this one.",
                );
            }
            Err(e) => self
                .toasts
                .show(Kind::Failed, "Cannot open the login", &e.to_string()),
        }
    }

    fn forget_account(&mut self, agent: Agent, id: &str) {
        if self.login(agent).account.as_deref() == Some(id) {
            return;
        }
        self.accounts.forget(id);
        self.save_accounts();
    }

    /// Whether the console of session `id` runs `agent` itself, so it
    /// holds `agent`'s login. For Claude not a shell put in its place with
    /// `HORADRIC_AGENT`.
    fn runs(&self, id: &str, agent: Agent) -> bool {
        let Some(c) = self.consoles.get(id) else {
            return false;
        };
        if c.exit_code().is_some() || c.shell || self.background_of(id).is_some() {
            return false;
        }
        if agent == Agent::Claude {
            return c.claude;
        }
        self.shared
            .registry
            .lock()
            .is_ok_and(|r| r.get(id).is_some_and(|s| s.agent == agent))
    }

    /// Switches `agent` to account `to`. The sessions of `agent` Horadric
    /// runs stop as each finishes its turn and resume on the new login
    /// together, see [`AccountSwitch`]. With `now` they stop at once. The
    /// other agents' sessions run on.
    fn switch_account(&mut self, agent: Agent, to: String, now: bool) {
        self.begin_switch(agent, to, now, true);
    }

    /// [`App::switch_account`], saying so with `say`: a switch a limit
    /// set off has said why already.
    fn begin_switch(&mut self, agent: Agent, to: String, now: bool, say: bool) {
        if let Some(why) = accounts::refused(agent) {
            self.toasts
                .show(Kind::Failed, "Cannot switch accounts", &why);
            return;
        }
        if let Some(s) = &mut self.login(agent).switch {
            s.to = to;
            s.now |= now;
            self.switch_step();
            return;
        }
        if self.login(agent).account.as_ref() == Some(&to) {
            return;
        }
        let running: Vec<String> = self
            .consoles
            .keys()
            .filter(|id| self.runs(id, agent))
            .cloned()
            .collect();
        if let (true, false, Some(a)) = (say, running.is_empty(), self.accounts.get(&to)) {
            self.toasts.show(
                Kind::Info,
                &format!("Switching to {}", a.email),
                "Each session stops once its turn is done, and all resume on the new account together.",
            );
        }
        self.login(agent).switch = Some(AccountSwitch {
            to,
            running,
            stopped: Vec::new(),
            limited: Vec::new(),
            now,
        });
        self.switch_step();
    }

    /// When a usage limit stopped a Claude Code session and the switch is
    /// on, switches to the account with the most left. The account that
    /// ran out is noted as out until its limit resets, so the next switch
    /// does not come straight back to it. Once for each stop.
    fn switch_when_out(&mut self) {
        let agent = Agent::Claude;
        let stops: Vec<(String, SystemTime)> = match self.shared.registry.lock() {
            Ok(r) => self
                .consoles
                .keys()
                .filter_map(|id| {
                    let s = r.get(id).filter(|s| s.limited())?;
                    Some((id.clone(), s.since))
                })
                .collect(),
            Err(_) => return,
        };
        self.out_seen
            .retain(|id, since| stops.iter().any(|(i, s)| i == id && s == since));
        let fresh: Vec<(String, SystemTime)> = stops
            .into_iter()
            .filter(|(id, _)| !self.out_seen.contains_key(id) && self.runs(id, agent))
            .collect();
        if !self.switch_accounts || fresh.is_empty() || self.login(agent).switch.is_some() {
            return;
        }
        self.out_seen.extend(fresh);
        let Some(live) = self.login(agent).account.clone() else {
            return;
        };
        let now = unix_now();
        // Not the fullest limit's reset, which can be the weekly one days
        // off when the refusal was the session's.
        let until = self
            .usage_of(agent)
            .and_then(|u| u.limits.out_until(now, false))
            .unwrap_or(now + OUT_FOR);
        self.accounts.set_out(&live, Some(until));
        self.save_accounts();
        let leaving = self.accounts.get(&live).map(|a| a.email.clone());
        match self.accounts.roomiest(agent, Some(&live), now) {
            Some(to) => {
                let (id, email) = (to.id.clone(), to.email.clone());
                self.toasts.show(
                    Kind::Info,
                    &format!("{} ran out", leaving.unwrap_or_default()),
                    &format!("Switching to {email}, and the stopped turns go on there."),
                );
                self.begin_switch(agent, id, false, false);
            }
            None if self.accounts.of(agent).count() > 1 && !self.all_out_told => {
                self.all_out_told = true;
                self.toasts.show(
                    Kind::Info,
                    "Every account has run out",
                    "The sessions wait for a limit to reset.",
                );
            }
            None => {}
        }
    }

    /// Moves every switch on, see [`App::switch_step_of`].
    fn switch_step(&mut self) {
        for agent in Agent::ALL {
            self.switch_step_of(agent);
        }
    }

    /// Moves `agent`'s switch on: stops each session free to stop, and
    /// once every one has, puts the new login in and resumes them all.
    fn switch_step_of(&mut self, agent: Agent) {
        let Some(mut s) = self.login(agent).switch.take() else {
            return;
        };
        if let Ok(r) = self.shared.registry.lock() {
            let consoles = &self.consoles;
            let stopped = &mut s.stopped;
            let limited = &mut s.limited;
            let now = s.now;
            s.running.retain(|id| {
                let Some(c) = consoles.get(id).filter(|c| c.exit_code().is_none()) else {
                    // Gone on its own, ended or crashed: nothing to carry.
                    return false;
                };
                let session = r.get(id);
                let free = now || session.is_some_and(|x| x.free_to_restart(c.typed_at()));
                if free {
                    c.kill();
                    stopped.push(id.clone());
                    if session.is_some_and(|x| x.limited()) {
                        limited.push(id.clone());
                    }
                }
                !free
            });
        }
        let stopping = s.stopped.iter().any(|id| {
            self.consoles
                .get(id)
                .is_some_and(|c| c.exit_code().is_none())
        });
        if !s.running.is_empty() || stopping {
            self.login(agent).switch = Some(s);
            return;
        }
        let email = self.accounts.get(&s.to).map(|a| a.email.clone());
        match self.put_login(agent, &s.to) {
            Ok(()) => self.toasts.show(
                Kind::Done,
                &format!("Switched to {}", email.unwrap_or_default()),
                "The sessions carry on with this account.",
            ),
            Err(e) => self.toasts.show(Kind::Failed, "Cannot switch accounts", &e),
        }
        // Once each, whatever happened: on the old login if the switch failed.
        let on_stage = self.shared.active.borrow().clone();
        for id in &s.stopped {
            if self.paused.contains_key(id) {
                // Its refused prompt is the last in the conversation, and
                // this takes it up again on the new login.
                let go_on = agent == Agent::Claude && s.limited.contains(id);
                if go_on {
                    self.tasks
                        .prompts
                        .insert(id.clone(), horadric_core::accounts::GO_ON.to_string());
                }
                if let Err(e) = self.resume(id, false) {
                    eprintln!("horadric: cannot resume {id}: {e}");
                    self.tasks.prompts.remove(id);
                }
            }
        }
        if let Some(id) = on_stage.filter(|id| s.stopped.contains(id)) {
            if self.consoles.contains_key(&id) {
                self.reveal(&id, false);
            }
        }
    }

    /// Logs `agent` in as account `to`, keeping the leaving login as it is
    /// now first, since it may have been refreshed since it was kept.
    fn put_login(&mut self, agent: Agent, to: &str) -> Result<(), String> {
        if let Some(live) = accounts::live(agent) {
            self.accounts.remember(live);
        }
        let target = self
            .accounts
            .get(to)
            .cloned()
            .ok_or("that account is no longer kept")?;
        self.put_file(&target).map_err(|e| e.to_string())?;
        self.adopt(agent, to.to_string());
        Ok(())
    }

    /// Writes `account`'s login into its agent's files, and watches for a
    /// while that it stays.
    fn put_file(&mut self, account: &Account) -> std::io::Result<()> {
        accounts::put(account)?;
        let stamp = accounts::stamp(account.agent);
        let login = self.login(account.agent);
        login.read = stamp.clone();
        login.seen = stamp;
        login.settling = Some((account.id.clone(), Instant::now()));
        Ok(())
    }

    /// A setting's list opened. Its row in the usage window stays lit
    /// while it is.
    fn dropped(&mut self, d: Box<Dropdown>) {
        if let Some(old) = self.dropdown.replace(d) {
            old.destroy();
        }
        let whose = self.dropdown.as_ref().map(|d| d.whose);
        let i = match whose {
            Some(Whose::Default(agent, setting)) => {
                agent.settings().iter().position(|s| *s == setting)
            }
            _ => None,
        };
        if let Some(u) = &self.usage_window {
            u.open.set(i);
            u.invalidate();
        }
        if let Some(w) = &self.settings_window {
            w.set_open(match whose {
                Some(Whose::Settings(field)) => Some(field),
                _ => None,
            });
        }
    }

    /// The settings as they are now, for the Settings window.
    fn settings_values(&self) -> settings::Values {
        let screens = monitors();
        let shown = screens::pick(&screens, self.screen.as_deref()).map(|s| s.name.clone());
        let projects = self.settings_projects();
        let project = self.settings_key().and_then(|key| {
            let i = projects.iter().position(|(k, _)| *k == key)?;
            Some(self.project_values(&key, i))
        });
        settings::Values {
            theme: theme::current(),
            font: self.shared.font.family(),
            screens: screens
                .iter()
                .enumerate()
                .map(|(i, s)| screens::short_label(i + 1, s))
                .collect(),
            screen: shown.and_then(|n| screens.iter().position(|s| s.name == n)),
            notify: !self.quiet,
            sounds: self.sounds,
            town: !self.town_hidden,
            animations: !self.still,
            switch_accounts: self.switch_accounts,
            agent: self.settings_agent,
            defaults: self.shared.defaults_of(self.settings_agent),
            offered: (self.settings_agent != Agent::Claude).then(|| {
                console::agent_program(self.settings_agent)
                    .map(|_| !self.agents_off.contains(&self.settings_agent))
            }),
            autostart: (!horadric_hooks::dev()).then(autostart::is_enabled),
            explorer: (!horadric_hooks::dev()).then(explorer::is_installed),
            discord: self.discord,
            recall_across: self.recall_across,
            backup: self.backup_values(),
            scroll: self.scroll_values(),
            update: self.update.as_ref().map(|m| m.version.clone()),
            checking: self.checking,
            version: env!("CARGO_PKG_VERSION").to_string(),
            ask: self.tome.ask,
            hidden: self.tome.hidden.len(),
            keys: Action::ALL.map(|a| settings::Key {
                chord: self.keys[a.index()].name(),
                live: self.keys_live[a.index()],
            }),
            listening: self.key_listening,
            keys_note: self.keys_note.clone(),
            projects: projects.into_iter().map(|(_, name)| name).collect(),
            project,
        }
    }

    /// What the Scroll section shows: the tongue's switch and the Lexicon
    /// of the project the Projects section has picked.
    fn scroll_values(&self) -> settings::Scroll {
        let Some(dir) = horadric_hooks::state_dir().map(|d| d.join("memory")) else {
            return settings::Scroll::default();
        };
        let mut scroll = settings::Scroll {
            off: horadric_memory::horadrim::is_off(&dir),
            ..Default::default()
        };
        let Some(key) = self.settings_key() else {
            return scroll;
        };
        scroll.project = project_name(&key);
        let lexicon = self
            .project_dir(&key)
            .and_then(|d| horadric_hooks::lore::Place::of(Path::new(&d)).ok())
            .and_then(|place| {
                horadric_memory::horadrim::Tongue::load(&dir)
                    .ok()?
                    .projects
                    .remove(place.key())
            });
        if let Some(lex) = lexicon {
            let mut live: Vec<_> = lex.sigils.iter().filter(|s| !s.retired).collect();
            scroll.sigils = live.len();
            live.sort_by_key(|s| std::cmp::Reverse(s.uses));
            scroll.top = live
                .iter()
                .map(|s| (s.sigil.clone(), s.name.clone(), s.uses))
                .collect();
            scroll.saved = lex.saved_this_week(horadric_hooks::lore::now());
        }
        scroll
    }

    /// Every project the Projects section offers, by key and name: the
    /// recent ones, the ones on screen and the ones with a quest log.
    fn settings_projects(&self) -> Vec<(String, String)> {
        let mut keys: Vec<String> = self
            .recent
            .iter()
            .filter(|p| Path::new(p).is_dir())
            .map(|p| folder_key(p))
            .collect();
        keys.extend(self.clusters.iter().map(|c| c.key.clone()));
        keys.extend(self.shared.boards.borrow().keys().cloned());
        keys.extend(self.drives.keys().cloned());
        keys.retain(|k| !k.is_empty());
        keys.sort();
        keys.dedup();
        let mut out: Vec<(String, String)> = keys
            .into_iter()
            .map(|k| {
                let name = project_name(&k);
                (k, name)
            })
            .collect();
        out.sort_by_key(|(_, name)| name.to_lowercase());
        out
    }

    /// The project the Projects section shows: the one picked, or the
    /// first.
    fn settings_key(&self) -> Option<String> {
        let projects = self.settings_projects();
        let picked = self
            .settings_project
            .as_ref()
            .filter(|k| projects.iter().any(|(key, _)| key == *k));
        picked
            .cloned()
            .or_else(|| projects.first().map(|(key, _)| key.clone()))
    }

    /// The settings of the project with key `key`, at `index` in the list.
    fn project_values(&self, key: &str, index: usize) -> settings::Project {
        let dir = self.project_dir(key);
        let branches = dir.as_deref().and_then(|d| {
            worktree::main_tree(d)
                .is_some()
                .then(|| horadric_hooks::tasks::worktrees(d).enabled)
        });
        let drive = self.drive_of(key);
        settings::Project {
            index,
            colour: theme::ACCENTS[theme::accent_index(key)].1.to_string(),
            branches,
            hosts: dir
                .as_deref()
                .map(horadric_hooks::tasks::hosts)
                .unwrap_or_default(),
            log: self.shared.boards.borrow().contains_key(key),
            drive: drive.as_ref().map(|d| d.ships_public),
            held: drive.is_some_and(|d| d.held.is_some()),
        }
    }

    /// The shortcuts' names for the tray, None for one not registered.
    fn key_names(&self) -> [Option<String>; hotkey::COUNT] {
        Action::ALL.map(|a| self.keys_live[a.index()].then(|| self.keys[a.index()].name()))
    }

    /// Registers every shortcut on its chord again.
    fn register_keys(&mut self) {
        for a in Action::ALL {
            unregister_key(self.notify, a);
            self.keys_live[a.index()] = register_key(self.notify, a, self.keys[a.index()]);
        }
    }

    /// Starts listening for `action`'s new chord.
    fn listen_for_key(&mut self, action: Action) {
        for a in Action::ALL {
            unregister_key(self.notify, a);
        }
        self.key_listening = Some(action);
        self.keys_note = None;
    }

    /// A key pressed in the Settings window while it listens for a chord:
    /// a chord no other shortcut has and no other program holds takes
    /// effect at once and is kept. Anything else says why and listens on.
    fn settings_press(&mut self, press: Press) {
        let Some(action) = self.key_listening else {
            return;
        };
        let i = action.index();
        let dev = horadric_hooks::dev();
        let chord = match press {
            Press::Wait => return,
            Press::Refused(why) => {
                self.keys_note = Some(why.to_string());
                self.refresh_settings();
                return;
            }
            Press::Keep => None,
            Press::Reset => Some(action.default_chord(dev)),
            Press::Set(c) => Some(c),
        };
        if let Some(chord) = chord {
            if let Some(other) = hotkey::clash(&self.keys, action, chord) {
                self.keys_note = Some(format!("{} has {} already", other.label(), chord.name()));
                self.refresh_settings();
                return;
            }
            let old = self.keys[i];
            self.keys[i] = chord;
            self.register_keys();
            if self.keys_live[i] {
                self.save();
            } else {
                // Keeps the chord it had rather than one that does nothing.
                self.keys[i] = old;
                self.key_listening = None;
                self.register_keys();
                self.keys_note = Some(format!("{} is taken by another program", chord.name()));
                self.refresh_settings();
                return;
            }
        }
        self.keys_note = None;
        self.key_listening = None;
        self.register_keys();
        self.refresh_settings();
    }

    /// The Backup section's view of the backup.
    fn backup_values(&self) -> settings::Backup {
        let cfg = &self.backup_cfg;
        let (last, failed) = match &cfg.last {
            None => (String::new(), false),
            Some(l) => match &l.error {
                None => (
                    format!(
                        "{}, {} files, {}",
                        horadric_core::tasks::utc(l.at),
                        l.files,
                        backup_size(l.bytes)
                    ),
                    false,
                ),
                Some(e) => {
                    let short: String = e.chars().take(48).collect();
                    let more = if short.len() < e.len() {
                        "\u{2026}"
                    } else {
                        ""
                    };
                    (format!("Failed: {short}{more}"), true)
                }
            },
        };
        settings::Backup {
            to: cfg.to,
            account: crate::google::account(),
            folder: cfg.folder.as_ref().map(|f| f.display().to_string()),
            passphrase: crate::backup::credential::read(crate::backup::credential::PASSPHRASE)
                .is_some(),
            last,
            failed,
            busy: self.backup_busy.clone(),
        }
    }

    /// Starts `job` on a thread of its own, its result waiting for the
    /// app in [`Self::take_backup`]. One at a time.
    fn backup_start(
        &mut self,
        busy: &str,
        job: impl FnOnce(&Path, crate::backup::Config) -> crate::backup::run::Done + Send + 'static,
    ) {
        let Some(state) = horadric_hooks::state_dir() else {
            return;
        };
        if self.backup_busy.is_some() {
            return;
        }
        self.backup_busy = Some(busy.to_string());
        let cfg = self.backup_cfg.clone();
        let inbox = Arc::clone(&self.backup_done);
        let notify = self.notify.0 as isize;
        thread::spawn(move || {
            let done = job(&state, cfg);
            if let Ok(mut q) = inbox.lock() {
                q.push(done);
            }
            post(notify, WM_HORADRIC_BACKUP, 0);
        });
    }

    fn backup_failed(&self, what: &str) {
        self.toasts.show(Kind::Failed, "Backup", what);
    }

    /// What is missing before a backup can run, said plainly.
    fn backup_missing(&self) -> Option<&'static str> {
        use crate::backup::config::To;
        if crate::backup::credential::read(crate::backup::credential::PASSPHRASE).is_none() {
            return Some("Choose a passphrase first.");
        }
        match self.backup_cfg.to {
            To::Off => Some("Backup is off."),
            To::Drive if crate::google::account().is_none() => Some("Sign in with Google first."),
            To::Folder if self.backup_cfg.folder.is_none() => Some("Pick a folder first."),
            _ => None,
        }
    }

    fn backup_set_to(&mut self, to: crate::backup::config::To) {
        self.backup_cfg.to = to;
        self.backup_save();
    }

    fn backup_folder(&mut self, dir: PathBuf) {
        self.backup_cfg.folder = Some(dir);
        self.backup_save();
    }

    /// Keeps the settings and has the schedule look again at once, so a
    /// change that makes a backup due starts it without waiting a minute.
    fn backup_save(&mut self) {
        if let Some(state) = horadric_hooks::state_dir() {
            if let Err(e) = self.backup_cfg.save(&state) {
                self.backup_failed(&e);
            }
        }
        self.backup_checked = None;
    }

    fn backup_passphrase(&mut self, new: String) {
        self.backup_start("Sealing the backup\u{2026}", move |state, cfg| {
            crate::backup::run::Done::Passphrase(crate::backup::run::set_passphrase(
                state, &cfg, &new,
            ))
        });
    }

    fn backup_restore(&mut self, machine: String, passphrase: Option<String>) {
        self.backup_start("Restoring\u{2026}", move |state, cfg| {
            let result =
                crate::backup::run::stage_restore(state, &cfg, &machine, passphrase.as_deref());
            crate::backup::run::Done::Restored(machine, result)
        });
    }

    fn backup_now(&mut self) {
        self.backup_start("Backing up\u{2026}", |state, _| {
            crate::backup::run::Done::BackedUp(crate::backup::run::run_once(state).1)
        });
    }

    /// A button of the Backup section.
    fn backup_click(&mut self, field: Field) {
        use crate::backup::run::Done;
        if self.backup_busy.is_some() {
            return;
        }
        match field {
            Field::BackupAccount if crate::google::account().is_some() => {
                self.backup_start("Signing out\u{2026}", |_, _| {
                    crate::google::sign_out();
                    Done::SignedOut
                });
            }
            Field::BackupAccount => {
                self.backup_start("Waiting for Google, in your browser\u{2026}", |_, _| {
                    Done::SignedIn(crate::google::sign_in())
                });
            }
            Field::BackupNow => match self.backup_missing() {
                Some(why) => self.backup_failed(why),
                None => self.backup_now(),
            },
            Field::BackupRestore => {
                use crate::backup::config::To;
                match self.backup_cfg.to {
                    To::Drive if crate::google::account().is_none() => {
                        self.backup_failed("Sign in with Google first.");
                    }
                    To::Folder if self.backup_cfg.folder.is_none() => {
                        self.backup_failed("Pick a folder first.");
                    }
                    _ => {
                        self.backup_start("Looking for backups\u{2026}", |_, cfg| {
                            Done::Machines(crate::backup::run::machines(&cfg))
                        });
                    }
                }
            }
            _ => {}
        }
    }

    /// What the backup's jobs have finished.
    fn take_backup(&mut self) {
        use crate::backup::run::Done;
        let done: Vec<Done> = self
            .backup_done
            .lock()
            .map(|mut q| std::mem::take(&mut *q))
            .unwrap_or_default();
        if done.is_empty() {
            return;
        }
        self.backup_busy = None;
        if let Some(state) = horadric_hooks::state_dir() {
            self.backup_cfg = crate::backup::Config::load(&state);
        }
        for d in done {
            match d {
                Done::SignedIn(Ok(_)) | Done::Passphrase(Ok(())) => self.backup_checked = None,
                Done::SignedIn(Err(e)) => self.backup_failed(&format!("Google sign in failed: {e}")),
                Done::SignedOut | Done::BackedUp(_) => {}
                Done::Passphrase(Err(e)) => self.backup_failed(&e),
                Done::Machines(Err(e)) => self.backup_failed(&e),
                Done::Machines(Ok(list)) if list.is_empty() => {
                    self.backup_failed("There is no backup there to restore.");
                }
                Done::Machines(Ok(list)) => {
                    self.backup_step = Some(RestoreStep::Machines(list));
                    self.settings_ask = Some(Field::BackupRestore);
                    post(self.notify.0 as isize, WM_HORADRIC_SETTINGS_ASK, 0);
                }
                Done::Restored(machine, Err(e)) if e == crate::backup::WRONG_PASSPHRASE => {
                    self.backup_step = Some(RestoreStep::Passphrase(machine));
                    self.settings_ask = Some(Field::BackupRestore);
                    post(self.notify.0 as isize, WM_HORADRIC_SETTINGS_ASK, 0);
                }
                Done::Restored(_, Err(e)) => self.backup_failed(&e),
                Done::Restored(machine, Ok((report, at))) => self.toasts.show(
                    Kind::Info,
                    "Restore is ready",
                    &format!(
                        "{} files from {machine}'s backup of {}. Quit Horadric from the tray and start it again to swap them in; the ones they replace are kept.",
                        report.files,
                        horadric_core::tasks::utc(at)
                    ),
                ),
            }
        }
        self.refresh_settings();
    }

    /// The schedule: once a minute, back up when it is switched on, can
    /// reach its target and the last try was an hour ago. The segments of
    /// months that are over hash the same and are never sent again.
    fn backup_tick(&mut self) {
        if let Some((failed, text)) = self.backup_note.take() {
            let kind = if failed { Kind::Failed } else { Kind::Info };
            self.toasts.show(kind, "Restore", &text);
        }
        let waited = self
            .backup_checked
            .is_some_and(|t| t.elapsed() < Duration::from_secs(60));
        if self.backup_busy.is_some() || waited {
            return;
        }
        self.backup_checked = Some(Instant::now());
        if let Some(state) = horadric_hooks::state_dir() {
            self.backup_cfg = crate::backup::Config::load(&state);
        }
        let cfg = &self.backup_cfg;
        if crate::backup::run::due(
            cfg,
            crate::backup::run::ready(cfg),
            crate::backup::run::unix_now(),
        ) {
            self.backup_now();
            self.refresh_settings();
        }
    }

    /// Hands the Settings window the settings again, after a change.
    fn refresh_settings(&self) {
        if let Some(w) = &self.settings_window {
            w.set_values(self.settings_values());
        }
    }

    /// What the list of a row of the Settings window offers.
    fn settings_list(&self, field: Field) -> Option<SettingsList> {
        let at = |found: Option<usize>| found.unwrap_or(0);
        let list = |note, labels, current| {
            Some(SettingsList {
                note,
                labels,
                current,
                apart: None,
            })
        };
        match field {
            Field::Theme => {
                let now = theme::current();
                list(
                    "Every window changes at once",
                    Theme::ALL.iter().map(|t| t.label().to_string()).collect(),
                    at(Theme::ALL.iter().position(|t| *t == now)),
                )
            }
            Field::Font => {
                let fonts = glyphs::monospaced(&self.shared.gpu.dw);
                let now = self.shared.font.family();
                let current = at(fonts.iter().position(|f| f.eq_ignore_ascii_case(&now)));
                list("Every terminal changes at once", fonts, current)
            }
            Field::Screen => {
                let screens = monitors();
                let shown = screens::pick(&screens, self.screen.as_deref()).map(|s| &s.name);
                list(
                    "The tiles move there at once",
                    screens
                        .iter()
                        .enumerate()
                        .map(|(i, s)| screens::label(i + 1, s).replace('\t', ", "))
                        .collect(),
                    at(screens.iter().position(|s| Some(&s.name) == shown)),
                )
            }
            Field::Discord => list(
                "What your Discord profile may show",
                Discord::ALL.iter().map(|d| d.label().to_string()).collect(),
                at(Discord::ALL.iter().position(|d| *d == self.discord)),
            ),
            Field::BackupTo => list(
                "Off keeps the memory on this PC only",
                crate::backup::config::To::ALL
                    .iter()
                    .map(|t| t.label().to_string())
                    .collect(),
                at(crate::backup::config::To::ALL
                    .iter()
                    .position(|t| *t == self.backup_cfg.to)),
            ),
            Field::Agent => list(
                "Whose defaults the rows show",
                Agent::ALL.iter().map(|a| a.label().to_string()).collect(),
                at(Agent::ALL.iter().position(|a| *a == self.settings_agent)),
            ),
            Field::Default(setting) => {
                let agent = self.settings_agent;
                let now = self.shared.defaults_of(agent);
                let (labels, current) = default_choices(agent, setting, now.get(setting));
                Some(SettingsList {
                    note: dropdown::note(agent, setting),
                    apart: (setting == Setting::Permissions).then(|| labels.len() - 1),
                    labels,
                    current,
                })
            }
            Field::Project => {
                let projects = self.settings_projects();
                let key = self.settings_key();
                list(
                    "Its settings show below it",
                    projects.iter().map(|(_, name)| name.clone()).collect(),
                    at(projects.iter().position(|(k, _)| Some(k) == key.as_ref())),
                )
            }
            Field::Colour => list(
                "Its tiles and terminals change at once",
                theme::ACCENTS
                    .iter()
                    .map(|(_, name)| name.to_string())
                    .collect(),
                theme::accent_index(&self.settings_key()?),
            ),
            Field::WorkMode => {
                let dir = self.project_dir(&self.settings_key()?)?;
                let on = horadric_hooks::tasks::worktrees(&dir).enabled;
                list(
                    "Sessions started from now on",
                    WORK_MODES.iter().map(|m| m.to_string()).collect(),
                    usize::from(on),
                )
            }
            _ => None,
        }
    }

    /// A row of the Settings window clicked: a list drops, a switch turns,
    /// a button does its one thing. Each takes effect at once.
    fn settings_click(&mut self, field: Field, row: RECT) {
        match field {
            Field::Theme
            | Field::Font
            | Field::Screen
            | Field::Discord
            | Field::Agent
            | Field::Default(_)
            | Field::Project
            | Field::Colour
            | Field::BackupTo
            | Field::WorkMode => {
                self.settings_list_for = Some((field, row));
                post(self.notify.0 as isize, WM_HORADRIC_SETTINGS_LIST, 0);
            }
            Field::AddHost | Field::ShipsPublic => {
                self.settings_ask = Some(field);
                post(self.notify.0 as isize, WM_HORADRIC_SETTINGS_ASK, 0);
            }
            Field::Drive => {
                if let Some(key) = self.settings_key() {
                    let on = self.drive_of(&key).is_none();
                    self.set_drive(&key, on);
                }
            }
            Field::BackupFolder | Field::BackupPassphrase if self.backup_busy.is_none() => {
                self.settings_ask = Some(field);
                post(self.notify.0 as isize, WM_HORADRIC_SETTINGS_ASK, 0);
            }
            Field::BackupAccount | Field::BackupNow | Field::BackupRestore => {
                self.backup_click(field);
            }
            Field::BackupFolder
            | Field::BackupPassphrase
            | Field::BackupNote
            | Field::BackupLast => {}
            Field::Hosts => {}
            Field::Notify => {
                self.quiet = !self.quiet;
                self.save();
            }
            Field::Town => self.hide_town(!self.town_hidden),
            Field::Animations => {
                self.still = !self.still;
                crate::backdrop::set_still(self.still);
                repaint_all();
                self.save();
            }
            Field::SwitchAccounts => {
                self.switch_accounts = !self.switch_accounts;
                self.save();
                self.switch_when_out();
            }
            Field::Sounds => {
                self.sounds = !self.sounds;
                self.save();
                // So the choice is heard the moment it is made.
                self.sound(Loot::Drop);
            }
            Field::Offered => {
                let agent = self.settings_agent;
                if agent != Agent::Claude && !self.agents_off.remove(&agent) {
                    self.agents_off.insert(agent);
                }
                self.save();
            }
            Field::Autostart => {
                if horadric_hooks::dev() {
                    return;
                }
                if autostart::is_enabled() {
                    autostart::disable();
                } else {
                    autostart::enable();
                }
            }
            Field::Explorer => {
                if horadric_hooks::dev() {
                    return;
                }
                if explorer::is_installed() {
                    let _ = explorer::uninstall();
                } else {
                    explorer::enable();
                }
            }
            // A release found is offered with its notes, outside the app's
            // borrow, as the usage window's Version row does it.
            Field::Updates if self.update.is_some() => {
                post(self.notify.0 as isize, WM_HORADRIC_VERSION, 0);
            }
            Field::Updates => self.check_update(true),
            Field::Version | Field::KeysNote => {}
            Field::Ask => {
                self.tome.ask = !self.tome.ask;
                self.save();
            }
            Field::Recall => {
                self.recall_across = !self.recall_across;
                crate::memory::set_across(self.recall_across);
                self.save();
            }
            Field::Horadrim => {
                if let Some(dir) = horadric_hooks::state_dir().map(|d| d.join("memory")) {
                    let off = horadric_memory::horadrim::is_off(&dir);
                    let _ = horadric_memory::horadrim::set_off(&dir, !off);
                }
            }
            Field::Hidden => self.hide_stone(None),
            Field::Key(action) => self.listen_for_key(action),
        }
        self.refresh_settings();
    }

    /// A list of the Settings window closed: what was picked, if anything,
    /// takes effect at once.
    fn settings_picked(&mut self, field: Field, pick: Option<usize>) {
        if let Some(d) = self.dropdown.take() {
            d.destroy();
        }
        if let Some(w) = &self.settings_window {
            w.set_open(None);
        }
        let Some(i) = pick else {
            return;
        };
        match field {
            Field::Theme => {
                if let Some(t) = Theme::ALL.get(i) {
                    self.set_theme(*t);
                }
            }
            Field::Font => {
                if let Some(family) = glyphs::monospaced(&self.shared.gpu.dw).get(i) {
                    self.set_font_family(family);
                }
            }
            Field::Screen => {
                if let Some(screen) = monitors().get(i) {
                    self.move_to_screen(screens::choice(screen));
                    // The tiles take the new screen's DPI when they get
                    // there, and lay out again for it.
                    unsafe {
                        SetTimer(Some(self.notify), SCREEN_TIMER, 1000, None);
                    }
                }
            }
            Field::Discord => {
                if let Some(d) = Discord::ALL.get(i) {
                    self.set_discord(*d);
                }
            }
            Field::BackupTo => {
                if let Some(to) = crate::backup::config::To::ALL.get(i) {
                    self.backup_set_to(*to);
                }
            }
            Field::Agent => {
                if let Some(a) = Agent::ALL.get(i) {
                    self.settings_agent = *a;
                }
            }
            Field::Default(setting) => {
                let agent = self.settings_agent;
                if let Some(value) = default_picked(agent, setting, i) {
                    self.set_default(agent, setting, value);
                }
            }
            Field::Project => {
                if let Some((key, _)) = self.settings_projects().into_iter().nth(i) {
                    self.settings_project = Some(key);
                }
            }
            Field::Colour => {
                if let Some(key) = self.settings_key() {
                    if i < theme::ACCENTS.len() {
                        self.recolour(&key, i);
                    }
                }
            }
            Field::WorkMode => {
                if let Some(dir) = self.settings_key().and_then(|k| self.project_dir(&k)) {
                    if let Err(e) = horadric_hooks::tasks::set_worktrees(&dir, i == 1) {
                        eprintln!("horadric: cannot write the project's config: {e}");
                    }
                }
            }
            _ => {}
        }
        self.refresh_settings();
    }

    fn close_settings(&mut self) {
        if self.key_listening.take().is_some() {
            self.register_keys();
        }
        self.keys_note = None;
        if self
            .dropdown
            .as_ref()
            .is_some_and(|d| matches!(d.whose, Whose::Settings(_)))
        {
            if let Some(d) = self.dropdown.take() {
                d.destroy();
            }
        }
        if let Some(w) = self.settings_window.take() {
            w.destroy();
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
                        last: s.last_line.clone(),
                        branch: s.worktree.as_ref().map(|w| w.branch.clone()),
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

    /// Deletes the checkpoints of a session that ends for good, on a thread
    /// since it runs git.
    fn drop_turns(&self, id: &str) {
        let Some(cwd) = self
            .shared
            .registry
            .lock()
            .ok()
            .and_then(|r| r.get(id).filter(|s| !s.shell).map(|s| s.cwd.clone()))
        else {
            return;
        };
        let id = id.to_string();
        thread::spawn(move || horadric_hooks::checkpoint::forget(&id, &cwd));
    }

    /// Whether the tile menu offers to undo the session's last turn: a
    /// Claude Code session, live or paused, between turns. Only its edit
    /// tools name the files they write; Codex and Grok tools do not.
    fn can_undo(&self, id: &str) -> bool {
        let between = self.shared.registry.lock().is_ok_and(|r| {
            r.get(id).is_some_and(|s| {
                !s.shell && s.agent == Agent::Claude && !s.phase.mid_turn() && !s.phase.asks()
            })
        });
        between
            && matches!(
                self.tile_kind(id),
                Some((TileKind::Live | TileKind::Paused, false))
            )
    }

    /// Puts back the files the session's last turn changed, on a thread,
    /// and toasts how it went.
    fn undo_turn(&self, id: &str) {
        let Some((label, cwd)) = self.shared.registry.lock().ok().and_then(|r| {
            let s = r.get(id)?;
            Some((s.label().to_string(), s.cwd.clone()))
        }) else {
            return;
        };
        let id = id.to_string();
        let undone = Arc::clone(&self.undone);
        let notify = self.notify.0 as isize;
        thread::spawn(move || {
            let outcome = horadric_hooks::checkpoint::undo(&id, &cwd);
            if let Ok(mut u) = undone.lock() {
                u.push((label, outcome));
            }
            post(notify, WM_HORADRIC_UNDONE, 0);
        });
    }

    fn take_undone(&mut self) {
        let done = self
            .undone
            .lock()
            .map(|mut u| std::mem::take(&mut *u))
            .unwrap_or_default();
        for (label, outcome) in done {
            let kind = match &outcome {
                checkpoint::Outcome::Undone { .. } => Kind::Done,
                checkpoint::Outcome::Refused(_) | checkpoint::Outcome::Failed(_) => Kind::Failed,
                _ => Kind::Info,
            };
            let (title, text) = checkpoint::summary(&label, &outcome);
            self.toasts.show(kind, &title, &text);
        }
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

    /// How many sessions the registry has in the project.
    fn sessions_in(&self, key: &str) -> usize {
        self.shared
            .registry
            .lock()
            .map_or(0, |r| r.all().filter(|s| project_key(s) == key).count())
    }

    /// Hides Tristram's tile, or shows it again. A session running in the
    /// town keeps it on screen until it ends.
    fn hide_town(&mut self, hidden: bool) {
        self.town_hidden = hidden;
        self.save();
        self.reconcile(false);
    }

    /// Ends the project's sessions and takes its cluster down, unfinished
    /// tasks or not.
    fn close_project(&mut self, key: &str) {
        if town::is(key) {
            return self.end_all(Some(key));
        }
        self.open.remove(key);
        self.closed.insert(key.to_string());
        self.end_all(Some(key));
    }

    /// Paints the project in another colour: its cluster, its stash and
    /// cube slots, and the stage while it shows the project.
    fn recolour(&mut self, key: &str, i: usize) {
        theme::set_accent(key, i);
        for c in self.clusters.iter().filter(|c| c.key == key) {
            c.invalidate();
        }
        if self.stage.as_ref().is_some_and(|s| s.project() == key) {
            self.fill_stage(key);
        }
        self.sync_stash();
        self.sync_cube();
        self.save();
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
        // Every way a session goes for good comes through here.
        self.keeping_ended(id);
        self.drop_turns(id);
        if let Some(c) = self.consoles.remove(id) {
            if c.exit_code().is_none() {
                c.kill();
            }
        }
        self.paused.remove(id);
        self.stop_agents.remove(id);
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
        // Asked for by name, a parked session comes back onto the stage.
        if self.shared.parked.borrow_mut().remove(id) {
            self.refresh_parked();
        }
        if self.far_reveal(id) {
            return;
        }
        let stashed = self.shared.registry.lock().is_ok_and(|r| r.is_stashed(id));
        if stashed {
            self.unstash(id, true);
            return;
        }
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
        if self.fill_stage(&key) {
            if let Some(stage) = &self.stage {
                stage.focus_session(id);
            }
        }
    }

    /// Parks a session's pane off the stage while the session runs on, for
    /// a grid of only what you are looking at, or brings a parked one back
    /// with the keyboard. Parking the last pane closes the stage.
    fn park(&mut self, id: &str) {
        let parked = {
            let mut p = self.shared.parked.borrow_mut();
            !p.remove(id) && p.insert(id.to_string())
        };
        self.refresh_parked();
        if !parked {
            self.reveal(id, false);
            return;
        }
        let key = self.project_of(id);
        if key.is_some() && self.stage.as_ref().map(|s| s.project()) == key {
            self.sync_stage();
        }
    }

    /// The tiles redraw their lamps' rings.
    fn refresh_parked(&self) {
        for c in &self.clusters {
            c.refresh();
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
            post_event(notify, true);
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
        live.extend(self.far_live_in(key));
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
        // Filtered after the order, so a parked session keeps its place.
        ids.retain(|id| !self.shared.parked.borrow().contains(id));
        // A file view comes last and is never saved in the order.
        if let Some(view) = self.views.get(key) {
            ids.push(view.id.clone());
        }
        if let Some(page) = self.webs.get(key) {
            ids.push(page.id.clone());
        }
        if let Some(sky) = self.skies.get(key) {
            ids.push(sky.id.clone());
        }
        ids
    }

    /// Puts a project's sessions on the stage, opening it when closed:
    /// where it was last, or else against the tiles as a square filling top
    /// to bottom. An open stage keeps its place and size. False when the
    /// project has nothing to show.
    fn fill_stage(&mut self, key: &str) -> bool {
        let ids = self.grid_of(key);
        let sessions: Vec<(Arc<Console>, String)> = ids
            .iter()
            .filter_map(|id| Some((Arc::clone(self.console_of(id)?), self.name_of(id))))
            .collect();
        if sessions.is_empty() {
            return false;
        }
        let switched = self.stage.as_ref().map(|s| s.project()).as_deref() != Some(key);
        if self.stage.is_none() {
            let place = match self.stage_rect {
                Some(r) => Place::Rect(r),
                None => {
                    let (area, tiles_left) = self.stage_area();
                    Place::Visible(layout::square(area, tiles_left))
                }
            };
            match TerminalWindow::open(Rc::clone(&self.shared), place) {
                Ok(t) => self.stage = Some(t),
                Err(e) => {
                    eprintln!("horadric: cannot open the stage: {e}");
                    return false;
                }
            }
        }
        self.stage_follows(self.tiles_edge);
        if let Some(stage) = &self.stage {
            stage.show(key, &project_name(key), sessions);
        }
        self.portal_show();
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
        if !self.fill_stage(&key) {
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
            self.tasks.ship_for = None;
            self.update_click = false;
            self.toasts.show(Kind::Waiting, &a.title, &a.text);
        }
    }

    /// Two sessions changed the same file in one tree. The agent was told
    /// in its hook's reply; this tells the human, and a click shows the
    /// session that edited last. Only for sessions this app holds: the
    /// installed Horadric hears a dev instance's hooks first.
    fn overlapped(&mut self, o: &Overlap) {
        let names: Option<(String, Vec<String>)> = self.shared.registry.lock().ok().and_then(|r| {
            let me = r.get(&o.session)?.label().to_string();
            let others = o
                .others
                .iter()
                .filter_map(|id| r.get(id).map(|s| s.label().to_string()))
                .collect();
            Some((me, others))
        });
        let Some((me, others)) = names.filter(|(_, others)| !others.is_empty()) else {
            return;
        };
        let file = o.file.rsplit(['/', '\\']).next().unwrap_or(&o.file);
        self.alert_for = Some(o.session.clone());
        self.tasks.merge_for = None;
        self.tasks.ship_for = None;
        self.update_click = false;
        self.toasts.show(
            Kind::Waiting,
            &format!("Two sessions in {file}"),
            &format!(
                "{me} changed {file}, which {} changed too and has not committed. \
                 {me} was told to keep their changes.",
                others.join(" and ")
            ),
        );
    }

    /// Answers Horadric's Claude mod: the quest its session holds and the
    /// files it shares with other sessions, those named as their tiles are.
    fn know_call(&mut self, call: KnowCall) {
        let quest = self.held_quest(&call.session);
        let shared: Vec<(String, Vec<String>)> = self
            .shared
            .registry
            .lock()
            .map(|r| {
                call.overlaps
                    .iter()
                    .map(|o| {
                        let names = o
                            .others
                            .iter()
                            .filter_map(|id| r.get(id))
                            .map(|s| s.label().to_string());
                        (o.file.clone(), names.collect())
                    })
                    .collect()
            })
            .unwrap_or_default();
        // A mod that gave up waiting never hears this answer, so its stops
        // wait for the next ask rather than being lost.
        let stop = if call.reply.wanted() {
            self.stop_agents.remove(&call.session).unwrap_or_default()
        } else {
            Vec::new()
        };
        call.reply.send(known(quest.as_deref(), &shared, &stop));
    }

    /// Asks the session's mod to stop one of its subagents, the next time
    /// it asks what Horadric knows (every few seconds).
    fn stop_subagent(&mut self, id: &str, agent: &str) {
        let wanted = self.stop_agents.entry(id.to_string()).or_default();
        if !wanted.iter().any(|a| a == agent) {
            wanted.push(agent.to_string());
        }
    }

    /// The subagents a tile's menu can stop, with their labels.
    fn stoppable_subagents(&self, id: &str) -> Vec<(String, String)> {
        let now = SystemTime::now();
        self.shared
            .registry
            .lock()
            .ok()
            .and_then(|r| {
                r.get(id).map(|s| {
                    // Only a mod that stamps start times knows how to stop
                    // one; an older mod still running would drop the stop.
                    s.running_subagents()
                        .into_iter()
                        .filter(|a| a.started_ms.is_some())
                        .map(|a| {
                            let label = horadric_core::session::subagent_label(a, now);
                            (a.agent_id.clone(), label)
                        })
                        .collect()
                })
            })
            .unwrap_or_default()
    }

    /// The notification was clicked: show the session it was about, or,
    /// for several, the one that has waited longest.
    /// The release the last notification told of, when it did, for a
    /// click on it.
    fn take_update_click(&mut self) -> Option<Manifest> {
        std::mem::take(&mut self.update_click)
            .then(|| self.update.clone())
            .flatten()
    }

    fn open_alert(&mut self) {
        if let Some(m) = self.tasks.merge_for.take() {
            runner::ask_for(self, runner::Menu::Merge(m));
            return;
        }
        if let Some(key) = self.tasks.ship_for.take() {
            runner::ask_for(self, runner::Menu::Ship(key));
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
    /// Draws every window in `t`. The cached layers know their theme and
    /// draw again for a new one, so asking each window to paint is enough.
    fn set_theme(&mut self, t: Theme) {
        theme::set(t);
        if let Some(stage) = &self.stage {
            stage.retheme();
        }
        repaint_all();
        self.save();
    }

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
        if self.far_rename(id, name) {
            return;
        }
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
            .or_else(|| id.starts_with(WEB).then(|| "Browser".to_string()))
            .or_else(|| id.starts_with(SKY).then(|| "Arcane Sanctuary".to_string()))
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
        if self.fill_stage(key) {
            if let Some(stage) = &self.stage {
                stage.focus_session(&id);
            }
        }
    }

    fn close_view(&mut self, serial: usize) {
        self.views.retain(|_, v| v.serial != serial);
        self.sync_stage();
    }

    /// Shows the project's browser pane on the stage beside its sessions,
    /// with the keyboard, at `url` when given. A new one with no address
    /// asks for one.
    fn open_web(&mut self, key: &str, url: Option<&str>) {
        let id = format!("{WEB}{key}");
        let new = !self.webs.contains_key(key);
        if new {
            let serial = self.next_serial;
            self.next_serial += 1;
            let page = Console::web(id.clone(), serial, key.to_string());
            self.webs.insert(key.to_string(), page);
        }
        web::open(key, url);
        if self.fill_stage(key) {
            if let Some(stage) = &self.stage {
                stage.focus_session(&id);
            }
        }
        if new && url.is_none() {
            self.web_ask = Some((key.to_string(), WebAsk::Address));
            post(self.notify.0 as isize, WM_HORADRIC_WEB, 0);
        }
    }

    /// Opens `url` in a new tab of the project's browser pane, shown on
    /// the stage with the keyboard.
    fn open_web_tab(&mut self, key: &str, url: &str) {
        web::open_tab(key, Some(url));
        self.open_web(key, None);
    }

    /// Opens the browsers left open before a reload or a crash, each in
    /// its project's grid, as an agent's would be: no stage, no keyboard.
    fn restore_webs(&mut self, pages: &BTreeMap<String, horadric_core::saved::SavedPages>) {
        for (key, saved) in pages {
            let serial = self.next_serial;
            self.next_serial += 1;
            let page = Console::web(format!("{WEB}{key}"), serial, key.clone());
            self.webs.insert(key.clone(), page);
            web::restore(key, saved);
        }
    }

    fn close_web(&mut self, key: &str) {
        self.webs.remove(key);
        self.sync_stage();
        web::close(key);
    }

    /// Shows the project's Arcane Sanctuary on the stage beside its
    /// sessions, with the keyboard, opening it when it is not yet.
    fn open_sky(&mut self, key: &str) {
        let Some(dir) = self.project_dir(key) else {
            return;
        };
        let id = format!("{SKY}{key}");
        if !self.skies.contains_key(key) {
            let serial = self.next_serial;
            self.next_serial += 1;
            self.skies.insert(
                key.to_string(),
                Console::sky(id.clone(), serial, key.to_string()),
            );
            sanctuary::open(key, &dir);
        }
        if self.fill_stage(key) {
            if let Some(stage) = &self.stage {
                stage.focus_session(&id);
            }
        }
    }

    /// An agent touched something: every sanctuary on the stage draws it
    /// lit and runs its clock while the light fades.
    fn light_skies(&mut self) {
        let stamp = crate::glows::stamp();
        if std::mem::replace(&mut self.glow_stamp, stamp) == stamp {
            return;
        }
        if let Some(stage) = &self.stage {
            for sky in self.skies.values() {
                stage.refresh(sky.serial);
            }
        }
    }

    fn close_sky(&mut self, key: &str) {
        self.skies.remove(key);
        self.sync_stage();
        sanctuary::close(key);
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
                c.refresh();
            }
        }
    }

    /// Moves the stage's edge to the tiles' when they reach under it, or
    /// when it stood against them at `was` before they changed width.
    fn stage_follows(&self, was: Option<i32>) {
        let (Some(stage), Some(edge)) = (&self.stage, self.tiles_edge) else {
            return;
        };
        let Ok(r) = snapping::visible_rect(stage.hwnd) else {
            return;
        };
        let work = work_area(self.screen.as_deref());
        let rect = [r.left, r.top, r.right, r.bottom];
        if let Some(to) =
            layout::follow_tiles(rect, was, edge, (work.0, work.2), self.px(STAGE_MIN_W_DIP))
        {
            stage.set_visible_rect(to);
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
            if self.fill_stage(&key) {
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
                if r.weigh_programs(SystemTime::now()) {
                    post_event(APP_WINDOW.with(Cell::get), true);
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
                c.tick();
            }
        }
        // Bringing the stage to the front says nothing to the app, so a
        // look is noticed here, within a second.
        self.identify();
        if let Some(since) = self.away.idle(idle_secs(), unix_now()) {
            self.welcome_back(since);
        }
        // A dev instance comes back from an hour away when told to, since
        // an absence can not be tried while anyone uses the machine.
        let back = store::dir().map(|d| d.join("away-now"));
        if let Some(back) = back.filter(|b| horadric_hooks::dev() && b.exists()) {
            let _ = std::fs::remove_file(back);
            self.welcome_back(unix_now().saturating_sub(3600));
        }
        // And leaves at once, which only spectator mode looks at.
        let left = store::dir().map(|d| d.join("spectate-now"));
        let left = left.filter(|l| horadric_hooks::dev() && l.exists());
        if let Some(l) = &left {
            let _ = std::fs::remove_file(l);
        }
        self.spectate(left.is_some());
        // And has every driven project's round come due, rather than wait
        // an hour for the clock.
        let round = store::dir().map(|d| d.join("round-now"));
        if let Some(r) = round.filter(|r| horadric_hooks::dev() && r.exists()) {
            let _ = std::fs::remove_file(r);
            self.round_for(None, horadric_core::warriv::Why::Hour);
        }
        // A run of work ends after a quiet spell no event marks.
        self.sync_discord();
        self.refresh_quest_log(false);
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
                tome_collapsed: c.tome_collapsed(),
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
            closed: {
                let mut closed: Vec<String> = self.closed.iter().cloned().collect();
                closed.sort();
                closed
            },
            autostart_offered: self.autostart_offered,
            explorer_offered: self.explorer_offered,
            stage: self
                .stage
                .as_ref()
                .and_then(|s| s.rect())
                .or(self.stage_rect),
            on_stage: self
                .stage
                .as_ref()
                .and_then(|s| s.active())
                .filter(|id| !id.starts_with(VIEW) && !id.starts_with(WEB) && !id.starts_with(SKY)),
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
            defaults: self.shared.defaults_of(Agent::Claude),
            agents_off: self.agents_off.clone(),
            agent_defaults: self
                .shared
                .defaults
                .borrow()
                .iter()
                .filter(|(a, d)| **a != Agent::Claude && **d != horadric_core::Defaults::default())
                .map(|(a, d)| (*a, d.clone()))
                .collect(),
            usage: self.shared.usage.lock().ok().and_then(|u| u.clone()),
            agent_usage: self
                .shared
                .agent_usage
                .lock()
                .map(|u| u.clone())
                .unwrap_or_default(),
            usage_window: self.usage_window.as_ref().map(|u| SavedPanel {
                collapsed: u.collapsed.get(),
                locked: u.locked.get(),
            }),
            font_size: Some(self.shared.font.size()).filter(|&s| s != keys::FONT_DEFAULT),
            quiet: self.quiet,
            sounds: self.sounds,
            town_hidden: self.town_hidden,
            still: self.still,
            switch_accounts: self.switch_accounts,
            discord: self.discord,
            recall_across: self.recall_across,
            run: self.run.saved(),
            cube: self.cube_on,
            font_family: self.font_family.clone(),
            theme: theme::current().saved(),
            screen: self.screen.clone(),
            live: !self.quit,
            recovering: self.recovering.is_some(),
            page_sizes: web::sizes(),
            accents: theme::accents(),
            page_docks: web::docks(),
            sky_docks: sanctuary::docks(),
            pages: web::pages(),
            runewords: self.tome.projects.clone(),
            stones_cast: self.tome.cast.clone(),
            cast_without_asking: !self.tome.ask,
            stones_hidden: self.tome.hidden.clone(),
            stones_order: self.tome.order.clone(),
            errands: self.tome.errands.clone(),
            update_told: self.update_told.clone(),
            drives: self.drives.clone(),
            warriv_heard: self.tasks.warriv.heard(),
            stopped: {
                let mut stopped: Vec<String> = self.stopped.iter().cloned().collect();
                stopped.sort();
                stopped
            },
            hotkeys: Action::ALL
                .into_iter()
                .filter(|a| self.keys[a.index()] != a.default_chord(horadric_hooks::dev()))
                .map(|a| (a.key().to_string(), self.keys[a.index()].name()))
                .collect(),
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
        self.cut_wakes_short();
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
        // Quit and reload clear the profile before the pipe closes, since
        // Discord shows a stale activity for a while after we are gone. The
        // wait is bounded by the client.
        if let Some(rich) = self.rich.take() {
            rich.stop();
        }
    }

    /// Every project the town agent is told about: the recent folders that
    /// still exist, with when a session last ran in each.
    fn town_projects(&self) -> Vec<town::Project> {
        let now = SystemTime::now();
        let last: HashMap<String, u64> = self
            .shared
            .registry
            .lock()
            .map(|r| {
                let mut m: HashMap<String, u64> = HashMap::new();
                for s in r.all().filter(|s| !s.cwd.is_empty()) {
                    let secs = now.duration_since(s.since).map_or(0, |d| d.as_secs());
                    let e = m.entry(project_key(s)).or_insert(secs);
                    *e = (*e).min(secs);
                }
                m
            })
            .unwrap_or_default();
        let mut seen = HashSet::new();
        self.recent
            .iter()
            .filter(|p| Path::new(p).is_dir())
            .filter_map(|p| {
                let key = folder_key(p);
                if town::is(&key) || !seen.insert(key.clone()) {
                    return None;
                }
                let intro = ["README.md", "CLAUDE.md"]
                    .iter()
                    .find_map(|f| std::fs::read_to_string(Path::new(p).join(f)).ok())
                    .map(|t| town::intro_of(&t, 3))
                    .unwrap_or_default();
                Some(town::Project {
                    name: project_name(&key),
                    dir: p.clone(),
                    intro,
                    last_ran: last.get(&key).copied(),
                })
            })
            .collect()
    }

    /// Rewrites the town's `CLAUDE.md` when the set of projects changed.
    /// Only the folders count: how long ago a session ran would rewrite it
    /// on every pass.
    fn refresh_town(&mut self) {
        let Some(dir) = town::dir().filter(|d| d.is_dir()) else {
            return;
        };
        let projects = self.town_projects();
        let same = self.town_listed.as_ref().is_some_and(|old| {
            old.len() == projects.len() && old.iter().zip(&projects).all(|(a, b)| a.dir == b.dir)
        });
        if !same {
            town::write(&dir, &projects);
            self.town_listed = Some(projects);
        }
    }

    /// Makes the windows match the projects in the registry.
    fn reconcile(&mut self, phase_changed: bool) {
        self.read_boards(false);
        self.refresh_town();
        let mut projects: HashSet<String> = self
            .shared
            .registry
            .lock()
            .map(|r| r.all().map(project_key).collect())
            .unwrap_or_default();
        self.closed.retain(|k| !projects.contains(k));
        self.open
            .extend(projects.iter().filter(|k| !k.is_empty()).cloned());
        // A brother's project has no folder here to stay open by: its
        // cluster goes with its last session.
        self.open
            .retain(|k| horadric_core::afar::split_key(k).is_none() || projects.contains(k));
        // A hidden town shows only while a session runs there.
        if self.town_hidden {
            self.open.retain(|k| !town::is(k));
        }
        projects.extend(self.open.iter().cloned());
        projects.extend(self.listed());
        // The town stays when it has no sessions, unless it is hidden.
        if !self.town_hidden {
            projects.extend(town::key().map(String::from));
        }

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
        let mut created = false;
        for key in &projects {
            if self.clusters.iter().any(|c| &c.key == key) {
                continue;
            }
            let open: Vec<&str> = self.clusters.iter().map(|c| c.key.as_str()).collect();
            theme::give_accent(key, &open);
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
                        c.set_tome_collapsed(place.tome_collapsed);
                    }
                    self.clusters.push(c);
                    created = true;
                }
                Err(e) => eprintln!("horadric: cannot create window: {e}"),
            }
        }
        // A new cluster is measured for its column below, and its Runetome
        // would otherwise come at the next tick, two rows of stones taller
        // than it was measured, and hang past the bottom of the screen.
        if created {
            self.refresh_tomes();
        }

        self.order_sessions();
        for c in &self.clusters {
            c.update();
        }
        self.sync_stash();
        self.sync_cube();
        self.sync_start();
        // A status line can bring the first limits, which adds rows, and a
        // session of another agent its screen.
        if let Some(u) = &self.usage_window {
            u.set_screens(self.agents_in_use());
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

        let (total, waiting, working) = self
            .shared
            .registry
            .lock()
            .map(|r| {
                let working = r.all().any(|s| s.phase == Phase::Working);
                (r.len(), r.waiting().len(), working)
            })
            .unwrap_or((0, 0, false));
        match self.tray.breathe(working) {
            Some(true) => unsafe {
                SetTimer(Some(self.notify), BREATH_TIMER, tray::BREATH_STEP_MS, None);
            },
            Some(false) => unsafe {
                let _ = KillTimer(Some(self.notify), BREATH_TIMER);
                self.breathe_stage();
            },
            None => {}
        }
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
        self.sync_discord();
        self.identify();
        self.announce();
        self.journal_phases();
    }

    /// Gives the stage's taskbar button the tray's current breath.
    fn breathe_stage(&self) {
        if let Some(stage) = &self.stage {
            stage.set_icon(self.tray.taskbar_icon());
        }
    }

    /// Writes a line for each session whose phase became one worth telling:
    /// waiting, done, or ended, which a session that vanishes did too.
    fn journal_phases(&mut self) {
        let now = unix_now();
        let mut lines = Vec::new();
        let mut turns = Vec::new();
        let mut seen = HashSet::new();
        let mut dropped = false;
        if let Ok(r) = self.shared.registry.lock() {
            for s in r.all() {
                if s.brother.is_some() {
                    // Journaled where it runs: here it would count twice in
                    // the history and the stats. A turn done still chimes.
                    let before = self.far.phases.insert(s.id.clone(), s.phase.clone());
                    dropped |= s.phase == Phase::Done && before.is_some_and(|b| b != s.phase);
                    continue;
                }
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
                if matches!(s.phase, Phase::Done | Phase::Ended) {
                    turns.extend(self.quest_turn(s, &project, now));
                }
                lines.push(Entry {
                    at: now,
                    session: s.id.clone(),
                    name,
                    project,
                    what,
                });
            }
        }
        if let Ok(r) = self.shared.registry.lock() {
            self.far.phases.retain(|id, _| r.get(id).is_some());
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
        for r in &turns {
            store::chronicle(r);
        }
        // With the beam, which rises the moment the tile turns done.
        if dropped {
            self.sound(Loot::Drop);
        }
    }

    /// The chronicle's record of a turn that ended, when the session works
    /// a quest of its project's list: what it said last, and its
    /// conversation, which the quest log reads or carries on later. Kept
    /// under the session's own id, so each tomb's word stays its own.
    fn quest_turn(&self, s: &Session, project: &str, now: u64) -> Option<chronicle::Record> {
        let boards = self.shared.boards.borrow();
        let task = chronicle::worked_by(&boards.get(project)?.tasks, &s.id)?;
        Some(chronicle::Record {
            at: now,
            project: project.to_string(),
            quest: s.id.clone(),
            title: task.title.clone(),
            what: chronicle::Happened::Turn {
                line: s.last_line.clone(),
                conversation: s.claude_session_id.clone().unwrap_or_default(),
                cwd: s.cwd.clone(),
                name: s.label().to_string(),
            },
        })
    }

    /// The one place the "Show on Discord" setting changes, so the presence
    /// is handed on, or cleared on Off, from here and nowhere else.
    fn set_discord(&mut self, discord: Discord) {
        if self.discord == discord {
            return;
        }
        self.discord = discord;
        self.save();
        self.sync_discord();
    }

    /// Hands the Discord client what the profile should say now. Called
    /// whenever the registry or the setting changes, and from the tick so a
    /// run of work ends on time; the client drops what did not change. It
    /// only ever sends on a channel, so the UI thread never waits on the
    /// pipe.
    fn sync_discord(&mut self) {
        if self.discord.is_off() || self.frozen {
            // Dropped, not stopped: the client clears and closes on its own
            // thread, and the UI does not wait for a Discord that may hang.
            self.rich = None;
            self.run = presence::Run::default();
            return;
        }
        let began = Instant::now();
        let stage = self.stage.as_ref().map(|s| project_name(&s.project()));
        let activity = {
            let Ok(r) = self.shared.registry.lock() else {
                return;
            };
            let named: Vec<(&Session, String)> = r
                .all()
                .map(|s| (s, project_name(&project_key(s))))
                .collect();
            let seen: Vec<presence::Seen> = named
                .iter()
                .map(|(session, project)| presence::Seen { session, project })
                .collect();
            let start = self.run.update(&seen, SystemTime::now());
            presence::presence(&seen, stage.as_deref(), self.discord, start)
        };
        self.rich
            .get_or_insert_with(crate::discord::Discord::start)
            .set(activity);
        if std::env::var_os("HORADRIC_DEBUG").is_some() {
            eprintln!("discord sync took {:?}", began.elapsed());
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
        if unix_now().saturating_sub(since) >= away::AWAY_FOR && self.tell_away(since) {
            return;
        }
        self.listen(since, Some(since), false);
    }

    /// Opens the away card if anything happened in a quest log since
    /// `since`, in place of the catch-up. True when it opened.
    fn tell_away(&mut self, since: u64) -> bool {
        let state = unsafe { SHQueryUserNotificationState() }.map_or(5, |s| s.0);
        if toast::hold_back(state) {
            return false;
        }
        let records = store::chronicle_all();
        let news: Vec<(String, String, chronicle::Away)> = {
            let boards = self.shared.boards.borrow();
            let warriv = self.shared.warriv.borrow();
            let mut keys: Vec<&String> = boards.keys().collect();
            keys.sort();
            keys.into_iter()
                .filter_map(|key| {
                    let b = &boards[key];
                    let has = warriv.get(key).cloned().unwrap_or_default();
                    let a = chronicle::away(&records, key, &b.tasks, &has, since);
                    let dir = self.project_dir(key)?;
                    a.happened()
                        .then(|| (project_name(key), dir.to_string_lossy().into_owned(), a))
                })
                .collect()
        };
        if news.is_empty() {
            return false;
        }
        if let Some(c) = self.away_card.take() {
            c.destroy();
        }
        let now = unix_now();
        let (rows, answers) = away::rows(&news, now);
        let sub = away::gone(now.saturating_sub(since));
        let stage = self.stage.as_ref().map(|s| s.hwnd);
        match AwayCard::open(Rc::clone(&self.shared), stage, sub, rows, answers) {
            Ok(c) => {
                self.away_card = Some(c);
                true
            }
            Err(e) => {
                eprintln!("horadric: cannot open the away card: {e}");
                false
            }
        }
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
        // What spectating shows was not looked at by anyone.
        if self.spectating() {
            return;
        }
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
                c.refresh();
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
                    // A town with no sessions and no browser has no tiles to
                    // fold, so its header opens the stage with its browser
                    // pane. Once it has a tile, a tab's included, the header
                    // folds it as every project's does.
                    let bare = self
                        .clusters
                        .iter()
                        .find(|c| c.hwnd.0 as isize == hwnd && town::is(&c.key))
                        .filter(|c| c.tileless())
                        .map(|c| c.key.clone());
                    if let Some(key) = bare {
                        self.open_web(&key, None);
                    } else if let Some(c) =
                        self.clusters.iter_mut().find(|c| c.hwnd.0 as isize == hwnd)
                    {
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
                Input::Park(id) => self.park(&id),
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
                Input::StageActive => self.raise(),
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
                    if self.far_start(&key) {
                    } else if let Some(dir) = self.project_dir(&key) {
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
                Input::Hand(id, text) => {
                    if let Some(stage) = &self.stage {
                        stage.hand(&id, &text);
                    }
                }
                Input::PaneAsk(id, what) => {
                    self.pane_ask = Some((id, what));
                    post(self.notify.0 as isize, WM_HORADRIC_PANE, 0);
                }
                Input::Browse => {
                    if let Some(key) = self.stage.as_ref().map(|s| s.project()) {
                        self.open_web(&key, None);
                    }
                }
                Input::Link(url) => match self.stage.as_ref().map(|s| s.project()) {
                    Some(key) if self.webs.contains_key(&key) => self.open_web_tab(&key, &url),
                    _ => watch::open_link(&crate::links::Target::Web(url), None),
                },
                Input::CloseWeb(key) => self.close_web(&key),
                Input::Pages(key) => {
                    for c in self.clusters.iter().filter(|c| c.key == key) {
                        relayout |= c.update();
                    }
                }
                Input::Page(key, i) => {
                    web::tab(&key, web::TabStep::Select(i));
                    self.open_web(&key, None);
                }
                Input::Sanctuary => {
                    if let Some(key) = self.stage.as_ref().map(|s| s.project()) {
                        self.open_sky(&key);
                    }
                }
                Input::CloseSky(key) => self.close_sky(&key),
                Input::WebTab(key, step) => web::tab(&key, step),
                Input::WebAsk(key, what) => {
                    self.web_ask = Some((key, what));
                    post(self.notify.0 as isize, WM_HORADRIC_WEB, 0);
                }
                Input::Font(step) => self.set_font(step),
                Input::FilesChanged(key) => self.files_changed(&key),
                Input::Arrange => relayout = true,
                Input::Spotlight => {
                    self.identify();
                    for c in &self.clusters {
                        c.invalidate();
                    }
                }
                Input::UsageMenu => post(self.notify.0 as isize, WM_HORADRIC_USAGE_MENU, 0),
                Input::PortalKey => post(self.notify.0 as isize, WM_HORADRIC_PORTAL_KEY, 0),
                Input::AccountMenu(a) => {
                    let i = Agent::ALL.iter().position(|x| *x == a).unwrap_or(0);
                    post(self.notify.0 as isize, WM_HORADRIC_ACCOUNT_MENU, i);
                }
                Input::Version => post(self.notify.0 as isize, WM_HORADRIC_VERSION, 0),
                Input::SettingMenu(a, s, row) => {
                    self.setting_menu_for = Some((a, s, row));
                    post(self.notify.0 as isize, WM_HORADRIC_SETTING_MENU, 0);
                }
                Input::Picked(agent, setting, pick) => {
                    if let Some(d) = self.dropdown.take() {
                        d.destroy();
                    }
                    if let Some(u) = &self.usage_window {
                        u.open.set(None);
                        u.invalidate();
                    }
                    if let Some(value) = pick {
                        self.set_default(agent, setting, value);
                    }
                }
                Input::SetDefault(agent, setting, value) => self.set_default(agent, setting, value),
                Input::SettingsClick(field, row) => self.settings_click(field, row),
                Input::SettingsPicked(field, pick) => self.settings_picked(field, pick),
                Input::SettingsClosed => self.close_settings(),
                Input::SettingsPress(press) => self.settings_press(press),
                Input::Answered {
                    dir,
                    title,
                    assumed: None,
                    text,
                } => {
                    self.human_tell(&dir, &title, &text);
                    self.run_tasks();
                }
                Input::Answered {
                    dir,
                    title,
                    assumed: Some(assumed),
                    text,
                } => {
                    self.overrule(&dir, &title, &assumed, &text);
                    self.run_tasks();
                }
                Input::AwayClosed => {
                    if let Some(c) = self.away_card.take() {
                        c.destroy();
                    }
                }
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
                Input::GiveQuests(key) => self.give_quests(&key),
                Input::QuestLog(ask) => self.quest_log_asks(ask),
                Input::Stone(key, None) => self.start_runesmith(&key, None),
                Input::Stone(key, Some(label)) => {
                    self.stone_for = Some((key, label));
                    post(self.notify.0 as isize, WM_HORADRIC_STONE, 0);
                }
                Input::StoneDrop(key, label, at) => self.stone_dropped(&key, &label, at),
                Input::StoneMove(key, from, to) => self.move_stone(&key, from, to),
                Input::StoneMenu(key, label) => {
                    self.stone_menu_for = Some((key, label));
                    post(self.notify.0 as isize, WM_HORADRIC_STONE_MENU, 0);
                }
            }
        }
        if relayout {
            self.arrange();
        }
    }

    /// Shows the start window while no project is open, and only then.
    /// The town is always open, so it does not count.
    fn sync_start(&mut self) {
        if self.clusters.iter().any(|c| !town::is(&c.key)) {
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
        if town::is(key) {
            return town::dir();
        }
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
        from_session
            .map(PathBuf::from)
            .or_else(|| {
                self.recent
                    .iter()
                    .find(|p| folder_key(p) == key)
                    .map(PathBuf::from)
            })
            // A project kept open after it left the recent list: the key is
            // its folder, spelled in lower case.
            .or_else(|| Some(PathBuf::from(key)).filter(|d| !key.is_empty() && d.is_dir()))
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
    /// the town and the usage window in the first column.
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
                .rposition(|t| match t {
                    Tile::Usage(_) => true,
                    Tile::Cluster(c) => town::is(&c.key),
                    _ => false,
                })
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
        if let Some(k) = town::key().filter(|k| is_present(k) && !self.columns.contains(k)) {
            self.columns.add_first(k);
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
                    let items: Vec<columns::Stacked> = self
                        .column_windows(keys, i == 0)
                        .iter()
                        .map(Tile::stacked)
                        .collect();
                    columns::room(&items, g.height, g.gap, g.min_files)
                })
                .collect();
            let col = columns::place_new(&rooms, need, shown.len() < g.fits);
            self.columns.add(&key, col, is_present);
        }

        let shown = self.columns.shown(g.fits, is_present);
        let bottom = g.top + g.height;
        let mut bounds = HashMap::new();
        let mut scrolls = Vec::new();
        let mut seen = HashSet::new();
        for (i, (model, keys)) in shown.iter().enumerate() {
            let x = g.x(i);
            let tiles = self.column_windows(keys, i == 0);
            let items: Vec<columns::Stacked> = tiles.iter().map(Tile::stacked).collect();
            // A locked usage window at the top of its column stays there
            // while the rest scrolls below it. Lower down it does not pin,
            // or the tiles above it could fill the column and leave the
            // ones below no room to scroll into.
            let pinned =
                usize::from(matches!(tiles.first(), Some(Tile::Usage(u)) if u.locked.get()));
            let scroll = self.columns.cols[*model].scroll;
            let (filled, room, below) =
                columns::fill_pinned(&items, g.top, g.height, g.gap, g.min_files, scroll, pinned);
            scrolls.push((*model, scroll.clamp(0, room)));
            for (n, (t, f)) in tiles.iter().zip(filled).enumerate() {
                let id = t.hwnd().0 as isize;
                seen.insert(id);
                bounds.insert(id, (if n < pinned { g.top } else { below }, bottom));
                // The one being dragged is where the cursor holds it, whole.
                if t.key().is_some() && t.key() == self.carried.as_deref() {
                    clip_window(&mut self.clipped.borrow_mut(), t.hwnd(), None);
                    continue;
                }
                t.place(x, f, &mut self.glides.borrow_mut(), animate, self.arranged);
                clip_window(
                    &mut self.clipped.borrow_mut(),
                    t.hwnd(),
                    bounds.get(&id).copied(),
                );
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
        self.column_bounds = bounds;
        // A handle Windows gives a new window must not inherit a cut.
        self.clipped.borrow_mut().retain(|id, _| seen.contains(id));
        crate::render::retain_cuts(|id| seen.contains(&id));
        // Mid drag the columns are not settled yet, so the stage waits for
        // the drop.
        if self.carried.is_none() {
            let edge = g.x(shown.len().max(1) - 1) + g.width + g.gap;
            let was = self.tiles_edge.replace(edge);
            self.stage_follows(was);
        }
        // The windows there at the start stand there at once; the ones
        // that come after arrive.
        self.arranged = !self.clusters.is_empty() || self.start_window.is_some();
        if self.glides.borrow().moving() && self.glided.get().is_none() {
            self.glided.set(Some(Instant::now()));
            crate::vsync::start(self.notify, GLIDE_TIMER);
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
                clip_window(
                    &mut self.clipped.borrow_mut(),
                    HWND(id as *mut c_void),
                    self.column_bounds.get(&id).copied(),
                );
            }
            if !glides.moving() {
                self.glided.set(None);
                crate::vsync::stop(self.notify, GLIDE_TIMER);
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

/// The cut of each window in the columns by its handle, with the width it
/// was made for.
type Clipped = HashMap<isize, (i32, Option<(i32, i32)>)>;

/// Cuts a window in the columns to `bounds`, the column's top and bottom
/// on screen, or shows all of it when None. Clicks fall through the cut
/// part as well, to whatever is below.
fn clip_window(clipped: &mut Clipped, hwnd: HWND, bounds: Option<(i32, i32)>) {
    let mut r = RECT::default();
    if unsafe { GetWindowRect(hwnd, &mut r) }.is_err() {
        return;
    }
    let w = r.right - r.left;
    let cut = bounds.and_then(|(top, bottom)| columns::clip(top, bottom, r.top, r.bottom - r.top));
    let id = hwnd.0 as isize;
    let was = clipped.get(&id).copied();
    if was == Some((w, cut)) || (was.is_none() && cut.is_none()) {
        return;
    }
    clipped.insert(id, (w, cut));
    crate::render::set_cut(hwnd, cut);
    unsafe {
        // The window owns the region once it is set.
        let region = cut.map(|(from, to)| CreateRectRgn(0, from, w, to));
        SetWindowRgn(hwnd, region, true);
    }
}

/// A window's top left corner on screen.
/// Whether the window at this point on the screen is `hwnd` or one in
/// it, so a drop lands on what is on top there and not on a window under
/// it.
pub(crate) fn window_under(at: POINT, hwnd: HWND) -> bool {
    unsafe {
        let w = WindowFromPoint(at);
        !w.is_invalid() && (w == hwnd || IsChild(hwnd, w).as_bool())
    }
}

fn window_at(hwnd: HWND) -> (i32, i32) {
    let mut r = RECT::default();
    unsafe {
        let _ = GetWindowRect(hwnd, &mut r);
    }
    (r.left, r.top)
}

pub(crate) fn unix_now() -> u64 {
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

/// What the mod is told: `quest` when there is one, and each file shared
/// with sessions still in Horadric, by its name alone since the band is
/// narrow, and `stop` with the subagents the human asked to stop. Empty when
/// there is nothing to say.
fn known(
    quest: Option<&str>,
    shared: &[(String, Vec<String>)],
    stop: &[String],
) -> serde_json::Value {
    let mut out = serde_json::Map::new();
    if let Some(q) = quest {
        out.insert("quest".into(), q.into());
    }
    if !stop.is_empty() {
        out.insert("stop".into(), stop.into());
    }
    let files: Vec<serde_json::Value> = shared
        .iter()
        .filter(|(_, others)| !others.is_empty())
        .map(|(file, others)| {
            let name = file.rsplit(['/', '\\']).next().unwrap_or(file);
            serde_json::json!({ "file": name, "path": file, "others": others })
        })
        .collect();
    if !files.is_empty() {
        out.insert("overlaps".into(), files.into());
    }
    out.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cast_out_names_the_line_that_goes_with_it() {
        let alone = cast_out_text("Laptop", &[], false);
        assert!(alone.starts_with("Laptop will be refused from now on"));
        assert!(!alone.contains("came in"));
        let line = [
            ("g".to_string(), "Ghost".to_string()),
            ("v".to_string(), "vps".to_string()),
        ];
        let text = cast_out_text("Laptop", &line, false);
        assert!(text.contains("Ghost, vps came in only through Laptop, so they go with it"));
        assert!(!text.contains("This PC came in"));
        let mine = cast_out_text("Laptop", &[], true);
        assert!(mine.contains("This PC came in through Laptop"));
    }

    #[test]
    fn the_mod_is_told_the_quest_and_files_shared_with_live_sessions() {
        assert_eq!(known(None, &[], &[]), serde_json::json!({}));
        assert_eq!(
            known(None, &[], &["a1".to_string()]),
            serde_json::json!({ "stop": ["a1"] })
        );
        let shared = [
            (
                r"C:\app\src\overlap.rs".to_string(),
                vec!["fix-a".to_string()],
            ),
            ("C:/app/gone.rs".to_string(), Vec::new()),
        ];
        assert_eq!(
            known(Some("Serve the API"), &shared, &[]),
            serde_json::json!({
                "quest": "Serve the API",
                "overlaps": [{
                    "file": "overlap.rs",
                    "path": r"C:\app\src\overlap.rs",
                    "others": ["fix-a"],
                }],
            })
        );
    }

    #[test]
    fn the_subagents_menu_stops_each_by_its_place() {
        let agents = [
            ("a1".to_string(), "Explore: map (5s)".to_string()),
            ("a2".to_string(), "Plan".to_string()),
        ];
        let Item::Submenu(title, items) = subagents_menu(&agents) else {
            panic!("not a submenu");
        };
        assert_eq!(title, "Subagents (2)");
        let picks: Vec<(usize, String)> = items
            .into_iter()
            .filter_map(|i| match i {
                Item::Action { id, label, .. } => Some((id, label)),
                _ => None,
            })
            .collect();
        assert_eq!(
            picks,
            vec![
                (STOP_SUBAGENT, "Stop Explore: map (5s)".to_string()),
                (STOP_SUBAGENT + 1, "Stop Plan".to_string()),
            ]
        );
    }

    #[test]
    fn a_default_list_puts_default_first_and_picks_back_its_value() {
        let (labels, at) = default_choices(Agent::Claude, Setting::Effort, Some("xhigh"));
        assert_eq!(labels[0], "Default");
        assert_eq!(labels[at], "Extra high");
        assert_eq!(
            default_picked(Agent::Claude, Setting::Effort, at),
            Some(Some("xhigh".to_string()))
        );
        assert_eq!(default_choices(Agent::Claude, Setting::Model, None).1, 0);
        assert_eq!(
            default_choices(Agent::Claude, Setting::Model, Some("opus")).1,
            0
        );
        assert_eq!(default_picked(Agent::Codex, Setting::Model, 0), Some(None));
        assert_eq!(default_picked(Agent::Grok, Setting::Model, 9), None);
    }

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
