//! What a key press sends to the program, in xterm's encoding.
//!
//! Windows splits typing in two. Keys that make a character (letters, digits,
//! Enter, Tab, Backspace, dead key compositions, AltGr symbols on a Danish
//! keyboard) arrive as `WM_CHAR` after the layout has done its work. Keys
//! that do not (arrows, Home, F5) only arrive as `WM_KEYDOWN`. This module
//! has one function for each half and no Win32 in it.
//!
//! A few chords belong to the stage rather than the program: zoom a pane,
//! move to the next one, the font size and search ([`chord`]).

use crate::layout::Dir;

/// A key that produces no character.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Up,
    Down,
    Right,
    Left,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    Delete,
    /// F1 to F12.
    F(u8),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Mods {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
}

impl Mods {
    pub const NONE: Mods = Mods {
        shift: false,
        ctrl: false,
        alt: false,
    };

    /// xterm's modifier parameter: 1 plus a bit per modifier.
    fn param(self) -> u8 {
        1 + self.shift as u8 + 2 * self.alt as u8 + 4 * self.ctrl as u8
    }

    fn any(self) -> bool {
        self.shift || self.ctrl || self.alt
    }
}

/// How a non character key is written: `CSI 1 ; m X` (or SS3 X and CSI X
/// without modifiers), or `CSI n ; m ~`.
#[derive(Clone, Copy)]
enum Form {
    Letter(u8),
    Tilde(u8),
}

fn form(key: Key) -> Option<Form> {
    let form = match key {
        Key::Up => Form::Letter(b'A'),
        Key::Down => Form::Letter(b'B'),
        Key::Right => Form::Letter(b'C'),
        Key::Left => Form::Letter(b'D'),
        Key::Home => Form::Letter(b'H'),
        Key::End => Form::Letter(b'F'),
        Key::F(1) => Form::Letter(b'P'),
        Key::F(2) => Form::Letter(b'Q'),
        Key::F(3) => Form::Letter(b'R'),
        Key::F(4) => Form::Letter(b'S'),
        Key::Insert => Form::Tilde(2),
        Key::Delete => Form::Tilde(3),
        Key::PageUp => Form::Tilde(5),
        Key::PageDown => Form::Tilde(6),
        Key::F(5) => Form::Tilde(15),
        Key::F(6) => Form::Tilde(17),
        Key::F(7) => Form::Tilde(18),
        Key::F(8) => Form::Tilde(19),
        Key::F(9) => Form::Tilde(20),
        Key::F(10) => Form::Tilde(21),
        Key::F(11) => Form::Tilde(23),
        Key::F(12) => Form::Tilde(24),
        _ => return None,
    };
    Some(form)
}

/// The bytes for a non character key. `app_cursor` is DECCKM, which switches
/// unmodified arrows to their SS3 form.
pub fn key_bytes(key: Key, mods: Mods, app_cursor: bool) -> Vec<u8> {
    match form(key) {
        Some(Form::Letter(l)) if mods.any() => {
            format!("\x1b[1;{}{}", mods.param(), l as char).into_bytes()
        }
        Some(Form::Letter(l)) if app_cursor || matches!(key, Key::F(_)) => vec![0x1b, b'O', l],
        Some(Form::Letter(l)) => vec![0x1b, b'[', l],
        Some(Form::Tilde(n)) if mods.any() => format!("\x1b[{n};{}~", mods.param()).into_bytes(),
        Some(Form::Tilde(n)) => format!("\x1b[{n}~").into_bytes(),
        None => Vec::new(),
    }
}

/// The kitty keyboard protocol's progressive enhancements a program has
/// asked for, one field a bit, numbered as the protocol numbers them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Kitty {
    /// 1: keys that are ambiguous in xterm's encoding (Esc, Alt and Ctrl
    /// chords, Shift+Enter) become `CSI code ; mods u`.
    pub disambiguate: bool,
    /// 2: repeats and releases are reported too.
    pub events: bool,
    /// 4: the shifted key comes with the key, as `code:shifted`.
    pub alternates: bool,
    /// 8: every key is an escape code, plain letters and Enter included.
    pub all_keys: bool,
    /// 16: with 8, the text a key makes comes with it.
    pub text: bool,
}

impl Kitty {
    pub fn any(self) -> bool {
        self.disambiguate || self.events || self.alternates || self.all_keys || self.text
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyEvent {
    Press,
    Repeat,
    Release,
}

impl KeyEvent {
    /// The event type after the modifiers, or none for a press, which is
    /// the default. Without flag 2 a repeat is sent as a press.
    fn param(self, flags: Kitty) -> Option<u8> {
        match self {
            _ if !flags.events => None,
            KeyEvent::Press => None,
            KeyEvent::Repeat => Some(2),
            KeyEvent::Release => Some(3),
        }
    }
}

/// `;m` or `;m:e`, or nothing when both are the default and `always` is
/// not set.
fn kitty_mods(mods: Mods, event: Option<u8>, always: bool) -> String {
    match event {
        Some(e) => format!(";{}:{e}", mods.param()),
        None if mods.any() || always => format!(";{}", mods.param()),
        None => String::new(),
    }
}

/// A non character key under the kitty protocol. The xterm forms stay, as
/// the protocol says, except F3: `CSI 1 ; m R` reads as a cursor position
/// report, so it is `CSI 13 ~`. Empty means nothing is sent.
pub fn kitty_key(key: Key, mods: Mods, event: KeyEvent, flags: Kitty, app_cursor: bool) -> Vec<u8> {
    if event == KeyEvent::Release && !flags.events {
        return Vec::new();
    }
    let ev = event.param(flags);
    let form = if key == Key::F(3) {
        Some(Form::Tilde(13))
    } else {
        form(key)
    };
    match form {
        _ if ev.is_none() && !flags.all_keys && key != Key::F(3) => {
            key_bytes(key, mods, app_cursor)
        }
        Some(Form::Letter(l)) if ev.is_none() && !mods.any() => vec![0x1b, b'[', l],
        Some(Form::Letter(l)) => {
            format!("\x1b[1{}{}", kitty_mods(mods, ev, true), l as char).into_bytes()
        }
        Some(Form::Tilde(n)) => format!("\x1b[{n}{}~", kitty_mods(mods, ev, false)).into_bytes(),
        None => Vec::new(),
    }
}

/// The kitty code of a key from what `MapVirtualKey` makes of it without
/// modifiers: letters in lower case, Backspace as DEL. None for a dead key
/// (the top bit) or a key that makes no character.
pub fn kitty_base(mapped: u32) -> Option<u32> {
    match mapped {
        0 => None,
        m if m & 0x8000_0000 != 0 => None,
        8 => Some(127),
        m => char::from_u32(m).map(|c| c.to_lowercase().next().unwrap_or(c) as u32),
    }
}

/// A key that makes a character, or Esc, Enter, Tab or Backspace, under the
/// kitty protocol. `base` is from [`kitty_base`], `typed` is the character
/// Windows made of the press, if any. None means the key is no escape code
/// with these flags and goes the xterm way, as its character.
pub fn kitty_text(
    base: u32,
    typed: Option<char>,
    mods: Mods,
    event: KeyEvent,
    flags: Kitty,
) -> Option<Vec<u8>> {
    let printable = typed.filter(|c| !c.is_control());
    // AltGr is Ctrl+Alt. When the pair typed a character, it is typing.
    let altgr = mods.ctrl && mods.alt && printable.is_some();
    let chord = (mods.ctrl || mods.alt) && !altgr;
    // Plain Enter, Tab and Backspace stay as they were, so a shell is still
    // usable after a program that set the flags dies without clearing them.
    let enter_like = matches!(base, 13 | 9 | 127);
    let escaped = flags.all_keys
        || (flags.disambiguate && (base == 27 || chord || (enter_like && mods.shift)));
    if !escaped {
        return None;
    }
    if event == KeyEvent::Release && !flags.events {
        return Some(Vec::new());
    }
    let mut out = format!("\x1b[{base}");
    if let Some(c) = printable.filter(|&c| flags.alternates && mods.shift && c as u32 != base) {
        out += &format!(":{}", c as u32);
    }
    let text =
        printable.filter(|_| flags.text && flags.all_keys && !chord && event != KeyEvent::Release);
    out += &kitty_mods(mods, event.param(flags), text.is_some());
    if let Some(c) = text {
        out += &format!(";{}", c as u32);
    }
    out.push('u');
    Some(out.into_bytes())
}

/// What a typed character means.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CharAction {
    Send(Vec<u8>),
    /// Ctrl+V, with or without Shift.
    Paste,
    /// Ctrl+C: copy when there is a selection, otherwise interrupt.
    CopyOrInterrupt,
    /// Ctrl+Shift+C: always copy, never interrupt.
    Copy,
    /// Ctrl+Shift+T: a new plain terminal in the project, as a new tab is
    /// in Windows Terminal. Plain Ctrl+T still reaches the program.
    NewShell,
}

/// Maps a `WM_CHAR` (or `WM_SYSCHAR` when `mods.alt`) to its bytes.
pub fn char_action(c: char, mods: Mods) -> CharAction {
    let mut out = Vec::new();
    match c {
        '\u{16}' => return CharAction::Paste,
        '\u{3}' if mods.shift => return CharAction::Copy,
        '\u{3}' => return CharAction::CopyOrInterrupt,
        '\u{14}' if mods.shift && mods.ctrl => return CharAction::NewShell,
        // Meta+Enter is the newline Claude Code understands everywhere.
        '\r' if mods.shift => out.extend_from_slice(b"\x1b\r"),
        '\t' if mods.shift => out.extend_from_slice(b"\x1b[Z"),
        // Windows reports Backspace as ^H and Ctrl+Backspace as DEL. Every
        // terminal program expects the reverse.
        '\u{8}' => out.push(0x7f),
        '\u{7f}' => out.push(0x08),
        _ => {
            if mods.alt {
                out.push(0x1b);
            }
            let mut buf = [0u8; 4];
            out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
        }
    }
    CharAction::Send(out)
}

/// Font size in DIPs when nothing else was chosen. 15 DIPs is 11.25
/// points at 100 % scaling.
pub const FONT_DEFAULT: f32 = 15.0;
const FONT_MIN: f32 = 9.0;
const FONT_MAX: f32 = 32.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FontStep {
    Bigger,
    Smaller,
    Reset,
}

/// The font size after a step, one DIP at a time and within reason.
pub fn font_size(current: f32, step: FontStep) -> f32 {
    let next = match step {
        FontStep::Bigger => current.round() + 1.0,
        FontStep::Smaller => current.round() - 1.0,
        FontStep::Reset => FONT_DEFAULT,
    };
    next.clamp(FONT_MIN, FONT_MAX)
}

/// A chord the stage keeps for itself rather than send to the program.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Chord {
    /// Ctrl+Shift+Enter: this pane alone fills the stage, or the grid
    /// comes back.
    Zoom,
    /// Ctrl+Alt+arrow: the keyboard to the pane in that direction.
    Focus(Dir),
    /// Ctrl and plus, minus or zero, as in a browser.
    Font(FontStep),
    /// Ctrl+Shift+F: search the history, as in Windows Terminal.
    Find,
}

// Virtual key codes, which Windows has kept the same since forever.
const VK_RETURN: u16 = 0x0D;
const VK_LEFT: u16 = 0x25;
const VK_UP: u16 = 0x26;
const VK_RIGHT: u16 = 0x27;
const VK_DOWN: u16 = 0x28;
const VK_0: u16 = 0x30;
const VK_F: u16 = 0x46;
const VK_NUMPAD0: u16 = 0x60;
const VK_ADD: u16 = 0x6B;
const VK_SUBTRACT: u16 = 0x6D;
/// The key right of 0 on a Danish keyboard, and `=` on a US one.
const VK_OEM_PLUS: u16 = 0xBB;
const VK_OEM_MINUS: u16 = 0xBD;

/// Maps a `WM_KEYDOWN` to a stage chord. AltGr is Ctrl+Alt, so no chord
/// that takes Ctrl alone also takes Alt.
pub fn chord(vk: u16, mods: Mods) -> Option<Chord> {
    let Mods { shift, ctrl, alt } = mods;
    if !ctrl {
        return None;
    }
    let chord = match vk {
        VK_RETURN if shift && !alt => Chord::Zoom,
        VK_F if shift && !alt => Chord::Find,
        VK_LEFT if alt && !shift => Chord::Focus(Dir::Left),
        VK_RIGHT if alt && !shift => Chord::Focus(Dir::Right),
        VK_UP if alt && !shift => Chord::Focus(Dir::Up),
        VK_DOWN if alt && !shift => Chord::Focus(Dir::Down),
        // Shift too, since `+` is Shift+`=` on a US keyboard.
        VK_OEM_PLUS | VK_ADD if !alt => Chord::Font(FontStep::Bigger),
        VK_OEM_MINUS | VK_SUBTRACT if !alt && !shift => Chord::Font(FontStep::Smaller),
        VK_0 | VK_NUMPAD0 if !alt && !shift => Chord::Font(FontStep::Reset),
        _ => return None,
    };
    Some(chord)
}

/// Clipboard text as the program should receive it. Line ends become a lone
/// CR, the way a typed Enter arrives. With bracketed paste on, the text is
/// wrapped so the program knows it was pasted, and escape characters are
/// dropped so the text can not end the bracket early and type commands.
pub fn paste_bytes(text: &str, bracketed: bool) -> Vec<u8> {
    let normalised = text.replace("\r\n", "\r").replace('\n', "\r");
    let mut out = Vec::with_capacity(normalised.len() + 12);
    if bracketed {
        out.extend_from_slice(b"\x1b[200~");
        out.extend(normalised.bytes().filter(|&b| b != 0x1b));
        out.extend_from_slice(b"\x1b[201~");
    } else {
        out.extend_from_slice(normalised.as_bytes());
    }
    out
}

/// A mouse button as programs number them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    Left,
    Middle,
    Right,
    WheelUp,
    WheelDown,
}

impl Button {
    fn code(self) -> u32 {
        match self {
            Button::Left => 0,
            Button::Middle => 1,
            Button::Right => 2,
            Button::WheelUp => 64,
            Button::WheelDown => 65,
        }
    }
}

/// What the mouse did, for a program that asked for the mouse.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseEvent {
    Press(Button),
    Release(Button),
    /// A move, with the button held down if any.
    Move(Option<Button>),
}

/// How a report is written. `Sgr` is mode 1006 and has no limit on the
/// cell. `Utf8` is mode 1005 and reaches cell 2015. `X10` is the old one
/// byte form, which can only reach cell 223.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseEncoding {
    X10,
    Utf8,
    Sgr,
}

/// A mouse report. `col` and `row` count cells from the top left, from 0.
pub fn mouse_bytes(
    event: MouseEvent,
    col: usize,
    row: usize,
    mods: Mods,
    encoding: MouseEncoding,
) -> Vec<u8> {
    let held = |b: Option<Button>| b.map_or(3, Button::code);
    let code = match event {
        MouseEvent::Press(b) => b.code(),
        // Only SGR can say which button came up; the older forms send 3.
        MouseEvent::Release(b) if encoding == MouseEncoding::Sgr => b.code(),
        MouseEvent::Release(_) => 3,
        MouseEvent::Move(b) => 32 + held(b),
    } + 4 * mods.shift as u32
        + 8 * mods.alt as u32
        + 16 * mods.ctrl as u32;
    match encoding {
        MouseEncoding::Sgr => {
            let end = if matches!(event, MouseEvent::Release(_)) {
                'm'
            } else {
                'M'
            };
            format!("\x1b[<{code};{};{}{end}", col + 1, row + 1).into_bytes()
        }
        MouseEncoding::Utf8 => {
            let mut out = b"\x1b[M".to_vec();
            for n in [
                32 + code,
                33 + col.min(2014) as u32,
                33 + row.min(2014) as u32,
            ] {
                let c = char::from_u32(n).unwrap_or(' ');
                out.extend_from_slice(c.encode_utf8(&mut [0; 4]).as_bytes());
            }
            out
        }
        MouseEncoding::X10 => {
            let at = |n: usize| (33 + n).min(255) as u8;
            vec![
                0x1b,
                b'[',
                b'M',
                (32 + code).min(255) as u8,
                at(col),
                at(row),
            ]
        }
    }
}

/// Whether a move is reported: every move in mode 1003, only moves with a
/// button down in mode 1002, none in plain click mode 1000.
pub fn reports_move(any: bool, drag: bool, held: bool) -> bool {
    any || (drag && held)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHIFT: Mods = Mods {
        shift: true,
        ctrl: false,
        alt: false,
    };
    const CTRL: Mods = Mods {
        shift: false,
        ctrl: true,
        alt: false,
    };
    const ALT: Mods = Mods {
        shift: false,
        ctrl: false,
        alt: true,
    };

    const KITTY: Kitty = Kitty {
        disambiguate: true,
        events: false,
        alternates: false,
        all_keys: false,
        text: false,
    };
    const EVENTS: Kitty = Kitty {
        events: true,
        ..KITTY
    };

    #[test]
    fn kitty_base_is_the_unshifted_code() {
        assert_eq!(kitty_base('A' as u32), Some('a' as u32));
        assert_eq!(kitty_base('1' as u32), Some('1' as u32));
        assert_eq!(kitty_base(0xC6), Some(0xE6));
        assert_eq!(kitty_base(8), Some(127));
        assert_eq!(kitty_base(13), Some(13));
        assert_eq!(kitty_base(0), None);
        assert_eq!(kitty_base(0x8000_00B4), None);
    }

    #[test]
    fn disambiguate_leaves_plain_typing_alone() {
        let press =
            |base: char, typed, mods| kitty_text(base as u32, typed, mods, KeyEvent::Press, KITTY);
        assert_eq!(press('a', Some('a'), Mods::NONE), None);
        assert_eq!(press('a', Some('A'), SHIFT), None);
        assert_eq!(press('\r', Some('\r'), Mods::NONE), None);
        let backspace = kitty_text(127, Some('\u{8}'), Mods::NONE, KeyEvent::Press, KITTY);
        assert_eq!(backspace, None);
        // AltGr+2 on a Danish keyboard is @, not Ctrl+Alt+2.
        let altgr = Mods { alt: true, ..CTRL };
        assert_eq!(press('2', Some('@'), altgr), None);
        let off = kitty_text(97, None, CTRL, KeyEvent::Press, Kitty::default());
        assert_eq!(off, None);
    }

    #[test]
    fn disambiguate_codes_the_ambiguous_keys() {
        let press =
            |base: u32, typed, mods| kitty_text(base, typed, mods, KeyEvent::Press, KITTY).unwrap();
        assert_eq!(press(27, Some('\u{1b}'), Mods::NONE), b"\x1b[27u");
        assert_eq!(press(13, Some('\r'), SHIFT), b"\x1b[13;2u");
        assert_eq!(press(13, Some('\n'), CTRL), b"\x1b[13;5u");
        assert_eq!(press(9, Some('\t'), SHIFT), b"\x1b[9;2u");
        assert_eq!(press(127, Some('\u{7f}'), CTRL), b"\x1b[127;5u");
        assert_eq!(press('c' as u32, Some('\u{3}'), CTRL), b"\x1b[99;5u");
        assert_eq!(press('x' as u32, Some('x'), ALT), b"\x1b[120;3u");
        let ctrl_shift = Mods {
            shift: true,
            ..CTRL
        };
        assert_eq!(press('a' as u32, Some('\u{1}'), ctrl_shift), b"\x1b[97;6u");
        // Ctrl+1 makes no character at all, and still reaches the program.
        assert_eq!(press('1' as u32, None, CTRL), b"\x1b[49;5u");
    }

    #[test]
    fn events_report_repeats_and_releases_of_coded_keys() {
        let key = |event, flags| kitty_text('a' as u32, None, CTRL, event, flags);
        assert_eq!(key(KeyEvent::Repeat, EVENTS).unwrap(), b"\x1b[97;5:2u");
        assert_eq!(key(KeyEvent::Release, EVENTS).unwrap(), b"\x1b[97;5:3u");
        assert_eq!(key(KeyEvent::Repeat, KITTY).unwrap(), b"\x1b[97;5u");
        assert_eq!(key(KeyEvent::Release, KITTY).unwrap(), b"");
        let esc = kitty_text(27, None, Mods::NONE, KeyEvent::Release, EVENTS).unwrap();
        assert_eq!(esc, b"\x1b[27;1:3u");
        // A plain letter was typed as text, so its release is not news.
        let plain = kitty_text(97, None, Mods::NONE, KeyEvent::Release, EVENTS);
        assert_eq!(plain, None);
    }

    #[test]
    fn all_keys_codes_everything_with_alternates_and_text() {
        let all = Kitty {
            all_keys: true,
            ..KITTY
        };
        let press = |base: u32, typed, mods, flags| {
            kitty_text(base, typed, mods, KeyEvent::Press, flags).unwrap()
        };
        assert_eq!(press(97, Some('a'), Mods::NONE, all), b"\x1b[97u");
        assert_eq!(press(13, Some('\r'), Mods::NONE, all), b"\x1b[13u");
        assert_eq!(press(97, Some('A'), SHIFT, all), b"\x1b[97;2u");
        let alternates = Kitty {
            alternates: true,
            ..all
        };
        assert_eq!(press(97, Some('A'), SHIFT, alternates), b"\x1b[97:65;2u");
        let text = Kitty { text: true, ..all };
        assert_eq!(press(97, Some('a'), Mods::NONE, text), b"\x1b[97;1;97u");
        assert_eq!(press(97, Some('A'), SHIFT, text), b"\x1b[97;2;65u");
        assert_eq!(press(97, Some('\u{1}'), CTRL, text), b"\x1b[97;5u");
    }

    #[test]
    fn kitty_function_keys_keep_their_xterm_forms() {
        use KeyEvent::*;
        let key = |k, m, e, f| kitty_key(k, m, e, f, false);
        assert_eq!(key(Key::Up, Mods::NONE, Press, KITTY), b"\x1b[A");
        assert_eq!(
            kitty_key(Key::Up, Mods::NONE, Press, KITTY, true),
            b"\x1bOA"
        );
        assert_eq!(key(Key::Left, CTRL, Press, KITTY), b"\x1b[1;5D");
        assert_eq!(key(Key::Delete, SHIFT, Press, KITTY), b"\x1b[3;2~");
        assert_eq!(key(Key::F(3), Mods::NONE, Press, KITTY), b"\x1b[13~");
        assert_eq!(key(Key::F(3), CTRL, Press, KITTY), b"\x1b[13;5~");
        assert_eq!(key(Key::Up, Mods::NONE, Release, KITTY), b"");
        assert_eq!(key(Key::Up, Mods::NONE, Release, EVENTS), b"\x1b[1;1:3A");
        assert_eq!(key(Key::Up, CTRL, Repeat, EVENTS), b"\x1b[1;5:2A");
        assert_eq!(
            key(Key::PageUp, Mods::NONE, Release, EVENTS),
            b"\x1b[5;1:3~"
        );
        let all = Kitty {
            all_keys: true,
            ..KITTY
        };
        assert_eq!(kitty_key(Key::Up, Mods::NONE, Press, all, true), b"\x1b[A");
        assert!(key(Key::F(13), CTRL, Press, KITTY).is_empty());
    }

    #[test]
    fn arrows_follow_cursor_mode_and_modifiers() {
        assert_eq!(key_bytes(Key::Up, Mods::NONE, false), b"\x1b[A");
        assert_eq!(key_bytes(Key::Up, Mods::NONE, true), b"\x1bOA");
        assert_eq!(key_bytes(Key::Left, CTRL, true), b"\x1b[1;5D");
        assert_eq!(key_bytes(Key::End, SHIFT, false), b"\x1b[1;2F");
    }

    #[test]
    fn tilde_keys_and_function_keys() {
        assert_eq!(key_bytes(Key::Delete, Mods::NONE, false), b"\x1b[3~");
        assert_eq!(key_bytes(Key::PageUp, ALT, false), b"\x1b[5;3~");
        assert_eq!(key_bytes(Key::F(1), Mods::NONE, false), b"\x1bOP");
        assert_eq!(key_bytes(Key::F(4), CTRL, false), b"\x1b[1;5S");
        assert_eq!(key_bytes(Key::F(5), Mods::NONE, false), b"\x1b[15~");
        assert_eq!(key_bytes(Key::F(12), SHIFT, false), b"\x1b[24;2~");
        assert!(key_bytes(Key::F(13), Mods::NONE, false).is_empty());
    }

    #[test]
    fn characters_map_to_what_programs_expect() {
        let send = |c, m| char_action(c, m);
        assert_eq!(send('a', Mods::NONE), CharAction::Send(b"a".to_vec()));
        assert_eq!(
            send('\u{e6}', Mods::NONE),
            CharAction::Send("\u{e6}".as_bytes().to_vec())
        );
        assert_eq!(send('x', ALT), CharAction::Send(b"\x1bx".to_vec()));
        assert_eq!(send('\r', Mods::NONE), CharAction::Send(b"\r".to_vec()));
        assert_eq!(send('\r', SHIFT), CharAction::Send(b"\x1b\r".to_vec()));
        assert_eq!(send('\t', SHIFT), CharAction::Send(b"\x1b[Z".to_vec()));
        assert_eq!(send('\u{8}', Mods::NONE), CharAction::Send(vec![0x7f]));
        assert_eq!(send('\u{7f}', CTRL), CharAction::Send(vec![0x08]));
        assert_eq!(send('\u{1b}', Mods::NONE), CharAction::Send(vec![0x1b]));
    }

    #[test]
    fn clipboard_keys_are_actions() {
        assert_eq!(char_action('\u{16}', CTRL), CharAction::Paste);
        assert_eq!(char_action('\u{3}', CTRL), CharAction::CopyOrInterrupt);
        let ctrl_shift = Mods {
            shift: true,
            ..CTRL
        };
        assert_eq!(char_action('\u{3}', ctrl_shift), CharAction::Copy);
        assert_eq!(char_action('\u{14}', ctrl_shift), CharAction::NewShell);
        assert_eq!(char_action('\u{14}', CTRL), CharAction::Send(vec![0x14]));
    }

    #[test]
    fn paste_normalises_lines_and_brackets() {
        assert_eq!(paste_bytes("a\r\nb\nc", false), b"a\rb\rc");
        assert_eq!(
            paste_bytes("x\x1b[201~rm", true),
            b"\x1b[200~x[201~rm\x1b[201~"
        );
    }

    #[test]
    fn chords_need_their_modifiers() {
        let ctrl_shift = Mods {
            shift: true,
            ..CTRL
        };
        let ctrl_alt = Mods { alt: true, ..CTRL };
        assert_eq!(chord(VK_RETURN, ctrl_shift), Some(Chord::Zoom));
        assert_eq!(chord(VK_RETURN, CTRL), None);
        assert_eq!(chord(VK_RETURN, SHIFT), None);
        assert_eq!(chord(VK_F, ctrl_shift), Some(Chord::Find));
        assert_eq!(chord(VK_F, CTRL), None);
        assert_eq!(chord(VK_LEFT, ctrl_alt), Some(Chord::Focus(Dir::Left)));
        assert_eq!(chord(VK_DOWN, ctrl_alt), Some(Chord::Focus(Dir::Down)));
        assert_eq!(chord(VK_LEFT, CTRL), None);
        assert_eq!(chord(VK_LEFT, ALT), None);
        let bigger = Some(Chord::Font(FontStep::Bigger));
        assert_eq!(chord(VK_OEM_PLUS, CTRL), bigger);
        assert_eq!(chord(VK_OEM_PLUS, ctrl_shift), bigger);
        assert_eq!(chord(VK_ADD, CTRL), bigger);
        assert_eq!(
            chord(VK_OEM_MINUS, CTRL),
            Some(Chord::Font(FontStep::Smaller))
        );
        assert_eq!(chord(VK_0, CTRL), Some(Chord::Font(FontStep::Reset)));
        // AltGr and a key is typing, not a chord.
        assert_eq!(chord(VK_OEM_PLUS, ctrl_alt), None);
        assert_eq!(chord(VK_0, ctrl_alt), None);
    }

    #[test]
    fn font_size_steps_and_stops() {
        assert_eq!(font_size(15.0, FontStep::Bigger), 16.0);
        assert_eq!(font_size(15.0, FontStep::Smaller), 14.0);
        assert_eq!(font_size(14.6, FontStep::Bigger), 16.0);
        assert_eq!(font_size(FONT_MAX, FontStep::Bigger), FONT_MAX);
        assert_eq!(font_size(FONT_MIN, FontStep::Smaller), FONT_MIN);
        assert_eq!(font_size(30.0, FontStep::Reset), FONT_DEFAULT);
    }

    #[test]
    fn wheel_reports_in_every_encoding() {
        let wheel = |up, col, row, mods, enc| {
            let b = if up {
                Button::WheelUp
            } else {
                Button::WheelDown
            };
            mouse_bytes(MouseEvent::Press(b), col, row, mods, enc)
        };
        use MouseEncoding::*;
        assert_eq!(wheel(true, 0, 0, Mods::NONE, Sgr), b"\x1b[<64;1;1M");
        assert_eq!(wheel(false, 9, 4, CTRL, Sgr), b"\x1b[<81;10;5M");
        assert_eq!(
            wheel(true, 2, 3, Mods::NONE, X10),
            [0x1b, b'[', b'M', 96, 35, 36]
        );
        assert_eq!(wheel(false, 500, 0, SHIFT, X10)[3..], [101, 255, 33]);
    }

    #[test]
    fn clicks_and_releases() {
        use MouseEncoding::*;
        let left = MouseEvent::Press(Button::Left);
        let up = MouseEvent::Release(Button::Right);
        assert_eq!(mouse_bytes(left, 0, 0, Mods::NONE, Sgr), b"\x1b[<0;1;1M");
        assert_eq!(mouse_bytes(up, 4, 2, Mods::NONE, Sgr), b"\x1b[<2;5;3m");
        assert_eq!(
            mouse_bytes(left, 1, 1, ALT, X10),
            [0x1b, b'[', b'M', 40, 34, 34]
        );
        assert_eq!(mouse_bytes(up, 1, 1, Mods::NONE, X10)[3], 35);
        let middle = MouseEvent::Press(Button::Middle);
        assert_eq!(mouse_bytes(middle, 0, 0, CTRL, Sgr), b"\x1b[<17;1;1M");
    }

    #[test]
    fn moves_add_32_and_say_which_button_is_held() {
        use MouseEncoding::*;
        let drag = MouseEvent::Move(Some(Button::Left));
        let hover = MouseEvent::Move(None);
        assert_eq!(mouse_bytes(drag, 2, 0, Mods::NONE, Sgr), b"\x1b[<32;3;1M");
        assert_eq!(mouse_bytes(hover, 2, 0, Mods::NONE, Sgr), b"\x1b[<35;3;1M");
        assert_eq!(mouse_bytes(hover, 0, 0, Mods::NONE, X10)[3], 67);
    }

    #[test]
    fn utf8_reaches_past_cell_223() {
        let b = mouse_bytes(
            MouseEvent::Press(Button::Left),
            300,
            0,
            Mods::NONE,
            MouseEncoding::Utf8,
        );
        let mut want = b"\x1b[M ".to_vec();
        want.extend_from_slice("\u{14d}!".as_bytes());
        assert_eq!(b, want);
    }

    #[test]
    fn which_moves_are_reported() {
        assert!(!reports_move(false, false, true));
        assert!(!reports_move(false, true, false));
        assert!(reports_move(false, true, true));
        assert!(reports_move(true, false, false));
    }
}
