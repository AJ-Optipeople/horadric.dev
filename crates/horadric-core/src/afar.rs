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
use std::time::SystemTime;

use serde_json::{json, Value};

use crate::session::{Phase, Session, WaitReason};

/// What a cluster key of a brother's project starts with. A local key is
/// a folder, so it never does.
const KEY_PREFIX: &str = "brother:";

/// The protocol this client speaks.
pub const VERSION: u64 = 3;

/// The cluster key of `project` on brother `brother`. Its own key even
/// for a repository this PC has too: they are two working trees.
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
/// session has too, so the brother is part of it.
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
    })
}

/// Where a brother's session lives, on its tile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OnBrother {
    /// The brother's name, as it answered hello.
    pub name: String,
    /// The project's name there, which is its cluster.
    pub project: String,
    /// Whether the brother answers now. Out of reach, its tiles keep
    /// their last phase, dimmed.
    pub reach: bool,
}

/// The session a brother's entry is, here. `was` is the tile as it stood,
/// so a phase that did not change keeps its age and its unread mark.
pub fn session(brother: &str, e: &Entry, was: Option<&Session>, now: SystemTime) -> Session {
    let mut s = was.cloned().unwrap_or_else(|| {
        let mut s = Session::new(local_id(brother, &e.id), e.name.clone(), "");
        s.since = now;
        s.created = now;
        s
    });
    s.name = e.name.clone();
    s.renamed = true;
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
        s.since = now;
    }
    s.last_line = e.line.clone();
    s.brother = Some(OnBrother {
        name: brother.to_string(),
        project: e.project.clone(),
        reach: true,
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
        };
        let s = session("vps", &e, None, t0);
        assert_eq!(s.id, "a@vps");
        assert_eq!(s.cwd, "", "no folder here: it is the brother's");
        assert_eq!(s.label(), "A");
        assert_eq!(s.brother.as_ref().unwrap().project, "p");
        // The same phase keeps its age.
        let again = session("vps", &e, Some(&s), t1);
        assert_eq!(again.since, t0);
        // A turn that ended there is unread here.
        let idle = Entry {
            phase: Phase::Idle,
            ..e.clone()
        };
        let done = session("vps", &idle, Some(&again), t1);
        assert_eq!(
            (done.phase.clone(), done.unseen, done.since),
            (Phase::Done, true, t1)
        );
        // And stays so while the brother says idle.
        let later = t1 + std::time::Duration::from_secs(5);
        let still = session("vps", &idle, Some(&done), later);
        assert_eq!((still.phase, still.since), (Phase::Done, t1));
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
}
