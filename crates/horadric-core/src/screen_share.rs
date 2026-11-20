//! The pure half of screen sharing: the app serving its own desktop to a
//! viewer page, for a machine that watches it through an SSH tunnel. What
//! a request asks for, whether its token is right, what an input event
//! says and where it lands. Capturing and injecting are Win32 and live in
//! the UI crate.

use serde::{Deserialize, Serialize};

/// The user environment variable that turns serving on. Off unless it is
/// exactly `1`, so a normal install serves nothing new.
pub const ENV: &str = "HORADRIC_SCREEN_SHARE";

/// The installed app's port, on 127.0.0.1 only.
pub const PORT: u16 = 43119;

/// A dev instance's port, so it serves beside the installed one.
pub const DEV_PORT: u16 = 43120;

/// The token's file in the state folder.
pub const TOKEN_FILE: &str = "screen-token";

/// The longest body `/input` reads. One event is a few dozen bytes; text
/// pasted in one go is the only thing that grows.
pub const MAX_BODY: usize = 64 * 1024;

/// Whether serving is on, from [`ENV`]'s value.
pub fn enabled(value: Option<&str>) -> bool {
    value == Some("1")
}

/// The port to serve on.
pub fn port(dev: bool) -> u16 {
    if dev {
        DEV_PORT
    } else {
        PORT
    }
}

/// Sixteen random bytes as the 32 hex characters the token file holds.
pub fn token_hex(bytes: &[u8; 16]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The token in a token file's text, or None when it is not one, so a
/// damaged file is replaced rather than served with.
pub fn read_token(text: &str) -> Option<String> {
    let t = text.trim();
    (t.len() == 32 && t.bytes().all(|b| b.is_ascii_hexdigit())).then(|| t.to_ascii_lowercase())
}

/// Whether a request's token is the right one. It compares every byte
/// whatever the first difference, so the time an answer takes says
/// nothing about how much of a guess was right.
pub fn token_ok(given: Option<&str>, expected: &str) -> bool {
    let Some(given) = given else {
        return false;
    };
    let (a, b) = (given.as_bytes(), expected.as_bytes());
    if a.len() != b.len() || b.is_empty() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// A request target split into its path and its query.
pub fn split_target(target: &str) -> (&str, &str) {
    target.split_once('?').unwrap_or((target, ""))
}

/// One parameter of a query string, percent decoded. The first one of a
/// name wins.
pub fn query_param(query: &str, name: &str) -> Option<String> {
    query
        .split('&')
        .filter_map(|pair| pair.split_once('=').or(Some((pair, ""))))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| percent_decode(v))
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => match (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                (Some(hi), Some(lo)) => {
                    out.push((hi << 4) | lo);
                    i += 2;
                }
                _ => out.push(b'%'),
            },
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(b: u8) -> Option<u8> {
    (b as char).to_digit(16).map(|d| d as u8)
}

/// The widest picture a viewer asked for, None when it did not say or
/// said nonsense. Below 16 pixels nothing is readable, so that is the
/// floor.
pub fn max_width(query: &str) -> Option<u32> {
    query_param(query, "w")?
        .parse::<u32>()
        .ok()
        .filter(|w| *w > 0)
        .map(|w| w.max(16))
}

/// The size a `width` by `height` screen is sent at: as it is, or scaled
/// down to `max` wide with its shape kept.
pub fn scaled(width: u32, height: u32, max: Option<u32>) -> (u32, u32) {
    match max {
        Some(m) if m < width && width > 0 => {
            let h = (u64::from(height) * u64::from(m) / u64::from(width)).max(1);
            (m, h as u32)
        }
        _ => (width, height),
    }
}

/// A mouse button.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Button {
    Left,
    Right,
    Middle,
}

/// One event the viewer posts. Coordinates are virtual screen pixels from
/// its top left.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Input {
    Move { x: i32, y: i32 },
    Down { x: i32, y: i32, button: Button },
    Up { x: i32, y: i32, button: Button },
    Wheel { x: i32, y: i32, delta: i32 },
    Key { vk: u16, down: bool },
    Text { text: String },
}

/// An event from the body `/input` was posted, or why it is not one.
pub fn parse_input(body: &str) -> Result<Input, String> {
    let input: Input = serde_json::from_str(body).map_err(|e| e.to_string())?;
    match &input {
        Input::Key { vk, .. } if *vk == 0 || *vk > 0xFE => Err(format!("no such key: {vk}")),
        Input::Text { text } if text.is_empty() => Err("no text".into()),
        _ => Ok(input),
    }
}

/// A pixel `at` on a virtual screen `extent` pixels across, as the 0 to
/// 65535 SendInput takes for an absolute position on the virtual desk.
/// It aims at the pixel's centre: Windows turns the number back into a
/// pixel by scaling down and dropping the fraction, so aiming at an edge
/// can land on the pixel before. Off the screen it is held to the edge.
pub fn absolute(at: i32, extent: i32) -> i32 {
    if extent <= 0 {
        return 0;
    }
    let at = i64::from(at.clamp(0, extent - 1));
    ((2 * at + 1) * 65536 / (2 * i64::from(extent))).clamp(0, 65535) as i32
}

/// Whether a virtual key sits in the extended part of the keyboard, which
/// SendInput must be told: without it an arrow arrives as a number pad
/// key, and right Ctrl as left.
pub fn extended_key(vk: u16) -> bool {
    matches!(
        vk,
        0x21..=0x28 // page up, page down, end, home, arrows
            | 0x2C // print screen
            | 0x2D | 0x2E // insert, delete
            | 0x5B..=0x5D // windows keys, menu
            | 0x6F // number pad divide
            | 0x90 // num lock
            | 0xA3 | 0xA5 // right ctrl, right alt
    )
}

/// One session as `/sessions` lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SessionRow {
    pub id: String,
    pub name: String,
    pub project: String,
    pub state: String,
    /// The question or approval it waits on, or None when it waits on
    /// nothing.
    pub waiting: Option<String>,
}

/// What `/sessions` answers: the waiting first, then by project and name,
/// so the viewer's list does not jump about between polls.
pub fn sessions_json(mut rows: Vec<SessionRow>) -> String {
    rows.sort_by(|a, b| {
        (b.waiting.is_some(), &a.project, &a.name, &a.id).cmp(&(
            a.waiting.is_some(),
            &b.project,
            &b.name,
            &b.id,
        ))
    });
    serde_json::to_string(&rows).unwrap_or_else(|_| "[]".into())
}

/// A display resolution in pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Size {
    pub width: u32,
    pub height: u32,
}

/// One mode the display driver offers, as EnumDisplaySettingsW lists it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mode {
    pub width: u32,
    pub height: u32,
    pub bits: u32,
    pub refresh: u32,
}

impl Mode {
    pub fn size(&self) -> Size {
        Size {
            width: self.width,
            height: self.height,
        }
    }
}

/// Below this nothing a program lays out fits, and some drivers offer
/// 640 by 480 still.
const SMALLEST: Size = Size {
    width: 800,
    height: 600,
};

/// The sizes worth offering out of every mode the driver lists: those at
/// the bits per pixel in use, none smaller than [`SMALLEST`], each size
/// once however many refresh rates it comes in, widest first.
pub fn display_sizes(modes: &[Mode], bits: u32) -> Vec<Size> {
    let mut sizes: Vec<Size> = modes
        .iter()
        .filter(|m| m.bits == bits && m.width >= SMALLEST.width && m.height >= SMALLEST.height)
        .map(Mode::size)
        .collect();
    sizes.sort_by_key(|s| std::cmp::Reverse((s.width, s.height)));
    sizes.dedup();
    sizes
}

/// What `GET /display` answers.
pub fn display_json(current: Size, modes: &[Size]) -> String {
    // A struct, not `json!`, so the keys keep the order the docs show.
    #[derive(Serialize)]
    struct Display<'a> {
        current: Size,
        modes: &'a [Size],
    }
    serde_json::to_string(&Display { current, modes }).unwrap_or_else(|_| "{}".into())
}

/// The size `POST /display` asks for, or why the body is not one.
pub fn parse_size(body: &str) -> Result<Size, String> {
    serde_json::from_str(body).map_err(|e| e.to_string())
}

/// Which of the driver's modes to switch to for `want`: one at the bits
/// per pixel in use, at the refresh rate in use when the size comes in
/// it, else at the highest it does. A mode the driver listed is one it
/// takes, where the current mode with a new size edited in may not be.
pub fn pick_mode(modes: &[Mode], want: Size, bits: u32, refresh: u32) -> Option<Mode> {
    modes
        .iter()
        .filter(|m| m.size() == want && m.bits == bits)
        .max_by_key(|m| (m.refresh == refresh, m.refresh))
        .copied()
}

/// The name of a code ChangeDisplaySettingsExW answers, for the reason a
/// refused change gives.
pub fn disp_change_name(code: i32) -> &'static str {
    match code {
        0 => "DISP_CHANGE_SUCCESSFUL",
        1 => "DISP_CHANGE_RESTART",
        -1 => "DISP_CHANGE_FAILED",
        -2 => "DISP_CHANGE_BADMODE",
        -3 => "DISP_CHANGE_NOTUPDATED",
        -4 => "DISP_CHANGE_BADFLAGS",
        -5 => "DISP_CHANGE_BADPARAM",
        -6 => "DISP_CHANGE_BADDUALVIEW",
        _ => "an unknown DISP_CHANGE code",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_one_turns_it_on() {
        assert!(enabled(Some("1")));
        assert!(!enabled(Some("0")));
        assert!(!enabled(Some("yes")));
        assert!(!enabled(Some("")));
        assert!(!enabled(None));
    }

    #[test]
    fn a_dev_instance_has_its_own_port() {
        assert_eq!(port(false), 43119);
        assert_eq!(port(true), 43120);
    }

    #[test]
    fn a_token_is_32_hex_characters() {
        let t = token_hex(&[0xab; 16]);
        assert_eq!(t.len(), 32);
        assert_eq!(read_token(&t), Some(t.clone()));
        assert_eq!(read_token(&format!("{t}\r\n")), Some(t.clone()));
        assert_eq!(read_token(&t.to_uppercase()), Some(t));
        assert_eq!(read_token("abc"), None);
        assert_eq!(read_token(&"g".repeat(32)), None);
        assert_eq!(read_token(""), None);
    }

    #[test]
    fn only_the_right_token_passes() {
        let t = "0123456789abcdef0123456789abcdef";
        assert!(token_ok(Some(t), t));
        assert!(!token_ok(Some("0123456789abcdef0123456789abcdee"), t));
        assert!(!token_ok(Some("0123"), t));
        assert!(!token_ok(Some(""), t));
        assert!(!token_ok(None, t));
        assert!(!token_ok(Some(""), ""));
    }

    #[test]
    fn a_target_splits_at_the_question_mark() {
        assert_eq!(
            split_target("/frame.jpg?t=a&w=3"),
            ("/frame.jpg", "t=a&w=3")
        );
        assert_eq!(split_target("/"), ("/", ""));
    }

    #[test]
    fn parameters_are_found_and_decoded() {
        let q = "t=abc&w=800&x=a%20b+c&flag";
        assert_eq!(query_param(q, "t").as_deref(), Some("abc"));
        assert_eq!(query_param(q, "w").as_deref(), Some("800"));
        assert_eq!(query_param(q, "x").as_deref(), Some("a b c"));
        assert_eq!(query_param(q, "flag").as_deref(), Some(""));
        assert_eq!(query_param(q, "nope"), None);
        assert_eq!(query_param("t=a&t=b", "t").as_deref(), Some("a"));
        assert_eq!(query_param("x=%zz%4", "x").as_deref(), Some("%zz%4"));
        assert_eq!(query_param("x=%", "x").as_deref(), Some("%"));
        assert_eq!(query_param("x=%c3%a6", "x").as_deref(), Some("æ"));
    }

    #[test]
    fn a_width_is_a_positive_number() {
        assert_eq!(max_width("w=1280"), Some(1280));
        assert_eq!(max_width("w=3"), Some(16));
        assert_eq!(max_width("w=0"), None);
        assert_eq!(max_width("w=-5"), None);
        assert_eq!(max_width("w=wide"), None);
        assert_eq!(max_width("t=x"), None);
    }

    #[test]
    fn a_picture_scales_down_only() {
        assert_eq!(scaled(3840, 1080, Some(1920)), (1920, 540));
        assert_eq!(scaled(1920, 1080, Some(4000)), (1920, 1080));
        assert_eq!(scaled(1920, 1080, None), (1920, 1080));
        assert_eq!(scaled(1920, 1, Some(16)), (16, 1));
        assert_eq!(scaled(0, 0, Some(16)), (0, 0));
    }

    #[test]
    fn every_kind_of_event_parses() {
        assert_eq!(
            parse_input(r#"{"kind":"move","x":10,"y":20}"#),
            Ok(Input::Move { x: 10, y: 20 })
        );
        assert_eq!(
            parse_input(r#"{"kind":"down","x":1,"y":2,"button":"right"}"#),
            Ok(Input::Down {
                x: 1,
                y: 2,
                button: Button::Right
            })
        );
        assert_eq!(
            parse_input(r#"{"kind":"up","x":1,"y":2,"button":"middle"}"#),
            Ok(Input::Up {
                x: 1,
                y: 2,
                button: Button::Middle
            })
        );
        assert_eq!(
            parse_input(r#"{"kind":"wheel","x":1,"y":2,"delta":-120}"#),
            Ok(Input::Wheel {
                x: 1,
                y: 2,
                delta: -120
            })
        );
        assert_eq!(
            parse_input(r#"{"kind":"key","vk":13,"down":true}"#),
            Ok(Input::Key { vk: 13, down: true })
        );
        assert_eq!(
            parse_input(r#"{"kind":"text","text":"æøå"}"#),
            Ok(Input::Text {
                text: "æøå".into()
            })
        );
    }

    #[test]
    fn a_bad_event_says_why() {
        assert!(parse_input("").is_err());
        assert!(parse_input(r#"{"kind":"fly"}"#).is_err());
        assert!(parse_input(r#"{"kind":"move","x":1}"#).is_err());
        assert!(parse_input(r#"{"kind":"down","x":1,"y":2,"button":"thumb"}"#).is_err());
        assert!(parse_input(r#"{"kind":"key","vk":0,"down":true}"#).is_err());
        assert!(parse_input(r#"{"kind":"key","vk":300,"down":true}"#).is_err());
        assert!(parse_input(r#"{"kind":"text","text":""}"#).is_err());
    }

    #[test]
    fn positions_aim_at_the_middle_of_their_pixel() {
        assert_eq!(absolute(0, 1920), 17);
        assert_eq!(absolute(1919, 1920), 65518);
        assert_eq!(absolute(5000, 1920), 65518);
        assert_eq!(absolute(-3, 1920), 17);
        assert_eq!(absolute(960, 1921), 32768);
        assert_eq!(absolute(5, 1), 32768);
        assert_eq!(absolute(5, 0), 0);
        // Scaled back as Windows does, every pixel lands on itself.
        for extent in [1, 3, 1920, 2561, 7680] {
            for at in 0..extent {
                let back = i64::from(absolute(at, extent)) * i64::from(extent) / 65536;
                assert_eq!(back, i64::from(at), "{at} of {extent}");
            }
        }
    }

    #[test]
    fn arrows_and_right_modifiers_are_extended() {
        for vk in [0x25, 0x26, 0x27, 0x28, 0x2E, 0x24, 0xA3, 0xA5, 0x5B] {
            assert!(extended_key(vk), "{vk:#x}");
        }
        for vk in [0x0D, 0x41, 0xA2, 0xA4, 0x10, 0x60] {
            assert!(!extended_key(vk), "{vk:#x}");
        }
    }

    #[test]
    fn the_waiting_come_first() {
        let row = |id: &str, project: &str, waiting: Option<&str>| SessionRow {
            id: id.into(),
            name: id.into(),
            project: project.into(),
            state: "working".into(),
            waiting: waiting.map(Into::into),
        };
        let json = sessions_json(vec![
            row("b", "p", None),
            row("a", "q", None),
            row("c", "z", Some("May I?")),
        ]);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        let ids: Vec<&str> = (v.as_array().unwrap().iter())
            .map(|r| r["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, ["c", "b", "a"]);
        assert_eq!(v[0]["waiting"], "May I?");
        assert!(v[1]["waiting"].is_null());
        assert_eq!(v[1]["project"], "p");
        assert_eq!(v[1]["state"], "working");
        assert_eq!(sessions_json(Vec::new()), "[]");
    }

    fn mode(width: u32, height: u32, bits: u32, refresh: u32) -> Mode {
        Mode {
            width,
            height,
            bits,
            refresh,
        }
    }

    fn size(width: u32, height: u32) -> Size {
        Size { width, height }
    }

    #[test]
    fn sizes_are_merged_filtered_and_widest_first() {
        let modes = [
            mode(640, 480, 32, 60),
            mode(800, 600, 32, 60),
            mode(1920, 1080, 32, 60),
            mode(1920, 1080, 32, 144),
            mode(2560, 1440, 32, 60),
            mode(1920, 1200, 32, 60),
            mode(3840, 2160, 16, 60),
            mode(1024, 576, 32, 60),
            mode(1680, 1050, 32, 60),
        ];
        assert_eq!(
            display_sizes(&modes, 32),
            [
                size(2560, 1440),
                size(1920, 1200),
                size(1920, 1080),
                size(1680, 1050),
                size(800, 600),
            ]
        );
        assert_eq!(display_sizes(&modes, 16), [size(3840, 2160)]);
        assert!(display_sizes(&[], 32).is_empty());
    }

    #[test]
    fn the_display_answer_has_current_and_modes() {
        let json = display_json(size(2560, 1440), &[size(2560, 1440), size(1920, 1080)]);
        assert_eq!(
            json,
            r#"{"current":{"width":2560,"height":1440},"modes":[{"width":2560,"height":1440},{"width":1920,"height":1080}]}"#
        );
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["current"]["width"], 2560);
        assert_eq!(v["modes"][1]["height"], 1080);
        assert_eq!(
            display_json(size(1, 2), &[]),
            r#"{"current":{"width":1,"height":2},"modes":[]}"#
        );
    }

    #[test]
    fn a_size_parses_or_says_why() {
        assert_eq!(
            parse_size(r#"{"width":1920,"height":1080}"#),
            Ok(size(1920, 1080))
        );
        assert!(parse_size(r#"{"width":1920}"#).is_err());
        assert!(parse_size(r#"{"width":-1,"height":2}"#).is_err());
        assert!(parse_size("").is_err());
    }

    #[test]
    fn the_mode_keeps_bits_and_refresh_where_it_can() {
        let modes = [
            mode(1920, 1080, 32, 60),
            mode(1920, 1080, 32, 144),
            mode(1920, 1080, 32, 120),
            mode(1920, 1080, 16, 60),
            mode(1280, 720, 32, 30),
            mode(1280, 720, 32, 50),
        ];
        let want = size(1920, 1080);
        assert_eq!(pick_mode(&modes, want, 32, 60), Some(modes[0]));
        assert_eq!(pick_mode(&modes, want, 32, 120), Some(modes[2]));
        assert_eq!(pick_mode(&modes, want, 32, 75), Some(modes[1]));
        assert_eq!(pick_mode(&modes, want, 16, 144), Some(modes[3]));
        assert_eq!(pick_mode(&modes, size(1280, 720), 32, 60), Some(modes[5]));
        assert_eq!(pick_mode(&modes, size(1024, 768), 32, 60), None);
        assert_eq!(pick_mode(&modes, want, 24, 60), None);
    }

    #[test]
    fn display_change_codes_have_names() {
        assert_eq!(disp_change_name(0), "DISP_CHANGE_SUCCESSFUL");
        assert_eq!(disp_change_name(1), "DISP_CHANGE_RESTART");
        assert_eq!(disp_change_name(-2), "DISP_CHANGE_BADMODE");
        assert_eq!(disp_change_name(-6), "DISP_CHANGE_BADDUALVIEW");
        assert_eq!(disp_change_name(7), "an unknown DISP_CHANGE code");
    }
}
