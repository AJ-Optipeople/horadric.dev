//! Linux nodes as the Windows UI reaches them, the parts with no I/O: the
//! list of nodes, the `ssh` line that speaks to one, the frames on it and
//! how long to wait before trying again (see docs/plan/the-brotherhood.md,
//! step 6).
//!
//! A node is joined by its SSH destination alone. The trust is the SSH
//! key, so there is no login and no pairing: whoever may `ssh` there may
//! see and drive its sessions, as they could by typing. `ssh` does the
//! encryption, so the messages go as plain frames ([`crate::node`]), not
//! sealed as on the relay.
//!
//! The list lives in `<state dir>/nodes.json`:
//! `[{"name": "vps", "ssh": "exit1-eu"}]`.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::node::{encode, Decoder, FrameError};
use crate::ssh;

pub const FILE: &str = "nodes.json";

/// What `ssh` runs there when the entry names nothing else. A command
/// over `ssh` gets no login shell, so `~/.local/bin`, where `horadric node
/// install` expects the binary, is often not on `PATH` yet.
pub const REMOTE_COMMAND: &str = "PATH=$HOME/.local/bin:$PATH exec horadric node --stdio";

/// One node in the list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Node {
    /// What its clusters are titled with ("project · vps").
    pub name: String,
    /// Anything `ssh` takes as a destination: an alias, `user@address`.
    pub ssh: String,
    /// The command to run there, for a node installed elsewhere than
    /// `~/.local/bin`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
}

/// The list in `text`, each name once. A destination `ssh` would read as
/// an option is left out rather than passed on, as for a project's hosts.
pub fn parse(text: &str) -> Vec<Node> {
    let list: Vec<Node> = serde_json::from_str(text).unwrap_or_default();
    let mut out: Vec<Node> = Vec::new();
    for n in list {
        let ok = ssh::usable(&n.ssh) && !n.name.trim().is_empty();
        if ok && !out.iter().any(|o| o.name == n.name) {
            out.push(n);
        }
    }
    out
}

pub fn to_text(list: &[Node]) -> String {
    let mut text = serde_json::to_string_pretty(list).unwrap_or_else(|_| "[]".into());
    text.push('\n');
    text
}

/// The name a node joined by `dest` gets: the host without its user,
/// made unique among `taken` with a number.
pub fn name_for(dest: &str, taken: &[Node]) -> String {
    let host = dest.rsplit('@').next().unwrap_or(dest);
    let host = if host.is_empty() { "node" } else { host };
    let free = |n: &str| !taken.iter().any(|t| t.name == n);
    if free(host) {
        return host.to_string();
    }
    (2..)
        .map(|k| format!("{host} {k}"))
        .find(|n| free(n))
        .unwrap_or_default()
}

/// `list` with a node for `dest` added at the end. None when `dest` is
/// not one `ssh` destination or is listed already.
pub fn with_node(list: &[Node], dest: &str) -> Option<Vec<Node>> {
    let dest = dest.trim();
    if !ssh::usable(dest) || list.iter().any(|n| n.ssh == dest) {
        return None;
    }
    let mut out = list.to_vec();
    out.push(Node {
        name: name_for(dest, list),
        ssh: dest.to_string(),
        command: None,
    });
    Some(out)
}

/// What `ssh.exe` is started with to speak to `node`. No terminal (`-T`),
/// since frames are bytes a tty would mangle; `BatchMode`, so a host that
/// wants a password or an unknown host key fails at once instead of
/// waiting for a prompt nobody sees; keepalives, so a link that died
/// quietly is noticed within a minute rather than at the next keystroke.
/// The `--` ends the options, so the destination is only ever that.
pub fn ssh_args(node: &Node) -> Vec<String> {
    let command = node
        .command
        .as_deref()
        .filter(|c| !c.trim().is_empty())
        .unwrap_or(REMOTE_COMMAND);
    [
        "-T",
        "-o",
        "BatchMode=yes",
        "-o",
        "ConnectTimeout=15",
        "-o",
        "ServerAliveInterval=15",
        "-o",
        "ServerAliveCountMax=3",
        "--",
        &node.ssh,
        command,
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// One message as the bytes that go to the node's stdin.
pub fn frame(msg: &Value) -> Vec<u8> {
    encode(msg.to_string().as_bytes())
}

/// The messages the node's stdout has brought whole so far. A frame that
/// is not JSON is skipped, one message lost; a length no message has means
/// the stream is lost, and the link with it.
pub fn messages(d: &mut Decoder, bytes: &[u8]) -> Result<Vec<Value>, FrameError> {
    d.push(bytes);
    let mut out = Vec::new();
    while let Some(f) = d.next_frame()? {
        if let Ok(v) = serde_json::from_slice::<Value>(&f) {
            out.push(v);
        }
    }
    Ok(out)
}

/// How long to wait before dialling a node again. Doubles from a second
/// to half a minute while links keep failing, and starts over once one
/// lasted: a node that is down is not dialled every second, and one whose
/// link dropped after an hour is dialled again at once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Backoff {
    next: Duration,
}

pub const FIRST_WAIT: Duration = Duration::from_secs(1);
pub const MOST_WAIT: Duration = Duration::from_secs(30);
/// A link up this long counts as having worked.
pub const LASTED: Duration = Duration::from_secs(10);

impl Default for Backoff {
    fn default() -> Self {
        Self { next: FIRST_WAIT }
    }
}

impl Backoff {
    /// The wait after a link that was up for `up` (zero when it never
    /// came up).
    pub fn after(&mut self, up: Duration) -> Duration {
        if up >= LASTED {
            self.next = FIRST_WAIT;
        }
        let wait = self.next;
        self.next = (self.next * 2).min(MOST_WAIT);
        wait
    }
}

/// What a node said was wrong in an `error` frame before its hello, as
/// `horadric node --stdio` does when no node runs: the tray's line then.
pub fn told(msg: &Value) -> Option<String> {
    if msg["t"] != "error" {
        return None;
    }
    let text = msg["text"].as_str()?.trim();
    (!text.is_empty()).then(|| text.chars().take(120).collect())
}

/// The last line `ssh` wrote to stderr that says something, for the
/// tray's line about a node that is not linked.
pub fn why(stderr: &str) -> Option<String> {
    stderr
        .lines()
        .map(str::trim)
        .rfind(|l| !l.is_empty())
        .map(|l| l.chars().take(120).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::PREAMBLE;
    use serde_json::json;

    fn node(name: &str, ssh: &str) -> Node {
        Node {
            name: name.into(),
            ssh: ssh.into(),
            command: None,
        }
    }

    #[test]
    fn the_list_reads_each_name_once_and_no_options() {
        let text = r#"[{"name":"vps","ssh":"exit1-eu"},{"name":"vps","ssh":"other"},
            {"name":"bad","ssh":"-oProxyCommand=x"},{"name":"two","ssh":"a b"},
            {"name":"box","ssh":"me@10.0.0.2","command":"/opt/h node --stdio"}]"#;
        let list = parse(text);
        assert_eq!(list.len(), 2);
        assert_eq!(list[0], node("vps", "exit1-eu"));
        assert_eq!(list[1].command.as_deref(), Some("/opt/h node --stdio"));
        assert!(parse("not json").is_empty());
        assert_eq!(parse(&to_text(&list)), list);
        assert!(!to_text(&[node("a", "b")]).contains("command"));
    }

    #[test]
    fn a_node_is_named_by_its_host_once() {
        let taken = vec![node("exit1-eu", "exit1-eu")];
        assert_eq!(name_for("agent@box", &taken), "box");
        assert_eq!(name_for("exit1-eu", &taken), "exit1-eu 2");
        assert_eq!(name_for("me@", &[]), "node");
    }

    #[test]
    fn a_node_is_added_once_and_only_as_a_destination() {
        let one = with_node(&[], " exit1-eu ").unwrap();
        assert_eq!(one, vec![node("exit1-eu", "exit1-eu")]);
        assert_eq!(with_node(&one, "exit1-eu"), None);
        assert_eq!(with_node(&one, "-oProxyCommand=calc"), None);
        assert_eq!(with_node(&one, ""), None);
        assert_eq!(with_node(&one, "a@b").unwrap().len(), 2);
    }

    #[test]
    fn ssh_runs_the_node_with_no_tty_and_no_prompts() {
        let a = ssh_args(&node("vps", "exit1-eu"));
        assert_eq!(a[0], "-T");
        assert!(a.windows(2).any(|w| w == ["-o", "BatchMode=yes"]));
        assert!(a.iter().any(|w| w == "ServerAliveInterval=15"));
        let end = a.iter().position(|w| w == "--").unwrap();
        assert_eq!(&a[end + 1..], ["exit1-eu", REMOTE_COMMAND]);
        let mut n = node("vps", "exit1-eu");
        n.command = Some("/opt/h node --stdio".into());
        assert_eq!(ssh_args(&n).last().unwrap(), "/opt/h node --stdio");
        n.command = Some("  ".into());
        assert_eq!(ssh_args(&n).last().unwrap(), REMOTE_COMMAND);
    }

    #[test]
    fn messages_come_out_of_a_banner_and_pieces() {
        let mut stream = b"Welcome\n".to_vec();
        stream.extend(PREAMBLE);
        stream.extend(frame(&json!({"t": "hello", "v": 3})));
        stream.extend(encode(b"not json"));
        stream.extend(frame(&json!({"t": "sessions", "list": []})));
        let mut d = Decoder::client();
        let mut got = Vec::new();
        for piece in stream.chunks(5) {
            got.extend(messages(&mut d, piece).unwrap());
        }
        assert_eq!(got.len(), 2);
        assert_eq!(got[0]["t"], "hello");
        assert_eq!(got[1]["t"], "sessions");
        let mut d = Decoder::node();
        assert!(messages(&mut d, &[0xff; 4]).is_err());
    }

    #[test]
    fn waits_double_while_links_fail_and_start_over_once_one_lasts() {
        let mut b = Backoff::default();
        let never = Duration::ZERO;
        let waits: Vec<u64> = (0..7).map(|_| b.after(never).as_secs()).collect();
        assert_eq!(waits, [1, 2, 4, 8, 16, 30, 30]);
        assert_eq!(b.after(Duration::from_secs(3)), MOST_WAIT);
        assert_eq!(b.after(LASTED), FIRST_WAIT);
        assert_eq!(b.after(never), Duration::from_secs(2));
    }

    #[test]
    fn why_is_the_last_line_ssh_said() {
        let err = "Warning: x\nagent@1.2.3.4: Permission denied (publickey).\n\n";
        assert_eq!(
            why(err).as_deref(),
            Some("agent@1.2.3.4: Permission denied (publickey).")
        );
        assert_eq!(why("  \n"), None);
    }

    #[test]
    fn a_nodes_own_error_is_its_words() {
        let no = json!({"t": "error", "text": " no node runs here (start `horadric node`) "});
        assert_eq!(
            told(&no).as_deref(),
            Some("no node runs here (start `horadric node`)")
        );
        assert_eq!(told(&json!({"t": "error", "text": ""})), None);
        assert_eq!(told(&json!({"t": "hello", "text": "x"})), None);
    }
}
