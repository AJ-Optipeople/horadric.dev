//! Colours and icons. A hardware control panel in the dark: matte metal
//! faceplates lit from above, sessions as keys that stand up off the plate,
//! lamps that say what each one is doing, and screens sunk into the plate
//! for anything that scrolls. No texture: the reality is in the bevels,
//! the shadows and the light.
//!
//! Colour has two jobs and they never share a place. A phase is a light:
//! a tile's lamp and icon, a waiting key's backlight, the line along a pane
//! header. A project is an accent: the wash down its cluster, the edge of
//! the stage showing it. So a project's colour is never mistaken for a
//! session needing you.
//!
//! A third job, how a session ended, is the colour of its name and only
//! that: Diablo's item colours, printed like a legend, never lit.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};

use horadric_core::rarity::Rarity;
use horadric_core::{Phase, Session, WaitReason};

use crate::files::Change;
use crate::layout::Button;

/// sRGB with straight alpha, 0.0 to 1.0.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Color {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Color {
    pub const fn rgb(hex: u32) -> Self {
        Color {
            r: ((hex >> 16) & 0xff) as f32 / 255.0,
            g: ((hex >> 8) & 0xff) as f32 / 255.0,
            b: (hex & 0xff) as f32 / 255.0,
            a: 1.0,
        }
    }

    pub const fn with_alpha(self, a: f32) -> Self {
        Color { a, ..self }
    }

    /// The same colour with its alpha scaled, for fading something out.
    pub fn fade(self, k: f32) -> Self {
        Color {
            a: self.a * k.clamp(0.0, 1.0),
            ..self
        }
    }

    /// `t` of the way from `self` to `other`, opaque.
    pub fn mix(self, other: Color, t: f32) -> Self {
        let l = |a: f32, b: f32| a + (b - a) * t;
        Color {
            r: l(self.r, other.r),
            g: l(self.g, other.g),
            b: l(self.b, other.b),
            a: 1.0,
        }
    }
}

/// Every colour a theme decides. The look of the app is these and the
/// shapes, and the shapes never change: a theme that wants no bevels makes
/// them transparent.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Palette {
    /// The plate every window is, a shade lighter at the top where the
    /// light falls.
    pub window_bg: Color,
    pub plate_top: Color,
    pub plate_bottom: Color,
    /// A key's face: a session's tile, a button.
    pub surface: Color,
    /// A bay sunk into the plate, where a key is yet to go, and the face of
    /// a key latched down.
    pub well: Color,
    /// The glass of a screen: the files list, the limits.
    pub screen: Color,
    /// A lamp with nothing behind it.
    pub lamp_off: Color,
    pub text: Color,
    pub text_dim: Color,
    /// Printed on the plate: labels, section names.
    pub legend: Color,
    /// The shadow a key casts on the plate.
    pub cast: Color,
    /// The light catching the top edge of anything raised, and the shade
    /// along its bottom.
    pub bevel_light: Color,
    pub bevel_shade: Color,
    /// Inside anything sunk: shade under its top edge, light on its bottom.
    pub hollow_shade: Color,
    pub hollow_light: Color,
    /// A line cut into the plate: its dark groove and the lit edge under it.
    pub engrave_dark: Color,
    pub engrave_light: Color,
    /// Behind a button under the cursor and one held down, laid over
    /// whatever is there, as Windows 11 does it, so it works on any surface.
    pub hover_fill: Color,
    pub press_fill: Color,
    pub working: Color,
    pub waiting: Color,
    pub error: Color,
    pub done: Color,
    pub idle: Color,
    /// The gold of the mark over a quest giver's head in the games, so the
    /// button that asks an agent for quests reads as one at a glance.
    pub quest: Color,
    /// Git change colours, VS Code's ones for a plate this light, so a file
    /// looks the same in the tile as in the editor.
    pub git_modified: Color,
    pub git_added: Color,
    pub git_untracked: Color,
    pub git_deleted: Color,
    pub git_conflict: Color,
    /// The inks of the rarities past normal, which is `text`.
    pub magic: Color,
    pub rare: Color,
    pub set: Color,
    pub unique: Color,
    /// The terminals stay dark in every theme, since near black is what
    /// every agent's own colours are made for. Only the tint follows.
    pub term_bg: Color,
    pub term_fg: Color,
    pub term_cursor: Color,
    pub term_selection: Color,
}

const BLACK: Color = Color::rgb(0x000000);
const WHITE: Color = Color::rgb(0xFFFFFF);

/// A hardware control panel in the dark: matte metal faceplates lit from
/// above, keys that stand up off the plate, screens sunk into it.
const SKEUOMORPH: Palette = Palette {
    window_bg: Color::rgb(0x141518),
    plate_top: Color::rgb(0x191A1E),
    plate_bottom: Color::rgb(0x111214),
    surface: Color::rgb(0x202227),
    well: Color::rgb(0x0C0D0F),
    screen: Color::rgb(0x08090B),
    lamp_off: Color::rgb(0x2A2C32),
    text: Color::rgb(0xE8E9ED),
    text_dim: Color::rgb(0x8F939E),
    legend: Color::rgb(0x6E727C),
    cast: BLACK.with_alpha(0.6),
    bevel_light: WHITE.with_alpha(0.11),
    bevel_shade: BLACK.with_alpha(0.35),
    hollow_shade: BLACK.with_alpha(0.55),
    hollow_light: WHITE.with_alpha(0.05),
    engrave_dark: BLACK.with_alpha(0.5),
    engrave_light: WHITE.with_alpha(0.05),
    hover_fill: WHITE.with_alpha(0.06),
    press_fill: WHITE.with_alpha(0.025),
    working: Color::rgb(0x3DB4FF),
    waiting: Color::rgb(0xFFB224),
    error: Color::rgb(0xFF5D66),
    done: Color::rgb(0x3DD68C),
    idle: Color::rgb(0x6E6882),
    quest: Color::rgb(0xFFD100),
    git_modified: Color::rgb(0xE2C08D),
    git_added: Color::rgb(0x81B88B),
    git_untracked: Color::rgb(0x73C991),
    git_deleted: Color::rgb(0xC74E39),
    git_conflict: Color::rgb(0xE4676B),
    magic: Color::rgb(0x9A9CFF),
    rare: Color::rgb(0xF2E27A),
    set: Color::rgb(0x9BE06A),
    unique: Color::rgb(0xCFAE72),
    term_bg: Color::rgb(0x08090B),
    term_fg: Color::rgb(0xE8E9ED),
    term_cursor: Color::rgb(0xF5F5F7),
    term_selection: Color::rgb(0x1E3A5C),
};

/// Flat design, the Swiss way: white paper, no light and no depth, every
/// edge said by a change of tone alone, and lamps in strong pure colour.
const FLAT: Palette = Palette {
    window_bg: Color::rgb(0xF4F5F7),
    plate_top: Color::rgb(0xF4F5F7),
    plate_bottom: Color::rgb(0xF4F5F7),
    surface: Color::rgb(0xFFFFFF),
    well: Color::rgb(0xE3E6EB),
    screen: Color::rgb(0xFFFFFF),
    lamp_off: Color::rgb(0xD5D9E0),
    text: Color::rgb(0x111318),
    text_dim: Color::rgb(0x5B6270),
    legend: Color::rgb(0x7A818E),
    cast: BLACK.with_alpha(0.0),
    bevel_light: WHITE.with_alpha(0.0),
    bevel_shade: BLACK.with_alpha(0.0),
    hollow_shade: BLACK.with_alpha(0.0),
    hollow_light: WHITE.with_alpha(0.0),
    engrave_dark: BLACK.with_alpha(0.1),
    engrave_light: WHITE.with_alpha(0.0),
    hover_fill: BLACK.with_alpha(0.06),
    press_fill: BLACK.with_alpha(0.03),
    working: Color::rgb(0x0A6CFF),
    waiting: Color::rgb(0xF08C00),
    error: Color::rgb(0xE5243B),
    done: Color::rgb(0x0E9F5A),
    idle: Color::rgb(0x8A8F9C),
    quest: Color::rgb(0xD9A400),
    git_modified: Color::rgb(0x895503),
    git_added: Color::rgb(0x587C0C),
    git_untracked: Color::rgb(0x007100),
    git_deleted: Color::rgb(0xAD0707),
    git_conflict: Color::rgb(0x6C6CC4),
    magic: Color::rgb(0x3F44D6),
    rare: Color::rgb(0x9A7A00),
    set: Color::rgb(0x2F7D14),
    unique: Color::rgb(0xA0522D),
    term_bg: Color::rgb(0x111318),
    term_fg: Color::rgb(0xECEEF2),
    term_cursor: Color::rgb(0xFFFFFF),
    term_selection: Color::rgb(0x1D3D6E),
};

/// Neumorphism, soft UI: one grey clay for plate and keys alike, every
/// shape pressed out of it or into it by a pale light from the top left
/// and a soft shade under it. No colour but the lamps.
const NEUMORPH: Palette = Palette {
    window_bg: Color::rgb(0xE4E8EE),
    plate_top: Color::rgb(0xE7EBF1),
    plate_bottom: Color::rgb(0xE0E5EC),
    surface: Color::rgb(0xE6EAF0),
    well: Color::rgb(0xD9DEE6),
    screen: Color::rgb(0xDDE2E9),
    lamp_off: Color::rgb(0xC9D0DA),
    text: Color::rgb(0x3B4454),
    text_dim: Color::rgb(0x6F7889),
    legend: Color::rgb(0x8A93A3),
    cast: Color::rgb(0x8B9AB2).with_alpha(0.5),
    bevel_light: WHITE.with_alpha(0.95),
    bevel_shade: Color::rgb(0x8B9AB2).with_alpha(0.35),
    hollow_shade: Color::rgb(0x8B9AB2).with_alpha(0.45),
    hollow_light: WHITE.with_alpha(0.9),
    engrave_dark: Color::rgb(0x8B9AB2).with_alpha(0.35),
    engrave_light: WHITE.with_alpha(0.9),
    hover_fill: WHITE.with_alpha(0.45),
    press_fill: Color::rgb(0x8B9AB2).with_alpha(0.12),
    working: Color::rgb(0x4A7CF0),
    waiting: Color::rgb(0xE8930C),
    error: Color::rgb(0xE5485F),
    done: Color::rgb(0x23A876),
    idle: Color::rgb(0x9AA2B1),
    quest: Color::rgb(0xC99A00),
    git_modified: Color::rgb(0x895503),
    git_added: Color::rgb(0x587C0C),
    git_untracked: Color::rgb(0x007100),
    git_deleted: Color::rgb(0xAD0707),
    git_conflict: Color::rgb(0x6C6CC4),
    magic: Color::rgb(0x4247C9),
    rare: Color::rgb(0x8F7400),
    set: Color::rgb(0x3B7F24),
    unique: Color::rgb(0xA0522D),
    term_bg: Color::rgb(0x2A303B),
    term_fg: Color::rgb(0xE6EAF0),
    term_cursor: Color::rgb(0xFFFFFF),
    term_selection: Color::rgb(0x46546E),
};

/// Brutalism: the raw material and nothing to soften it. Pure black, pure
/// white, hard grooves, no gradient, and lamps at full saturation.
const BRUTAL: Palette = Palette {
    window_bg: Color::rgb(0x000000),
    plate_top: Color::rgb(0x000000),
    plate_bottom: Color::rgb(0x000000),
    surface: Color::rgb(0x1A1A1A),
    well: Color::rgb(0x000000),
    screen: Color::rgb(0x000000),
    lamp_off: Color::rgb(0x333333),
    text: Color::rgb(0xFFFFFF),
    text_dim: Color::rgb(0xB0B0B0),
    legend: Color::rgb(0x8C8C8C),
    cast: BLACK.with_alpha(0.0),
    bevel_light: WHITE.with_alpha(0.35),
    bevel_shade: BLACK.with_alpha(0.0),
    hollow_shade: BLACK.with_alpha(0.0),
    hollow_light: WHITE.with_alpha(0.35),
    engrave_dark: WHITE.with_alpha(0.3),
    engrave_light: WHITE.with_alpha(0.0),
    hover_fill: WHITE.with_alpha(0.14),
    press_fill: WHITE.with_alpha(0.07),
    working: Color::rgb(0x00A3FF),
    waiting: Color::rgb(0xFFE600),
    error: Color::rgb(0xFF1F1F),
    done: Color::rgb(0x00FF66),
    idle: Color::rgb(0x8C8C8C),
    quest: Color::rgb(0xFF9900),
    git_modified: Color::rgb(0xE2C08D),
    git_added: Color::rgb(0x81B88B),
    git_untracked: Color::rgb(0x73C991),
    git_deleted: Color::rgb(0xC74E39),
    git_conflict: Color::rgb(0xE4676B),
    magic: Color::rgb(0x8A8CFF),
    rare: Color::rgb(0xFFF59A),
    set: Color::rgb(0x9BE06A),
    unique: Color::rgb(0xD9A35B),
    term_bg: Color::rgb(0x000000),
    term_fg: Color::rgb(0xFFFFFF),
    term_cursor: Color::rgb(0xFFFFFF),
    term_selection: Color::rgb(0x2A2A2A),
};

/// Glassmorphism: frosted panes over a deep night sky, their faces a pale
/// wash of the light behind them, with bright rims where the glass is cut.
const GLASS: Palette = Palette {
    window_bg: Color::rgb(0x111A33),
    plate_top: Color::rgb(0x18234A),
    plate_bottom: Color::rgb(0x0D1328),
    surface: Color::rgb(0x26335C),
    well: Color::rgb(0x0B1124),
    screen: Color::rgb(0x0A1020),
    lamp_off: Color::rgb(0x34416B),
    text: Color::rgb(0xF0F4FF),
    text_dim: Color::rgb(0xA3B0D6),
    legend: Color::rgb(0x7F8DB8),
    cast: Color::rgb(0x02040C).with_alpha(0.55),
    bevel_light: WHITE.with_alpha(0.28),
    bevel_shade: WHITE.with_alpha(0.04),
    hollow_shade: BLACK.with_alpha(0.4),
    hollow_light: WHITE.with_alpha(0.14),
    engrave_dark: WHITE.with_alpha(0.08),
    engrave_light: WHITE.with_alpha(0.08),
    hover_fill: WHITE.with_alpha(0.1),
    press_fill: WHITE.with_alpha(0.05),
    working: Color::rgb(0x5CD0FF),
    waiting: Color::rgb(0xFFB547),
    error: Color::rgb(0xFF6B81),
    done: Color::rgb(0x4CE6A8),
    idle: Color::rgb(0x8890B5),
    quest: Color::rgb(0xFFD84D),
    git_modified: Color::rgb(0xE2C08D),
    git_added: Color::rgb(0x81B88B),
    git_untracked: Color::rgb(0x73C991),
    git_deleted: Color::rgb(0xC74E39),
    git_conflict: Color::rgb(0xE4676B),
    magic: Color::rgb(0xA9AAFF),
    rare: Color::rgb(0xF2E27A),
    set: Color::rgb(0xA6E07A),
    unique: Color::rgb(0xD8B57A),
    term_bg: Color::rgb(0x0A1020),
    term_fg: Color::rgb(0xF0F4FF),
    term_cursor: Color::rgb(0xFFFFFF),
    term_selection: Color::rgb(0x2B4580),
};

/// Winamp's classic skin: brushed grey-blue chrome with hard bevels like
/// a Windows 95 button, a black LCD with green text, a gold title bar for
/// the legends, and lamps in the colours of the spectrum analyser.
const WINAMP: Palette = Palette {
    window_bg: Color::rgb(0x26263A),
    plate_top: Color::rgb(0x34344E),
    plate_bottom: Color::rgb(0x1E1E2E),
    surface: Color::rgb(0x34344C),
    well: Color::rgb(0x0A0A12),
    screen: Color::rgb(0x000000),
    lamp_off: Color::rgb(0x2E3A2E),
    text: Color::rgb(0x30FF30),
    text_dim: Color::rgb(0x9AB89A),
    legend: Color::rgb(0xC8B464),
    cast: BLACK.with_alpha(0.7),
    bevel_light: WHITE.with_alpha(0.32),
    bevel_shade: BLACK.with_alpha(0.65),
    hollow_shade: BLACK.with_alpha(0.7),
    hollow_light: WHITE.with_alpha(0.18),
    engrave_dark: BLACK.with_alpha(0.6),
    engrave_light: WHITE.with_alpha(0.12),
    hover_fill: WHITE.with_alpha(0.08),
    press_fill: WHITE.with_alpha(0.03),
    working: Color::rgb(0x3AA0FF),
    waiting: Color::rgb(0xFFC800),
    error: Color::rgb(0xFF2A1A),
    done: Color::rgb(0x00C8A0),
    idle: Color::rgb(0x7878A0),
    quest: Color::rgb(0xFFE14D),
    git_modified: Color::rgb(0xE2C08D),
    git_added: Color::rgb(0x81B88B),
    git_untracked: Color::rgb(0x73C991),
    git_deleted: Color::rgb(0xC74E39),
    git_conflict: Color::rgb(0xE4676B),
    magic: Color::rgb(0x9A9CFF),
    rare: Color::rgb(0xF2E27A),
    set: Color::rgb(0xB0E07A),
    unique: Color::rgb(0xD8A86A),
    term_bg: Color::rgb(0x000000),
    term_fg: Color::rgb(0x28E828),
    term_cursor: Color::rgb(0x30FF30),
    term_selection: Color::rgb(0x1E3A7A),
};

/// The Matrix: the code raining down a black screen. Everything is that
/// green on black, rims glow instead of catching light, and the lamps are
/// the pills, blue while it works and red when it fails.
const MATRIX: Palette = Palette {
    window_bg: Color::rgb(0x000000),
    plate_top: Color::rgb(0x020A04),
    plate_bottom: Color::rgb(0x000000),
    surface: Color::rgb(0x03140A),
    well: Color::rgb(0x000000),
    screen: Color::rgb(0x000000),
    lamp_off: Color::rgb(0x0A2412),
    text: Color::rgb(0x00FF41),
    text_dim: Color::rgb(0x00A82B),
    legend: Color::rgb(0x0D7A2B),
    cast: BLACK.with_alpha(0.0),
    bevel_light: Color::rgb(0x00FF41).with_alpha(0.22),
    bevel_shade: BLACK.with_alpha(0.0),
    hollow_shade: BLACK.with_alpha(0.5),
    hollow_light: Color::rgb(0x00FF41).with_alpha(0.12),
    engrave_dark: Color::rgb(0x00FF41).with_alpha(0.16),
    engrave_light: WHITE.with_alpha(0.0),
    hover_fill: Color::rgb(0x00FF41).with_alpha(0.1),
    press_fill: Color::rgb(0x00FF41).with_alpha(0.04),
    working: Color::rgb(0x2E7BFF),
    waiting: Color::rgb(0xFFC400),
    error: Color::rgb(0xFF2B2B),
    done: Color::rgb(0x7CFFB0),
    idle: Color::rgb(0x2F6B40),
    quest: Color::rgb(0xE8FFE8),
    git_modified: Color::rgb(0xE2C08D),
    git_added: Color::rgb(0x81B88B),
    git_untracked: Color::rgb(0x73C991),
    git_deleted: Color::rgb(0xC74E39),
    git_conflict: Color::rgb(0xE4676B),
    magic: Color::rgb(0x9A9CFF),
    rare: Color::rgb(0xF2E27A),
    set: Color::rgb(0xC8FF5A),
    unique: Color::rgb(0xCFAE72),
    term_bg: Color::rgb(0x000000),
    term_fg: Color::rgb(0x00FF41),
    term_cursor: Color::rgb(0xB8FFC8),
    term_selection: Color::rgb(0x003B12),
};

/// A look for the whole app, picked from the tray.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Theme {
    Skeuomorph,
    Flat,
    Neumorph,
    Brutal,
    Glass,
    Winamp,
    Matrix,
}

impl Theme {
    pub const ALL: [Theme; 7] = [
        Theme::Skeuomorph,
        Theme::Flat,
        Theme::Neumorph,
        Theme::Brutal,
        Theme::Glass,
        Theme::Winamp,
        Theme::Matrix,
    ];

    pub fn palette(self) -> &'static Palette {
        match self {
            Theme::Skeuomorph => &SKEUOMORPH,
            Theme::Flat => &FLAT,
            Theme::Neumorph => &NEUMORPH,
            Theme::Brutal => &BRUTAL,
            Theme::Glass => &GLASS,
            Theme::Winamp => &WINAMP,
            Theme::Matrix => &MATRIX,
        }
    }

    /// Its name in the tray menu.
    pub fn label(self) -> &'static str {
        match self {
            Theme::Skeuomorph => "Skeuomorphism",
            Theme::Flat => "Flat",
            Theme::Neumorph => "Neumorphism",
            Theme::Brutal => "Brutalism",
            Theme::Glass => "Glass",
            Theme::Winamp => "Winamp",
            Theme::Matrix => "The Matrix",
        }
    }

    /// Its name in state.json, which never changes once given.
    pub fn key(self) -> &'static str {
        match self {
            Theme::Skeuomorph => "skeuomorph",
            Theme::Flat => "flat",
            Theme::Neumorph => "neumorph",
            Theme::Brutal => "brutal",
            Theme::Glass => "glass",
            Theme::Winamp => "winamp",
            Theme::Matrix => "matrix",
        }
    }

    /// The theme saved as `key`, the first one for none or one this build
    /// does not know.
    pub fn from_key(key: Option<&str>) -> Theme {
        Theme::ALL
            .into_iter()
            .find(|t| Some(t.key()) == key)
            .unwrap_or(Theme::Skeuomorph)
    }

    /// What state.json keeps: nothing for the default, so a file from
    /// before themes and one that never picked one read alike.
    pub fn saved(self) -> Option<String> {
        (self != Theme::Skeuomorph).then(|| self.key().to_string())
    }
}

/// The theme every window draws in, as its place in [`Theme::ALL`]. An
/// atomic, not a thread local, so a window drawn on any thread agrees.
static CURRENT: AtomicUsize = AtomicUsize::new(0);

pub fn current() -> Theme {
    Theme::ALL[CURRENT.load(Ordering::Relaxed) % Theme::ALL.len()]
}

/// Draws everything in `t` from the next paint on. The caller repaints.
pub fn set(t: Theme) {
    let i = Theme::ALL.iter().position(|&x| x == t).unwrap_or(0);
    CURRENT.store(i, Ordering::Relaxed);
}

pub fn palette() -> &'static Palette {
    current().palette()
}

macro_rules! colours {
    ($($name:ident),* $(,)?) => {
        $(
            pub fn $name() -> Color {
                palette().$name
            }
        )*
    };
}

colours!(
    window_bg,
    plate_top,
    plate_bottom,
    surface,
    well,
    screen,
    lamp_off,
    text,
    text_dim,
    legend,
    cast,
    bevel_light,
    bevel_shade,
    hollow_shade,
    hollow_light,
    engrave_dark,
    engrave_light,
    hover_fill,
    press_fill,
    working,
    waiting,
    error,
    done,
    idle,
    quest,
    git_modified,
    git_added,
    git_untracked,
    git_deleted,
    git_conflict,
);

pub fn change_color(change: Change) -> Color {
    match change {
        Change::Modified => git_modified(),
        Change::Added => git_added(),
        Change::Untracked | Change::Renamed => git_untracked(),
        Change::Deleted => git_deleted(),
        Change::Conflict => git_conflict(),
    }
}

/// The fill behind a button and the colour of its glyph. Pressed dims back
/// below hover, which is what makes the press read as a push.
pub fn button_look(b: Button) -> (Option<Color>, Color) {
    match b {
        Button::Idle => (None, text_dim()),
        Button::Hover => (Some(hover_fill()), text()),
        Button::Pressed => (Some(press_fill()), text_dim()),
    }
}

/// Project colours: distinct from each other and from every phase colour,
/// so an accent never reads as a state. Soft enough to sit beside text.
/// Saved by their place here, so new ones go on the end.
pub const ACCENTS: [(Color, &str); 8] = [
    (Color::rgb(0xA78BFA), "Violet"),
    (Color::rgb(0x818CF8), "Indigo"),
    (Color::rgb(0xE879F9), "Orchid"),
    (Color::rgb(0xF472B6), "Pink"),
    (Color::rgb(0x2DD4BF), "Teal"),
    (Color::rgb(0xBEF264), "Lime"),
    (Color::rgb(0xE0976B), "Copper"),
    (Color::rgb(0xFDA4AF), "Rose"),
];

thread_local! {
    /// Each project's colour once given, by project key, as its place in
    /// [`ACCENTS`]. Kept for good, since a project is known by its colour.
    static GIVEN: RefCell<BTreeMap<String, usize>> = const { RefCell::new(BTreeMap::new()) };
}

/// A project's colour: the one it was given, or the one its key hashes to
/// until it is given one.
pub fn accent(key: &str) -> Color {
    ACCENTS[accent_index(key)].0
}

/// A project's colour as its place in [`ACCENTS`].
pub fn accent_index(key: &str) -> usize {
    GIVEN
        .with(|g| g.borrow().get(key).copied())
        .filter(|&i| i < ACCENTS.len())
        .unwrap_or_else(|| hashed(key))
}

/// Gives a project with no colour yet the one fewest of the `open`
/// projects wear, so projects side by side stand apart. True when it was
/// given one.
pub fn give_accent(key: &str, open: &[&str]) -> bool {
    if GIVEN.with(|g| g.borrow().contains_key(key)) {
        return false;
    }
    let worn: Vec<usize> = open
        .iter()
        .filter(|&&k| k != key)
        .map(|k| accent_index(k))
        .collect();
    let i = least_worn(hashed(key), &worn);
    GIVEN.with(|g| g.borrow_mut().insert(key.to_string(), i));
    true
}

/// Paints a project in the colour picked from its menu.
pub fn set_accent(key: &str, i: usize) {
    GIVEN.with(|g| g.borrow_mut().insert(key.to_string(), i));
}

/// Every project's colour, to save.
pub fn accents() -> BTreeMap<String, usize> {
    GIVEN.with(|g| g.borrow().clone())
}

/// The colours saved last time.
pub fn set_accents(saved: &BTreeMap<String, usize>) {
    GIVEN.with(|g| *g.borrow_mut() = saved.clone());
}

/// The colour the fewest of `worn` are, ties going to `first` and the
/// ones after it, so a project keeps the colour its key hashes to when no
/// one else wears it.
pub fn least_worn(first: usize, worn: &[usize]) -> usize {
    let n = ACCENTS.len();
    (0..n)
        .map(|k| (first + k) % n)
        .min_by_key(|&i| worn.iter().filter(|&&w| w == i).count())
        .unwrap_or(first % n)
}

/// The colour a key hashes to, the same every time.
fn hashed(key: &str) -> usize {
    // FNV-1a: tiny, and stable across runs and builds, unlike std's hasher.
    let hash = key.bytes().fold(0x811c_9dc5u32, |h, b| {
        (h ^ b as u32).wrapping_mul(0x0100_0193)
    });
    hash as usize % ACCENTS.len()
}

/// A plain terminal's tile, and the button that opens one.
pub const SHELL_ICON: char = '\u{E756}';

/// An SSH terminal's tile: a terminal on another machine.
pub const SSH_ICON: char = '\u{E968}';

/// A background session's mark: Claude Code's daemon holds it, not a
/// terminal of ours.
pub const BACKGROUND_ICON: char = '\u{E753}';

/// The Segoe Fluent Icons glyph for a session: the tool it is in while it
/// works, otherwise what its phase is.
/// The glyph on a session's tile: what its agent is doing, or what kind
/// of terminal it is while it does nothing.
pub fn tile_icon(s: &Session) -> char {
    match s.phase {
        Phase::Idle if s.ssh.is_some() => SSH_ICON,
        Phase::Idle if s.shell => SHELL_ICON,
        _ => icon(&s.phase, s.tool.as_deref()),
    }
}

pub fn icon(phase: &Phase, tool: Option<&str>) -> char {
    match phase {
        Phase::Working => tool.map_or('\u{EA80}', tool_icon),
        Phase::Waiting(WaitReason::Permission) => '\u{E72E}',
        Phase::Waiting(WaitReason::Input) => '\u{E9CE}',
        Phase::Waiting(WaitReason::Dialog) => '\u{E8BD}',
        Phase::Waiting(WaitReason::Error(_)) => '\u{E783}',
        Phase::Done => '\u{E73E}',
        Phase::Paused => '\u{E769}',
        Phase::Ended => '\u{E7E8}',
        Phase::Idle => '\u{E708}',
    }
}

/// A Claude Code tool as an icon. Anything unknown is a wrench.
pub fn tool_icon(tool: &str) -> char {
    if tool.starts_with("mcp__") {
        return '\u{E950}';
    }
    match tool {
        "Bash" | "PowerShell" | "BashOutput" | "KillShell" | "Monitor" => '\u{E756}',
        "Read" => '\u{E8A5}',
        "Edit" | "MultiEdit" | "NotebookEdit" => '\u{E70F}',
        "Write" => '\u{E70B}',
        "Grep" | "Glob" | "ToolSearch" => '\u{E721}',
        "WebFetch" | "WebSearch" => '\u{E774}',
        "Task" | "Agent" | "SendMessage" => '\u{E716}',
        "TodoWrite" | "TaskCreate" | "TaskUpdate" => '\u{E9D5}',
        "Skill" | "Workflow" => '\u{E945}',
        "LSP" => '\u{E943}',
        _ => '\u{E90F}',
    }
}

/// How strongly a tile's edge glows at rest, by phase. Zero is no edge.
pub fn edge_strength(phase: &Phase) -> f32 {
    match phase {
        Phase::Waiting(_) => 0.34,
        Phase::Done => 0.10,
        Phase::Working => 0.09,
        Phase::Idle | Phase::Ended | Phase::Paused => 0.0,
    }
}

/// How far a session's key stands off the plate, by phase. One is a key
/// at rest. A session that can no longer act is latched down.
pub fn depth(phase: &Phase) -> f32 {
    match phase {
        Phase::Waiting(_) | Phase::Working | Phase::Done | Phase::Idle => 1.0,
        Phase::Paused | Phase::Ended => 0.25,
    }
}

/// How far off the plate the key of the session with the keyboard stands:
/// latched in level with it, as the one button held down on a tape deck.
pub const LATCHED: f32 = 0.0;

/// How far a session's key stands off the plate: by phase, and latched
/// down while its pane on the stage has the keyboard.
pub fn key_depth(phase: &Phase, selected: bool) -> f32 {
    if selected {
        LATCHED
    } else {
        depth(phase)
    }
}

/// How bright a session's lamp burns at rest, by phase. Off is zero: a
/// session doing nothing has a dark lamp, so a lit one always means
/// something.
pub fn lamp(phase: &Phase) -> f32 {
    match phase {
        Phase::Waiting(_) => 1.0,
        Phase::Working => 0.85,
        Phase::Done => 0.7,
        Phase::Idle | Phase::Ended | Phase::Paused => 0.0,
    }
}

/// A session that cannot act on its own right now fades back, so the ones
/// that can, or that need you, come forward.
pub fn presence(phase: &Phase) -> f32 {
    match phase {
        Phase::Paused | Phase::Ended => 0.55,
        Phase::Idle => 0.8,
        _ => 1.0,
    }
}

/// The full strength colour for a phase.
pub fn phase_color(phase: &Phase) -> Color {
    match phase {
        Phase::Working => working(),
        Phase::Waiting(WaitReason::Error(_)) => error(),
        Phase::Waiting(_) => waiting(),
        Phase::Done => done(),
        Phase::Idle | Phase::Ended | Phase::Paused => idle(),
    }
}

/// A session's key face. Waiting is backlit in its colour, since it needs
/// you and has to be seen from across the room. The rest leave the lamp to
/// say what they do, and one that has stopped is latched down, darker.
pub fn phase_fill(phase: &Phase) -> Color {
    match phase {
        Phase::Waiting(_) => surface().mix(phase_color(phase), 0.22),
        Phase::Working | Phase::Done | Phase::Idle => surface(),
        Phase::Ended | Phase::Paused => well().mix(surface(), 0.5),
    }
}

/// The ink of a session's name for how it ended. Paler than the lamps and
/// drawn as text, so a yellow name never reads as a waiting lamp, and each
/// leans off its nearest phase colour: magic towards violet, rare towards
/// lemon, set towards leaf.
pub fn rarity_color(r: Rarity) -> Color {
    match r {
        Rarity::Normal => text(),
        Rarity::Magic => palette().magic,
        Rarity::Rare => palette().rare,
        Rarity::Set => palette().set,
        Rarity::Unique => palette().unique,
    }
}

/// How much of a limit or a context window is used, in percent, as a
/// colour: calm while there is room, amber getting close, red at the end.
pub fn fullness_color(percent: f32) -> Color {
    if percent >= 90.0 {
        error()
    } else if percent >= 75.0 {
        waiting()
    } else {
        working()
    }
}

/// What the age line says before the duration: "waiting 40 min".
pub fn phase_verb(phase: &Phase) -> &'static str {
    match phase {
        Phase::Idle => "idle",
        Phase::Working => "working",
        Phase::Waiting(WaitReason::Permission) => "needs permission",
        Phase::Waiting(WaitReason::Input) => "asked you",
        Phase::Waiting(WaitReason::Dialog) => "dialog open",
        Phase::Waiting(WaitReason::Error(_)) => "failed",
        Phase::Done => "done",
        Phase::Ended => "ended",
        Phase::Paused => "paused",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn distance(a: Color, b: Color) -> f32 {
        ((a.r - b.r).powi(2) + (a.g - b.g).powi(2) + (a.b - b.b).powi(2)).sqrt()
    }

    #[test]
    fn rarities_stand_apart_from_each_other_and_from_the_lamps() {
        let all = [
            Rarity::Normal,
            Rarity::Magic,
            Rarity::Rare,
            Rarity::Set,
            Rarity::Unique,
        ];
        assert_eq!(rarity_color(Rarity::Normal), text());
        for (i, a) in all.iter().enumerate() {
            for b in &all[i + 1..] {
                assert!(
                    distance(rarity_color(*a), rarity_color(*b)) > 0.2,
                    "{a:?} {b:?}"
                );
            }
            for lamp in [working(), waiting(), done(), error()] {
                assert!(distance(rarity_color(*a), lamp) > 0.2, "{a:?}");
            }
        }
    }

    #[test]
    fn every_theme_keeps_its_lamps_apart_from_the_accents_and_the_rarities() {
        for t in Theme::ALL {
            let p = t.palette();
            let lamps = [p.working, p.waiting, p.error, p.done, p.idle];
            for (a, _) in ACCENTS {
                for l in lamps {
                    let d = (a.r - l.r).abs() + (a.g - l.g).abs() + (a.b - l.b).abs();
                    assert!(d > 0.25, "{t:?}: {a:?} is too close to {l:?}");
                }
            }
            let inks = [p.text, p.magic, p.rare, p.set, p.unique];
            for (i, a) in inks.iter().enumerate() {
                for b in &inks[i + 1..] {
                    assert!(distance(*a, *b) > 0.2, "{t:?}: {a:?} {b:?}");
                }
                for l in &lamps[..4] {
                    assert!(distance(*a, *l) > 0.2, "{t:?}: {a:?} {l:?}");
                }
            }
        }
    }

    #[test]
    fn every_theme_reads_text_on_its_plate_and_its_terminal() {
        let luma = |c: Color| 0.2126 * c.r + 0.7152 * c.g + 0.0722 * c.b;
        for t in Theme::ALL {
            let p = t.palette();
            for back in [p.window_bg, p.surface, p.screen] {
                assert!(
                    (luma(p.text) - luma(back)).abs() > 0.5,
                    "{t:?}: text on {back:?}"
                );
            }
            assert!(luma(p.term_bg) < 0.2, "{t:?}: the terminal stays dark");
            assert!(luma(p.term_fg) - luma(p.term_bg) > 0.6, "{t:?}");
            assert!(p.hover_fill.a > p.press_fill.a, "{t:?}");
        }
    }

    #[test]
    fn the_themes_differ_and_are_saved_by_a_key_that_reads_back() {
        for (i, a) in Theme::ALL.iter().enumerate() {
            for b in &Theme::ALL[i + 1..] {
                assert_ne!(a.palette(), b.palette());
                assert_ne!(a.label(), b.label());
            }
            assert_eq!(Theme::from_key(a.saved().as_deref()), *a);
        }
        assert_eq!(Theme::Skeuomorph.saved(), None);
        assert_eq!(Theme::from_key(None), Theme::Skeuomorph);
        assert_eq!(Theme::from_key(Some("vaporwave")), Theme::Skeuomorph);
        assert_eq!(Theme::from_key(Some("glass")), Theme::Glass);
    }

    #[test]
    fn mix_ends_are_the_colours() {
        let a = Color::rgb(0x000000);
        let b = Color::rgb(0xFFFFFF);
        assert_eq!(a.mix(b, 0.0), a);
        assert_eq!(a.mix(b, 1.0), b);
        assert_eq!(a.mix(b, 0.5).g, 0.5);
    }

    #[test]
    fn a_button_brightens_on_hover_and_sinks_back_when_pressed() {
        let (idle, idle_ink) = button_look(Button::Idle);
        let (hover, hover_ink) = button_look(Button::Hover);
        let (press, press_ink) = button_look(Button::Pressed);
        assert!(idle.is_none());
        assert!(hover.unwrap().a > press.unwrap().a);
        assert_eq!(hover_ink, text());
        assert_eq!(idle_ink, press_ink);
    }

    #[test]
    fn fuller_turns_amber_then_red() {
        assert_eq!(fullness_color(10.0), working());
        assert_eq!(fullness_color(75.0), waiting());
        assert_eq!(fullness_color(99.5), error());
        assert_eq!(fullness_color(140.0), error());
    }

    #[test]
    fn only_waiting_is_backlit_and_a_stopped_key_is_latched_down() {
        for p in [Phase::Working, Phase::Done, Phase::Idle] {
            assert_eq!(phase_fill(&p), surface());
        }
        assert_ne!(phase_fill(&Phase::Waiting(WaitReason::Input)), surface());
        for p in [Phase::Ended, Phase::Paused] {
            assert!(depth(&p) < depth(&Phase::Idle));
            assert!(depth(&p) > 0.0);
        }
    }

    #[test]
    fn the_key_with_the_keyboard_latches_down_below_every_other() {
        let all = [
            Phase::Working,
            Phase::Waiting(WaitReason::Input),
            Phase::Done,
            Phase::Idle,
            Phase::Paused,
            Phase::Ended,
        ];
        for p in &all {
            assert_eq!(key_depth(p, false), depth(p));
            assert_eq!(key_depth(p, true), LATCHED);
        }
        let lowest = all.iter().map(depth).fold(f32::MAX, f32::min);
        assert!(key_depth(&Phase::Working, true) < lowest);
    }

    #[test]
    fn a_lamp_burns_only_while_there_is_something_to_say() {
        let waiting = lamp(&Phase::Waiting(WaitReason::Permission));
        assert!(waiting > lamp(&Phase::Working));
        assert!(lamp(&Phase::Working) > 0.0 && lamp(&Phase::Done) > 0.0);
        for p in [Phase::Idle, Phase::Ended, Phase::Paused] {
            assert_eq!(lamp(&p), 0.0);
        }
    }

    #[test]
    fn a_project_keeps_its_accent_and_projects_spread_over_them() {
        assert_eq!(accent("c:/code/horadric"), accent("c:/code/horadric"));
        let keys = ["a", "b", "c", "horadric", "purrch", "opticore", "x/y", "zz"];
        let distinct: std::collections::HashSet<usize> = keys.iter().map(|k| hashed(k)).collect();
        assert!(
            distinct.len() >= 4,
            "eight keys landed on {}",
            distinct.len()
        );
    }

    #[test]
    fn a_new_project_takes_the_colour_fewest_wear() {
        assert_eq!(least_worn(3, &[]), 3, "its own when no one wears it");
        assert_eq!(least_worn(3, &[0, 1, 2]), 3);
        assert_eq!(least_worn(3, &[3]), 4, "the next when taken");
        assert_eq!(least_worn(7, &[7]), 0, "round past the end");
        let all: Vec<usize> = (0..ACCENTS.len()).collect();
        assert_eq!(least_worn(2, &all), 2, "shared once every one is worn");
        let mut twice = all.clone();
        twice.extend([2, 3]);
        assert_eq!(least_worn(2, &twice), 4);
    }

    #[test]
    fn open_projects_are_given_colours_apart_and_keep_them() {
        set_accents(&BTreeMap::new());
        let keys: Vec<String> = (0..ACCENTS.len()).map(|i| format!("p{i}")).collect();
        let mut open: Vec<&str> = Vec::new();
        for k in &keys {
            assert!(give_accent(k, &open));
            open.push(k);
        }
        let worn: std::collections::HashSet<usize> = keys.iter().map(|k| accent_index(k)).collect();
        assert_eq!(worn.len(), ACCENTS.len());
        let before = accent_index("p0");
        assert!(!give_accent("p0", &[]));
        assert_eq!(accent_index("p0"), before);
        set_accent("p0", 5);
        assert_eq!(accent_index("p0"), 5);
        assert_eq!(accents().get("p0"), Some(&5));
    }

    #[test]
    fn no_accent_is_a_phase_colour() {
        for (a, _) in ACCENTS {
            for p in [working(), waiting(), error(), done(), idle()] {
                let d = (a.r - p.r).abs() + (a.g - p.g).abs() + (a.b - p.b).abs();
                assert!(d > 0.25, "{a:?} is too close to {p:?}");
            }
        }
    }

    #[test]
    fn the_icon_follows_the_tool_while_working_and_the_phase_otherwise() {
        assert_eq!(icon(&Phase::Working, Some("Bash")), '\u{E756}');
        assert_eq!(icon(&Phase::Working, Some("PowerShell")), '\u{E756}');
        assert_eq!(
            icon(&Phase::Working, Some("mcp__github__search")),
            '\u{E950}'
        );
        assert_eq!(icon(&Phase::Working, Some("SomethingNew")), '\u{E90F}');
        assert_eq!(icon(&Phase::Working, None), '\u{EA80}');
        // A stale tool never shows once the turn is over.
        assert_eq!(icon(&Phase::Done, Some("Bash")), '\u{E73E}');
        assert_eq!(
            icon(&Phase::Waiting(WaitReason::Permission), Some("Bash")),
            '\u{E72E}'
        );
    }

    #[test]
    fn waiting_glows_brightest_and_quiet_sessions_recede() {
        let waiting = edge_strength(&Phase::Waiting(WaitReason::Input));
        assert!(waiting > edge_strength(&Phase::Working));
        assert!(waiting > edge_strength(&Phase::Done));
        assert_eq!(edge_strength(&Phase::Idle), 0.0);
        assert_eq!(presence(&Phase::Working), 1.0);
        assert!(presence(&Phase::Paused) < presence(&Phase::Idle));
    }

    #[test]
    fn fading_scales_alpha_and_nothing_else() {
        let c = working().with_alpha(0.5).fade(0.5);
        assert_eq!(c.a, 0.25);
        assert_eq!(c.r, working().r);
        assert_eq!(working().fade(2.0).a, 1.0);
    }

    #[test]
    fn waiting_is_tinted_hardest() {
        let dist = |c: Color| (c.r - surface().r).abs() + (c.b - surface().b).abs();
        let waiting = phase_fill(&Phase::Waiting(WaitReason::Input));
        let working = phase_fill(&Phase::Working);
        assert!(dist(waiting) > dist(working));
        assert!(waiting.r > waiting.b, "amber, not blue");
    }
}
