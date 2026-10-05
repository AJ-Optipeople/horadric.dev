//! The notification area icon: the one piece of Horadric that is always on
//! screen, even with no sessions and so no tiles. Its menu starts sessions
//! and quits the app.

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

use horadric_core::experience;
use horadric_core::saved::Discord;

use crate::menu::{self, Item};
use crate::screens::{self, Screen};
use crate::theme::Theme;
use crate::{icon, motion, recent};

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
    /// Stand the columns on the screen at this place in the list given.
    Screen(usize),
    /// Draw the whole app in this theme.
    Theme(Theme),
    /// Draw the terminals in this font family, by its place in the list
    /// given.
    Font(usize),
    ToggleAutostart,
    /// Say, or stop saying, when a session starts waiting.
    ToggleNotify,
    /// Play, or stop playing, the loot sounds.
    ToggleSounds,
    /// Show this much on the human's Discord profile.
    Discord(Discord),
    /// Look for a newer release now, and say what was found.
    CheckUpdates,
    /// Install the newer release the menu offered.
    Update,
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

/// Where the Quest log menu's ids start, one per recent project.
const QUEST_LOG: usize = 1000;

/// The tray menu. `autostart` is None when the switch is not offered,
/// `hotkeys` are the shortcuts for the next waiting session and for the
/// catch-up, where they have one, `notify` whether a session that starts
/// waiting says so, `sounds` whether loot drops are heard, and `discord`
/// what the Discord profile is allowed to show.
/// `screens` are offered when there is more than one, with the one named
/// `shown` checked. `update` is a newer release's version, when a check
/// found one. `fonts` are the families the terminals can be drawn in,
/// with `font` checked, and `theme` is the one the app is drawn in. `xp` is the experience counted from git, None
/// until the first count is back.
#[allow(clippy::too_many_arguments)]
pub fn menu(
    recent_projects: &[String],
    autostart: Option<bool>,
    hotkeys: [Option<&str>; 2],
    notify: bool,
    sounds: bool,
    discord: Discord,
    terminal: bool,
    screens: &[Screen],
    shown: Option<&str>,
    update: Option<&str>,
    fonts: &[String],
    font: &str,
    theme: Theme,
    xp: Option<u64>,
) -> Option<Choice> {
    const NEW: usize = 1;
    const QUIT: usize = 2;
    const AUTOSTART: usize = 3;
    const TIDY: usize = 4;
    const NEXT: usize = 5;
    const ARRANGE: usize = 6;
    const RAISE: usize = 7;
    const END_ALL: usize = 8;
    const NOTIFY: usize = 9;
    const STAGE: usize = 10;
    const CHECK: usize = 11;
    const UPDATE: usize = 12;
    const LISTEN: usize = 13;
    const SOUNDS: usize = 14;
    const DISCORD: usize = 20;
    const THEME: usize = 40;
    const SCREEN: usize = 50;
    const RECENT: usize = 100;
    const FONT: usize = 200;
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
    if screens.len() > 1 {
        let lines = screens
            .iter()
            .enumerate()
            .map(|(i, screen)| Item::Action {
                id: SCREEN + i,
                label: screens::label(i + 1, screen),
                checked: Some(screen.name.as_str()) == shown,
            })
            .collect();
        items.push(Item::Submenu("Tiles on screen".into(), lines));
    }
    let lines = Theme::ALL
        .iter()
        .enumerate()
        .map(|(i, &t)| Item::Action {
            id: THEME + i,
            label: t.label().into(),
            checked: t == theme,
        })
        .collect();
    items.push(Item::Submenu("Theme".into(), lines));
    if !fonts.is_empty() {
        // Up to the first Quest log id, which is more families than anyone
        // has installed.
        let lines = fonts
            .iter()
            .take(QUEST_LOG - FONT)
            .enumerate()
            .map(|(i, name)| Item::Action {
                id: FONT + i,
                label: name.clone(),
                checked: name.eq_ignore_ascii_case(font),
            })
            .collect();
        items.push(Item::Submenu("Terminal font".into(), lines));
    }
    items.push(Item::Action {
        id: NOTIFY,
        label: "Notify when a session needs you".into(),
        checked: notify,
    });
    items.push(Item::Action {
        id: SOUNDS,
        label: "Loot sounds".into(),
        checked: sounds,
    });
    // A submenu, not a switch: what a public profile may say is worth a
    // second look at, and the choice between names and none is the point.
    let lines = Discord::ALL
        .iter()
        .enumerate()
        .map(|(i, &d)| Item::Action {
            id: DISCORD + i,
            label: d.label().into(),
            checked: d == discord,
        })
        .collect();
    items.push(Item::Submenu("Show on Discord".into(), lines));
    if let Some(checked) = autostart {
        items.push(Item::Action {
            id: AUTOSTART,
            label: "Start with Windows".into(),
            checked,
        });
    }
    items.push(Item::action(CHECK, "Check for updates"));
    if let Some(version) = update {
        items.push(Item::action(UPDATE, format!("Update to {version}\u{2026}")));
    }
    items.push(Item::Separator);
    items.push(Item::action(END_ALL, "End all sessions"));
    items.push(Item::action(QUIT, "Quit Horadric"));

    match menu::popup(&items)? {
        NEW => Some(Choice::New),
        QUIT => Some(Choice::Quit),
        AUTOSTART => Some(Choice::ToggleAutostart),
        NOTIFY => Some(Choice::ToggleNotify),
        SOUNDS => Some(Choice::ToggleSounds),
        TIDY => Some(Choice::Tidy),
        NEXT => Some(Choice::NextWaiting),
        LISTEN => Some(Choice::Listen),
        ARRANGE => Some(Choice::Arrange),
        RAISE => Some(Choice::Raise),
        STAGE => Some(Choice::ShowStage),
        CHECK => Some(Choice::CheckUpdates),
        UPDATE => Some(Choice::Update),
        END_ALL => Some(Choice::EndAll),
        i if (THEME..SCREEN).contains(&i) => Theme::ALL.get(i - THEME).copied().map(Choice::Theme),
        i if (DISCORD..THEME).contains(&i) => {
            Discord::ALL.get(i - DISCORD).copied().map(Choice::Discord)
        }
        i if i >= QUEST_LOG => recent_projects
            .get(i - QUEST_LOG)
            .map(|p| Choice::QuestLog(p.clone())),
        i if (SCREEN..RECENT).contains(&i) => Some(Choice::Screen(i - SCREEN)),
        i if (FONT..QUEST_LOG).contains(&i) => Some(Choice::Font(i - FONT)),
        i if i >= RECENT => recent_projects
            .get(i - RECENT)
            .map(|p| Choice::Recent(p.clone())),
        _ => None,
    }
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
