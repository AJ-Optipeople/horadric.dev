//! Watching another machine's desktop: the pure half of the client. A
//! remote Horadric started with `HORADRIC_SCREEN_SHARE=1` serves its screen
//! on its own `127.0.0.1`, this machine reaches it through `ssh -L`, and
//! every request carries the token the remote keeps in its state folder.
//! See `docs/plan/remote-screen.md`.
//!
//! Here: what `ssh` is started with, what a token looks like, the URLs and
//! how they are shown without the token, reading an HTTP answer, and the
//! file that remembers the tunnels so sessions share them.

use serde::{Deserialize, Serialize};

use crate::ssh;

/// Where a remote's installed Horadric serves its screen.
pub const PORT: u16 = 43119;
/// Where a remote's dev instance serves it, so one on this machine can be
/// tried against the installed one beside it.
pub const DEV_PORT: u16 = 43120;

/// What stands for the token wherever a URL is shown.
pub const MASK: &str = "<token>";

/// How wide a remote screenshot is unless asked otherwise.
pub const FRAME_WIDTH: u32 = 1600;

/// The file in the state folder that remembers the tunnels.
pub const TUNNELS_FILE: &str = "remote-tunnels.json";

/// The remote port to forward to.
pub fn remote_port(dev: bool) -> u16 {
    if dev {
        DEV_PORT
    } else {
        PORT
    }
}

/// Why `dest` can not be handed to `ssh` as a destination, None when it can.
pub fn refusal(dest: &str) -> Option<String> {
    if ssh::usable(dest) {
        None
    } else {
        Some(format!(
            "{dest:?} is not an ssh destination: one word, not starting with -"
        ))
    }
}

/// Options every `ssh` here is started with. `BatchMode`, since nobody
/// sees a prompt for a password or a new host key; keepalives, so a link
/// that died quietly ends the tunnel and the next call starts a fresh one.
fn common() -> Vec<String> {
    [
        "-o",
        "BatchMode=yes",
        "-o",
        "ConnectTimeout=15",
        "-o",
        "ServerAliveInterval=15",
        "-o",
        "ServerAliveCountMax=3",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// What `ssh` is started with to forward `local` on this machine to
/// `remote` on the remote's own loopback. `ExitOnForwardFailure`, so a
/// port taken meanwhile ends it rather than leaving a tunnel that forwards
/// nothing. The local end binds to loopback by name, so no `ssh_config`
/// `GatewayPorts` can open it to the network. The end of options marker
/// before `dest` keeps it only a destination.
pub fn tunnel_args(dest: &str, local: u16, remote: u16) -> Vec<String> {
    let mut args = vec!["-N".to_string()];
    args.extend(common());
    args.extend([
        "-o".into(),
        "ExitOnForwardFailure=yes".into(),
        "-L".into(),
        format!("127.0.0.1:{local}:127.0.0.1:{remote}"),
        "--".into(),
        dest.into(),
    ]);
    args
}

/// What `ssh` is started with to print the remote's token. Through
/// `cmd /c`, since a Windows host's default shell may be PowerShell, and
/// this reads the same from either.
pub fn token_args(dest: &str, dev: bool) -> Vec<String> {
    let folder = if dev { "Horadric-dev" } else { "Horadric" };
    let mut args = common();
    args.extend([
        "--".into(),
        dest.into(),
        format!(r#"cmd /c type "%APPDATA%\{folder}\screen-token""#),
    ]);
    args
}

/// The token in what the remote printed: 32 hex characters, nothing else
/// but white space and a byte order mark around them.
pub fn parse_token(printed: &str) -> Option<String> {
    let t = printed.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}');
    (t.len() == 32 && t.chars().all(|c| c.is_ascii_hexdigit())).then(|| t.to_ascii_lowercase())
}

/// The viewer page through a tunnel on `port`.
pub fn view_url(port: u16, token: &str) -> String {
    format!("http://127.0.0.1:{port}/?t={token}")
}

/// The path of one frame, at most `width` pixels wide.
pub fn frame_path(token: &str, width: u32) -> String {
    format!("/frame.jpg?t={token}&w={width}")
}

/// `text` with the token in it replaced, for anything shown or logged. An
/// empty token would replace between every character, so it changes
/// nothing.
pub fn mask(text: &str, token: &str) -> String {
    if token.is_empty() {
        text.to_string()
    } else {
        text.replace(token, MASK)
    }
}

/// An HTTP answer read whole: the status, the headers with their names in
/// lower case, and the body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Response {
    /// The first header called `name`, any case.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// Why what came back is not an answer to use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unread {
    /// Not HTTP at all: a tunnel whose far end refused closes the
    /// connection with nothing said.
    NotHttp,
    /// The body stopped before the length the head gave, so a link that
    /// dropped mid frame is not taken for a whole picture.
    CutShort { said: usize, got: usize },
}

/// Reads what an HTTP/1.0 request got back. A body longer than its
/// `Content-Length` is cut to it; one shorter is refused.
pub fn parse_response(bytes: &[u8]) -> Result<Response, Unread> {
    let end = bytes
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or(Unread::NotHttp)?;
    let head = std::str::from_utf8(&bytes[..end]).map_err(|_| Unread::NotHttp)?;
    let mut lines = head.split("\r\n");
    let mut parts = lines.next().unwrap_or("").split_whitespace();
    if !parts.next().is_some_and(|p| p.starts_with("HTTP/")) {
        return Err(Unread::NotHttp);
    }
    let status = parts
        .next()
        .and_then(|s| s.parse().ok())
        .ok_or(Unread::NotHttp)?;
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(n, v)| (n.trim().to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    let mut body = bytes[end + 4..].to_vec();
    let said = headers
        .iter()
        .find(|(n, _)| n == "content-length")
        .and_then(|(_, v)| v.parse::<usize>().ok());
    if let Some(said) = said {
        if body.len() < said {
            return Err(Unread::CutShort {
                said,
                got: body.len(),
            });
        }
        body.truncate(said);
    }
    Ok(Response {
        status,
        headers,
        body,
    })
}

/// The line said beside a frame: how large the remote's screen is, which
/// the picture is a scaled copy of, and how to turn a spot in the picture
/// into the screen position the input tools take. `image` is the
/// picture's own size, when it could be read.
pub fn screen_line(r: &Response, image: Option<(u32, u32)>) -> String {
    let n = |k: &str| r.header(k).and_then(|v| v.parse::<i64>().ok());
    let (Some(w), Some(h)) = (n("x-screen-width"), n("x-screen-height")) else {
        return "The remote did not say how large its screen is.".to_string();
    };
    let screen = format!("The remote screen is {w} x {h} physical pixels.");
    match image {
        Some((iw, ih)) if iw > 0 => {
            let factor = w as f64 / f64::from(iw);
            let factor = format!("{:.3}", factor);
            let factor = factor.trim_end_matches('0').trim_end_matches('.');
            format!(
                "{screen} This image is {iw} x {ih} of that screen: multiply image \
                 coordinates by {factor} for the x and y the remote_ input tools take, \
                 counted from the screen's top left."
            )
        }
        _ => format!(
            "{screen} Scale image coordinates by {w} / the image's width for the x and y \
             the remote_ input tools take, counted from the screen's top left."
        ),
    }
}

/// The width and height a JPEG says it has, from its start of frame
/// marker. None when the bytes are not a JPEG with one.
pub fn jpeg_size(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.get(..2)? != [0xff, 0xd8] {
        return None;
    }
    let mut at = 2;
    loop {
        // Fill bytes may stand before a marker.
        while *bytes.get(at)? == 0xff && *bytes.get(at + 1)? == 0xff {
            at += 1;
        }
        if *bytes.get(at)? != 0xff {
            return None;
        }
        let marker = *bytes.get(at + 1)?;
        let len = usize::from(u16::from_be_bytes([
            *bytes.get(at + 2)?,
            *bytes.get(at + 3)?,
        ]));
        // Every start of frame but the three that are not one: DHT, JPG, DAC.
        if (0xc0..=0xcf).contains(&marker) && !matches!(marker, 0xc4 | 0xc8 | 0xcc) {
            let h = u16::from_be_bytes([*bytes.get(at + 5)?, *bytes.get(at + 6)?]);
            let w = u16::from_be_bytes([*bytes.get(at + 7)?, *bytes.get(at + 8)?]);
            return Some((u32::from(w), u32::from(h)));
        }
        if marker == 0xda || len < 2 {
            return None;
        }
        at += 2 + len;
    }
}

/// The path input events are posted to.
pub fn input_path(token: &str) -> String {
    format!("/input?t={token}")
}

/// The events of a click at `x`, `y` with `button` (left, right or
/// middle; left when None), twice over for a double click: a move first,
/// so the program under it sees the pointer arrive as it would by hand.
pub fn click_events(
    x: i32,
    y: i32,
    button: Option<&str>,
    double: bool,
) -> Result<Vec<String>, String> {
    let button = match button.unwrap_or("left") {
        b @ ("left" | "right" | "middle") => b,
        other => return Err(format!("button is left, right or middle, not {other:?}")),
    };
    let mut out = vec![serde_json::json!({ "kind": "move", "x": x, "y": y }).to_string()];
    for _ in 0..if double { 2 } else { 1 } {
        for kind in ["down", "up"] {
            out.push(
                serde_json::json!({ "kind": kind, "x": x, "y": y, "button": button }).to_string(),
            );
        }
    }
    Ok(out)
}

/// One notch of a mouse wheel, as Windows counts it.
pub const WHEEL_NOTCH: i32 = 120;

/// The event of scrolling `notches` at `x`, `y`, negative being down.
/// Held to a hundred notches either way, more than any page needs.
pub fn wheel_event(x: i32, y: i32, notches: i32) -> Result<String, String> {
    if notches == 0 {
        return Err("delta is a number of notches other than 0: negative scrolls down".into());
    }
    let delta = notches.clamp(-100, 100) * WHEEL_NOTCH;
    Ok(serde_json::json!({ "kind": "wheel", "x": x, "y": y, "delta": delta }).to_string())
}

/// The most characters one text event carries. The remote reads a body of
/// 64 KiB at most, and a character escaped in JSON takes 12 bytes at most.
const TEXT_CHUNK: usize = 4000;

/// The events that type `text`, in pieces small enough for the remote to
/// read each, split between characters.
pub fn text_events(text: &str) -> Result<Vec<String>, String> {
    if text.is_empty() {
        return Err("say what to type: text".into());
    }
    let chars: Vec<char> = text.chars().collect();
    Ok(chars
        .chunks(TEXT_CHUNK)
        .map(|c| {
            let piece: String = c.iter().collect();
            serde_json::json!({ "kind": "text", "text": piece }).to_string()
        })
        .collect())
}

/// The virtual key of a modifier name, any case.
fn modifier_vk(name: &str) -> Option<u16> {
    Some(match name.to_ascii_lowercase().as_str() {
        "ctrl" | "control" => 0x11,
        "shift" => 0x10,
        "alt" => 0x12,
        "win" | "windows" | "meta" | "super" => 0x5b,
        _ => return None,
    })
}

/// The virtual key of a key name, any case: a letter, a digit, F1 to F24
/// or a named key.
fn key_vk(name: &str) -> Option<u16> {
    let lower = name.to_ascii_lowercase();
    let mut chars = lower.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        if c.is_ascii_lowercase() {
            return Some(c.to_ascii_uppercase() as u16);
        }
        if c.is_ascii_digit() {
            return Some(c as u16);
        }
    }
    if let Some(n) = lower.strip_prefix('f').and_then(|n| n.parse::<u16>().ok()) {
        return (1..=24).contains(&n).then_some(0x6f + n);
    }
    Some(match lower.as_str() {
        "enter" | "return" => 0x0d,
        "tab" => 0x09,
        "escape" | "esc" => 0x1b,
        "backspace" => 0x08,
        "delete" | "del" => 0x2e,
        "insert" | "ins" => 0x2d,
        "space" => 0x20,
        "up" | "arrowup" => 0x26,
        "down" | "arrowdown" => 0x28,
        "left" | "arrowleft" => 0x25,
        "right" | "arrowright" => 0x27,
        "home" => 0x24,
        "end" => 0x23,
        "pageup" | "pgup" => 0x21,
        "pagedown" | "pgdn" => 0x22,
        _ => modifier_vk(name)?,
    })
}

/// The virtual keys of a key or chord such as `Enter`, `Ctrl+S` or
/// `Ctrl+Shift+Esc`, in the order they go down: the modifiers as written,
/// then the key. A modifier alone (`Win`) is a key too.
pub fn parse_chord(chord: &str) -> Result<Vec<u16>, String> {
    let parts: Vec<&str> = chord.split('+').map(str::trim).collect();
    let Some((key, mods)) = parts.split_last() else {
        return Err("say which key".into());
    };
    let mut vks = Vec::with_capacity(parts.len());
    for m in mods {
        let vk = modifier_vk(m).ok_or_else(|| {
            format!("{m:?} in {chord:?} is not Ctrl, Shift, Alt or Win, which come before the key")
        })?;
        if vks.contains(&vk) {
            return Err(format!("{m:?} is in {chord:?} twice"));
        }
        vks.push(vk);
    }
    let vk = key_vk(key).ok_or_else(|| {
        format!(
            "{key:?} is not a key: a letter, a digit, F1 to F24, Enter, Tab, Escape, \
             Backspace, Delete, Insert, Space, Up, Down, Left, Right, Home, End, PageUp, \
             PageDown, or a modifier"
        )
    })?;
    if vks.contains(&vk) {
        return Err(format!("{key:?} is in {chord:?} twice"));
    }
    vks.push(vk);
    Ok(vks)
}

/// The events that press `vks` together: each down in order, then each up
/// in reverse, so the modifiers hold until the key is let go.
pub fn press_events(vks: &[u16]) -> Vec<String> {
    let event = |vk: u16, down: bool| {
        serde_json::json!({ "kind": "key", "vk": vk, "down": down }).to_string()
    };
    vks.iter()
        .map(|&vk| event(vk, true))
        .chain(vks.iter().rev().map(|&vk| event(vk, false)))
        .collect()
}

/// Why the remote did not take an input event, in words for the caller:
/// what it said, and for a refusal what usually causes one.
pub fn input_refusal(status: u16, said: &str) -> String {
    let said = said.trim();
    match status {
        409 => format!(
            "the remote's Windows did not take the input ({}): the window under it runs as \
             administrator (Sysmac Studio does), which the remote's Horadric can not reach, \
             or the desktop is locked",
            if said.is_empty() {
                "no reason given"
            } else {
                said
            }
        ),
        400 => format!("the remote did not take the event: {said}"),
        413 => "the remote found the event too large".to_string(),
        s => status_refusal(s),
    }
}

/// Why an answer other than 200 came back, in words for the caller.
pub fn status_refusal(status: u16) -> String {
    match status {
        403 => "the remote refused the token: it may have been made again since it was read"
            .to_string(),
        404 => "the remote's Horadric does not serve its screen at that path: it may be older"
            .to_string(),
        s => format!("the remote answered {s}"),
    }
}

/// One tunnel kept open: `ssh` process `pid` forwards `port` here to
/// `remote` on `dest`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tunnel {
    pub dest: String,
    pub remote: u16,
    pub port: u16,
    pub pid: u32,
}

/// The tunnels remembered in [`TUNNELS_FILE`]. A file that does not read
/// is no tunnels, which only means a fresh one is started.
pub fn parse_tunnels(text: &str) -> Vec<Tunnel> {
    serde_json::from_str(text).unwrap_or_default()
}

/// The tunnels as the file keeps them.
pub fn tunnels_text(list: &[Tunnel]) -> String {
    let mut out = serde_json::to_string_pretty(list).unwrap_or_else(|_| "[]".into());
    out.push('\n');
    out
}

/// The tunnel remembered for `dest` and `remote`, if any.
pub fn find_tunnel<'a>(list: &'a [Tunnel], dest: &str, remote: u16) -> Option<&'a Tunnel> {
    list.iter().find(|t| t.dest == dest && t.remote == remote)
}

/// `list` with `t` in place of any other for the same destination and
/// remote port.
pub fn with_tunnel(list: &[Tunnel], t: Tunnel) -> Vec<Tunnel> {
    let mut out: Vec<Tunnel> = list
        .iter()
        .filter(|o| !(o.dest == t.dest && o.remote == t.remote))
        .cloned()
        .collect();
    out.push(t);
    out
}

/// `list` without the tunnel for `dest` and `remote`, for closing it.
pub fn without_tunnel(list: &[Tunnel], dest: &str, remote: u16) -> Vec<Tunnel> {
    list.iter()
        .filter(|t| !(t.dest == dest && t.remote == remote))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    use serde_json::{json, Value};

    const TOKEN: &str = "0123456789abcdef0123456789abcdef";

    fn values(events: &[String]) -> Vec<Value> {
        events
            .iter()
            .map(|e| serde_json::from_str(e).unwrap())
            .collect()
    }

    #[test]
    fn dev_forwards_to_its_own_port() {
        assert_eq!(remote_port(false), 43119);
        assert_eq!(remote_port(true), 43120);
    }

    #[test]
    fn a_destination_ssh_would_misread_is_refused() {
        assert_eq!(refusal("me@vm"), None);
        assert!(refusal("-oProxyCommand=x").is_some());
        assert!(refusal("a b").is_some());
        assert!(refusal("").is_some());
    }

    #[test]
    fn the_tunnel_forwards_to_the_remote_loopback_and_ends_on_failure() {
        let a = tunnel_args("vm", 50000, 43119);
        assert_eq!(a[0], "-N");
        assert!(a.contains(&"BatchMode=yes".to_string()));
        assert!(a.contains(&"ExitOnForwardFailure=yes".to_string()));
        let l = a.iter().position(|x| x == "-L").unwrap();
        assert_eq!(a[l + 1], "127.0.0.1:50000:127.0.0.1:43119");
        assert_eq!(&a[a.len() - 2..], ["--", "vm"]);
    }

    #[test]
    fn the_token_is_read_through_cmd_from_the_right_folder() {
        let a = token_args("vm", false);
        assert!(a.contains(&"BatchMode=yes".to_string()));
        assert_eq!(a[a.len() - 3..a.len() - 1], ["--", "vm"]);
        assert_eq!(
            a.last().unwrap(),
            r#"cmd /c type "%APPDATA%\Horadric\screen-token""#
        );
        assert!(token_args("vm", true)
            .last()
            .unwrap()
            .contains(r"%APPDATA%\Horadric-dev\screen-token"));
    }

    #[test]
    fn a_token_is_32_hex_and_nothing_else() {
        assert_eq!(parse_token(&format!("{TOKEN}\r\n")).as_deref(), Some(TOKEN));
        assert_eq!(
            parse_token(&format!("\u{feff}{}", TOKEN.to_uppercase())).as_deref(),
            Some(TOKEN)
        );
        assert_eq!(parse_token(&TOKEN[..31]), None);
        assert_eq!(parse_token(&format!("{TOKEN}0")), None);
        assert_eq!(parse_token(&TOKEN.replace('a', "g")), None);
        assert_eq!(
            parse_token("The system cannot find the file specified."),
            None
        );
        assert_eq!(parse_token(""), None);
    }

    #[test]
    fn urls_carry_the_token_and_masking_hides_it() {
        let url = view_url(50123, TOKEN);
        assert_eq!(url, format!("http://127.0.0.1:50123/?t={TOKEN}"));
        assert_eq!(mask(&url, TOKEN), "http://127.0.0.1:50123/?t=<token>");
        assert_eq!(
            frame_path(TOKEN, 800),
            format!("/frame.jpg?t={TOKEN}&w=800")
        );
        assert_eq!(mask("abc", ""), "abc");
    }

    #[test]
    fn an_http_answer_is_read_into_status_headers_and_body() {
        let raw = b"HTTP/1.0 200 OK\r\nContent-Type: image/jpeg\r\nX-Screen-Width: 2560\r\nX-Screen-Height:1440\r\nX-Screen-Left: -1920\r\n\r\n\xff\xd8\xff";
        let r = parse_response(raw).unwrap();
        assert_eq!(r.status, 200);
        assert_eq!(r.header("content-type"), Some("image/jpeg"));
        assert_eq!(r.header("X-SCREEN-HEIGHT"), Some("1440"));
        assert_eq!(r.body, b"\xff\xd8\xff");
        assert_eq!(
            screen_line(&r, Some((1600, 900))),
            "The remote screen is 2560 x 1440 physical pixels. This image is 1600 x 900 of \
             that screen: multiply image coordinates by 1.6 for the x and y the remote_ input \
             tools take, counted from the screen's top left."
        );
        assert!(screen_line(&r, Some((2560, 1440))).contains("multiply image coordinates by 1 "));
        assert!(screen_line(&r, Some((1000, 563))).contains("by 2.56 "));
        assert!(screen_line(&r, None).contains("2560 / the image's width"));
    }

    /// A JPEG head with a start of frame `marker` of `w` by `h`, after an
    /// APP0 segment, as an encoder writes them.
    fn jpeg(marker: u8, w: u16, h: u16) -> Vec<u8> {
        let mut b = vec![0xff, 0xd8, 0xff, 0xe0, 0x00, 0x04, 0xaa, 0xbb];
        b.extend([0xff, 0xc4, 0x00, 0x03, 0x00]);
        b.extend([0xff, marker, 0x00, 0x11, 0x08]);
        b.extend(h.to_be_bytes());
        b.extend(w.to_be_bytes());
        b.extend([0x03, 0x01]);
        b
    }

    #[test]
    fn a_jpeg_says_its_size_in_its_start_of_frame() {
        assert_eq!(jpeg_size(&jpeg(0xc0, 1600, 900)), Some((1600, 900)));
        assert_eq!(jpeg_size(&jpeg(0xc2, 800, 451)), Some((800, 451)));
        let mut filled = jpeg(0xc0, 10, 20);
        filled.insert(8, 0xff);
        assert_eq!(jpeg_size(&filled), Some((10, 20)));
        assert_eq!(jpeg_size(&jpeg(0xc0, 10, 20)[..15]), None);
        assert_eq!(jpeg_size(b"\x89PNG\r\n"), None);
        assert_eq!(jpeg_size(&[0xff, 0xd8, 0xff, 0xda, 0x00, 0x02]), None);
        assert_eq!(jpeg_size(&[]), None);
    }

    #[test]
    fn a_click_moves_then_presses_and_lets_go() {
        assert_eq!(input_path(TOKEN), format!("/input?t={TOKEN}"));
        let one = values(&click_events(812, 440, None, false).unwrap());
        assert_eq!(
            one,
            [
                json!({ "kind": "move", "x": 812, "y": 440 }),
                json!({ "kind": "down", "x": 812, "y": 440, "button": "left" }),
                json!({ "kind": "up", "x": 812, "y": 440, "button": "left" }),
            ]
        );
        let double = values(&click_events(1, 2, Some("right"), true).unwrap());
        assert_eq!(double.len(), 5);
        assert_eq!(double[3], double[1]);
        assert_eq!(
            double[4],
            json!({ "kind": "up", "x": 1, "y": 2, "button": "right" })
        );
        assert!(click_events(1, 2, Some("back"), false).is_err());
    }

    #[test]
    fn a_scroll_counts_notches_down_as_negative() {
        assert_eq!(
            values(&[wheel_event(5, 6, -3).unwrap()]),
            [json!({ "kind": "wheel", "x": 5, "y": 6, "delta": -360 })]
        );
        let delta = |n| values(&[wheel_event(0, 0, n).unwrap()])[0]["delta"].clone();
        assert_eq!(delta(1), 120);
        assert_eq!(delta(1000), 12000);
        assert!(wheel_event(0, 0, 0).is_err());
    }

    #[test]
    fn text_goes_in_pieces_the_remote_reads_whole() {
        assert_eq!(
            values(&text_events("æøå \"x\"").unwrap()),
            [json!({ "kind": "text", "text": "æøå \"x\"" })]
        );
        let long = "\u{1F600}".repeat(TEXT_CHUNK + 1);
        let pieces = text_events(&long).unwrap();
        assert_eq!(pieces.len(), 2);
        assert!(pieces.iter().all(|p| p.len() < 64 * 1024));
        assert!(text_events("").is_err());
    }

    #[test]
    fn keys_are_named_as_on_the_keyboard() {
        assert_eq!(parse_chord("a"), Ok(vec![0x41]));
        assert_eq!(parse_chord("Z"), Ok(vec![0x5a]));
        assert_eq!(parse_chord("0"), Ok(vec![0x30]));
        assert_eq!(parse_chord("9"), Ok(vec![0x39]));
        assert_eq!(parse_chord("F1"), Ok(vec![0x70]));
        assert_eq!(parse_chord("f5"), Ok(vec![0x74]));
        assert_eq!(parse_chord("F24"), Ok(vec![0x87]));
        assert!(parse_chord("F0").is_err());
        assert!(parse_chord("F25").is_err());
        for (name, vk) in [
            ("Enter", 0x0d),
            ("Return", 0x0d),
            ("Tab", 0x09),
            ("Escape", 0x1b),
            ("Esc", 0x1b),
            ("Backspace", 0x08),
            ("Delete", 0x2e),
            ("Insert", 0x2d),
            ("Space", 0x20),
            ("Up", 0x26),
            ("Down", 0x28),
            ("Left", 0x25),
            ("ArrowRight", 0x27),
            ("Home", 0x24),
            ("End", 0x23),
            ("PageUp", 0x21),
            ("PageDown", 0x22),
            ("Win", 0x5b),
        ] {
            assert_eq!(parse_chord(name), Ok(vec![vk]), "{name}");
            assert_eq!(parse_chord(&name.to_uppercase()), Ok(vec![vk]), "{name}");
        }
    }

    #[test]
    fn chords_hold_their_modifiers_in_order() {
        assert_eq!(parse_chord("Ctrl+S"), Ok(vec![0x11, 0x53]));
        assert_eq!(parse_chord("Alt+F4"), Ok(vec![0x12, 0x73]));
        assert_eq!(parse_chord("Ctrl+Shift+Esc"), Ok(vec![0x11, 0x10, 0x1b]));
        assert_eq!(parse_chord("Win+R"), Ok(vec![0x5b, 0x52]));
        assert_eq!(
            parse_chord("ctrl + alt + delete"),
            Ok(vec![0x11, 0x12, 0x2e])
        );
        assert!(parse_chord("S+Ctrl").is_err());
        assert!(parse_chord("Ctrl+Ctrl+S").is_err());
        assert!(parse_chord("Ctrl+Ctrl").is_err());
        assert!(parse_chord("Ctrl+").is_err());
        assert!(parse_chord("").is_err());
        assert!(parse_chord("Hyper+S").is_err());
        assert!(parse_chord("Ctrl+Enterr").is_err());
    }

    #[test]
    fn a_chord_goes_down_in_order_and_up_in_reverse() {
        let key = |vk: u16, down: bool| json!({ "kind": "key", "vk": vk, "down": down });
        assert_eq!(
            values(&press_events(&[0x11, 0x53])),
            [
                key(0x11, true),
                key(0x53, true),
                key(0x53, false),
                key(0x11, false)
            ]
        );
    }

    #[test]
    fn input_refusals_pass_on_what_the_remote_said() {
        let e = input_refusal(409, "1 of 2 events taken\n");
        assert!(
            e.contains("1 of 2 events taken") && e.contains("administrator"),
            "{e}"
        );
        assert!(input_refusal(409, "").contains("no reason given"));
        assert!(input_refusal(400, "no such key: 0").ends_with("no such key: 0"));
        assert!(input_refusal(413, "").contains("too large"));
        assert!(input_refusal(403, "").contains("token"));
    }

    #[test]
    fn nothing_or_not_http_is_no_answer() {
        assert_eq!(parse_response(b""), Err(Unread::NotHttp));
        assert_eq!(parse_response(b"HTTP/1.0 200 OK\r\n"), Err(Unread::NotHttp));
        assert_eq!(
            parse_response(b"SSH-2.0-OpenSSH\r\n\r\n"),
            Err(Unread::NotHttp)
        );
        assert_eq!(parse_response(b"HTTP/1.0\r\n\r\n"), Err(Unread::NotHttp));
        let r = parse_response(b"HTTP/1.1 403 Forbidden\r\n\r\n").unwrap();
        assert_eq!(r.status, 403);
        assert!(r.body.is_empty());
        assert_eq!(
            screen_line(&r, Some((10, 10))),
            "The remote did not say how large its screen is."
        );
    }

    #[test]
    fn a_body_is_held_to_its_content_length() {
        let short = b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\n12345";
        assert_eq!(
            parse_response(short),
            Err(Unread::CutShort { said: 10, got: 5 })
        );
        let exact = b"HTTP/1.1 200 OK\r\ncontent-length: 5\r\n\r\n12345";
        assert_eq!(parse_response(exact).unwrap().body, b"12345");
        let long = b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\n12345";
        assert_eq!(parse_response(long).unwrap().body, b"123");
        let unsaid = b"HTTP/1.0 200 OK\r\n\r\n12345";
        assert_eq!(parse_response(unsaid).unwrap().body, b"12345");
    }

    #[test]
    fn refusals_say_why() {
        assert!(status_refusal(403).contains("token"));
        assert!(status_refusal(404).contains("older"));
        assert_eq!(status_refusal(500), "the remote answered 500");
    }

    #[test]
    fn tunnels_are_kept_one_per_destination_and_port() {
        let a = Tunnel {
            dest: "vm".into(),
            remote: PORT,
            port: 50000,
            pid: 10,
        };
        let dev = Tunnel {
            remote: DEV_PORT,
            port: 50001,
            pid: 11,
            ..a.clone()
        };
        let list = with_tunnel(&with_tunnel(&[], a.clone()), dev.clone());
        assert_eq!(list.len(), 2);
        let newer = Tunnel {
            port: 50002,
            pid: 12,
            ..a.clone()
        };
        let list = with_tunnel(&list, newer.clone());
        assert_eq!(list.len(), 2);
        assert_eq!(find_tunnel(&list, "vm", PORT), Some(&newer));
        assert_eq!(find_tunnel(&list, "vm", DEV_PORT), Some(&dev));
        assert_eq!(find_tunnel(&list, "other", PORT), None);
        assert_eq!(parse_tunnels(&tunnels_text(&list)), list);
        let closed = without_tunnel(&list, "vm", PORT);
        assert_eq!(closed, vec![dev.clone()]);
        assert_eq!(without_tunnel(&closed, "other", PORT), closed);
    }

    #[test]
    fn an_unreadable_tunnels_file_is_no_tunnels() {
        assert!(parse_tunnels("").is_empty());
        assert!(parse_tunnels("{not json").is_empty());
        assert!(parse_tunnels(r#"{"dest":"vm"}"#).is_empty());
    }
}
