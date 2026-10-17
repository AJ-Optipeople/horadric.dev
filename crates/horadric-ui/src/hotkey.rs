//! The app's three global shortcuts: what each does, the chord it has
//! until the human sets another, and a chord as state.json keeps it and
//! the Settings window shows it ("Ctrl+Alt+Space"), both the same text.
//!
//! A new chord is set by pressing it. [`press`] turns one key press, with
//! the modifiers held, into what that press means while the window
//! listens. No Win32 here: keys are virtual key codes as numbers.

/// What a shortcut does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Action {
    /// Brings up the session that has waited on you longest.
    Next,
    /// The catch-up on demand, "Stay a while and listen".
    Listen,
    /// Stops every drive of Warriv's at once.
    Stop,
    /// The list of Claude accounts to switch to.
    Account,
}

/// How many global shortcuts there are.
pub const COUNT: usize = 4;

impl Action {
    pub const ALL: [Action; COUNT] = [Action::Next, Action::Listen, Action::Stop, Action::Account];

    /// Its place in [`Action::ALL`], which is also its hotkey id less one.
    pub fn index(self) -> usize {
        self as usize
    }

    /// The hotkey id `RegisterHotKey` is given, never 0.
    pub fn id(self) -> i32 {
        self.index() as i32 + 1
    }

    pub fn from_id(id: i32) -> Option<Action> {
        Action::ALL.into_iter().find(|a| a.id() == id)
    }

    /// Its key in state.json.
    pub fn key(self) -> &'static str {
        match self {
            Action::Next => "next",
            Action::Listen => "listen",
            Action::Stop => "stop",
            Action::Account => "account",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Action::Next => "Next waiting session",
            Action::Listen => "Stay a while and listen",
            Action::Stop => "Stop Warriv",
            Action::Account => "Switch account",
        }
    }

    /// The chord it has until another is set. A dev instance adds Shift,
    /// so it never fights the installed Horadric for it. AltGr is
    /// Ctrl+Alt, and AltGr+Space or AltGr+K types nothing on the layouts
    /// that matter here, where AltGr+A types a letter in Polish.
    pub fn default_chord(self, dev: bool) -> Chord {
        let key = match self {
            Action::Next => VK_SPACE,
            Action::Listen => VK_HOME,
            Action::Stop => u16::from(b'W'),
            Action::Account => u16::from(b'K'),
        };
        Chord {
            mods: Mods {
                ctrl: true,
                alt: true,
                shift: dev,
                win: false,
            },
            key,
        }
    }
}

/// The modifiers held with a key.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Mods {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub win: bool,
}

impl Mods {
    /// The flags `RegisterHotKey` takes: MOD_ALT, MOD_CONTROL, MOD_SHIFT
    /// and MOD_WIN.
    pub fn flags(self) -> u32 {
        [
            (self.alt, 1),
            (self.ctrl, 2),
            (self.shift, 4),
            (self.win, 8),
        ]
        .iter()
        .filter(|(on, _)| *on)
        .map(|(_, bit)| bit)
        .sum()
    }

    /// Shift alone is not enough: Shift and a letter is typing.
    fn holds_one(self) -> bool {
        self.ctrl || self.alt || self.win
    }
}

/// A key and the modifiers held with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Chord {
    pub mods: Mods,
    /// A virtual key code.
    pub key: u16,
}

const VK_BACK: u16 = 0x08;
const VK_ESCAPE: u16 = 0x1B;
const VK_SPACE: u16 = 0x20;
const VK_HOME: u16 = 0x24;

/// The keys a chord may end in that are not a letter, a digit or an F key,
/// by name. Punctuation is left out: where it sits changes with the layout,
/// so its name would lie on some keyboards.
const NAMED: [(u16, &str); 13] = [
    (VK_SPACE, "Space"),
    (0x0D, "Enter"),
    (0x13, "Pause"),
    (0x21, "PageUp"),
    (0x22, "PageDown"),
    (0x23, "End"),
    (VK_HOME, "Home"),
    (0x25, "Left"),
    (0x26, "Up"),
    (0x27, "Right"),
    (0x28, "Down"),
    (0x2D, "Insert"),
    (0x2E, "Delete"),
];

/// The keys that only modify: Shift, Ctrl, Alt, either side, and Win.
fn is_modifier(key: u16) -> bool {
    matches!(key, 0x10..=0x12 | 0xA0..=0xA5 | 0x5B | 0x5C)
}

/// A key's name, or None for one a chord may not end in.
fn key_name(key: u16) -> Option<String> {
    match key {
        0x30..=0x39 | 0x41..=0x5A => Some(char::from(key as u8).to_string()),
        0x60..=0x69 => Some(format!("Num{}", key - 0x60)),
        0x70..=0x87 => Some(format!("F{}", key - 0x6F)),
        _ => NAMED
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, n)| n.to_string()),
    }
}

/// A key by its name, any case.
fn key_by_name(name: &str) -> Option<u16> {
    let upper = name.to_ascii_uppercase();
    let b = upper.as_bytes();
    if b.len() == 1 && (b[0].is_ascii_digit() || b[0].is_ascii_uppercase()) {
        return Some(u16::from(b[0]));
    }
    if let Some(n) = upper
        .strip_prefix("NUM")
        .and_then(|n| n.parse::<u16>().ok())
    {
        return (n <= 9).then_some(0x60 + n);
    }
    if let Some(n) = upper.strip_prefix('F').and_then(|n| n.parse::<u16>().ok()) {
        return (1..=24).contains(&n).then_some(0x6F + n);
    }
    NAMED
        .iter()
        .find(|(_, n)| n.eq_ignore_ascii_case(name))
        .map(|(k, _)| *k)
}

impl Chord {
    /// "Ctrl+Alt+Shift+Win+Space", the modifiers always in that order.
    pub fn name(&self) -> String {
        let m = self.mods;
        let mut parts: Vec<String> = [
            (m.ctrl, "Ctrl"),
            (m.alt, "Alt"),
            (m.shift, "Shift"),
            (m.win, "Win"),
        ]
        .iter()
        .filter(|(on, _)| *on)
        .map(|(_, n)| n.to_string())
        .collect();
        parts.push(key_name(self.key).unwrap_or_else(|| format!("0x{:02X}", self.key)));
        parts.join("+")
    }

    /// A chord as [`Chord::name`] writes it, any case and any order of
    /// modifiers. None for one that could not be set by pressing it.
    pub fn parse(s: &str) -> Option<Chord> {
        let mut mods = Mods::default();
        let mut key = None;
        for part in s.split('+').map(str::trim) {
            match part.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => mods.ctrl = true,
                "alt" => mods.alt = true,
                "shift" => mods.shift = true,
                "win" => mods.win = true,
                _ if key.is_none() => key = Some(key_by_name(part)?),
                _ => return None,
            }
        }
        let key = key?;
        mods.holds_one().then_some(Chord { mods, key })
    }
}

/// What a key press means while the window listens for a new chord.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Press {
    /// A chord to set.
    Set(Chord),
    /// Esc alone: keep the chord there is.
    Keep,
    /// Backspace alone: back to the chord it had at first.
    Reset,
    /// A modifier alone: the key is still to come.
    Wait,
    /// Not a chord, and why, to say before the next try.
    Refused(&'static str),
}

/// What pressing `key` with `mods` held means while listening.
pub fn press(key: u16, mods: Mods) -> Press {
    if is_modifier(key) {
        return Press::Wait;
    }
    if mods == Mods::default() {
        match key {
            VK_ESCAPE => return Press::Keep,
            VK_BACK => return Press::Reset,
            _ => {}
        }
    }
    if key_name(key).is_none() {
        return Press::Refused("That key can not be a shortcut");
    }
    if !mods.holds_one() {
        return Press::Refused("Hold Ctrl, Alt or Win with the key");
    }
    Press::Set(Chord { mods, key })
}

/// The other action already on `chord`, which setting it for `action`
/// would take from it.
pub fn clash(chords: &[Chord; COUNT], action: Action, chord: Chord) -> Option<Action> {
    Action::ALL
        .into_iter()
        .find(|a| *a != action && chords[a.index()] == chord)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CTRL_ALT: Mods = Mods {
        ctrl: true,
        alt: true,
        shift: false,
        win: false,
    };

    #[test]
    fn the_defaults_are_named_as_the_tray_showed_them() {
        let names: Vec<String> = Action::ALL
            .iter()
            .map(|a| a.default_chord(false).name())
            .collect();
        assert_eq!(
            names,
            [
                "Ctrl+Alt+Space",
                "Ctrl+Alt+Home",
                "Ctrl+Alt+W",
                "Ctrl+Alt+K"
            ]
        );
        assert_eq!(
            Action::Next.default_chord(true).name(),
            "Ctrl+Alt+Shift+Space"
        );
    }

    #[test]
    fn a_name_parses_back_to_its_chord() {
        for key in (0x30..=0x39)
            .chain(0x41..=0x5A)
            .chain(0x60..=0x69)
            .chain(0x70..=0x87)
        {
            let c = Chord {
                mods: CTRL_ALT,
                key,
            };
            assert_eq!(Chord::parse(&c.name()), Some(c), "{}", c.name());
        }
        for (key, _) in NAMED {
            let c = Chord {
                mods: Mods {
                    win: true,
                    shift: true,
                    ..Mods::default()
                },
                key,
            };
            assert_eq!(Chord::parse(&c.name()), Some(c));
        }
    }

    #[test]
    fn parsing_forgives_case_spaces_and_order() {
        let c = Chord::parse(" alt + CTRL + pageup").unwrap();
        assert_eq!(c.name(), "Ctrl+Alt+PageUp");
        assert_eq!(Chord::parse("Control+f12").unwrap().name(), "Ctrl+F12");
    }

    #[test]
    fn parsing_refuses_what_could_not_be_pressed() {
        for bad in [
            "",
            "Ctrl",
            "Space",
            "Shift+A",
            "Ctrl+A+B",
            "Ctrl+F25",
            "Ctrl+Num10",
            "Ctrl+;",
        ] {
            assert_eq!(Chord::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn the_flags_are_register_hotkeys() {
        assert_eq!(CTRL_ALT.flags(), 0x3);
        let all = Mods {
            ctrl: true,
            alt: true,
            shift: true,
            win: true,
        };
        assert_eq!(all.flags(), 0xF);
    }

    #[test]
    fn a_press_while_listening() {
        assert_eq!(press(0x11, CTRL_ALT), Press::Wait);
        assert_eq!(press(0x5B, Mods::default()), Press::Wait);
        assert_eq!(press(VK_ESCAPE, Mods::default()), Press::Keep);
        assert_eq!(press(VK_BACK, Mods::default()), Press::Reset);
        assert_eq!(
            press(u16::from(b'K'), CTRL_ALT),
            Press::Set(Chord {
                mods: CTRL_ALT,
                key: u16::from(b'K')
            })
        );
        assert!(matches!(
            press(u16::from(b'K'), Mods::default()),
            Press::Refused(_)
        ));
        let shift = Mods {
            shift: true,
            ..Mods::default()
        };
        assert!(matches!(press(u16::from(b'K'), shift), Press::Refused(_)));
        // Punctuation, whose place changes with the layout.
        assert!(matches!(press(0xBA, CTRL_ALT), Press::Refused(_)));
    }

    #[test]
    fn a_chord_another_shortcut_has_clashes() {
        let chords = Action::ALL.map(|a| a.default_chord(false));
        let home = Action::Listen.default_chord(false);
        assert_eq!(clash(&chords, Action::Next, home), Some(Action::Listen));
        assert_eq!(clash(&chords, Action::Listen, home), None);
        let fresh = Chord {
            mods: CTRL_ALT,
            key: u16::from(b'J'),
        };
        assert_eq!(clash(&chords, Action::Next, fresh), None);
    }

    #[test]
    fn ids_are_never_zero_and_go_both_ways() {
        for a in Action::ALL {
            assert!(a.id() != 0);
            assert_eq!(Action::from_id(a.id()), Some(a));
        }
        assert_eq!(Action::from_id(0), None);
    }
}
