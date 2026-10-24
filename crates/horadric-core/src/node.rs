//! `horadric node` on Linux, the parts with no I/O: how messages are
//! framed on `horadric node --stdio`, and the systemd user unit `horadric
//! node install` writes (see docs/plan/the-brotherhood.md, step 5).
//!
//! Over `--stdio` the node speaks the same v3 messages as the relay, each
//! a JSON frame behind a four byte big endian length. `ssh` does the
//! encryption. A login shell may print a banner before the node starts,
//! so the node first writes [`PREAMBLE`] and the reader skips anything
//! before it.

/// What the node writes once before its first frame.
pub const PREAMBLE: &[u8] = b"\x00horadric-node 1\n";

/// No message comes near this; a length past it means the stream is
/// garbage, not a frame worth waiting 4 GB for.
pub const MOST_FRAME: usize = 16 * 1024 * 1024;

/// One message as a frame.
pub fn encode(json: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(json.len() + 4);
    out.extend((json.len() as u32).to_be_bytes());
    out.extend(json);
    out
}

/// Why a stream was given up on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameError {
    TooLong(usize),
}

/// Frames out of a stream that arrives in pieces of any size.
#[derive(Debug, Clone, Default)]
pub struct Decoder {
    buf: Vec<u8>,
    /// Whether the preamble has gone by.
    started: bool,
    /// Whether to wait for the preamble at all. The node reading its own
    /// stdin gets frames with none before them.
    skip: bool,
}

impl Decoder {
    /// A decoder for the UI's end, which skips what comes before the
    /// preamble.
    pub fn client() -> Self {
        Self {
            skip: true,
            ..Self::default()
        }
    }

    /// A decoder for the node's end, frames from the first byte.
    pub fn node() -> Self {
        Self {
            started: true,
            ..Self::default()
        }
    }

    /// Whether the preamble has gone by: the node answered, whatever a
    /// login shell printed first.
    pub fn started(&self) -> bool {
        self.started
    }

    pub fn push(&mut self, bytes: &[u8]) {
        self.buf.extend(bytes);
    }

    /// The next whole frame, None until one has arrived.
    pub fn next_frame(&mut self) -> Result<Option<Vec<u8>>, FrameError> {
        if !self.started && self.skip {
            match find(&self.buf, PREAMBLE) {
                Some(at) => {
                    self.buf.drain(..at + PREAMBLE.len());
                    self.started = true;
                }
                None => {
                    // Keep only what could be the start of the preamble.
                    let keep = PREAMBLE.len() - 1;
                    if self.buf.len() > keep {
                        self.buf.drain(..self.buf.len() - keep);
                    }
                    return Ok(None);
                }
            }
        }
        let Some(head) = self.buf.get(..4) else {
            return Ok(None);
        };
        let len = u32::from_be_bytes([head[0], head[1], head[2], head[3]]) as usize;
        if len > MOST_FRAME {
            return Err(FrameError::TooLong(len));
        }
        if self.buf.len() < 4 + len {
            return Ok(None);
        }
        let frame = self.buf[4..4 + len].to_vec();
        self.buf.drain(..4 + len);
        Ok(Some(frame))
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// The unit's file name under `~/.config/systemd/user`.
pub const UNIT: &str = "horadric.service";

/// Where the unit goes: `$XDG_CONFIG_HOME/systemd/user`, else
/// `~/.config/systemd/user`.
pub fn unit_path(xdg_config_home: Option<&str>, home: &str) -> String {
    let base = match xdg_config_home.filter(|x| x.starts_with('/')) {
        Some(x) => x.trim_end_matches('/').to_string(),
        None => format!("{}/.config", home.trim_end_matches('/')),
    };
    format!("{base}/systemd/user/{UNIT}")
}

/// One word of an `ExecStart` line, quoted as systemd reads it. A `%`
/// would be a specifier, so it is doubled.
fn exec_word(word: &str) -> String {
    let word = word.replace('%', "%%");
    if !word.is_empty() && !word.contains([' ', '\t', '"', '\'', '\\', ';']) {
        return word;
    }
    let escaped = word.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

/// The systemd user unit that runs the node at `exe`. Restarted when it
/// fails, never when it is stopped on purpose.
pub fn unit_text(exe: &str) -> String {
    format!(
        "[Unit]\n\
         Description=Horadric node\n\
         Wants=network-online.target\n\
         After=network-online.target\n\
         \n\
         [Service]\n\
         ExecStart={} node\n\
         Restart=on-failure\n\
         RestartSec=5\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n",
        exec_word(exe)
    )
}

/// What `horadric node install` runs once the unit is written, in order.
/// Lingering is what keeps a user's units running with nobody logged in,
/// which a node on a server is for.
pub fn install_commands(user: &str) -> Vec<Vec<String>> {
    let words = |w: &[&str]| w.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    vec![
        words(&["systemctl", "--user", "daemon-reload"]),
        words(&["systemctl", "--user", "enable", "--now", UNIT]),
        words(&["loginctl", "enable-linger", user]),
    ]
}

/// What `horadric node install` says when lingering could not be turned
/// on, which some systems keep for root.
pub fn linger_note(user: &str) -> String {
    format!(
        "The node runs while {user} is logged in. To keep it running logged out, \
         have an administrator run: sudo loginctl enable-linger {user}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_come_out_whatever_the_pieces() {
        let a = encode(br#"{"t":"hello"}"#);
        let b = encode(b"");
        let c = encode(&vec![b'x'; 70_000]);
        let stream: Vec<u8> = [a, b, c].concat();
        for size in [1, 3, 4, 5, 1000, stream.len()] {
            let mut d = Decoder::node();
            let mut got = Vec::new();
            for piece in stream.chunks(size) {
                d.push(piece);
                while let Some(f) = d.next_frame().unwrap() {
                    got.push(f);
                }
            }
            assert_eq!(got.len(), 3, "pieces of {size}");
            assert_eq!(got[0], br#"{"t":"hello"}"#);
            assert!(got[1].is_empty());
            assert_eq!(got[2].len(), 70_000);
        }
    }

    #[test]
    fn a_banner_before_the_preamble_is_skipped() {
        let mut stream = b"Welcome to Ubuntu\nLast login: today\n".to_vec();
        stream.extend(PREAMBLE);
        stream.extend(encode(b"{}"));
        for size in [1, 2, 7, stream.len()] {
            let mut d = Decoder::client();
            let mut got = Vec::new();
            assert!(!d.started());
            for piece in stream.chunks(size) {
                d.push(piece);
                while let Some(f) = d.next_frame().unwrap() {
                    got.push(f);
                }
            }
            assert_eq!(got, [b"{}".to_vec()], "pieces of {size}");
            assert!(d.started());
        }
        let mut d = Decoder::client();
        d.push(&encode(b"{}"));
        assert_eq!(d.next_frame(), Ok(None));
    }

    #[test]
    fn a_length_past_any_message_is_garbage() {
        let mut d = Decoder::node();
        d.push(&[0xff, 0xff, 0xff, 0xff]);
        assert_eq!(d.next_frame(), Err(FrameError::TooLong(0xffff_ffff)));
    }

    #[test]
    fn the_unit_runs_the_node_and_restarts_it() {
        let text = unit_text("/home/m/.local/bin/horadric");
        assert!(text.contains("\nExecStart=/home/m/.local/bin/horadric node\n"));
        assert!(text.contains("\nRestart=on-failure\n"));
        assert!(text.contains("\nWantedBy=default.target\n"));
        assert!(text.starts_with("[Unit]\n"));
        assert!(unit_text("/opt/my apps/h%1").contains("ExecStart=\"/opt/my apps/h%%1\" node"));
        assert!(unit_text(r#"/a"b"#).contains(r#"ExecStart="/a\"b" node"#));
    }

    #[test]
    fn the_unit_goes_in_the_users_config() {
        assert_eq!(
            unit_path(None, "/home/m/"),
            "/home/m/.config/systemd/user/horadric.service"
        );
        assert_eq!(
            unit_path(Some("/x/cfg/"), "/home/m"),
            "/x/cfg/systemd/user/horadric.service"
        );
        assert_eq!(
            unit_path(Some("relative"), "/home/m"),
            "/home/m/.config/systemd/user/horadric.service"
        );
    }

    #[test]
    fn install_enables_the_unit_and_lingers() {
        let cmds = install_commands("m");
        assert_eq!(cmds[1], ["systemctl", "--user", "enable", "--now", UNIT]);
        assert_eq!(cmds[2], ["loginctl", "enable-linger", "m"]);
        assert!(linger_note("m").contains("sudo loginctl enable-linger m"));
    }
}
