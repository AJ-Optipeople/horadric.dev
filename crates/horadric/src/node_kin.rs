//! A node as a brother of the machines it joined over the relay: it
//! serves them as any brother does, through the one `Server`, and is a
//! client of each in turn, as a Windows PC is of its brothers. It shows
//! no tiles, so as a client it says hello, tells what it holds of each
//! shared lore and takes in what it lacks, hears whether each brother is
//! awake and what it lists and who keeps it, and sends what a node itself
//! asks of a brother: its handoffs' `bases` asks and bundles, and its
//! snapshots to its keeper. Each brother's asks carry the connection id
//! the brother's hello answer gave and a count that only goes up
//! (`afar::Client`), so they wait until that answer is in.
//!
//! Within the node a relay brother goes by its relay id, which no other
//! shares, and is shown by its name.

use std::collections::{HashMap, HashSet};

use horadric_core::afar::{self, Client};
use horadric_core::brotherhood::{Heard, Said as State};
use horadric_core::portal::Kind;
use horadric_core::rostering;
use horadric_core::serving::Serve;
use horadric_core::standing::Standing;
use serde_json::{json, Value};

use super::{machine_name, relay, Machine};

/// One linked brother, by the serving thread's channel.
pub(super) struct Peer {
    pub(super) id: String,
    pub(super) name: String,
    /// The relay's channel.
    ch: u32,
    client: Client,
    /// What waits for the hello answer.
    pending: Vec<Value>,
    /// The sessions it lists, by its own ids.
    pub(super) listed: HashSet<String>,
    /// It listed its sessions at least once on this link, so what it does
    /// not list is its word.
    pub(super) heard_list: bool,
    /// It said its lore changed: this node says again what it holds, at
    /// the next tick, once for all the projects it named.
    lore_due: bool,
}

/// Every linked brother, and what was last heard of each, linked or not.
#[derive(Default)]
pub(super) struct Kin {
    pub(super) peers: HashMap<u32, Peer>,
    /// By brother id: what it last said of itself and when it was heard.
    pub(super) heard: HashMap<String, Heard>,
    /// The project folders the brothers were last told of, sorted.
    lore_dirs: Vec<String>,
}

impl Kin {
    /// The channel of the brother with this id, the newest should there
    /// be two.
    fn chan(&self, id: &str) -> Option<u32> {
        self.peers
            .iter()
            .filter(|(_, p)| p.id == id)
            .map(|(ich, _)| *ich)
            .max()
    }

    /// The brother of a name or id, as a PC's `handoff` or a human names
    /// it, by its id. A name two brothers share names neither: two
    /// machines may be called the same, and the wrong one must not get a
    /// session.
    pub(super) fn id_of(&self, who: &str) -> Option<String> {
        if self.peers.values().any(|p| p.id == who) {
            return Some(who.to_string());
        }
        let mut ids: Vec<&str> = self
            .peers
            .values()
            .filter(|p| p.name == who)
            .map(|p| p.id.as_str())
            .collect();
        ids.sort();
        ids.dedup();
        match ids.as_slice() {
            [one] => Some(one.to_string()),
            _ => None,
        }
    }

    /// Whether brother `id` is linked and has answered hello, so what is
    /// sent it is taken.
    pub(super) fn ready(&self, id: &str) -> bool {
        self.peers.values().any(|p| p.id == id && p.client.ready())
    }

    /// The ids of the brothers ready now.
    pub(super) fn ready_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self
            .peers
            .values()
            .filter(|p| p.client.ready())
            .map(|p| p.id.clone())
            .collect();
        ids.sort();
        ids.dedup();
        ids
    }

    /// What was last heard of brother `id`.
    pub(super) fn heard_of(&self, id: &str) -> Option<Heard> {
        self.heard.get(id).copied()
    }

    /// Sends `msg` to brother `id`, stamped, or keeps it for when its
    /// hello is answered. False when it is not linked.
    pub(super) fn tell(&mut self, id: &str, msg: Value) -> bool {
        let Some(ich) = self.chan(id) else {
            return false;
        };
        let Some(p) = self.peers.get_mut(&ich) else {
            return false;
        };
        if !p.client.ready() {
            p.pending.push(msg);
            return true;
        }
        let msg = p.client.stamp(msg);
        relay::reply(p.ch, &msg)
    }
}

impl Machine {
    /// The name brother `who` is shown by: a relay brother's own, else
    /// what a viewer called itself.
    pub(super) fn shown(&self, who: &str) -> String {
        self.kin
            .peers
            .values()
            .find(|p| p.id == who)
            .map_or_else(|| who.to_string(), |p| p.name.clone())
    }

    /// A brother's channel came up: this node greets it as a client.
    pub(super) fn kin_up(&mut self, ich: u32, ch: u32, id: &str, name: &str) {
        let now = relay::now();
        self.kin.peers.insert(
            ich,
            Peer {
                id: id.to_string(),
                name: name.to_string(),
                ch,
                client: Client::default(),
                pending: Vec::new(),
                listed: HashSet::new(),
                heard_list: false,
                lore_due: false,
            },
        );
        self.kin.heard.insert(
            id.to_string(),
            Heard {
                said: State::Awake,
                last: now,
                linked: true,
            },
        );
        self.names.insert(ich, id.to_string());
        if let Some(p) = self.kin.peers.get_mut(&ich) {
            let hello = p.client.hello();
            relay::reply(ch, &hello);
        }
        eprintln!("horadric node: linked to {name}");
    }

    pub(super) fn kin_down(&mut self, ich: u32) {
        if let Some(p) = self.kin.peers.remove(&ich) {
            let still = self.kin.peers.values().any(|o| o.id == p.id);
            if !still {
                if let Some(h) = self.kin.heard.get_mut(&p.id) {
                    h.linked = false;
                    h.last = relay::now();
                }
            }
            eprintln!("horadric node: {} is out of reach", p.name);
        }
        self.names.remove(&ich);
    }

    /// What a brother answered this node, as its client.
    pub(super) fn kin_heard(&mut self, ich: u32, msg: &Value) {
        let Some(id) = self.kin.peers.get(&ich).map(|p| p.id.clone()) else {
            return;
        };
        if let Some(h) = self.kin.heard.get_mut(&id) {
            h.last = relay::now();
        }
        let t = msg["t"].as_str().unwrap_or_default();
        if std::env::var_os("HORADRIC_DEBUG").is_some() && t != "out" {
            eprintln!("horadric node: {} says {t}", self.shown(&id));
        }
        match t {
            "hello" => self.kin_hello(ich, &id, msg),
            // Answers to this node's own handoffs; a brother's asks of
            // it come through the server.
            "bases" => {
                let _ = self.serve_handing(&id, msg);
            }
            "took" => self.heard_answer(&id, msg),
            "error" if msg["for"] == "bundle" || msg["for"] == "handoff" => {
                self.heard_answer(&id, msg)
            }
            "error" => eprintln!(
                "horadric node: {} says {}",
                self.shown(&id),
                msg["text"].as_str().unwrap_or("no")
            ),
            "kept" => self.keeping_kept(&id, msg),
            "roster" => self.keeping_roster(&id, msg),
            "sessions" => {
                if let Some(p) = self.kin.peers.get_mut(&ich) {
                    p.listed = msg["list"]
                        .as_array()
                        .map(|l| {
                            l.iter()
                                .filter_map(|e| e["id"].as_str().map(str::to_string))
                                .collect()
                        })
                        .unwrap_or_default();
                    p.heard_list = true;
                }
            }
            "state" => {
                if let (Some(h), afar::Heard::State(said)) =
                    (self.kin.heard.get_mut(&id), afar::hear(msg))
                {
                    h.said = said;
                }
            }
            // The brother's server says its lore changed, a project opened
            // there: what this node holds is said again, so a project both
            // have now is caught up without waiting days for a relink.
            "lore" => {
                if let Some(p) = self.kin.peers.get_mut(&ich) {
                    p.lore_due = true;
                }
            }
            "entries" => {
                if !self.kin_takes_lore(&id) {
                    eprintln!(
                        "horadric node: lore from {} is not taken: it may not control this node",
                        self.shown(&id)
                    );
                    return;
                }
                if let (false, Some(b64)) = (msg["project"].is_null(), msg["b64"].as_str()) {
                    let project = msg["project"].clone();
                    self.lore.import(&project, b64, horadric_hooks::lore::now());
                }
            }
            _ => {}
        }
    }

    /// The brother answered hello: what waited goes, then what this node
    /// holds of each shared lore, so it sends what this node lacks and
    /// then what is written after, and who owns this node's sessions.
    fn kin_hello(&mut self, ich: u32, id: &str, msg: &Value) {
        let Some(p) = self.kin.peers.get_mut(&ich) else {
            return;
        };
        if msg["v"].as_u64().unwrap_or(0) < afar::VERSION {
            eprintln!(
                "horadric node: {} needs a newer Horadric to be a brother",
                p.name
            );
            return;
        }
        p.client.heard_hello(msg);
        let waiting = std::mem::take(&mut p.pending);
        let ch = p.ch;
        for m in waiting {
            let m = p.client.stamp(m);
            relay::reply(ch, &m);
        }
        self.kin_lore_ask(id);
        self.keeping_linked(id);
    }

    /// Whether this node takes in the lore entries brother `id` sends: only
    /// from one it may be controlled by (see `afar::takes_entries`).
    fn kin_takes_lore(&self, id: &str) -> bool {
        let Some(dir) = horadric_hooks::state_dir() else {
            return false;
        };
        let held = relay::store().get(id).is_some();
        afar::takes_entries(false, held, Standing::load(&dir).controls(id))
    }

    /// Tells brother `id` what this node holds of each shared lore, so it
    /// sends what this node lacks: not to one whose entries are not taken.
    fn kin_lore_ask(&mut self, id: &str) {
        if !self.kin_takes_lore(id) {
            return;
        }
        for (project, have) in self.lore_projects() {
            self.kin
                .tell(id, json!({"t": "lore", "project": project, "have": have}));
        }
    }

    /// Says again what this node holds to each brother whose lore changed,
    /// and to every one when this node's projects did: a brother's server
    /// shares only the projects this node named.
    pub(super) fn kin_lore_tick(&mut self) {
        let mut dirs = super::project_dirs(&self.node);
        dirs.sort();
        dirs.dedup();
        let changed = dirs != self.kin.lore_dirs;
        self.kin.lore_dirs = dirs;
        let mut due: Vec<String> = self
            .kin
            .peers
            .values_mut()
            .filter(|p| p.client.ready())
            .filter_map(|p| (std::mem::take(&mut p.lore_due) || changed).then(|| p.id.clone()))
            .collect();
        due.sort();
        due.dedup();
        for id in due {
            self.kin_lore_ask(&id);
        }
    }

    /// Whether brother `who` may type, start and resume here, or only
    /// watch: a relay brother as `standing.json` says, a viewer over
    /// `ssh` always, since only the node's own user's keys reach it.
    pub(super) fn kin_control(&self, who: &str, _kind: Kind) -> bool {
        if !self.kin.peers.values().any(|p| p.id == who) {
            return true;
        }
        let Some(dir) = horadric_hooks::state_dir() else {
            return false;
        };
        let joined = relay::store().get(who).is_some();
        joined && Standing::load(&dir).controls(who)
    }

    /// The roster this node serves its brothers.
    pub(super) fn kin_roster(&self) -> Vec<Value> {
        let (Some(dir), Some(me)) = (horadric_hooks::state_dir(), relay::me()) else {
            return Vec::new();
        };
        let sees: Vec<String> =
            relay::with(|b| b.linked().into_iter().map(|(_, id)| id).collect()).unwrap_or_default();
        rostering::served(&dir, &me, &machine_name(), &relay::LinkCrypto, &sees)
            .map(|r| rostering::messages(&r))
            .unwrap_or_default()
    }
}
