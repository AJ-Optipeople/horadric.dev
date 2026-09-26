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

/// The faceplate every window is, a dark matte metal, a shade lighter at
/// the top where the light falls.
pub const WINDOW_BG: Color = Color::rgb(0x141518);
pub const PLATE_TOP: Color = Color::rgb(0x191A1E);
pub const PLATE_BOTTOM: Color = Color::rgb(0x111214);
/// A key's face: a session's tile, a button.
pub const SURFACE: Color = Color::rgb(0x202227);
/// A bay sunk into the plate, where a key is yet to go, and the face of a
/// key latched down.
pub const WELL: Color = Color::rgb(0x0C0D0F);
/// The glass of a screen: the files list, the limits, the terminals.
pub const SCREEN: Color = Color::rgb(0x08090B);
/// A lamp with nothing behind it.
pub const LAMP_OFF: Color = Color::rgb(0x2A2C32);
pub const TEXT: Color = Color::rgb(0xE8E9ED);
pub const TEXT_DIM: Color = Color::rgb(0x8F939E);
/// Printed on the plate: labels, section names.
pub const LEGEND: Color = Color::rgb(0x6E727C);

/// The shadow a key casts on the plate.
pub const CAST: Color = Color::rgb(0x000000).with_alpha(0.6);
/// The light catching the top edge of anything raised, and the shade
/// along its bottom.
pub const BEVEL_LIGHT: Color = Color::rgb(0xFFFFFF).with_alpha(0.11);
pub const BEVEL_SHADE: Color = Color::rgb(0x000000).with_alpha(0.35);
/// Inside anything sunk: shade under its top edge, light on its bottom.
pub const HOLLOW_SHADE: Color = Color::rgb(0x000000).with_alpha(0.55);
pub const HOLLOW_LIGHT: Color = Color::rgb(0xFFFFFF).with_alpha(0.05);
/// A line cut into the plate: its dark groove and the lit edge under it.
pub const ENGRAVE_DARK: Color = Color::rgb(0x000000).with_alpha(0.5);
pub const ENGRAVE_LIGHT: Color = Color::rgb(0xFFFFFF).with_alpha(0.05);

/// Behind a button under the cursor and one held down: white laid over
/// whatever is there, as Windows 11 does it, so it works on any surface.
pub const HOVER_FILL: Color = Color::rgb(0xFFFFFF).with_alpha(0.06);
pub const PRESS_FILL: Color = Color::rgb(0xFFFFFF).with_alpha(0.025);

pub const WORKING: Color = Color::rgb(0x3DB4FF);
pub const WAITING: Color = Color::rgb(0xFFB224);
pub const ERROR: Color = Color::rgb(0xFF5D66);
pub const DONE: Color = Color::rgb(0x3DD68C);
pub const IDLE: Color = Color::rgb(0x6E6882);

/// Git change colours, VS Code's dark theme ones, so a file looks the same
/// in the tile as in the editor.
pub const GIT_MODIFIED: Color = Color::rgb(0xE2C08D);
pub const GIT_ADDED: Color = Color::rgb(0x81B88B);
pub const GIT_UNTRACKED: Color = Color::rgb(0x73C991);
pub const GIT_DELETED: Color = Color::rgb(0xC74E39);
pub const GIT_CONFLICT: Color = Color::rgb(0xE4676B);

pub fn change_color(change: Change) -> Color {
    match change {
        Change::Modified => GIT_MODIFIED,
        Change::Added => GIT_ADDED,
        Change::Untracked | Change::Renamed => GIT_UNTRACKED,
        Change::Deleted => GIT_DELETED,
        Change::Conflict => GIT_CONFLICT,
    }
}

/// The fill behind a button and the colour of its glyph. Pressed dims back
/// below hover, which is what makes the press read as a push.
pub fn button_look(b: Button) -> (Option<Color>, Color) {
    match b {
        Button::Idle => (None, TEXT_DIM),
        Button::Hover => (Some(HOVER_FILL), TEXT),
        Button::Pressed => (Some(PRESS_FILL), TEXT_DIM),
    }
}

/// Project colours: distinct from each other and from every phase colour,
/// so an accent never reads as a state. Soft enough to sit beside text.
pub const ACCENTS: [Color; 8] = [
    Color::rgb(0xA78BFA), // violet
    Color::rgb(0x818CF8), // indigo
    Color::rgb(0xE879F9), // orchid
    Color::rgb(0xF472B6), // pink
    Color::rgb(0x2DD4BF), // teal
    Color::rgb(0xBEF264), // lime
    Color::rgb(0xE0976B), // copper
    Color::rgb(0xFDA4AF), // rose
];

/// A project's colour, the same every time for the same key.
pub fn accent(key: &str) -> Color {
    // FNV-1a: tiny, and stable across runs and builds, unlike std's hasher.
    let hash = key.bytes().fold(0x811c_9dc5u32, |h, b| {
        (h ^ b as u32).wrapping_mul(0x0100_0193)
    });
    ACCENTS[hash as usize % ACCENTS.len()]
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
        Phase::Working => WORKING,
        Phase::Waiting(WaitReason::Error(_)) => ERROR,
        Phase::Waiting(_) => WAITING,
        Phase::Done => DONE,
        Phase::Idle | Phase::Ended | Phase::Paused => IDLE,
    }
}

/// A session's key face. Waiting is backlit in its colour, since it needs
/// you and has to be seen from across the room. The rest leave the lamp to
/// say what they do, and one that has stopped is latched down, darker.
pub fn phase_fill(phase: &Phase) -> Color {
    match phase {
        Phase::Waiting(_) => SURFACE.mix(phase_color(phase), 0.22),
        Phase::Working | Phase::Done | Phase::Idle => SURFACE,
        Phase::Ended | Phase::Paused => WELL.mix(SURFACE, 0.5),
    }
}

/// How much of a limit or a context window is used, in percent, as a
/// colour: calm while there is room, amber getting close, red at the end.
pub fn fullness_color(percent: f32) -> Color {
    if percent >= 90.0 {
        ERROR
    } else if percent >= 75.0 {
        WAITING
    } else {
        WORKING
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
        assert_eq!(hover_ink, TEXT);
        assert_eq!(idle_ink, press_ink);
    }

    #[test]
    fn fuller_turns_amber_then_red() {
        assert_eq!(fullness_color(10.0), WORKING);
        assert_eq!(fullness_color(75.0), WAITING);
        assert_eq!(fullness_color(99.5), ERROR);
        assert_eq!(fullness_color(140.0), ERROR);
    }

    #[test]
    fn only_waiting_is_backlit_and_a_stopped_key_is_latched_down() {
        for p in [Phase::Working, Phase::Done, Phase::Idle] {
            assert_eq!(phase_fill(&p), SURFACE);
        }
        assert_ne!(phase_fill(&Phase::Waiting(WaitReason::Input)), SURFACE);
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
        let distinct: std::collections::HashSet<u32> = keys
            .iter()
            .map(|k| {
                let c = accent(k);
                ((c.r * 255.0) as u32) << 16 | ((c.g * 255.0) as u32) << 8 | (c.b * 255.0) as u32
            })
            .collect();
        assert!(
            distinct.len() >= 4,
            "eight keys landed on {}",
            distinct.len()
        );
    }

    #[test]
    fn no_accent_is_a_phase_colour() {
        for a in ACCENTS {
            for p in [WORKING, WAITING, ERROR, DONE, IDLE] {
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
        let c = WORKING.with_alpha(0.5).fade(0.5);
        assert_eq!(c.a, 0.25);
        assert_eq!(c.r, WORKING.r);
        assert_eq!(WORKING.fade(2.0).a, 1.0);
    }

    #[test]
    fn waiting_is_tinted_hardest() {
        let dist = |c: Color| (c.r - SURFACE.r).abs() + (c.b - SURFACE.b).abs();
        let waiting = phase_fill(&Phase::Waiting(WaitReason::Input));
        let working = phase_fill(&Phase::Working);
        assert!(dist(waiting) > dist(working));
        assert!(waiting.r > waiting.b, "amber, not blue");
    }
}
