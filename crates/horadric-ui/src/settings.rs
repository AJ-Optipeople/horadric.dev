//! The Settings window: every setting of the app in one place, sections
//! down the left and the rows of the one picked on the right.
//!
//! A window of its own that closes, not a tile in the columns: settings
//! are visited, not watched. Drawn like the usage window, its lists drop
//! the same list. There is no Save button. A row changes the setting the
//! moment it is clicked or picked, as the tray's lines used to, so the
//! window and the app never disagree. The app owns the values and hands
//! them in again after every change.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::rc::Rc;

use crate::backup::config::To;
use horadric_core::saved::Discord;
use horadric_core::{Agent, Defaults, Setting};
use windows::core::{w, Result, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
    DWM_WINDOW_CORNER_PREFERENCE,
};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, InvalidateRect, MonitorFromPoint, ScreenToClient, ValidateRect, MONITORINFO,
    MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, GetDpiForWindow, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT,
    VIRTUAL_KEY, VK_CONTROL, VK_DOWN, VK_ESCAPE, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, CreateWindowExW, DefWindowProcW, DestroyWindow, GetCursorPos,
    GetForegroundWindow, GetWindowLongPtrW, GetWindowRect, IsIconic, LoadCursorW, RegisterClassW,
    SendMessageW, SetForegroundWindow, SetWindowLongPtrW, SetWindowPos, SetWindowsHookExW,
    ShowWindow, UnhookWindowsHookEx, CREATESTRUCTW, CS_HREDRAW, CS_VREDRAW, GWLP_USERDATA, HHOOK,
    HICON, HTCAPTION, HTCLIENT, ICON_BIG, ICON_SMALL, IDC_ARROW, KBDLLHOOKSTRUCT, SWP_NOACTIVATE,
    SWP_NOZORDER, SW_RESTORE, SW_SHOW, WH_KEYBOARD_LL, WM_CAPTURECHANGED, WM_CLOSE, WM_DPICHANGED,
    WM_ERASEBKGND, WM_KEYDOWN, WM_KILLFOCUS, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE,
    WM_NCCREATE, WM_NCDESTROY, WM_NCHITTEST, WM_PAINT, WM_SETICON, WM_SIZE, WM_SYSCHAR,
    WM_SYSKEYDOWN, WM_SYSKEYUP, WNDCLASSW, WS_EX_APPWINDOW, WS_MINIMIZEBOX, WS_POPUP, WS_SYSMENU,
};

use crate::app::{self, Input};
use crate::backdrop;
use crate::hotkey::{self, Action, Mods, Press};
use crate::layout::{self, SettingsHit, SettingsLayout};
use crate::render::{SettingsLook, SettingsScene, Target};
use crate::theme::Theme;
use crate::window::Shared;

pub(crate) const CLASS: PCWSTR = w!("HoradricSettings");
/// The windows crate files this under `Win32_UI_Controls`.
const WM_MOUSELEAVE: u32 = 0x02A3;

/// The window's sections, in the order they stand down the left.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Section {
    #[default]
    Appearance,
    Notifications,
    Sessions,
    Keys,
    Startup,
    Privacy,
    Backup,
    Scroll,
    Runetome,
    Projects,
}

impl Section {
    pub const ALL: [Section; 10] = [
        Section::Appearance,
        Section::Notifications,
        Section::Sessions,
        Section::Keys,
        Section::Startup,
        Section::Privacy,
        Section::Backup,
        Section::Scroll,
        Section::Runetome,
        Section::Projects,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Section::Appearance => "Appearance",
            Section::Notifications => "Notifications",
            Section::Sessions => "Sessions",
            Section::Keys => "Keys",
            Section::Startup => "Startup and updates",
            Section::Privacy => "Privacy",
            Section::Backup => "Backup",
            Section::Scroll => "The Scroll",
            Section::Runetome => "Runetome",
            Section::Projects => "Projects",
        }
    }
}

/// One setting the window shows, which is what a click on its row or a
/// pick from its list changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Theme,
    Font,
    Screen,
    /// Whether Tristram's tile shows.
    Town,
    Notify,
    Sounds,
    /// Whose session defaults the rows under it show.
    Agent,
    /// A default for the sessions Horadric starts, the same value the
    /// usage window's rows show.
    Default(Setting),
    /// Whether a usage limit switches to the account with the most left.
    SwitchAccounts,
    /// Whether a project's menu offers to start the agent.
    Offered,
    Autostart,
    /// "Open in Horadric" on folders in Explorer.
    Explorer,
    /// Look for a newer release now, or install the one found.
    Updates,
    Version,
    Discord,
    /// Whether recall looks in other projects' memory too.
    Recall,
    /// Where the memory is backed up to, from Off to a folder.
    BackupTo,
    /// Signs in or out of the Google account the backup uses.
    BackupAccount,
    /// The folder a backup to a folder goes in.
    BackupFolder,
    BackupPassphrase,
    /// What the Backup section has to say, as a line across the row.
    BackupNote,
    BackupLast,
    BackupNow,
    BackupRestore,
    /// Whether recall's answers are written in the Horadrim's tongue.
    Horadrim,
    /// Whether a click on a stone asks before it casts.
    Ask,
    /// Brings back every built in stone put away.
    Hidden,
    /// Which project the rows below it are about.
    Project,
    Colour,
    WorkMode,
    Hosts,
    AddHost,
    Drive,
    ShipsPublic,
    /// A global shortcut: a click listens for its new chord.
    Key(Action),
    /// What the Keys section has to say: how to set one, or why the last
    /// try did not take.
    KeysNote,
}

impl Field {
    /// A row of the Backup section that asks something before it acts.
    pub fn asks_backup(self) -> bool {
        matches!(
            self,
            Field::BackupFolder | Field::BackupPassphrase | Field::BackupRestore
        )
    }
}

/// What a row does when clicked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Control {
    /// Drops a list to pick from.
    List,
    /// Turns on or off, and shows which it is.
    Switch(bool),
    /// Does something once.
    Button,
    /// Only says, a click does nothing.
    Fixed,
    /// A line of help across the row, no label.
    Note,
}

/// A row as the window shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub field: Field,
    pub label: &'static str,
    pub value: String,
    pub control: Control,
}

/// The settings as the app has them now, which is all the window shows.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Values {
    pub theme: Theme,
    pub font: String,
    /// The screens' names in the order offered, and the one the tiles
    /// stand on.
    pub screens: Vec<String>,
    pub screen: Option<usize>,
    pub notify: bool,
    pub sounds: bool,
    /// Tristram's tile shows.
    pub town: bool,
    /// The agent the Sessions section shows, and its defaults.
    pub agent: Agent,
    pub defaults: Defaults,
    /// Whether its project menu line is on. None where it can not be
    /// turned off, Claude Code, and Some(None) where it is not installed.
    pub offered: Option<Option<bool>>,
    /// Claude Code switches account by itself when one runs out.
    pub switch_accounts: bool,
    /// None where the switch is not offered, a dev instance.
    pub autostart: Option<bool>,
    /// The same for "Open in Horadric" in Explorer.
    pub explorer: Option<bool>,
    pub discord: Discord,
    pub recall_across: bool,
    pub backup: Backup,
    pub scroll: Scroll,
    /// A newer release a check found.
    pub update: Option<String>,
    pub checking: bool,
    pub version: String,
    pub ask: bool,
    /// How many built in stones are put away.
    pub hidden: usize,
    /// The global shortcuts in [`Action::ALL`]'s order.
    pub keys: [Key; hotkey::COUNT],
    /// The shortcut whose new chord the window waits for.
    pub listening: Option<Action>,
    /// Why the last chord pressed did not take, until the next try.
    pub keys_note: Option<String>,
    /// Every project's name, in the order offered.
    pub projects: Vec<String>,
    /// The one picked, None when there are none.
    pub project: Option<Project>,
}

/// What the Backup section shows of the memory's backup.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Backup {
    pub to: To,
    /// The signed in Google account's email.
    pub account: Option<String>,
    pub folder: Option<String>,
    /// A passphrase is kept in Credential Manager.
    pub passphrase: bool,
    /// How the last backup went, worded: when, how much, or what failed.
    pub last: String,
    pub failed: bool,
    /// What is being done now, which holds the buttons back.
    pub busy: Option<String>,
}

/// What the Scroll section reads of the tongue.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Scroll {
    /// The tongue is turned off.
    pub off: bool,
    /// The project whose Lexicon this is, "" when none is picked.
    pub project: String,
    /// Sigils in use, and the most used few: sigil, name, uses.
    pub sigils: usize,
    pub top: Vec<(String, String, u64)>,
    /// Tokens saved this week in this project.
    pub saved: i64,
}

/// How the yardstick scored recall's answers (`yardstick/judge.py`): right out of 65 questions, in the tongue and
/// verbatim.
const SCORE: &str = "59 of 65, verbatim 52";

/// `n` with a comma between thousands.
fn thousands(n: i64) -> String {
    let digits = n.unsigned_abs().to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    if n < 0 {
        out.insert(0, '-');
    }
    out
}

/// A sigil of the Lexicon in a note's room.
fn sigil_line(sigil: &str, name: &str, uses: u64) -> String {
    let name: String = if name.chars().count() > 34 {
        let head: String = name.chars().take(33).collect();
        format!("{head}\u{2026}")
    } else {
        name.to_string()
    };
    format!("{sigil}  {name}, used {uses}")
}

/// The settings of the project the Projects section shows.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Project {
    /// Its place in [`Values::projects`].
    pub index: usize,
    pub colour: String,
    /// Whether each session gets a worktree of its own, None outside a
    /// repository, where there is no choice.
    pub branches: Option<bool>,
    pub hosts: Vec<String>,
    /// Whether it has a quest log, without which Warriv has nothing to
    /// drive.
    pub log: bool,
    /// Whether Warriv drives it, and if so whether it ships public.
    pub drive: Option<bool>,
    /// Shipping held until a fix lands.
    pub held: bool,
}

/// The rows the Projects section has with a project picked, which the
/// window is sized for even before there is one.
const PROJECT_ROWS: usize = 7;

/// The project's hosts in a row's room: a few by name, the rest counted.
fn hosts_line(hosts: &[String]) -> String {
    match hosts {
        [] => "None".into(),
        [a] => a.clone(),
        [a, b] => format!("{a}, {b}"),
        [a, b, rest @ ..] => format!("{a}, {b} and {} more", rest.len()),
    }
}

/// A global shortcut as the app has it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Key {
    /// Its chord's name, "Ctrl+Alt+Space".
    pub chord: String,
    /// Registered. False when another program holds the chord.
    pub live: bool,
}

/// The Keys section's note, given the shortcuts in `v`.
fn keys_note(v: &Values) -> String {
    if let Some(note) = &v.keys_note {
        return note.clone();
    }
    if v.listening.is_some() {
        return "Esc keeps it, Backspace puts back the default".to_string();
    }
    match v.keys.iter().find(|k| !k.live) {
        Some(k) => format!("{} is taken by another program", k.chord),
        None => "Click a shortcut, then press its new keys".to_string(),
    }
}

/// The rows of `section`, given the settings in `v`.
pub fn lines(section: Section, v: &Values) -> Vec<Line> {
    let line = |field, label, value: &str, control| Line {
        field,
        label,
        value: value.to_string(),
        control,
    };
    let switch = |on: bool| (if on { "On" } else { "Off" }, Control::Switch(on));
    match section {
        Section::Appearance => {
            let screen = v.screen.and_then(|i| v.screens.get(i));
            let (name, control) = match (screen, v.screens.len()) {
                (Some(s), n) if n > 1 => (s.as_str(), Control::List),
                (Some(s), _) => (s.as_str(), Control::Fixed),
                (None, _) => ("Primary", Control::Fixed),
            };
            let (town, t) = switch(v.town);
            vec![
                line(Field::Theme, "Theme", v.theme.label(), Control::List),
                line(Field::Font, "Terminal font", &v.font, Control::List),
                line(Field::Screen, "Tiles on screen", name, control),
                line(Field::Town, "Show Tristram", town, t),
            ]
        }
        Section::Notifications => {
            let (notify, n) = switch(v.notify);
            let (sounds, s) = switch(v.sounds);
            vec![
                line(Field::Notify, "Notify when a session waits", notify, n),
                line(Field::Sounds, "Loot sounds", sounds, s),
            ]
        }
        Section::Sessions => {
            let mut rows = vec![line(Field::Agent, "Agent", v.agent.label(), Control::List)];
            // Workflows are Claude Code's alone.
            let workflow = (v.agent == Agent::Claude).then_some(&Setting::Workflow);
            rows.extend(v.agent.settings().iter().chain(workflow).map(|s| {
                let value = s.name_of(v.agent, v.defaults.get(*s));
                line(Field::Default(*s), s.label(), value, Control::List)
            }));
            let offered = match v.offered {
                None => line(
                    Field::Offered,
                    "In the project menu",
                    "Always",
                    Control::Fixed,
                ),
                Some(None) => line(
                    Field::Offered,
                    "In the project menu",
                    "Not installed",
                    Control::Fixed,
                ),
                Some(Some(on)) => {
                    let (word, c) = switch(on);
                    line(Field::Offered, "In the project menu", word, c)
                }
            };
            rows.push(offered);
            // The limit that stops a turn is Claude Code's to report.
            if v.agent == Agent::Claude {
                let (word, c) = switch(v.switch_accounts);
                rows.push(line(
                    Field::SwitchAccounts,
                    "Switch account when one runs out",
                    word,
                    c,
                ));
            }
            rows
        }
        Section::Startup => {
            let autostart = match v.autostart {
                Some(on) => {
                    let (word, c) = switch(on);
                    line(Field::Autostart, "Start with Windows", word, c)
                }
                None => line(
                    Field::Autostart,
                    "Start with Windows",
                    "Not in a dev instance",
                    Control::Fixed,
                ),
            };
            let explorer = match v.explorer {
                Some(on) => {
                    let (word, c) = switch(on);
                    line(Field::Explorer, "Open in Horadric in Explorer", word, c)
                }
                None => line(
                    Field::Explorer,
                    "Open in Horadric in Explorer",
                    "Not in a dev instance",
                    Control::Fixed,
                ),
            };
            let updates = match (&v.update, v.checking) {
                (Some(version), _) => format!("Update to {version}"),
                (None, true) => "Checking\u{2026}".to_string(),
                (None, false) => "Check now".to_string(),
            };
            let control = if v.checking && v.update.is_none() {
                Control::Fixed
            } else {
                Control::Button
            };
            vec![
                autostart,
                explorer,
                line(Field::Updates, "Updates", &updates, control),
                line(Field::Version, "Version", &v.version, Control::Fixed),
            ]
        }
        Section::Keys => {
            let mut rows: Vec<Line> = Action::ALL
                .iter()
                .map(|a| {
                    let k = &v.keys[a.index()];
                    let value = if v.listening == Some(*a) {
                        "Press the keys\u{2026}".to_string()
                    } else if k.live {
                        k.chord.clone()
                    } else {
                        format!("{}, taken", k.chord)
                    };
                    line(Field::Key(*a), a.label(), &value, Control::Button)
                })
                .collect();
            rows.push(line(Field::KeysNote, "", &keys_note(v), Control::Note));
            rows
        }
        Section::Privacy => {
            let (across, a) = switch(v.recall_across);
            vec![
                line(
                    Field::Discord,
                    "Show on Discord",
                    v.discord.label(),
                    Control::List,
                ),
                line(Field::Recall, "Recall across projects", across, a),
            ]
        }
        Section::Backup => backup_lines(&v.backup),
        Section::Scroll => {
            let sc = &v.scroll;
            let (word, c) = switch(!sc.off);
            let lexicon = match (sc.project.as_str(), sc.sigils) {
                ("", _) => "No project".to_string(),
                (p, 0) => format!("{p}, no sigils yet"),
                (p, n) => format!("{p}, {n} sigils"),
            };
            let mut rows = vec![
                line(
                    Field::KeysNote,
                    "",
                    "Recall's hits are inscribed: long names bound to sigils.",
                    Control::Note,
                ),
                line(Field::Project, "Lexicon", &lexicon, Control::Fixed),
            ];
            rows.extend(sc.top.iter().take(2).map(|(sigil, name, uses)| {
                line(
                    Field::KeysNote,
                    "",
                    &sigil_line(sigil, name, *uses),
                    Control::Note,
                )
            }));
            rows.push(line(
                Field::Version,
                "Saved this week",
                &format!("{} tokens", thousands(sc.saved)),
                Control::Fixed,
            ));
            rows.push(line(Field::Version, "Answers right", SCORE, Control::Fixed));
            rows.push(line(Field::Horadrim, "The tongue", word, c));
            rows
        }
        Section::Runetome => {
            let (ask, a) = switch(v.ask);
            let hidden = match v.hidden {
                0 => line(
                    Field::Hidden,
                    "Built in stones put away",
                    "None",
                    Control::Fixed,
                ),
                n => line(
                    Field::Hidden,
                    "Built in stones put away",
                    &format!("{n}, bring back"),
                    Control::Button,
                ),
            };
            vec![line(Field::Ask, "Ask before a click casts", ask, a), hidden]
        }
        Section::Projects => {
            let Some(p) = &v.project else {
                return vec![line(
                    Field::Project,
                    "Project",
                    "None opened yet",
                    Control::Fixed,
                )];
            };
            let name = v.projects.get(p.index).map_or("", String::as_str);
            let mode = match p.branches {
                Some(on) => {
                    let word = if on { "Branch per session" } else { "Trunk" };
                    line(Field::WorkMode, "Work mode", word, Control::List)
                }
                None => line(
                    Field::WorkMode,
                    "Work mode",
                    "Not a git repository",
                    Control::Fixed,
                ),
            };
            let drive = match (p.drive, p.log) {
                (Some(_), _) if p.held => line(
                    Field::Drive,
                    "Warriv drives",
                    "On, shipping held",
                    Control::Switch(true),
                ),
                (None, false) => line(
                    Field::Drive,
                    "Warriv drives",
                    "Needs a quest log",
                    Control::Fixed,
                ),
                (on, _) => {
                    let (word, c) = switch(on.is_some());
                    line(Field::Drive, "Warriv drives", word, c)
                }
            };
            let public = match p.drive {
                Some(on) => {
                    let (word, c) = switch(on);
                    line(Field::ShipsPublic, "And ships public", word, c)
                }
                None => line(
                    Field::ShipsPublic,
                    "And ships public",
                    "Only while Warriv drives",
                    Control::Fixed,
                ),
            };
            vec![
                line(Field::Project, "Project", name, Control::List),
                line(Field::Colour, "Colour", &p.colour, Control::List),
                mode,
                line(
                    Field::Hosts,
                    "SSH hosts",
                    &hosts_line(&p.hosts),
                    Control::Fixed,
                ),
                line(
                    Field::AddHost,
                    "Add an SSH host",
                    "Add\u{2026}",
                    Control::Button,
                ),
                drive,
                public,
            ]
        }
    }
}

/// The Backup section's rows. The ones about a target show only when
/// there is one, and the buttons rest while a job runs.
fn backup_lines(b: &Backup) -> Vec<Line> {
    let line = |field, label, value: &str, control| Line {
        field,
        label,
        value: value.to_string(),
        control,
    };
    let mut rows = vec![line(
        Field::BackupTo,
        "Back up to",
        b.to.label(),
        Control::List,
    )];
    if b.to == To::Off {
        rows.push(line(
            Field::BackupNote,
            "",
            "Encrypted copies of the memory and the lore notes, kept off this PC.",
            Control::Note,
        ));
        return rows;
    }
    let idle = if b.busy.is_some() {
        Control::Fixed
    } else {
        Control::Button
    };
    match b.to {
        To::Drive => {
            let account = match (&b.busy, &b.account) {
                (Some(what), _) => what.clone(),
                (None, Some(email)) => format!("{email}, sign out"),
                (None, None) => "Sign in with Google".to_string(),
            };
            rows.push(line(Field::BackupAccount, "Google account", &account, idle));
        }
        _ => {
            let folder = b.folder.as_deref().unwrap_or("Pick a folder\u{2026}");
            rows.push(line(Field::BackupFolder, "Folder", folder, idle));
        }
    }
    let passphrase = if b.passphrase {
        "Kept in Windows, change"
    } else {
        "Not set, choose one"
    };
    rows.push(line(
        Field::BackupPassphrase,
        "Passphrase",
        passphrase,
        idle,
    ));
    rows.push(line(
        Field::BackupNote,
        "",
        "Without the passphrase the backup cannot be read.",
        Control::Note,
    ));
    let last = if b.last.is_empty() { "Never" } else { &b.last };
    rows.push(line(Field::BackupLast, "Last backup", last, Control::Fixed));
    let now = b.busy.as_deref().unwrap_or("Start");
    rows.push(line(Field::BackupNow, "Back up now", now, idle));
    rows.push(line(
        Field::BackupRestore,
        "Restore",
        "Restore\u{2026}",
        idle,
    ));
    rows
}

/// The most rows any section has, which sets the window's height. The
/// Sessions section counts with whichever agent has the most, so picking
/// another does not leave the window a size its rows no longer fill.
fn tallest(v: &Values) -> usize {
    let sessions = Agent::ALL.iter().map(|a| {
        let v = Values {
            agent: *a,
            ..v.clone()
        };
        lines(Section::Sessions, &v).len()
    });
    Section::ALL
        .iter()
        .map(|s| lines(*s, v).len())
        .chain(sessions)
        .max()
        .unwrap_or(1)
        .max(PROJECT_ROWS)
}

/// The section `step` places after `at`, round at either end, for the
/// arrow keys.
fn step(at: Section, step: isize) -> Section {
    let n = Section::ALL.len() as isize;
    let i = Section::ALL.iter().position(|s| *s == at).unwrap_or(0) as isize;
    Section::ALL[(i + step).rem_euclid(n) as usize]
}

thread_local! {
    /// The keyboard hook while the window listens for a chord, and the
    /// window it listens for.
    static HOOK: Cell<Option<(HHOOK, HWND)>> = const { Cell::new(None) };
}

/// Hooks the keyboard while `hwnd` listens for a chord, and unhooks it
/// after. A hook, not the window's own key messages: a chord another
/// program registered never reaches the window, and that is exactly the
/// one to say is taken.
fn hook(hwnd: HWND, on: bool) {
    match (HOOK.with(Cell::get), on) {
        (None, true) => {
            let made = unsafe {
                SetWindowsHookExW(
                    WH_KEYBOARD_LL,
                    Some(keyboard),
                    GetModuleHandleW(None).ok().map(Into::into),
                    0,
                )
            };
            match made {
                Ok(h) => HOOK.with(|c| c.set(Some((h, hwnd)))),
                Err(e) => eprintln!("horadric: no keyboard hook to set a shortcut: {e}"),
            }
        }
        (Some((h, _)), false) => {
            unsafe {
                let _ = UnhookWindowsHookEx(h);
            }
            HOOK.with(|c| c.set(None));
        }
        _ => {}
    }
}

/// Takes a key pressed while the window listens and has the focus, before
/// any hotkey can. Modifiers go on, so the keys held stay known.
unsafe extern "system" fn keyboard(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let down = matches!(wparam.0 as u32, WM_KEYDOWN | WM_SYSKEYDOWN);
    let ours = HOOK
        .with(Cell::get)
        .is_some_and(|(_, hwnd)| GetForegroundWindow() == hwnd);
    if code >= 0 && down && ours {
        let key = (*(lparam.0 as *const KBDLLHOOKSTRUCT)).vkCode as u16;
        match hotkey::press(key, held()) {
            Press::Wait => {}
            p => {
                app::push(Input::SettingsPress(p));
                return LRESULT(1);
            }
        }
    }
    CallNextHookEx(None, code, wparam, lparam)
}

/// The modifiers held now. Asked of the keyboard, not the message queue:
/// a hook runs before the queue knows of the key.
fn held() -> Mods {
    let down = |vk: VIRTUAL_KEY| unsafe { GetAsyncKeyState(i32::from(vk.0)) } < 0;
    Mods {
        ctrl: down(VK_CONTROL),
        alt: down(VK_MENU),
        shift: down(VK_SHIFT),
        win: down(VK_LWIN) || down(VK_RWIN),
    }
}

/// Brings the window back to the front, for a second "Settings..." while
/// it is open. Given its handle rather than the window, so the app need
/// not be borrowed while it takes the focus.
pub fn bring_back(hwnd: HWND) {
    unsafe {
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
        let _ = SetForegroundWindow(hwnd);
    }
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

pub struct SettingsWindow {
    pub hwnd: HWND,
    shared: Rc<Shared>,
    target: RefCell<Option<Target>>,
    layout: RefCell<SettingsLayout>,
    values: RefCell<Values>,
    section: Cell<Section>,
    hot: Cell<SettingsHit>,
    pressed: Cell<Option<SettingsHit>>,
    tracking: Cell<bool>,
    /// The row whose list is dropped down, set by the app.
    open: Cell<Option<Field>>,
}

impl SettingsWindow {
    /// Opens the window in the middle of the screen the cursor is on,
    /// showing `section` of the settings in `values`, and gives it the
    /// keyboard.
    pub fn create(
        shared: Rc<Shared>,
        values: Values,
        section: Section,
        icon: HICON,
    ) -> Result<Box<Self>> {
        let l = layout::settings(
            &shared.metrics,
            Section::ALL.len(),
            lines(section, &values).len(),
            tallest(&values),
        );
        let mut cursor = POINT::default();
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        let (mut dpi, mut dy) = (96u32, 96u32);
        unsafe {
            let _ = GetCursorPos(&mut cursor);
            let monitor = MonitorFromPoint(cursor, MONITOR_DEFAULTTONEAREST);
            let _ = GetMonitorInfoW(monitor, &mut info);
            let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi, &mut dy);
        }
        // Born on the right screen and at its size: a window that moves
        // after it is made has been seen not to paint.
        let work = info.rcWork;
        let s = dpi.max(96) as f32 / 96.0;
        let (w, h) = ((l.size.0 * s).round() as i32, (l.size.1 * s).round() as i32);
        let x = work.left + (work.right - work.left - w) / 2;
        let y = work.top + (work.bottom - work.top - h) / 2;
        let mut win = Box::new(SettingsWindow {
            hwnd: HWND::default(),
            shared,
            target: RefCell::new(None),
            layout: RefCell::new(l),
            values: RefCell::new(values),
            section: Cell::new(section),
            hot: Cell::new(SettingsHit::Nothing),
            pressed: Cell::new(None),
            tracking: Cell::new(false),
            open: Cell::new(None),
        });
        unsafe {
            let hwnd = CreateWindowExW(
                WS_EX_APPWINDOW,
                CLASS,
                w!("Horadric settings"),
                WS_POPUP | WS_SYSMENU | WS_MINIMIZEBOX,
                x,
                y,
                w,
                h,
                None,
                None,
                Some(GetModuleHandleW(None)?.into()),
                Some(&*win as *const SettingsWindow as *const c_void),
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
            for size in [ICON_BIG, ICON_SMALL] {
                SendMessageW(
                    hwnd,
                    WM_SETICON,
                    Some(WPARAM(size as usize)),
                    Some(LPARAM(icon.0 as isize)),
                );
            }
            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = SetForegroundWindow(hwnd);
        }
        Ok(win)
    }

    pub fn destroy(&self) {
        hook(self.hwnd, false);
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }

    /// Lights the row whose list is dropped down, None once it closes.
    pub fn set_open(&self, field: Option<Field>) {
        if self.open.replace(field) != field {
            self.invalidate();
        }
    }

    /// Takes the settings as they are now and draws them.
    pub fn set_values(&self, values: Values) {
        if *self.values.borrow() == values {
            return;
        }
        hook(self.hwnd, values.listening.is_some());
        *self.values.borrow_mut() = values;
        self.relayout();
    }

    pub fn dpi(&self) -> u32 {
        unsafe { GetDpiForWindow(self.hwnd) }.max(96)
    }

    fn scale(&self) -> f32 {
        self.dpi() as f32 / 96.0
    }

    fn invalidate(&self) {
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    fn lines(&self) -> Vec<Line> {
        lines(self.section.get(), &self.values.borrow())
    }

    /// Lays out again for the section shown. The window keeps its size,
    /// which is the tallest section's.
    fn relayout(&self) {
        let v = self.values.borrow();
        let l = layout::settings(
            &self.shared.metrics,
            Section::ALL.len(),
            lines(self.section.get(), &v).len(),
            tallest(&v),
        );
        drop(v);
        *self.layout.borrow_mut() = l;
        self.refresh_hover();
        self.invalidate();
    }

    /// Shows `section`, for the project menu's "Project settings...".
    pub fn show(&self, section: Section) {
        self.pick_section(section);
    }

    fn pick_section(&self, section: Section) {
        if self.section.replace(section) != section {
            // Leaving the keys stops listening for one.
            if self.listening() {
                app::push(Input::SettingsPress(Press::Keep));
            }
            self.relayout();
        }
    }

    fn listening(&self) -> bool {
        self.values.borrow().listening.is_some()
    }

    fn paint(&self) {
        let l = self.layout.borrow();
        let s = self.scale();
        let (w, h) = ((l.size.0 * s).round() as u32, (l.size.1 * s).round() as u32);
        let mut slot = self.target.borrow_mut();
        if slot.is_none() {
            match Target::new(&self.shared.gpu, self.hwnd, w, h, self.dpi()) {
                Ok(t) => *slot = Some(t),
                Err(e) => {
                    eprintln!("horadric: render target for the Settings window: {e}");
                    return;
                }
            }
        }
        let open = self.open.get();
        let rows: Vec<SettingsLook> = self
            .lines()
            .into_iter()
            .map(|line| SettingsLook {
                open: open == Some(line.field),
                label: line.label,
                value: line.value,
                control: line.control,
            })
            .collect();
        let scene = SettingsScene {
            layout: &l,
            sections: &Section::ALL.map(Section::label),
            section: Section::ALL
                .iter()
                .position(|x| *x == self.section.get())
                .unwrap_or(0),
            heading: self.section.get().label(),
            rows: &rows,
            scroll: self.section.get() == Section::Scroll,
            hot: self.hot.get(),
            pressed: self.pressed.get(),
        };
        let result = slot
            .as_ref()
            .map(|t| t.draw_settings(&self.shared.gpu, &self.shared.metrics, &scene));
        if let Some(Err(_)) = result {
            *slot = None;
        }
    }

    fn hit_at(&self, x: i32, y: i32) -> SettingsHit {
        let s = self.scale();
        layout::settings_hit(&self.layout.borrow(), x as f32 / s, y as f32 / s)
    }

    fn hit(&self, lparam: LPARAM) -> SettingsHit {
        let x = (lparam.0 & 0xffff) as i16 as i32;
        let y = ((lparam.0 >> 16) & 0xffff) as i16 as i32;
        self.hit_at(x, y)
    }

    fn hover(&self, hot: SettingsHit) {
        if self.hot.replace(hot) != hot {
            self.invalidate();
        }
    }

    fn press(&self, pressed: Option<SettingsHit>) {
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

    fn refresh_hover(&self) {
        if !self.tracking.get() {
            return;
        }
        let mut p = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut p);
            let _ = ScreenToClient(self.hwnd, &mut p);
        }
        self.hover(self.hit_at(p.x, p.y));
    }

    fn click(&self, hit: SettingsHit) {
        match hit {
            SettingsHit::Close => app::push(Input::SettingsClosed),
            SettingsHit::Section(i) => {
                if let Some(s) = Section::ALL.get(i) {
                    self.pick_section(*s);
                }
            }
            SettingsHit::Row(i) => {
                let Some(line) = self.lines().into_iter().nth(i) else {
                    return;
                };
                if matches!(line.control, Control::Fixed | Control::Note) {
                    return;
                }
                if let Some(row) = self.row_on_screen(i) {
                    app::push(Input::SettingsClick(line.field, row));
                }
            }
            SettingsHit::Title | SettingsHit::Nothing => {}
        }
    }

    /// Row `i` in screen pixels, for its list to drop from.
    fn row_on_screen(&self, i: usize) -> Option<RECT> {
        let r = self.layout.borrow().rows.get(i)?.rect;
        let mut w = RECT::default();
        unsafe {
            let _ = GetWindowRect(self.hwnd, &mut w);
        }
        let s = self.scale();
        let px = |v: f32| (v * s).round() as i32;
        Some(RECT {
            left: w.left + px(r.x),
            top: w.top + px(r.y),
            right: w.left + px(r.right()),
            bottom: w.top + px(r.bottom()),
        })
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
            // The title bar moves the window the way a caption does, snap
            // and all, with nothing of ours to get wrong.
            WM_NCHITTEST => {
                let mut p = POINT {
                    x: (lparam.0 & 0xffff) as i16 as i32,
                    y: ((lparam.0 >> 16) & 0xffff) as i16 as i32,
                };
                unsafe {
                    let _ = ScreenToClient(self.hwnd, &mut p);
                }
                let at = if self.hit_at(p.x, p.y) == SettingsHit::Title {
                    HTCAPTION
                } else {
                    HTCLIENT
                };
                Some(LRESULT(at as isize))
            }
            WM_CLOSE => {
                app::push(Input::SettingsClosed);
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
                let r = unsafe { *(lparam.0 as *const RECT) };
                unsafe {
                    let _ = SetWindowPos(
                        self.hwnd,
                        None,
                        r.left,
                        r.top,
                        r.right - r.left,
                        r.bottom - r.top,
                        SWP_NOACTIVATE | SWP_NOZORDER,
                    );
                }
                Some(LRESULT(0))
            }
            // The hook has the keys while listening. What it lets through
            // is a modifier, eaten here so Alt does not open the window's
            // menu.
            WM_KEYDOWN | WM_SYSKEYDOWN | WM_SYSKEYUP | WM_SYSCHAR if self.listening() => {
                Some(LRESULT(0))
            }
            WM_KILLFOCUS if self.listening() => {
                app::push(Input::SettingsPress(Press::Keep));
                None
            }
            WM_KEYDOWN => {
                match wparam.0 as u16 {
                    k if k == VK_ESCAPE.0 => app::push(Input::SettingsClosed),
                    k if k == VK_UP.0 => self.pick_section(step(self.section.get(), -1)),
                    k if k == VK_DOWN.0 => self.pick_section(step(self.section.get(), 1)),
                    _ => {}
                }
                Some(LRESULT(0))
            }
            WM_LBUTTONDOWN => {
                unsafe {
                    SetCapture(self.hwnd);
                }
                self.press(Some(self.hit(lparam)));
                Some(LRESULT(0))
            }
            WM_MOUSEMOVE => {
                self.track();
                self.hover(self.hit(lparam));
                Some(LRESULT(0))
            }
            WM_LBUTTONUP => {
                // Before letting go: releasing capture sends
                // WM_CAPTURECHANGED at once, and that clears the press.
                let pressed = self.pressed.get();
                unsafe {
                    let _ = ReleaseCapture();
                }
                self.press(None);
                let hit = self.hit(lparam);
                // Only where the press began, as a button does.
                if pressed == Some(hit) {
                    self.click(hit);
                }
                Some(LRESULT(0))
            }
            WM_MOUSELEAVE => {
                self.tracking.set(false);
                self.hover(SettingsHit::Nothing);
                Some(LRESULT(0))
            }
            WM_CAPTURECHANGED => {
                self.press(None);
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
    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const SettingsWindow;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn values() -> Values {
        Values {
            theme: Theme::default(),
            font: "Cascadia Mono".into(),
            screens: vec!["Screen 1".into(), "Screen 2".into()],
            screen: Some(1),
            notify: true,
            sounds: false,
            town: true,
            agent: Agent::Claude,
            defaults: Defaults {
                effort: Some("xhigh".into()),
                ..Default::default()
            },
            offered: None,
            switch_accounts: false,
            autostart: Some(true),
            explorer: Some(false),
            discord: Discord::Unnamed,
            recall_across: false,
            backup: Backup::default(),
            scroll: Scroll::default(),
            update: None,
            checking: false,
            version: "0.9.0".into(),
            ask: true,
            hidden: 0,
            keys: Action::ALL.map(|a| Key {
                chord: a.default_chord(false).name(),
                live: true,
            }),
            listening: None,
            keys_note: None,
            projects: vec!["app".into(), "site".into()],
            project: Some(Project {
                index: 1,
                colour: "Violet".into(),
                branches: Some(false),
                hosts: vec!["pi".into()],
                log: true,
                drive: None,
                held: false,
            }),
        }
    }

    fn fields(section: Section, v: &Values) -> Vec<Field> {
        lines(section, v).iter().map(|l| l.field).collect()
    }

    #[test]
    fn the_backup_section_shows_only_what_its_target_needs() {
        let mut v = values();
        assert_eq!(
            fields(Section::Backup, &v),
            [Field::BackupTo, Field::BackupNote]
        );
        v.backup.to = To::Drive;
        assert_eq!(
            fields(Section::Backup, &v),
            [
                Field::BackupTo,
                Field::BackupAccount,
                Field::BackupPassphrase,
                Field::BackupNote,
                Field::BackupLast,
                Field::BackupNow,
                Field::BackupRestore,
            ]
        );
        assert_eq!(lines(Section::Backup, &v)[1].value, "Sign in with Google");
        v.backup.account = Some("me@example.com".into());
        assert_eq!(
            lines(Section::Backup, &v)[1].value,
            "me@example.com, sign out"
        );
        v.backup.to = To::Folder;
        assert_eq!(fields(Section::Backup, &v)[1], Field::BackupFolder);
    }

    #[test]
    fn a_running_job_holds_the_backup_buttons() {
        let mut v = values();
        v.backup.to = To::Folder;
        v.backup.busy = Some("Backing up".into());
        let rows = lines(Section::Backup, &v);
        let button = |f| rows.iter().find(|l| l.field == f).unwrap().control;
        assert_eq!(button(Field::BackupNow), Control::Fixed);
        assert_eq!(button(Field::BackupRestore), Control::Fixed);
        assert_eq!(button(Field::BackupTo), Control::List);
    }

    #[test]
    fn each_section_has_its_rows() {
        let v = values();
        assert_eq!(
            fields(Section::Appearance, &v),
            [Field::Theme, Field::Font, Field::Screen, Field::Town]
        );
        assert_eq!(
            fields(Section::Notifications, &v),
            [Field::Notify, Field::Sounds]
        );
        assert_eq!(
            fields(Section::Startup, &v),
            [
                Field::Autostart,
                Field::Explorer,
                Field::Updates,
                Field::Version
            ]
        );
        assert_eq!(
            fields(Section::Sessions, &v),
            [
                Field::Agent,
                Field::Default(Setting::Model),
                Field::Default(Setting::Effort),
                Field::Default(Setting::Permissions),
                Field::Default(Setting::Workflow),
                Field::Offered,
                Field::SwitchAccounts,
            ]
        );
        assert_eq!(
            fields(Section::Privacy, &v),
            [Field::Discord, Field::Recall]
        );
        assert_eq!(fields(Section::Runetome, &v), [Field::Ask, Field::Hidden]);
        assert_eq!(
            fields(Section::Scroll, &v),
            [
                Field::KeysNote,
                Field::Project,
                Field::Version,
                Field::Version,
                Field::Horadrim
            ]
        );
        assert_eq!(
            fields(Section::Keys, &v),
            [
                Field::Key(Action::Next),
                Field::Key(Action::Listen),
                Field::Key(Action::Stop),
                Field::Key(Action::Account),
                Field::KeysNote
            ]
        );
        assert_eq!(
            fields(Section::Projects, &v),
            [
                Field::Project,
                Field::Colour,
                Field::WorkMode,
                Field::Hosts,
                Field::AddHost,
                Field::Drive,
                Field::ShipsPublic
            ]
        );
        assert_eq!(tallest(&v), PROJECT_ROWS);
    }

    #[test]
    fn the_scroll_reads_the_lexicon_and_its_switch() {
        let mut v = values();
        v.scroll = Scroll {
            off: false,
            project: "app".into(),
            sigils: 3,
            top: vec![("\u{a7}lore".into(), "horadric-memory".into(), 12)],
            saved: 12_345,
        };
        let rows = lines(Section::Scroll, &v);
        assert_eq!(rows[1].value, "app, 3 sigils");
        assert_eq!(rows[2].value, "\u{a7}lore  horadric-memory, used 12");
        assert_eq!(rows[3].value, "12,345 tokens");
        assert_eq!(rows.last().unwrap().control, Control::Switch(true));
        v.scroll.off = true;
        assert_eq!(lines(Section::Scroll, &v).last().unwrap().value, "Off");
    }

    #[test]
    fn thousands_are_set_apart() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1_000), "1,000");
        assert_eq!(thousands(-1_234_567), "-1,234,567");
    }

    #[test]
    fn a_project_shows_its_own_settings() {
        let v = values();
        let rows = lines(Section::Projects, &v);
        let shown: Vec<&str> = rows.iter().map(|l| l.value.as_str()).collect();
        assert_eq!(shown[..4], ["site", "Violet", "Trunk", "pi"]);
        assert_eq!(rows[5].control, Control::Switch(false));
        assert_eq!(rows[6].control, Control::Fixed);
    }

    #[test]
    fn warriv_drives_only_a_project_with_a_quest_log() {
        let mut v = values();
        let p = v.project.as_mut().unwrap();
        p.log = false;
        assert_eq!(lines(Section::Projects, &v)[5].control, Control::Fixed);
        let p = v.project.as_mut().unwrap();
        p.drive = Some(true);
        p.held = true;
        let rows = lines(Section::Projects, &v);
        assert_eq!(rows[5].value, "On, shipping held");
        assert_eq!(rows[6].control, Control::Switch(true));
    }

    #[test]
    fn no_project_and_no_repository_are_said_not_offered() {
        let mut v = values();
        v.project.as_mut().unwrap().branches = None;
        assert_eq!(lines(Section::Projects, &v)[2].control, Control::Fixed);
        v.project = None;
        let rows = lines(Section::Projects, &v);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].control, Control::Fixed);
        assert_eq!(tallest(&v), PROJECT_ROWS);
    }

    #[test]
    fn many_hosts_are_counted() {
        let h = |n: usize| (0..n).map(|i| format!("h{i}")).collect::<Vec<_>>();
        assert_eq!(hosts_line(&h(0)), "None");
        assert_eq!(hosts_line(&h(2)), "h0, h1");
        assert_eq!(hosts_line(&h(5)), "h0, h1 and 3 more");
    }

    #[test]
    fn a_row_says_what_its_setting_is_now() {
        let v = values();
        let look = lines(Section::Appearance, &v);
        assert_eq!(look[0].value, Theme::default().label());
        assert_eq!(look[1].value, "Cascadia Mono");
        assert_eq!(look[2].value, "Screen 2");
        let n = lines(Section::Notifications, &v);
        assert_eq!(n[0].control, Control::Switch(true));
        assert_eq!(n[0].value, "On");
        assert_eq!(n[1].control, Control::Switch(false));
        assert_eq!(
            lines(Section::Privacy, &v)[0].value,
            "Without project names"
        );
    }

    #[test]
    fn the_sessions_rows_show_the_picked_agents_defaults() {
        let mut v = values();
        let rows = lines(Section::Sessions, &v);
        let values: Vec<&str> = rows.iter().map(|l| l.value.as_str()).collect();
        assert_eq!(
            values,
            [
                "Claude Code",
                "Default",
                "Extra high",
                "Default",
                "Default",
                "Always",
                "Off"
            ]
        );
        assert_eq!(rows[5].control, Control::Fixed);
        assert_eq!(rows[6].control, Control::Switch(false));
        v.agent = Agent::Codex;
        v.defaults = Defaults {
            model: Some("gpt-5.5".into()),
            ..Default::default()
        };
        v.offered = Some(Some(false));
        let rows = lines(Section::Sessions, &v);
        assert_eq!(
            rows.iter().map(|l| l.field).collect::<Vec<_>>(),
            [
                Field::Agent,
                Field::Default(Setting::Model),
                Field::Default(Setting::Effort),
                Field::Offered,
            ]
        );
        assert_eq!(rows[1].value, "GPT-5.5");
        assert!(tallest(&v) >= 6);
        assert_eq!(rows[3].control, Control::Switch(false));
        v.offered = Some(None);
        let rows = lines(Section::Sessions, &v);
        assert_eq!(
            (rows[3].value.as_str(), rows[3].control),
            ("Not installed", Control::Fixed)
        );
    }

    #[test]
    fn one_screen_is_said_not_offered() {
        let mut v = values();
        assert_eq!(lines(Section::Appearance, &v)[2].control, Control::List);
        v.screens.truncate(1);
        v.screen = Some(0);
        let row = &lines(Section::Appearance, &v)[2];
        assert_eq!(
            (row.value.as_str(), row.control),
            ("Screen 1", Control::Fixed)
        );
    }

    #[test]
    fn the_updates_row_checks_then_offers_what_it_found() {
        let mut v = values();
        let row = |v: &Values| {
            lines(Section::Startup, v)
                .into_iter()
                .find(|l| l.field == Field::Updates)
                .unwrap()
        };
        assert_eq!(row(&v).value, "Check now");
        assert_eq!(row(&v).control, Control::Button);
        v.checking = true;
        assert_eq!(row(&v).control, Control::Fixed);
        v.update = Some("1.0.0".into());
        assert_eq!(row(&v).value, "Update to 1.0.0");
        assert_eq!(row(&v).control, Control::Button);
    }

    #[test]
    fn a_dev_instance_offers_neither_startup_switch() {
        let mut v = values();
        v.autostart = None;
        v.explorer = None;
        let startup = lines(Section::Startup, &v);
        assert_eq!(startup[0].control, Control::Fixed);
        assert_eq!(startup[1].control, Control::Fixed);
    }

    #[test]
    fn the_keys_show_their_chords_and_listen_for_one() {
        let mut v = values();
        let keys = lines(Section::Keys, &v);
        assert_eq!(keys[0].value, "Ctrl+Alt+Space");
        assert_eq!(keys[2].value, "Ctrl+Alt+W");
        assert_eq!(keys[0].control, Control::Button);
        assert_eq!(keys[4].control, Control::Note);
        assert_eq!(keys[4].value, "Click a shortcut, then press its new keys");
        v.listening = Some(Action::Listen);
        let keys = lines(Section::Keys, &v);
        assert_eq!(keys[1].value, "Press the keys\u{2026}");
        assert_eq!(keys[0].value, "Ctrl+Alt+Space");
        assert!(keys[4].value.starts_with("Esc keeps it"));
        v.keys_note = Some("Hold Ctrl, Alt or Win with the key".into());
        assert_eq!(
            lines(Section::Keys, &v)[4].value,
            "Hold Ctrl, Alt or Win with the key"
        );
    }

    #[test]
    fn put_away_stones_are_brought_back_only_when_there_are_some() {
        let mut v = values();
        let row = |v: &Values| lines(Section::Runetome, v)[1].clone();
        assert_eq!(
            (row(&v).value.as_str(), row(&v).control),
            ("None", Control::Fixed)
        );
        v.hidden = 2;
        assert_eq!(
            (row(&v).value.as_str(), row(&v).control),
            ("2, bring back", Control::Button)
        );
        assert_eq!(
            lines(Section::Runetome, &v)[0].control,
            Control::Switch(true)
        );
    }

    #[test]
    fn a_key_another_program_holds_says_so() {
        let mut v = values();
        v.keys[1].live = false;
        let keys = lines(Section::Keys, &v);
        assert_eq!(keys[1].value, "Ctrl+Alt+Home, taken");
        assert_eq!(keys[4].value, "Ctrl+Alt+Home is taken by another program");
    }

    #[test]
    fn the_arrow_keys_go_round_the_sections() {
        assert_eq!(step(Section::Appearance, 1), Section::Notifications);
        assert_eq!(step(Section::Notifications, 1), Section::Sessions);
        assert_eq!(step(Section::Runetome, 1), Section::Projects);
        assert_eq!(step(Section::Appearance, -1), Section::Projects);
        assert_eq!(step(Section::Projects, 1), Section::Appearance);
    }
}
