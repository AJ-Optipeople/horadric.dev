//! What a session host and the UI say to each other over the pipe, and the
//! output ring the host keeps. Pure, so all of it is tested.
//!
//! A connection starts with the host's hello: four magic bytes and the
//! protocol version. Then frames, each a kind byte, a little endian length
//! and that many bytes. Five kinds and no more: input, resize, output,
//! exit, kill. Hosts from older builds live on until their session ends,
//! so a new UI has to understand every version still running, and a small
//! protocol keeps that easy.

use std::collections::VecDeque;

/// This build's protocol. A UI speaks every version up to its own.
pub const VERSION: u8 = 1;

/// Starts every connection, before the version byte.
pub const MAGIC: &[u8; 4] = b"HRDH";

/// The hello the host sends first.
pub fn hello() -> [u8; 5] {
    [MAGIC[0], MAGIC[1], MAGIC[2], MAGIC[3], VERSION]
}

/// The version in a hello, or None when it is not one this build speaks.
pub fn read_hello(bytes: &[u8; 5]) -> Option<u8> {
    (&bytes[..4] == MAGIC && (1..=VERSION).contains(&bytes[4])).then_some(bytes[4])
}

/// Anything longer is a broken stream, not a message.
const MAX_FRAME: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    /// Keys for the program, UI to host.
    Input(Vec<u8>),
    /// UI to host: the console's new size. Host to UI, first after the
    /// hello: the size it has now, which the replay was drawn at.
    Resize { cols: u16, rows: u16 },
    /// What the program printed, host to UI.
    Output(Vec<u8>),
    /// The program ended with this code, host to UI, last.
    Exit(u32),
    /// End the program now, UI to host.
    Kill,
}

const INPUT: u8 = 1;
const RESIZE: u8 = 2;
const OUTPUT: u8 = 3;
const EXIT: u8 = 4;
const KILL: u8 = 5;

impl Message {
    pub fn encode(&self) -> Vec<u8> {
        let (kind, body): (u8, Vec<u8>) = match self {
            Message::Input(b) => (INPUT, b.clone()),
            Message::Resize { cols, rows } => {
                (RESIZE, [cols.to_le_bytes(), rows.to_le_bytes()].concat())
            }
            Message::Output(b) => (OUTPUT, b.clone()),
            Message::Exit(code) => (EXIT, code.to_le_bytes().to_vec()),
            Message::Kill => (KILL, Vec::new()),
        };
        let mut out = Vec::with_capacity(5 + body.len());
        out.push(kind);
        out.extend((body.len() as u32).to_le_bytes());
        out.extend(body);
        out
    }
}

/// Collects bytes as they arrive and hands out whole messages.
#[derive(Default)]
pub struct Decoder {
    buf: Vec<u8>,
}

impl Decoder {
    pub fn push(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// The next whole message, None until one has arrived, or an error
    /// when the stream can not be a conversation with a host. A kind this
    /// build does not know is skipped, so a newer peer can add one without
    /// breaking an older one.
    pub fn take(&mut self) -> Result<Option<Message>, String> {
        loop {
            if self.buf.len() < 5 {
                return Ok(None);
            }
            let len =
                u32::from_le_bytes([self.buf[1], self.buf[2], self.buf[3], self.buf[4]]) as usize;
            if len > MAX_FRAME {
                return Err(format!("a frame of {len} bytes"));
            }
            if self.buf.len() < 5 + len {
                return Ok(None);
            }
            let kind = self.buf[0];
            let body: Vec<u8> = self.buf.drain(..5 + len).skip(5).collect();
            let message = match (kind, body.len()) {
                (INPUT, _) => Message::Input(body),
                (OUTPUT, _) => Message::Output(body),
                (RESIZE, 4) => Message::Resize {
                    cols: u16::from_le_bytes([body[0], body[1]]),
                    rows: u16::from_le_bytes([body[2], body[3]]),
                },
                (EXIT, 4) => {
                    Message::Exit(u32::from_le_bytes([body[0], body[1], body[2], body[3]]))
                }
                (KILL, 0) => Message::Kill,
                (RESIZE | EXIT | KILL, n) => return Err(format!("kind {kind} with {n} bytes")),
                _ => continue,
            };
            return Ok(Some(message));
        }
    }
}

/// The last `cap` bytes of a session's output, which a UI that attaches
/// replays into a fresh terminal.
pub struct Ring {
    bytes: VecDeque<u8>,
    cap: usize,
    /// Whether the oldest bytes were ever dropped.
    trimmed: bool,
    /// Every byte ever pushed, so a reader that took the ring up to some
    /// point can ask for what came after it.
    pushed: u64,
}

impl Ring {
    pub fn new(cap: usize) -> Ring {
        Ring {
            bytes: VecDeque::new(),
            cap,
            trimmed: false,
            pushed: 0,
        }
    }

    pub fn push(&mut self, bytes: &[u8]) {
        self.pushed += bytes.len() as u64;
        self.bytes.extend(bytes);
        let over = self.bytes.len().saturating_sub(self.cap);
        if over > 0 {
            self.bytes.drain(..over);
            self.trimmed = true;
        }
    }

    /// What to replay. Once the ring has dropped its oldest bytes it most
    /// likely starts inside a line or an escape sequence, so it starts
    /// after the first line break instead, where the terminal is in a
    /// state it knows.
    pub fn replay(&self) -> Vec<u8> {
        self.replay_at().1
    }

    /// The replay with the offset of its first byte, counted as
    /// [`Ring::end`] counts.
    pub fn replay_at(&self) -> (u64, Vec<u8>) {
        let (a, b) = self.bytes.as_slices();
        let all = [a, b].concat();
        let start = self.end() - all.len() as u64;
        if !self.trimmed {
            return (start, all);
        }
        match all.iter().position(|&c| c == b'\n') {
            Some(i) => (start + i as u64 + 1, all[i + 1..].to_vec()),
            None => (self.end(), Vec::new()),
        }
    }

    /// How many bytes were ever pushed: the offset the next byte will have.
    pub fn end(&self) -> u64 {
        self.pushed
    }

    /// The bytes from offset `from` on, or None when the ring has dropped
    /// some of them, and a reader that wants them all has to start over
    /// from a replay.
    pub fn since(&self, from: u64) -> Option<Vec<u8>> {
        let start = self.end() - self.bytes.len() as u64;
        if from < start || from > self.end() {
            return None;
        }
        Some(
            self.bytes
                .range((from - start) as usize..)
                .copied()
                .collect(),
        )
    }

    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }
}

/// The pipe of one session, unique to the Horadric instance, so a dev UI
/// never attaches to a session of the installed one.
pub fn pipe_name(instance: &str, id: &str) -> String {
    format!(r"\\.\pipe\{}{}", pipe_prefix(instance), clean(id))
}

/// What every pipe name of an instance starts with after `\\.\pipe\`.
pub fn pipe_prefix(instance: &str) -> String {
    format!("horadric-{}-", clean(instance))
}

/// The job holding a session's processes, which the UI opens to ask
/// whether a window's process belongs to the session.
pub fn job_name(instance: &str, id: &str) -> String {
    format!(r"Local\horadric-{}-{}", clean(instance), clean(id))
}

/// A backslash would be a path in both namespaces.
fn clean(s: &str) -> String {
    s.replace(['\\', '/'], "_")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all(d: &mut Decoder) -> Vec<Message> {
        let mut out = Vec::new();
        while let Some(m) = d.take().unwrap() {
            out.push(m);
        }
        out
    }

    #[test]
    fn every_message_comes_back_as_it_went() {
        let messages = vec![
            Message::Input(b"hi\r".to_vec()),
            Message::Resize {
                cols: 120,
                rows: 36,
            },
            Message::Output(vec![0x1b, b'[', b'm']),
            Message::Output(Vec::new()),
            Message::Exit(0xC000_013A),
            Message::Kill,
        ];
        let mut d = Decoder::default();
        for m in &messages {
            d.push(&m.encode());
        }
        assert_eq!(all(&mut d), messages);
    }

    #[test]
    fn a_message_split_anywhere_waits_for_the_rest() {
        let bytes = Message::Output(b"hello".to_vec()).encode();
        for cut in 0..bytes.len() {
            let mut d = Decoder::default();
            d.push(&bytes[..cut]);
            assert_eq!(d.take().unwrap(), None, "cut at {cut}");
            d.push(&bytes[cut..]);
            assert_eq!(d.take().unwrap(), Some(Message::Output(b"hello".to_vec())));
        }
    }

    #[test]
    fn unknown_kinds_are_skipped_and_nonsense_is_an_error() {
        let mut d = Decoder::default();
        d.push(&[9, 2, 0, 0, 0, 1, 2]);
        d.push(&Message::Kill.encode());
        assert_eq!(all(&mut d), vec![Message::Kill]);

        let mut d = Decoder::default();
        d.push(&[EXIT, 1, 0, 0, 0, 7]);
        assert!(d.take().is_err());
        let mut d = Decoder::default();
        d.push(&[OUTPUT, 0xff, 0xff, 0xff, 0xff]);
        assert!(d.take().is_err());
    }

    #[test]
    fn hello_names_a_version_this_build_speaks() {
        assert_eq!(read_hello(&hello()), Some(VERSION));
        assert_eq!(read_hello(b"HRDH\0"), None);
        assert_eq!(read_hello(&[b'H', b'R', b'D', b'H', VERSION + 1]), None);
        assert_eq!(read_hello(b"HTTP/"), None);
    }

    #[test]
    fn the_ring_keeps_the_newest_bytes() {
        let mut r = Ring::new(8);
        r.push(b"abc\n");
        assert_eq!(r.replay(), b"abc\n");
        r.push(b"defghij");
        assert_eq!(r.len(), 8);
        assert_eq!(r.replay(), b"defghij", "after the line break it kept");
        r.push(b"\nkl");
        assert_eq!(r.replay(), b"kl");
    }

    #[test]
    fn a_reader_takes_what_came_after_it_until_the_ring_drops_it() {
        let mut r = Ring::new(8);
        r.push(b"ab");
        assert_eq!(r.replay_at(), (0, b"ab".to_vec()));
        assert_eq!(r.end(), 2);
        assert_eq!(r.since(2), Some(Vec::new()));
        r.push(b"cd");
        assert_eq!(r.since(2), Some(b"cd".to_vec()));
        assert_eq!(r.since(5), None, "nothing is there yet");
        r.push(b"ef\ngh\n");
        assert_eq!(r.end(), 10);
        assert_eq!(r.since(1), None, "dropped");
        assert_eq!(r.since(2), Some(b"cdef\ngh\n".to_vec()));
        assert_eq!(
            r.replay_at(),
            (7, b"gh\n".to_vec()),
            "after the first break"
        );
    }

    #[test]
    fn a_trimmed_replay_starts_on_a_new_line() {
        let mut r = Ring::new(30);
        r.push(b"xxxxxxxx\x1b[3mtail of a line\r\nnext line\r\nlast");
        assert_eq!(r.replay(), b"next line\r\nlast");
    }

    #[test]
    fn names_are_per_instance_and_never_paths() {
        assert_eq!(
            pipe_name("43117", "fix-1"),
            r"\\.\pipe\horadric-43117-fix-1"
        );
        assert_eq!(
            pipe_name("43118", r"a\b/c"),
            r"\\.\pipe\horadric-43118-a_b_c"
        );
        assert_eq!(job_name("43118", "fix-1"), r"Local\horadric-43118-fix-1");
        assert!(pipe_name("43117", "x").contains(&pipe_prefix("43117")));
        assert!(!pipe_name("43118", "x").contains(&pipe_prefix("43117")));
    }
}
