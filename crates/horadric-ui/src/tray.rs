//! The notification area icon: the one piece of Horadric that is always on
//! screen, even with no sessions and so no tiles. Its menu starts sessions,
//! opens the Settings window and quits the app. It holds actions, not
//! settings, with one exception: Warriv drives, the go ahead to ship, which
//! should be one click and in plain sight.

use std::ffi::c_void;
use std::time::Duration;

use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Gdi::{
    CreateBitmap, CreateDIBSection, DeleteObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
    DIB_RGB_COLORS,
};
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY,
    NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateIconIndirect, DestroyIcon, GetSystemMetrics, HICON, ICONINFO, SM_CXICON, SM_CXSMICON,
};

use horadric_core::warriv::Drive;
use horadric_core::{experience, Agent};

use crate::joining;
use crate::menu::{self, Item};
use crate::{hotkey, icon, motion, recent};

const ID: u32 = 1;

pub struct Tray {
    hwnd: HWND,
    callback: u32,
    icon: HICON,
    /// The breathing light, one icon per step, made once and kept.
    frames: Vec<HICON>,
    /// The same breath at the taskbar's size, for the stage's button.
    big: Vec<HICON>,
    /// The frame showing while it breathes, `None` while it rests.
    frame: Option<usize>,
    tip: String,
}

/// How many steps one breath takes. At [`BREATH_STEP_MS`] each, a breath
/// lasts about two seconds, slow enough to read as calm work.
pub const BREATH_FRAMES: usize = 24;
pub const BREATH_STEP_MS: u32 = 80;

/// How bright the light is at this step of a breath: full at the start and
/// end, dimmest halfway. The same curve as a waiting tile's breath.
pub fn breath(step: usize) -> f32 {
    let at = Duration::from_millis((step as u64) * BREATH_STEP_MS as u64);
    let period = Duration::from_millis((BREATH_FRAMES as u64) * BREATH_STEP_MS as u64);
    1.0 - 0.6 * motion::breathe(at, period)
}

/// What the user picked from the menu.
pub enum Choice {
    /// Start a session in a folder chosen with the picker.
    New,
    /// Start a session in this recent project.
    Recent(String),
    /// Open the quest log of this recent project, the way back to its
    /// quests and its conversations.
    QuestLog(String),
    /// Put every cluster back in the auto layout.
    Tidy,
    /// Bring every cluster in front of the other windows.
    Raise,
    /// Open the terminal again after it was closed.
    ShowStage,
    /// Show the session that has waited on you longest.
    NextWaiting,
    /// Say what happened since this morning.
    Listen,
    /// Fill the space beside the clusters with the terminal.
    Arrange,
    /// Open the Settings window.
    Settings,
    /// Install the newer release the menu offered.
    Update,
    /// Let Warriv drive this project, by key, or stop it.
    Drive(String, bool),
    /// Let Warriv ship this project public, by key, or no longer.
    ShipsPublic(String, bool),
    /// Stop every drive at once.
    StopWarriv,
    /// Switch this agent to its account at this place in the list given.
    Account(Agent, usize),
    /// Log this agent in to another account.
    AddAccount(Agent),
    /// Sign in to WorkOS, so this PC finds its brothers.
    SignIn,
    SignOut,
    /// Hand every session to this brother, or with None stop doing so.
    Leave(Option<String>),
    /// Who keeps this PC's sessions if it falls silent, by id, or no one.
    Keeper(Option<String>),
    /// Whether that keeper takes over by itself after ten minutes.
    AutoTakeover(bool),
    /// Whether this brother, by id, may control this PC or only watch.
    Control(String, bool),
    /// Cast this brother out, by id and name, after asking.
    CastOut(String, String),
    /// Join a Linux node by its SSH destination.
    AddNode,
    /// Take this node off the list, by name.
    ForgetNode(String),
    /// End every session in every project, after asking.
    EndAll,
    Quit,
}

impl Tray {
    /// Adds the icon. Clicks arrive at `hwnd` as `callback` messages.
    pub fn add(hwnd: HWND, callback: u32) -> Tray {
        let small = unsafe { GetSystemMetrics(SM_CXSMICON) }.max(16);
        let big = unsafe { GetSystemMetrics(SM_CXICON) }.max(32);
        let tray = Tray {
            hwnd,
            callback,
            icon: make_icon(small, 1.0),
            frames: (0..BREATH_FRAMES)
                .map(|i| make_icon(small, breath(i)))
                .collect(),
            big: (0..BREATH_FRAMES)
                .map(|i| make_icon(big, breath(i)))
                .collect(),
            frame: None,
            tip: "Horadric".into(),
        };
        tray.show();
        tray
    }

    /// Adds the icon again. Needed after Explorer restarts, which forgets
    /// every icon and says so with a `TaskbarCreated` broadcast.
    pub fn show(&self) {
        unsafe {
            let _ = Shell_NotifyIconW(NIM_ADD, &self.data());
        }
    }

    pub fn set_tip(&mut self, tip: &str) {
        if self.tip == tip {
            return;
        }
        self.tip = tip.to_string();
        unsafe {
            let _ = Shell_NotifyIconW(NIM_MODIFY, &self.data());
        }
    }

    /// Starts or stops the light breathing. Returns the new state when it
    /// changed, so the caller starts or stops the timer that steps it.
    pub fn breathe(&mut self, on: bool) -> Option<bool> {
        if on == self.frame.is_some() {
            return None;
        }
        self.frame = on.then_some(0);
        self.refresh();
        Some(on)
    }

    /// Moves the breath on one frame.
    pub fn step(&mut self) {
        if let Some(f) = self.frame {
            self.frame = Some((f + 1) % self.frames.len().max(1));
            self.refresh();
        }
    }

    /// The icon for the stage's taskbar button, breathing with the tray's.
    /// At rest it is the first frame, which is the light at full.
    pub fn taskbar_icon(&self) -> HICON {
        self.big
            .get(self.frame.unwrap_or(0))
            .copied()
            .unwrap_or_default()
    }

    fn refresh(&self) {
        unsafe {
            let _ = Shell_NotifyIconW(NIM_MODIFY, &self.data());
        }
    }

    fn data(&self) -> NOTIFYICONDATAW {
        let mut d = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.hwnd,
            uID: ID,
            uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
            uCallbackMessage: self.callback,
            hIcon: self
                .frame
                .and_then(|f| self.frames.get(f).copied())
                .unwrap_or(self.icon),
            ..Default::default()
        };
        for (slot, unit) in d.szTip.iter_mut().zip(self.tip.encode_utf16().take(127)) {
            *slot = unit;
        }
        d
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        unsafe {
            let _ = Shell_NotifyIconW(NIM_DELETE, &self.data());
            let _ = DestroyIcon(self.icon);
            for f in self.frames.iter().chain(&self.big) {
                let _ = DestroyIcon(*f);
            }
        }
    }
}

/// Where the Linux nodes menu's Forget ids start, one per node.
const NODE: usize = 800;
/// Where the Brothers menu's ids start, one per brother the relay lists.
const BROTHER: usize = 900;
/// Where the Quest log menu's ids start, one per recent project.
const QUEST_LOG: usize = 1000;
/// "Warriv drives" for the project at this place in the list given, and
/// its "and ships public". Past the recent projects' quest logs.
const DRIVE: usize = 5000;
const PUBLIC: usize = 6000;

/// Where the Account menu's ids start: a hundred for each agent, its
/// accounts and then Add account.
const ACCOUNT: usize = 7000;
const ADD_ACCOUNT: usize = 99;

/// One agent's kept accounts, as the Account menu lists them.
pub struct Logins {
    pub agent: Agent,
    pub accounts: Vec<Pick>,
}

/// One account in the Account menu.
pub struct Pick {
    pub id: String,
    pub label: String,
    /// The one in use, ticked.
    pub live: bool,
}

/// A project with a quest log, as the "Warriv drives" menu lists it.
pub struct Driven {
    pub key: String,
    pub name: String,
    pub drive: Option<Drive>,
}

/// The tray menu. `hotkeys` are the shortcuts for the next waiting
/// session, the catch-up and stopping Warriv, where they have one.
/// `update` is a newer release's version, when a check found one. `xp` is
/// the experience counted from git, None until the first count is back.
/// `logins` are the accounts to switch between, a click away.
pub fn menu(
    recent_projects: &[String],
    hotkeys: [Option<&str>; hotkey::COUNT],
    driven: &[Driven],
    logins: &[Logins],
    terminal: bool,
    update: Option<&str>,
    xp: Option<u64>,
) -> Option<Choice> {
    const NEW: usize = 1;
    const QUIT: usize = 2;
    const SETTINGS: usize = 3;
    const TIDY: usize = 4;
    const NEXT: usize = 5;
    const ARRANGE: usize = 6;
    const RAISE: usize = 7;
    const END_ALL: usize = 8;
    const STAGE: usize = 10;
    const UPDATE: usize = 12;
    const LISTEN: usize = 13;
    const STOP: usize = 15;
    const SIGN_IN: usize = 16;
    const SIGN_OUT: usize = 17;
    const ADD_NODE: usize = 18;
    const RECENT: usize = 100;
    let mut items = Vec::new();
    if let Some(xp) = xp {
        items.push(Item::Disabled(experience::label(xp)));
        items.push(Item::Separator);
    }
    items.push(Item::action(NEW, "New session\u{2026}"));
    items.push(Item::Separator);
    if recent_projects.is_empty() {
        items.push(Item::Disabled("No recent projects".into()));
    }
    // A tab right aligns the rest.
    let label = |path: &str| {
        let (name, place) = recent::label(path);
        format!("{name}\t{place}")
    };
    for (i, path) in recent_projects.iter().enumerate() {
        items.push(Item::action(RECENT + i, label(path)));
    }
    // The way back to a project with no tiles left, whose menu went with
    // its cluster.
    let logs: Vec<Item> = recent_projects
        .iter()
        .enumerate()
        .map(|(i, path)| Item::action(QUEST_LOG + i, label(path)))
        .collect();
    if !logs.is_empty() {
        items.push(Item::Submenu("Quest log".into(), logs));
    }
    items.push(Item::Separator);
    let with_key = |label: &str, key: Option<&str>| match key {
        Some(key) => format!("{label}\t{key}"),
        None => label.to_string(),
    };
    items.push(Item::action(
        NEXT,
        with_key("Next waiting session", hotkeys[0]),
    ));
    items.push(Item::action(
        LISTEN,
        with_key("Stay a while and listen", hotkeys[1]),
    ));
    if !driven.is_empty() {
        let mut lines: Vec<Item> = driven
            .iter()
            .enumerate()
            .map(|(i, d)| Item::Action {
                id: DRIVE + i,
                label: match d.drive.as_ref().and_then(|d| d.held.as_ref()) {
                    Some(_) => format!("{}, shipping held", d.name),
                    None => d.name.clone(),
                },
                checked: d.drive.is_some(),
            })
            .collect();
        let public: Vec<Item> = driven
            .iter()
            .enumerate()
            .filter_map(|(i, d)| {
                Some(Item::Action {
                    id: PUBLIC + i,
                    label: d.name.clone(),
                    checked: d.drive.as_ref()?.ships_public,
                })
            })
            .collect();
        if !public.is_empty() {
            lines.push(Item::Separator);
            lines.push(Item::Disabled("And ships public".into()));
            lines.extend(public);
        }
        items.push(Item::Submenu("Warriv drives".into(), lines));
        let stop = with_key("Stop Warriv", hotkeys[2]);
        if driven.iter().any(|d| d.drive.is_some()) {
            items.push(Item::action(STOP, stop));
        } else {
            items.push(Item::Disabled(stop));
        }
    }
    // A closed terminal leaves only its tiles, and a tile click shows one
    // project. This is the way back without picking one.
    if terminal {
        items.push(Item::action(STAGE, "Show terminal"));
    } else {
        items.push(Item::Disabled("Show terminal".into()));
    }
    items.push(Item::action(RAISE, "Bring tiles to front"));
    items.push(Item::action(ARRANGE, "Fit terminal beside tiles"));
    items.push(Item::action(TIDY, "Tidy up tiles"));
    items.push(Item::Separator);
    if !logins.is_empty() {
        items.push(Item::Submenu("Account".into(), account_items(logins)));
    }
    let mut brothers = Vec::new();
    let keeping = crate::app::keeping_tray();
    let standing = horadric_hooks::state_dir()
        .map(|d| crate::standing::Standing::load(&d))
        .unwrap_or_default();
    let joined: Vec<(String, String, bool)> = keeping
        .0
        .iter()
        .map(|(id, name)| (id.clone(), name.clone(), standing.controls(id)))
        .collect();
    match crate::workos::user() {
        Some(user) => {
            items.push(Item::Disabled(format!("Signed in as {}", user.email)));
            let listed = crate::brothers::listed();
            items.push(Item::Submenu(
                "Brothers".into(),
                brother_items(listed.as_deref(), &joined, &crate::far_node::folded_ids()),
            ));
            brothers = listed.unwrap_or_default();
            items.extend(crate::app::leaving_menu());
            items.extend(keeping_items(&keeping));
            items.push(Item::action(SIGN_OUT, "Sign out"));
        }
        None => items.push(Item::action(SIGN_IN, "Sign in\u{2026}")),
    }
    let nodes = horadric_hooks::state_dir()
        .map(|d| crate::far_node::load(&d))
        .unwrap_or_default();
    let lines: Vec<(String, String, Option<String>)> = nodes
        .iter()
        .map(|n| {
            let brother =
                crate::far_node::folded(&n.name).map(|id| crate::project::brother_name(&id));
            (n.name.clone(), crate::far_node::status(&n.name), brother)
        })
        .collect();
    items.push(Item::Submenu(
        "Linux nodes".into(),
        node_items(&lines, ADD_NODE),
    ));
    items.push(Item::action(SETTINGS, "Settings\u{2026}"));
    if let Some(version) = update {
        items.push(Item::action(UPDATE, format!("Update to {version}\u{2026}")));
    }
    items.push(Item::Separator);
    items.push(Item::action(END_ALL, "End all sessions"));
    items.push(Item::action(QUIT, "Quit Horadric"));

    match menu::popup(&items)? {
        NEW => Some(Choice::New),
        QUIT => Some(Choice::Quit),
        SETTINGS => Some(Choice::Settings),
        TIDY => Some(Choice::Tidy),
        NEXT => Some(Choice::NextWaiting),
        LISTEN => Some(Choice::Listen),
        ARRANGE => Some(Choice::Arrange),
        RAISE => Some(Choice::Raise),
        STAGE => Some(Choice::ShowStage),
        UPDATE => Some(Choice::Update),
        END_ALL => Some(Choice::EndAll),
        STOP => Some(Choice::StopWarriv),
        SIGN_IN => Some(Choice::SignIn),
        SIGN_OUT => Some(Choice::SignOut),
        NO_KEEPER => Some(Choice::Keeper(None)),
        AUTO_TAKEOVER => Some(Choice::AutoTakeover(!keeping.2)),
        i if (CONTROL..CONTROL + RIGHTS).contains(&i) => joined
            .get(i - CONTROL)
            .map(|(id, _, on)| Choice::Control(id.clone(), !on)),
        i if (CAST_OUT..CAST_OUT + RIGHTS).contains(&i) => joined
            .get(i - CAST_OUT)
            .map(|(id, name, _)| Choice::CastOut(id.clone(), name.clone())),
        i if (KEEPER..KEEPER + KEEPERS).contains(&i) => keeping
            .0
            .get(i - KEEPER)
            .map(|(id, _)| Choice::Keeper(Some(id.clone()))),
        i if crate::app::leaving_pick(i).is_some() => {
            crate::app::leaving_pick(i).map(Choice::Leave)
        }
        ADD_NODE => Some(Choice::AddNode),
        i if (NODE..BROTHER).contains(&i) => nodes
            .get(i - NODE)
            .map(|n| Choice::ForgetNode(n.name.clone())),
        i if (ACCOUNT..ACCOUNT + 100 * Agent::ALL.len()).contains(&i) => {
            let agent = Agent::ALL[(i - ACCOUNT) / 100];
            match (i - ACCOUNT) % 100 {
                ADD_ACCOUNT => Some(Choice::AddAccount(agent)),
                n => Some(Choice::Account(agent, n)),
            }
        }
        i if i >= PUBLIC => driven.get(i - PUBLIC).map(|d| {
            Choice::ShipsPublic(
                d.key.clone(),
                !d.drive.as_ref().is_some_and(|d| d.ships_public),
            )
        }),
        i if (BROTHER..QUEST_LOG).contains(&i) => {
            if let Some(b) = brothers.get(i - BROTHER) {
                crate::brothers::ask(&b.id);
            }
            None
        }
        i if i >= DRIVE => driven
            .get(i - DRIVE)
            .map(|d| Choice::Drive(d.key.clone(), d.drive.is_none())),
        i if i >= QUEST_LOG => recent_projects
            .get(i - QUEST_LOG)
            .map(|p| Choice::QuestLog(p.clone())),
        i if i >= RECENT => recent_projects
            .get(i - RECENT)
            .map(|p| Choice::Recent(p.clone())),
        _ => None,
    }
}

/// Where a joined brother's "Can control this PC" and "Cast out" ids
/// start, one each per joined brother.
const CONTROL: usize = 9600;
const CAST_OUT: usize = 9700;
const RIGHTS: usize = 80;

/// The Brothers menu: the other PCs signed in as this human that the relay
/// sees now, one not joined yet as a way to ask it in, and each joined
/// one, listed now or not, as a submenu of what it may do here.
/// `None` is a portal that is closed or not through yet. `joined` is
/// (id, name, whether it may control this PC). `ssh` are the brothers
/// that are a Linux node reached over `ssh` too: one machine, one line,
/// which says the `ssh` link waits behind the relay one.
fn brother_items(
    listed: Option<&[joining::Listed]>,
    joined: &[(String, String, bool)],
    ssh: &[String],
) -> Vec<Item> {
    let joined = &joined[..joined.len().min(RIGHTS)];
    let rights = |j: usize, label: String| {
        let (_, name, on) = &joined[j];
        Item::Submenu(
            label,
            vec![
                Item::Action {
                    id: CONTROL + j,
                    label: "Can control this PC".into(),
                    checked: *on,
                },
                Item::Separator,
                Item::action(CAST_OUT + j, format!("Cast out {name}\u{2026}")),
            ],
        )
    };
    let at = |id: &str| joined.iter().position(|(j, _, _)| j == id);
    let mut items: Vec<Item> = match listed {
        None => vec![Item::Disabled("Open the Town Portal to find them".into())],
        Some([]) => vec![Item::Disabled("No other PC is signed in".into())],
        Some(listed) => listed
            .iter()
            .take(QUEST_LOG - BROTHER)
            .enumerate()
            .map(|(i, b)| {
                let label = if ssh.contains(&b.id) {
                    format!("{}, ssh standby", b.label())
                } else {
                    b.label()
                };
                match (at(&b.id), b.joined || b.linked) {
                    (Some(j), _) => rights(j, label),
                    (None, true) => Item::Disabled(label),
                    (None, false) => Item::action(BROTHER + i, label),
                }
            })
            .collect(),
    };
    let listed = listed.unwrap_or_default();
    let away: Vec<Item> = joined
        .iter()
        .enumerate()
        .filter(|(_, (id, _, _))| !listed.iter().any(|b| &b.id == id))
        .map(|(j, (id, name, _))| {
            let how = if ssh.contains(id) {
                "over ssh"
            } else {
                "joined"
            };
            rights(j, format!("{name}\t{how}"))
        })
        .collect();
    if !away.is_empty() {
        items.push(Item::Separator);
        items.extend(away);
    }
    items
}

/// Where the keeping lines' ids start, past the Leaving menu's.
const NO_KEEPER: usize = 9500;
const AUTO_TAKEOVER: usize = 9501;
const KEEPER: usize = 9510;
const KEEPERS: usize = 80;

/// "Keeps my sessions", a joined brother to pick (or no one), and the
/// switch for taking over by itself. `keeping` is (brothers as (id,
/// name), the first keeper, whether the switch is on). Nothing until a
/// brother has joined.
fn keeping_items(keeping: &(Vec<(String, String)>, Option<String>, bool)) -> Vec<Item> {
    let (brothers, keeper, auto) = keeping;
    if brothers.is_empty() {
        return Vec::new();
    }
    let mut picks: Vec<Item> = brothers
        .iter()
        .take(KEEPERS)
        .enumerate()
        .map(|(i, (id, name))| Item::Action {
            id: KEEPER + i,
            label: name.clone(),
            checked: keeper.as_deref() == Some(id.as_str()),
        })
        .collect();
    picks.push(Item::Separator);
    picks.push(Item::Action {
        id: NO_KEEPER,
        label: "No one".into(),
        checked: keeper.is_none(),
    });
    let mut items = vec![Item::Submenu("Keeps my sessions".into(), picks)];
    if keeper.is_some() {
        items.push(Item::Action {
            id: AUTO_TAKEOVER,
            label: "Take over by itself after 10 minutes".into(),
            checked: *auto,
        });
    }
    items
}

/// The Linux nodes menu: each node joined, with how its link stands, the
/// way to join another, and the way to forget one. `lines` are (name,
/// status, the relay brother it is too). A node that is a relay brother
/// too is that brother's line under Brothers, not one here as well, and
/// only its Forget says so.
fn node_items(lines: &[(String, String, Option<String>)], add: usize) -> Vec<Item> {
    let mut items: Vec<Item> = lines
        .iter()
        .filter(|(_, _, brother)| brother.is_none())
        .map(|(name, status, _)| Item::Disabled(format!("{name}\t{status}")))
        .collect();
    if !items.is_empty() {
        items.push(Item::Separator);
    }
    items.push(Item::action(add, "Join a Linux node\u{2026}"));
    if !lines.is_empty() {
        let forget = lines
            .iter()
            .take(BROTHER - NODE)
            .enumerate()
            .map(|(i, (name, _, brother))| {
                let label = match brother {
                    Some(b) => format!("{name}\tssh to brother {b}"),
                    None => name.clone(),
                };
                Item::action(NODE + i, label)
            })
            .collect();
        items.push(Item::Submenu("Forget".into(), forget));
    }
    items
}

/// The Account menu: each agent's accounts with what each has left, a
/// pick switching to it, and Add account. An agent's name heads its own
/// only when there are several.
fn account_items(logins: &[Logins]) -> Vec<Item> {
    let mut items = Vec::new();
    for l in logins {
        let place = Agent::ALL.iter().position(|a| *a == l.agent).unwrap_or(0);
        let base = ACCOUNT + 100 * place;
        if logins.len() > 1 {
            if !items.is_empty() {
                items.push(Item::Separator);
            }
            items.push(Item::Disabled(l.agent.provider().to_string()));
        }
        items.extend(
            l.accounts
                .iter()
                .take(ADD_ACCOUNT)
                .enumerate()
                .map(|(i, p)| Item::Action {
                    id: base + i,
                    label: p.label.clone(),
                    checked: p.live,
                }),
        );
        items.push(Item::action(base + ADD_ACCOUNT, "Add account"));
    }
    items
}

/// Turns the drawn pixels into a `size` pixel icon, its light dimmed to
/// `bright`.
fn make_icon(size: i32, bright: f32) -> HICON {
    let light = if horadric_hooks::dev() {
        icon::ERROR
    } else {
        icon::GOLD
    };
    let pixels = icon::lit(size as u32, dim(light, bright));
    unsafe {
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: size,
                // Negative: rows run top down, as the pixels do.
                biHeight: -size,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut c_void = std::ptr::null_mut();
        let Ok(color) = CreateDIBSection(None, &info, DIB_RGB_COLORS, &mut bits, None, 0) else {
            return HICON::default();
        };
        std::ptr::copy_nonoverlapping(pixels.as_ptr(), bits as *mut u32, pixels.len());
        // An all zero mask: the alpha channel decides what shows.
        let mask_bits = vec![0u8; (size as usize).div_ceil(16) * 2 * size as usize];
        let mask = CreateBitmap(size, size, 1, 1, Some(mask_bits.as_ptr() as *const c_void));
        let icon = CreateIconIndirect(&ICONINFO {
            fIcon: true.into(),
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: mask,
            hbmColor: color,
        })
        .unwrap_or_default();
        let _ = DeleteObject(color.into());
        let _ = DeleteObject(mask.into());
        icon
    }
}

/// A `0xRRGGBB` colour with each channel scaled by `k`.
fn dim(rgb: u32, k: f32) -> u32 {
    [16, 8, 0].iter().fold(0, |acc, shift| {
        let c = ((rgb >> shift) & 0xff) as f32 * k.clamp(0.0, 1.0);
        acc | (c.round() as u32) << shift
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_keeper_is_ticked_and_the_switch_shows_only_with_one() {
        assert!(keeping_items(&(Vec::new(), None, false)).is_empty());
        let brothers = vec![("d".to_string(), "Desktop".to_string())];
        let items = keeping_items(&(brothers.clone(), None, false));
        assert_eq!(items.len(), 1);
        let Item::Submenu(label, picks) = &items[0] else {
            panic!("a submenu first");
        };
        assert_eq!(label, "Keeps my sessions");
        assert!(matches!(
            &picks[0],
            Item::Action {
                id: KEEPER,
                checked: false,
                ..
            }
        ));
        assert!(matches!(
            &picks[2],
            Item::Action {
                id: NO_KEEPER,
                checked: true,
                ..
            }
        ));
        let items = keeping_items(&(brothers, Some("d".into()), true));
        let Item::Submenu(_, picks) = &items[0] else {
            panic!("a submenu first");
        };
        assert!(matches!(&picks[0], Item::Action { checked: true, .. }));
        assert!(matches!(
            &items[1],
            Item::Action {
                id: AUTO_TAKEOVER,
                checked: true,
                ..
            }
        ));
    }

    #[test]
    fn a_joined_brother_gets_its_control_switch_and_cast_out() {
        let listed = [
            joining::Listed {
                id: "d".into(),
                name: "Desktop".into(),
                joined: true,
                linked: true,
                old: false,
            },
            joining::Listed {
                id: "n".into(),
                name: "New".into(),
                joined: false,
                linked: false,
                old: false,
            },
        ];
        let joined = vec![
            ("d".to_string(), "Desktop".to_string(), false),
            ("v".to_string(), "Vps".to_string(), true),
        ];
        let items = brother_items(Some(&listed), &joined, &[]);
        let Item::Submenu(label, rights) = &items[0] else {
            panic!("a joined brother is a submenu");
        };
        assert_eq!(label, "Desktop\tlinked");
        assert!(matches!(
            &rights[0],
            Item::Action {
                id: CONTROL,
                checked: false,
                ..
            }
        ));
        assert!(
            matches!(&rights[2], Item::Action { id: CAST_OUT, label, .. }
            if label == "Cast out Desktop\u{2026}")
        );
        assert!(matches!(&items[1], Item::Action { id: BROTHER_1, .. }));
        assert!(matches!(&items[2], Item::Separator));
        // Joined but not on the relay now: still there to switch or cast out.
        let Item::Submenu(label, rights) = &items[3] else {
            panic!("an away brother is a submenu too");
        };
        assert_eq!(label, "Vps\tjoined");
        assert!(matches!(
            &rights[0],
            Item::Action {
                id: CONTROL_1,
                checked: true,
                ..
            }
        ));
        let closed = brother_items(None, &joined, &[]);
        assert_eq!(closed.len(), 4);
        assert!(brother_items(Some(&[]), &[], &[]).len() == 1);
        // A brother that is a node over ssh too says so, once.
        let both = brother_items(Some(&listed), &joined, &["d".into(), "v".into()]);
        assert!(matches!(&both[0], Item::Submenu(l, _) if l == "Desktop\tlinked, ssh standby"));
        assert!(matches!(&both[3], Item::Submenu(l, _) if l == "Vps\tover ssh"));
    }

    const BROTHER_1: usize = BROTHER + 1;
    const CONTROL_1: usize = CONTROL + 1;

    #[test]
    fn the_nodes_menu_lists_each_and_offers_join_and_forget() {
        let only = node_items(&[], 18);
        assert_eq!(only.len(), 1);
        assert!(matches!(&only[0], Item::Action { id: 18, .. }));
        let lines = vec![
            ("vps".to_string(), "linked".to_string(), None),
            ("box".to_string(), "Permission denied".to_string(), None),
            (
                "lab".to_string(),
                "linked".to_string(),
                Some("Lab".to_string()),
            ),
        ];
        let items = node_items(&lines, 18);
        assert!(matches!(&items[0], Item::Disabled(l) if l == "vps\tlinked"));
        assert!(matches!(&items[2], Item::Separator));
        // The node that is a brother too has no line of its own.
        assert!(!items
            .iter()
            .any(|i| matches!(i, Item::Disabled(l) if l.starts_with("lab"))));
        let Some(Item::Submenu(_, forget)) = items.last() else {
            panic!("no Forget");
        };
        assert!(
            matches!(&forget[1], Item::Action { id, label, .. } if *id == NODE + 1 && label == "box")
        );
        assert!(
            matches!(&forget[2], Item::Action { id, label, .. } if *id == NODE + 2 && label == "lab\tssh to brother Lab")
        );
    }

    #[test]
    fn a_breath_starts_bright_dips_halfway_and_comes_back() {
        assert_eq!(breath(0), 1.0);
        assert!((breath(BREATH_FRAMES / 2) - 0.4).abs() < 1e-5);
        assert_eq!(breath(BREATH_FRAMES), breath(0));
        assert!(breath(3) > breath(6), "dims smoothly on the way down");
    }

    #[test]
    fn dimming_scales_each_channel() {
        assert_eq!(dim(0xE8B04A, 1.0), 0xE8B04A);
        assert_eq!(dim(0xFF8040, 0.5), 0x804020);
        assert_eq!(dim(0xFFFFFF, 0.0), 0);
    }
}
