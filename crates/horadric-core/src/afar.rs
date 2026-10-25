//! Sessions on a brother, as this PC sees them: the client's half of
//! Town Portal protocol v3 (`docs/PORTAL.md`). A brother's sessions join
//! the registry under ids of their own, in clusters of their own, and
//! their panes are drawn from the raw output the brother streams. Pure:
//! the link, the grid and the clock are the caller's.
//!
//! Where the key comes from is not this file's business: a phone's
//! pairing ticket now, the key two joined brothers share later. Either
//! way the link is sealed the same and speaks the same messages.

use std::collections::HashMap;
use std::time::{Duration, SystemTime};

use serde_json::{json, Value};

use crate::agent::Agent;
use crate::session::{Phase, Session, WaitReason};

/// What a cluster key of a brother's project starts with. A local key is
/// a folder, so it never does.
const KEY_PREFIX: &str = "brother:";

/// The protocol this client speaks.
pub const VERSION: u64 = 3;

/// The cluster key of `project` on the brother keyed `brother` (see
/// [`OnBrother::id`]). Its own key even for a repository this PC has too:
/// they are two working trees.
pub fn key(brother: &str, project: &str) -> String {
    format!("{KEY_PREFIX}{}/{project}", brother.replace('/', " "))
}

/// The brother and project of a cluster key, None for a local one.
pub fn split_key(key: &str) -> Option<(&str, &str)> {
    key.strip_prefix(KEY_PREFIX)?.split_once('/')
}

/// What a brother's cluster is titled: the project, then where it lives.
pub fn title(brother: &str, project: &str) -> String {
    format!("{project} \u{b7} {brother}")
}

/// The id a brother's session has here. Its own id may be one a local
/// session has too, so the brother's key is part of it.
pub fn local_id(brother: &str, id: &str) -> String {
    format!("{id}@{brother}")
}

/// The id on the brother of a session shown here, given whose it is.
pub fn remote_id<'a>(local: &'a str, brother: &str) -> Option<&'a str> {
    local.strip_suffix(brother)?.strip_suffix('@')
}

/// A pairing link, the ticket a phone trades for its key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ticket {
    pub relay: String,
    pub id: String,
    pub key: String,
    /// Unix seconds it is good until.
    pub until: u64,
    pub name: String,
}

/// Reads `https://.../#r=<relay>&id=<id>&p=<ticket>&x=<until>&n=<name>`.
pub fn ticket(link: &str) -> Option<Ticket> {
    let (_, fragment) = link.split_once('#')?;
    let mut fields: HashMap<&str, String> = HashMap::new();
    for part in fragment.split('&') {
        let (k, v) = part.split_once('=')?;
        fields.insert(k, uri_decode(v)?);
    }
    let field = |k: &str| fields.get(k).cloned().filter(|v| !v.is_empty());
    Some(Ticket {
        relay: field("r")?,
        id: field("id")?,
        key: field("p")?,
        until: field("x")?.parse().ok()?,
        name: field("n").unwrap_or_default(),
    })
}

fn uri_decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' => {
                let hex = std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?;
                out.push(u8::from_str_radix(hex, 16).ok()?);
                i += 3;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

/// The key a Linux node goes by here: its name, which `nodes.json` keeps
/// unique, marked so it is never a PC's relay id, and without the `@` and
/// `/` that ids and cluster keys are split at.
pub fn node_key(name: &str) -> String {
    format!("node-{}", name.replace(['@', '/'], "-"))
}

/// The brother a Linux node reached over `ssh` is, by what its hello says
/// of itself: its relay id, when this machine holds that brother under the
/// very key the hello names (`held`, id to public key). Then the node and
/// that brother are one machine, shown once. A hello naming an id held
/// under another key is no one this machine knows.
pub fn folds_into(
    hello: &Value,
    held: &std::collections::BTreeMap<String, String>,
) -> Option<String> {
    let id = hello.get("id")?.as_str()?;
    let key = hello.get("key")?.as_str()?;
    (!key.is_empty() && held.get(id).map(String::as_str) == Some(key)).then(|| id.to_string())
}

/// What brother `peer` lists that is its to show: a session the roster says
/// another brother owns now (taken over or handed on while `peer` was
/// silent, its last list still kept) is shown there and not here too.
/// `relay` says `peer` is a brother of the roster at all; a node reached
/// only over `ssh` is not, and its list stands as it is.
pub fn shown_list(
    list: &[Entry],
    peer: &str,
    owners: &crate::brotherhood::Owners,
    relay: bool,
) -> Vec<Entry> {
    list.iter()
        .filter(|e| !relay || owners.get(&e.id).is_none_or(|o| o.brother == peer))
        .cloned()
        .collect()
}

/// The names to show for brothers, `(key, name)`, so two that share a name
/// are told apart: each of those gets the start of its key beside it.
pub fn distinct_names(named: &[(String, String)]) -> Vec<(String, String)> {
    named
        .iter()
        .map(|(key, name)| {
            let shared = named.iter().any(|(k, n)| k != key && n == name);
            let shown = if shared {
                let tag: String = key.trim_start_matches("node-").chars().take(4).collect();
                format!("{name} ({tag})")
            } else {
                name.clone()
            };
            (key.clone(), shown)
        })
        .collect()
}

/// The URL a client dials for the PC with this id.
pub fn phone_url(relay: &str, id: &str) -> String {
    format!("{}/phone?id={id}", relay.trim_end_matches('/'))
}

/// One session in a brother's `sessions` list.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub id: String,
    pub name: String,
    pub project: String,
    pub phase: Phase,
    pub line: String,
    pub paused: bool,
    /// When it took its phase there, Unix seconds. None from a brother
    /// older than the field.
    pub since: Option<u64>,
    /// The agent it runs, None for a plain shell, which cannot be handed
    /// off. Claude Code from a brother older than the field.
    pub agent: Option<Agent>,
    /// Whether it has a conversation to carry on, which handing it off
    /// needs. True from a brother older than the field.
    pub talked: bool,
}

/// What one message from a brother says.
#[derive(Debug, Clone, PartialEq)]
pub enum Heard {
    /// The answer to hello: its name, whether this PC may type into it,
    /// and the version it speaks.
    Hello {
        name: String,
        control: bool,
        v: u64,
    },
    Sessions(Vec<Entry>),
    /// Its projects, (path, name), in its tiles' order.
    Projects(Vec<(String, String)>),
    Out {
        id: String,
        seq: u64,
        bytes: Vec<u8>,
        reset: bool,
    },
    Size {
        id: String,
        cols: u16,
        rows: u16,
    },
    Started {
        project: String,
        id: String,
    },
    Error {
        id: Option<String>,
        text: String,
    },
    /// What it says it is doing: awake, about to sleep, or leaving.
    State(crate::brotherhood::Said),
    /// Lore this PC lacks for `project`, the repository this PC named in
    /// its own `lore`: packed entries, b64url.
    Entries {
        project: Value,
        b64: String,
    },
    /// What it holds of the lore of `project`, its repository, by
    /// writing brother: said after hello by a node, which links to no one
    /// itself, so this PC sends it what it lacks.
    Lore {
        project: Value,
        have: Value,
    },
    /// It forgot this PC: the key opens nothing any more.
    Unpaired,
    Other,
}

/// Reads one message, opened.
pub fn hear(msg: &Value) -> Heard {
    let text = |k: &str| msg.get(k).and_then(Value::as_str).map(str::to_string);
    let num = |k: &str| msg.get(k).and_then(Value::as_u64);
    match msg.get("t").and_then(Value::as_str) {
        Some("hello") => Heard::Hello {
            name: text("brother").or_else(|| text("pc")).unwrap_or_default(),
            control: msg.get("control").and_then(Value::as_bool).unwrap_or(false),
            v: num("v").unwrap_or(1),
        },
        Some("sessions") => Heard::Sessions(
            msg.get("list")
                .and_then(Value::as_array)
                .map(|l| l.iter().filter_map(entry).collect())
                .unwrap_or_default(),
        ),
        Some("projects") => Heard::Projects(
            msg.get("list")
                .and_then(Value::as_array)
                .map(|l| {
                    l.iter()
                        .filter_map(|p| {
                            let path = p.get("path")?.as_str()?;
                            let name = p.get("name")?.as_str()?;
                            Some((path.to_string(), name.to_string()))
                        })
                        .collect()
                })
                .unwrap_or_default(),
        ),
        Some("out") => match (text("id"), num("seq")) {
            (Some(id), Some(seq)) => Heard::Out {
                id,
                seq,
                bytes: text("b64")
                    .and_then(|b| crate::portal::unb64url(&b))
                    .unwrap_or_default(),
                reset: msg.get("reset").and_then(Value::as_bool).unwrap_or(false),
            },
            _ => Heard::Other,
        },
        Some("size") => match (text("id"), num("cols"), num("rows")) {
            (Some(id), Some(cols), Some(rows)) if cols > 0 && rows > 0 => Heard::Size {
                id,
                cols: cols.min(u16::MAX as u64) as u16,
                rows: rows.min(u16::MAX as u64) as u16,
            },
            _ => Heard::Other,
        },
        Some("started") => match (text("project"), text("id")) {
            (Some(project), Some(id)) => Heard::Started { project, id },
            _ => Heard::Other,
        },
        Some("error") => Heard::Error {
            id: text("id"),
            text: text("text").unwrap_or_default(),
        },
        Some("state") => match msg.get("state").cloned().map(serde_json::from_value) {
            Some(Ok(said)) => Heard::State(said),
            _ => Heard::Other,
        },
        Some("entries") => match (msg.get("project"), text("b64")) {
            (Some(project), Some(b64)) if !project.is_null() && !b64.is_empty() => Heard::Entries {
                project: project.clone(),
                b64,
            },
            _ => Heard::Other,
        },
        Some("lore") => match msg.get("project") {
            Some(project) if !project.is_null() => Heard::Lore {
                project: project.clone(),
                have: msg.get("have").cloned().unwrap_or(Value::Null),
            },
            _ => Heard::Other,
        },
        Some("unpaired") => Heard::Unpaired,
        _ => Heard::Other,
    }
}

fn entry(v: &Value) -> Option<Entry> {
    let text = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let id = text("id");
    if id.is_empty() {
        return None;
    }
    let paused = v.get("paused").and_then(Value::as_bool).unwrap_or(false);
    let ask = v
        .get("ask")
        .and_then(|a| a.get("kind"))
        .and_then(Value::as_str);
    let phase = match (paused, text("phase").as_str()) {
        (true, _) => Phase::Paused,
        (_, "working") => Phase::Working,
        (_, "waiting") if ask == Some("permission") => Phase::Waiting(WaitReason::Permission),
        (_, "waiting") => Phase::Waiting(WaitReason::Input),
        (_, "ended") => Phase::Ended,
        _ => Phase::Idle,
    };
    Some(Entry {
        id,
        name: text("name"),
        project: text("project"),
        phase,
        line: text("line"),
        paused,
        since: v.get("since").and_then(Value::as_u64).filter(|s| *s > 0),
        agent: match v.get("shell").and_then(Value::as_bool) {
            Some(true) => None,
            _ => Some(
                v.get("agent")
                    .and_then(Value::as_str)
                    .and_then(Agent::from_name)
                    .unwrap_or_default(),
            ),
        },
        talked: v.get("talked").and_then(Value::as_bool).unwrap_or(true),
    })
}

/// Where a brother's session lives, on its tile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OnBrother {
    /// The brother, by the key this PC knows it by: a PC's relay id, or a
    /// node's [`node_key`]. Two brothers may share a name, never a key, so
    /// ids and clusters go by this.
    pub id: String,
    /// The brother's name, as shown.
    pub name: String,
    /// The project's name there, which is its cluster.
    pub project: String,
    /// Whether the brother answers now. Out of reach, its tiles keep
    /// their last phase, dimmed.
    pub reach: bool,
    /// The cluster the tile keeps its place in when the session was handed
    /// to this brother from where it was shown before: None for one shown
    /// in the brother's own cluster.
    pub home: Option<String>,
}

impl OnBrother {
    /// The cluster the tile is in.
    pub fn cluster(&self) -> String {
        self.home
            .clone()
            .unwrap_or_else(|| key(&self.id, &self.project))
    }
}

/// A tile that went, kept a while so the same session arriving on
/// another brother takes its place rather than a new one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Left {
    /// Its cluster.
    pub home: String,
    /// Its place in that cluster's order.
    pub slot: Option<usize>,
    pub created: SystemTime,
    /// When it went.
    pub at: SystemTime,
}

/// How long a tile that went holds its place for the session to arrive
/// elsewhere, and how long one that arrived waits for the tile it was to
/// go: a handoff's two lists cross within its own 180 second limit.
pub const LEFT_FOR: Duration = Duration::from_secs(180);

/// The place the session `remote`, new on a brother, takes: that of the
/// tile it had until a moment ago, when one went within [`LEFT_FOR`]. Not
/// by name as well: a handoff renames it (the receiver names it after its
/// folder), and ids are only shared by sessions started in folders of one
/// name in the same second, which then also have to go and come together.
pub fn place_of<'a>(
    left: &'a HashMap<String, Left>,
    remote: &str,
    now: SystemTime,
) -> Option<&'a Left> {
    left.get(remote)
        .filter(|l| now.duration_since(l.at).is_ok_and(|d| d < LEFT_FOR))
}

/// Moves brother's tile `s`, which arrived a moment before the tile it
/// was went, into that tile's place. Nothing for one back in its own
/// brother's cluster.
pub fn take_place(s: &mut Session, left: &Left) {
    let Some(b) = s.brother.as_mut() else {
        return;
    };
    if left.home == key(&b.id, &b.project) || b.home.is_some() {
        return;
    }
    b.home = Some(left.home.clone());
    s.last_line = placed_line(&b.name, &s.last_line);
    s.created = left.created;
}

/// What a tile that kept its place says under its name: where the
/// session runs now, then what it does.
pub fn placed_line(name: &str, line: &str) -> String {
    if line.is_empty() {
        format!("on {name}")
    } else {
        format!("on {name}: {line}")
    }
}

/// The session a brother's entry is, here. `brother` is where it lives,
/// its `home` the place a tile that came from elsewhere keeps. `was` is
/// the tile as it stood, so a phase that did not change keeps its age and
/// its unread mark.
pub fn session(brother: &OnBrother, e: &Entry, was: Option<&Session>, now: SystemTime) -> Session {
    // The brother's clock says when, so a tile keeps its age when this
    // client starts again. Never later than now: the clocks may differ.
    let since = e
        .since
        .map(|secs| (SystemTime::UNIX_EPOCH + Duration::from_secs(secs)).min(now))
        .unwrap_or(now);
    let mut s = was.cloned().unwrap_or_else(|| {
        let mut s = Session::new(local_id(&brother.id, &e.id), e.name.clone(), "");
        s.since = since;
        s.created = since;
        s
    });
    s.name = e.name.clone();
    s.renamed = true;
    s.shell = e.agent.is_none();
    s.agent = e.agent.unwrap_or_default();
    s.prompted = e.talked;
    // A phone hears a finished turn as idle, so done here is idle there.
    let same = s.phase == e.phase || (s.phase == Phase::Done && e.phase == Phase::Idle);
    if !same {
        // A turn that ended there is unread here too, as a local one is.
        s.unseen = e.phase == Phase::Idle && s.phase.mid_turn();
        s.phase = if s.unseen {
            Phase::Done
        } else {
            e.phase.clone()
        };
        s.since = since;
    } else if e.since.is_some() {
        s.since = since;
    }
    let home = was
        .and_then(|w| w.brother.as_ref())
        .and_then(|b| b.home.clone())
        .or_else(|| brother.home.clone())
        // Back where it was first shown, it is the brother's own again.
        .filter(|h| *h != key(&brother.id, &e.project));
    s.last_line = match home {
        Some(_) => placed_line(&brother.name, &e.line),
        None => e.line.clone(),
    };
    s.brother = Some(OnBrother {
        project: e.project.clone(),
        reach: true,
        home,
        ..brother.clone()
    });
    s
}

/// The project folder on the brother a name stands for, when its
/// projects list names one folder so. A name two folders share is not
/// guessed between.
pub fn project_path<'a>(projects: &'a [(String, String)], name: &str) -> Option<&'a str> {
    let mut found = projects.iter().filter(|(_, n)| n == name);
    let first = found.next()?;
    found.next().is_none().then_some(first.0.as_str())
}

/// The client's side of one link: the connection id the brother gave
/// and the count every message after hello carries, so the relay cannot
/// play one back.
#[derive(Debug, Default, Clone)]
pub struct Client {
    s: Option<String>,
    n: u64,
}

impl Client {
    /// The first message on a connection. It starts a new count.
    pub fn hello(&mut self) -> Value {
        self.s = None;
        self.n = 0;
        json!({"t": "hello", "v": VERSION, "kind": "brother"})
    }

    /// Takes the answer to hello's connection id.
    pub fn heard_hello(&mut self, answer: &Value) {
        self.s = answer.get("s").and_then(Value::as_str).map(str::to_string);
    }

    /// Whether hello has been answered on this connection.
    pub fn ready(&self) -> bool {
        self.s.is_some()
    }

    /// `msg` with the connection id and the next count, as every message
    /// after hello must be.
    pub fn stamp(&mut self, mut msg: Value) -> Value {
        if let (Some(s), Some(o)) = (&self.s, msg.as_object_mut()) {
            self.n += 1;
            o.insert("s".into(), json!(s));
            o.insert("n".into(), json!(self.n));
        }
        msg
    }
}

/// How an `out` fits the stream as drawn so far.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fit {
    /// Start a fresh terminal, then draw it.
    Reset,
    /// Draw it.
    Next,
    /// Already drawn: a repeat.
    Old,
    /// Something between was lost: ask for the stream again.
    Gap,
}

/// Where each attached stream has got to, by the brother's session id.
#[derive(Debug, Default, Clone)]
pub struct Streams {
    next: HashMap<String, u64>,
}

impl Streams {
    pub fn fit(&mut self, id: &str, seq: u64, len: usize, reset: bool) -> Fit {
        let end = seq + len as u64;
        if reset {
            self.next.insert(id.to_string(), end);
            return Fit::Reset;
        }
        match self.next.get_mut(id) {
            None => Fit::Gap,
            Some(next) if seq == *next => {
                *next = end;
                Fit::Next
            }
            Some(next) if end <= *next => Fit::Old,
            Some(_) => Fit::Gap,
        }
    }

    /// The stream stopped, or is asked for again from its start.
    pub fn forget(&mut self, id: &str) {
        self.next.remove(id);
    }

    pub fn clear(&mut self) {
        self.next.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_brothers_project_has_a_key_of_its_own() {
        let k = key("vps", "horadric.dev");
        assert_eq!(split_key(&k), Some(("vps", "horadric.dev")));
        assert_ne!(k, "c:/users/me/horadric.dev");
        assert_eq!(split_key("c:/users/me/horadric.dev"), None);
        assert_eq!(title("vps", "horadric.dev"), "horadric.dev \u{b7} vps");
        // A slash in a name cannot move the split.
        assert_eq!(split_key(&key("a/b", "p")), Some(("a b", "p")));
    }

    #[test]
    fn a_node_over_ssh_folds_into_the_brother_it_says_it_is() {
        let held = std::collections::BTreeMap::from([("r1".to_string(), "K1".to_string())]);
        let hello = |id: &str, key: &str| json!({"t": "hello", "v": 3, "id": id, "key": key});
        assert_eq!(folds_into(&hello("r1", "K1"), &held), Some("r1".into()));
        // Another key under that id, an id not held, or no word at all:
        // it stays a node of its own.
        assert_eq!(folds_into(&hello("r1", "K2"), &held), None);
        assert_eq!(folds_into(&hello("r2", "K1"), &held), None);
        assert_eq!(folds_into(&hello("r1", ""), &held), None);
        assert_eq!(folds_into(&json!({"t": "hello"}), &held), None);
    }

    #[test]
    fn a_session_another_brother_owns_now_is_shown_there_alone() {
        let entry = |id: &str| Entry {
            id: id.into(),
            name: id.into(),
            project: "p".into(),
            phase: Phase::Idle,
            line: String::new(),
            paused: false,
            since: None,
            agent: None,
            talked: true,
        };
        let list = vec![entry("s1"), entry("s2"), entry("s3")];
        let owner = |b: &str| crate::brotherhood::Owner {
            n: 2,
            brother: b.into(),
            since: 0,
        };
        let owners = crate::brotherhood::Owners::from([
            ("s1".to_string(), owner("vps")),
            ("s2".to_string(), owner("lap")),
        ]);
        let ids = |l: Vec<Entry>| l.into_iter().map(|e| e.id).collect::<Vec<_>>();
        assert_eq!(ids(shown_list(&list, "lap", &owners, true)), ["s2", "s3"]);
        // A node over ssh alone is no brother of the roster: all of it.
        assert_eq!(ids(shown_list(&list, "node-x", &owners, false)).len(), 3);
    }

    #[test]
    fn two_brothers_of_one_name_are_shown_apart() {
        let named = vec![
            ("a1b2c3".to_string(), "vps".to_string()),
            ("node-vps".to_string(), "vps".to_string()),
            ("ffee".to_string(), "Desk".to_string()),
        ];
        let shown = distinct_names(&named);
        assert_eq!(shown[0].1, "vps (a1b2)");
        assert_eq!(shown[1].1, "vps (vps)");
        assert_eq!(shown[2].1, "Desk");
        assert_ne!(shown[0].1, shown[1].1);
    }

    fn on(id: &str) -> OnBrother {
        OnBrother {
            id: id.into(),
            name: id.to_uppercase(),
            project: String::new(),
            reach: true,
            home: None,
        }
    }

    #[test]
    fn a_node_key_is_never_split() {
        let k = node_key("agent@exit1/eu");
        assert_eq!(k, "node-agent-exit1-eu");
        let id = local_id(&k, "x-1");
        assert_eq!(remote_id(&id, &k), Some("x-1"));
        assert_eq!(split_key(&key(&k, "p")), Some((k.as_str(), "p")));
    }

    #[test]
    fn two_brothers_of_one_name_keep_apart() {
        let a = OnBrother {
            name: "Alpha".into(),
            ..on("a1")
        };
        let b = OnBrother {
            name: "Alpha".into(),
            ..on("b2")
        };
        let e = Entry {
            id: "demo-1".into(),
            name: "x".into(),
            project: "demo".into(),
            phase: Phase::Idle,
            line: String::new(),
            paused: false,
            since: None,
            agent: Some(Agent::Claude),
            talked: true,
        };
        let t = SystemTime::UNIX_EPOCH;
        let (sa, sb) = (session(&a, &e, None, t), session(&b, &e, None, t));
        assert_ne!(sa.id, sb.id);
        assert_ne!(sa.brother.unwrap().cluster(), sb.brother.unwrap().cluster());
    }

    fn e0() -> Entry {
        Entry {
            id: "s1".into(),
            name: "fix".into(),
            project: "p".into(),
            phase: Phase::Working,
            line: "Bash".into(),
            paused: false,
            since: None,
            agent: Some(Agent::Claude),
            talked: true,
        }
    }

    #[test]
    fn a_handed_off_tile_keeps_its_place() {
        let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        let mut left = HashMap::new();
        left.insert(
            "s1".to_string(),
            Left {
                home: "c:/work/app".into(),
                slot: Some(2),
                created: SystemTime::UNIX_EPOCH,
                at: t0,
            },
        );
        assert_eq!(
            place_of(&left, "s1", t0 + Duration::from_secs(5))
                .unwrap()
                .slot,
            Some(2)
        );
        assert!(place_of(&left, "s1", t0 + LEFT_FOR).is_none());
        assert!(place_of(&left, "s2", t0).is_none());
        // Arrived before the tile it was went: it moves into its place.
        let mut early = session(
            &on("d1"),
            &Entry {
                project: "app".into(),
                ..e0()
            },
            None,
            t0,
        );
        take_place(&mut early, &left["s1"]);
        assert_eq!(early.brother.as_ref().unwrap().cluster(), "c:/work/app");
        assert_eq!(
            (early.last_line.as_str(), early.created),
            ("on D1: Bash", SystemTime::UNIX_EPOCH)
        );
        // Not when the place was its own brother's cluster.
        let mut own = session(
            &on("d1"),
            &Entry {
                project: "app".into(),
                ..e0()
            },
            None,
            t0,
        );
        let back = Left {
            home: key("d1", "app"),
            ..left["s1"].clone()
        };
        take_place(&mut own, &back);
        assert_eq!(own.brother.unwrap().home, None);
        let desk = OnBrother {
            name: "Desktop".into(),
            home: Some("c:/work/app".into()),
            ..on("d1")
        };
        let e = Entry {
            id: "s1".into(),
            name: "fix".into(),
            project: "app".into(),
            phase: Phase::Working,
            line: "Bash".into(),
            paused: false,
            since: None,
            agent: Some(Agent::Claude),
            talked: true,
        };
        let s = session(&desk, &e, None, t0);
        let b = s.brother.clone().unwrap();
        assert_eq!(b.cluster(), "c:/work/app");
        assert_eq!(s.last_line, "on Desktop: Bash");
        // The place stays with the tile while it lists, whatever the
        // caller passes next time.
        let again = session(&on("d1"), &e, Some(&s), t0);
        assert_eq!(again.brother.unwrap().cluster(), "c:/work/app");
        // One that never moved is in its brother's own cluster.
        let own = session(&on("d1"), &e, None, t0);
        assert_eq!(own.brother.unwrap().cluster(), key("d1", "app"));
        assert_eq!(own.last_line, "Bash");
        assert_eq!(placed_line("vps", ""), "on vps");
        // Handed back to the brother whose cluster it kept, it is its own.
        let back = OnBrother {
            home: Some(key("d1", "app")),
            ..on("d1")
        };
        let home = session(&back, &e, None, t0);
        assert_eq!(
            (home.brother.unwrap().home, home.last_line.as_str()),
            (None, "Bash")
        );
    }

    #[test]
    fn a_brothers_session_id_names_the_brother() {
        let id = local_id("vps", "x-2");
        assert_eq!(id, "x-2@vps");
        assert_eq!(remote_id(&id, "vps"), Some("x-2"));
        assert_eq!(remote_id(&id, "laptop"), None);
        assert_eq!(remote_id("x-2", "vps"), None);
    }

    #[test]
    fn a_pairing_link_reads() {
        let link = "https://portal.horadric.dev/#r=wss%3A%2F%2Fportal.horadric.dev\
                    &id=abc&p=KEY_x-y&x=1760000300&n=My%20PC";
        let t = ticket(link).unwrap();
        assert_eq!(t.relay, "wss://portal.horadric.dev");
        assert_eq!(t.id, "abc");
        assert_eq!(t.key, "KEY_x-y");
        assert_eq!(t.until, 1_760_000_300);
        assert_eq!(t.name, "My PC");
        assert_eq!(ticket("https://portal.horadric.dev/"), None);
        assert_eq!(ticket("https://x/#r=a&id=b&x=1"), None);
        assert_eq!(
            phone_url("wss://portal.horadric.dev/", "abc"),
            "wss://portal.horadric.dev/phone?id=abc"
        );
    }

    #[test]
    fn a_brother_is_heard() {
        let hello = json!({"t":"hello","v":3,"pc":"PC","brother":"Alpha","control":true,"s":"S"});
        assert_eq!(
            hear(&hello),
            Heard::Hello {
                name: "Alpha".into(),
                control: true,
                v: 3
            }
        );
        let list = json!({"t":"sessions","list":[
            {"id":"a","name":"A","project":"p","phase":"working","line":"Bash","paused":false},
            {"id":"b","name":"B","project":"p","phase":"waiting","line":"ok?","paused":false,
             "ask":{"kind":"permission"}},
            {"id":"c","name":"C","project":"q","phase":"idle","line":"","paused":true},
            {"name":"no id"}
        ]});
        let Heard::Sessions(l) = hear(&list) else {
            panic!("not sessions");
        };
        assert_eq!(l.len(), 3);
        assert_eq!(l[0].phase, Phase::Working);
        assert_eq!(l[1].phase, Phase::Waiting(WaitReason::Permission));
        assert_eq!(l[2].phase, Phase::Paused);
        assert_eq!(l[0].since, None);
        let timed = json!({"t":"sessions","list":[{"id":"a","since":1700000000}]});
        let Heard::Sessions(l) = hear(&timed) else {
            panic!("not sessions");
        };
        assert_eq!(l[0].since, Some(1_700_000_000));
        let out = json!({"t":"out","id":"a","seq":5,"b64":"aGk","reset":true});
        assert_eq!(
            hear(&out),
            Heard::Out {
                id: "a".into(),
                seq: 5,
                bytes: b"hi".to_vec(),
                reset: true
            }
        );
        assert_eq!(
            hear(&json!({"t":"size","id":"a","cols":80,"rows":0})),
            Heard::Other
        );
        assert_eq!(
            hear(&json!({"t":"state","state":"sleeping"})),
            Heard::State(crate::brotherhood::Said::Sleeping)
        );
        assert_eq!(
            hear(&json!({"t":"state","state":"awake"})),
            Heard::State(crate::brotherhood::Said::Awake)
        );
        assert_eq!(hear(&json!({"t":"state","state":"dreaming"})), Heard::Other);
        let repo = json!({"remotes": ["git@x:r.git"], "roots": []});
        assert_eq!(
            hear(&json!({"t":"entries","project":repo,"b64":"eA","from":1})),
            Heard::Entries {
                project: repo.clone(),
                b64: "eA".into()
            }
        );
        // A catch up with nothing in it carries no entries to take.
        assert_eq!(
            hear(&json!({"t":"entries","project":repo,"b64":""})),
            Heard::Other
        );
        assert_eq!(
            hear(&json!({"t":"lore","project":repo,"have":{"a":3}})),
            Heard::Lore {
                project: repo.clone(),
                have: json!({"a": 3})
            }
        );
        assert_eq!(hear(&json!({"t":"lore","have":{}})), Heard::Other);
        assert_eq!(
            hear(&json!({"t":"projects","list":[{"path":"C:/p","name":"p"}]})),
            Heard::Projects(vec![("C:/p".into(), "p".into())])
        );
        assert_eq!(
            hear(&json!({"t":"error","id":"a","text":"paused"})),
            Heard::Error {
                id: Some("a".into()),
                text: "paused".into()
            }
        );
    }

    #[test]
    fn every_message_after_hello_is_stamped() {
        let mut c = Client::default();
        assert_eq!(c.hello()["kind"], "brother");
        assert!(!c.ready());
        assert_eq!(c.stamp(json!({"t":"x"})), json!({"t":"x"}));
        c.heard_hello(&json!({"t":"hello","s":"S"}));
        assert!(c.ready());
        assert_eq!(c.stamp(json!({"t":"x"}))["n"], 1);
        let second = c.stamp(json!({"t":"y"}));
        assert_eq!(
            (second["s"].as_str(), second["n"].as_u64()),
            (Some("S"), Some(2))
        );
        // A new connection counts again.
        c.hello();
        assert!(!c.ready());
    }

    #[test]
    fn a_stream_is_drawn_in_order() {
        let mut s = Streams::default();
        assert_eq!(s.fit("a", 10, 3, false), Fit::Gap);
        assert_eq!(s.fit("a", 10, 3, true), Fit::Reset);
        assert_eq!(s.fit("a", 13, 2, false), Fit::Next);
        assert_eq!(s.fit("a", 13, 2, false), Fit::Old);
        assert_eq!(s.fit("a", 20, 1, false), Fit::Gap);
        s.forget("a");
        assert_eq!(s.fit("a", 15, 1, false), Fit::Gap);
    }

    #[test]
    fn a_brothers_entry_becomes_a_tile() {
        let t0 = SystemTime::UNIX_EPOCH;
        let t1 = t0 + std::time::Duration::from_secs(60);
        let e = Entry {
            id: "a".into(),
            name: "A".into(),
            project: "p".into(),
            phase: Phase::Working,
            line: "Bash".into(),
            paused: false,
            since: None,
            agent: Some(Agent::Claude),
            talked: true,
        };
        let s = session(&on("vps"), &e, None, t0);
        assert_eq!(s.id, "a@vps");
        assert_eq!(s.cwd, "", "no folder here: it is the brother's");
        assert_eq!(s.label(), "A");
        assert_eq!(s.brother.as_ref().unwrap().project, "p");
        // The same phase keeps its age.
        let again = session(&on("vps"), &e, Some(&s), t1);
        assert_eq!(again.since, t0);
        // A turn that ended there is unread here.
        let idle = Entry {
            phase: Phase::Idle,
            ..e.clone()
        };
        let done = session(&on("vps"), &idle, Some(&again), t1);
        assert_eq!(
            (done.phase.clone(), done.unseen, done.since),
            (Phase::Done, true, t1)
        );
        // And stays so while the brother says idle.
        let later = t1 + std::time::Duration::from_secs(5);
        let still = session(&on("vps"), &idle, Some(&done), later);
        assert_eq!((still.phase, still.since), (Phase::Done, t1));
    }

    #[test]
    fn a_tile_takes_its_age_from_the_brother() {
        let t0 = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000);
        let e = Entry {
            id: "a".into(),
            name: "A".into(),
            project: "p".into(),
            phase: Phase::Working,
            line: String::new(),
            paused: false,
            since: Some(400),
            agent: Some(Agent::Claude),
            talked: true,
        };
        // A client that just started still shows when the phase began.
        let s = session(&on("vps"), &e, None, t0);
        let at = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(400);
        assert_eq!((s.since, s.created), (at, at));
        // A brother clock ahead of this one is held to now.
        let ahead = Entry {
            since: Some(5_000),
            agent: Some(Agent::Claude),
            talked: true,
            ..e.clone()
        };
        assert_eq!(session(&on("vps"), &ahead, None, t0).since, t0);
        // A phase that changed there takes the brother's time too.
        let idle = Entry {
            phase: Phase::Idle,
            since: Some(900),
            agent: Some(Agent::Claude),
            talked: true,
            ..e.clone()
        };
        let done = session(&on("vps"), &idle, Some(&s), t0);
        let at = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(900);
        assert_eq!((done.phase, done.since), (Phase::Done, at));
    }

    #[test]
    fn a_project_is_found_by_a_name_only_one_folder_has() {
        let p = vec![
            ("C:/a/x".to_string(), "x".to_string()),
            ("C:/a/y".to_string(), "y".to_string()),
            ("D:/y".to_string(), "y".to_string()),
        ];
        assert_eq!(project_path(&p, "x"), Some("C:/a/x"));
        assert_eq!(project_path(&p, "y"), None);
        assert_eq!(project_path(&p, "z"), None);
    }

    #[test]
    fn a_brother_says_which_tiles_run_claude_code() {
        let list = json!({"t":"sessions","list":[
            {"id":"a"},
            {"id":"b","agent":"claude","shell":false},
            {"id":"c","agent":"codex","shell":false},
            {"id":"d","agent":"claude","shell":true},
            {"id":"e","agent":"claude","shell":false,"talked":false}
        ]});
        let Heard::Sessions(l) = hear(&list) else {
            panic!("not sessions");
        };
        let agents: Vec<_> = l.iter().map(|e| e.agent).collect();
        assert_eq!(
            agents,
            [
                Some(Agent::Claude),
                Some(Agent::Claude),
                Some(Agent::Codex),
                None,
                Some(Agent::Claude)
            ]
        );
        assert!(l[0].talked && !l[4].talked);
        let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        let shell = session(&on("b"), &l[3], None, t0);
        assert!(shell.shell);
        let codex = session(&on("b"), &l[2], None, t0);
        assert!(!codex.shell);
        assert_eq!(codex.agent, Agent::Codex);
        assert!(!session(&on("b"), &l[4], None, t0).prompted);
    }
}
