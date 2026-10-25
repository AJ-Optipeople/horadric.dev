//! The app's side of keepers and takeover (`horadric_core::keeping` has
//! the bookkeeping, `horadric_core::keepers` the snapshots; this is the
//! glue to tiles, consoles and links).
//!
//! As owner, at every Claude Code turn end it cuts a snapshot on a thread
//! and sends it to its first keeper in `keep` parts through
//! [`App::far_tell`]. As keeper it joins the parts and files them in a
//! [`Store`] under its state folder, pruned once the session ends or
//! moves, once its brother has not listed it for ten minutes while
//! linked, once it is a month old, and oldest first past the disk cap in
//! `keeping.json`. When a brother has been silent two minutes its tiles offer
//! "Take over on <this PC>", which lands the kept snapshot as a handoff
//! lands and resumes it told what happened; with the brother's switch on
//! that happens by itself after ten. Whoever owns a session tells every
//! brother with `takeover`, at once and again whenever a link comes up,
//! and a brother that hears one of its sessions went keeps it paused, its
//! work on `horadric/orphaned/<session>`.
//!
//! A keeper that starts while an owner is dead still has its stores, so
//! it shows them as that owner's tiles, silent since it last heard of
//! it, and offers takeover once the owner has had two minutes to link.
//!
//! Everything but cutting, landing and committing runs on the UI thread.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use horadric_core::brotherhood::{
    clock, orphan_branch, Heard, Owner, Owners, Presence, Roster, SILENT_SECS,
};
use horadric_core::handoff::{takeover_line, Record};
use horadric_core::keeper::{self, Cut, Joins, Pruning};
use horadric_core::keepers::{too_old, Store};
use horadric_core::keeping::{
    self, auto_due, keep_messages, kept_entry, kept_full, kept_of, owners_of, restored_heard,
    seen_due, stores, takeable, takeover_message, waited, Book, Holding, Next, Seen, Untakeable,
};
use horadric_core::serving::Job;
use horadric_core::{mesh, Phase};
use serde_json::Value;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

use super::{unix_now, App, WM_HORADRIC_FAR};
use crate::joining;
use crate::portal_pair::pc_name;
use crate::toast::Kind;

/// A turn that ended, heard by the hook thread: the session and its
/// transcript.
static TURNS: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());

/// What threads hand back to the UI thread.
enum Back {
    /// A snapshot cut for `keeper` (id, name).
    Cut {
        id: String,
        keeper: (String, String),
        cut: Result<Cut, String>,
    },
    /// A takeover landed, or why it did not.
    Landed {
        store: PathBuf,
        holding: Holding,
        landed: Result<horadric_core::handing::Landed, String>,
    },
}

static BACK: Mutex<Vec<Back>> = Mutex::new(Vec::new());

/// One snapshot at a time goes into a keeper's store, so two that came
/// close together never write one repository at once.
static RECEIVING: Mutex<()> = Mutex::new(());

#[derive(Default)]
struct State {
    loaded: bool,
    book: Book,
    /// This PC's id, as the relay and the roster name it.
    me: String,
    /// Sessions with a snapshot being cut, and a turn that ended meanwhile.
    cutting: HashSet<String>,
    again: HashMap<String, String>,
    /// Keep parts arriving, by (brother, session).
    joins: Joins,
    /// Brothers told the owners since their link last came up.
    told: HashSet<String>,
    /// Sessions held back at start until the keeper is heard from.
    held: HashSet<String>,
    /// When a kept session goes because its brother no longer lists it.
    pruning: Pruning,
    /// When the stores were last looked at for their age.
    aged: u64,
    /// Takeovers under way, by the brother's tile id.
    taking: HashSet<String>,
    /// When this run began, so an owner not heard from since gets its
    /// chance to link before anything is taken over.
    started: u64,
    /// Whether the kept sessions were shown as tiles at start.
    restored: bool,
    /// When the keeper's word of its owners was last written.
    noted: u64,
    /// Sessions handed to another, which go from here without ending.
    handed: HashSet<String>,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::default();
}

fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        if s.started == 0 {
            s.started = unix_now();
        }
        if !s.loaded {
            s.loaded = true;
            if let Some(dir) = horadric_hooks::state_dir() {
                s.book = Book::load(&dir);
                s.me = horadric_core::portal::Config::load(&dir)
                    .map(|c| horadric_core::portal::id(&c.secret))
                    .unwrap_or_default();
            }
        }
        f(&mut s)
    })
}

fn save(book: &Book) {
    if let Some(dir) = horadric_hooks::state_dir() {
        if let Err(e) = book.save(&dir) {
            eprintln!("horadric: cannot keep the keeping book: {e}");
        }
    }
}

fn debug(what: &str) {
    if std::env::var_os("HORADRIC_DEBUG").is_some() {
        eprintln!("keeping: {what}");
    }
}

fn wake(notify: isize) {
    unsafe {
        let _ = PostMessageW(
            Some(HWND(notify as *mut _)),
            WM_HORADRIC_FAR,
            WPARAM(0),
            LPARAM(0),
        );
    }
}

fn back(b: Back, notify: isize) {
    if let Ok(mut q) = BACK.lock() {
        q.push(b);
    }
    wake(notify);
}

/// The hook thread heard a Claude Code turn end in session `id`.
pub(super) fn turn_ended(id: &str, transcript: &str) {
    if transcript.is_empty() {
        return;
    }
    if let Ok(mut t) = TURNS.lock() {
        t.push((id.to_string(), transcript.to_string()));
    }
}

/// The joined brothers, (id, name).
fn joined() -> Vec<(String, String)> {
    horadric_hooks::state_dir()
        .and_then(|d| joining::Store::load(&d))
        .map(|s| {
            s.brothers
                .iter()
                .map(|b| (b.id.clone(), b.name.clone()))
                .collect()
        })
        .unwrap_or_default()
}

/// A joined brother's id by the name its link goes by.
fn id_of(name: &str) -> Option<String> {
    let joined = joined();
    // A link goes by the brother's id, which is its key here; a name is
    // what an older holding or the wire may give.
    if joined.iter().any(|(i, _)| i == name) {
        return Some(name.to_string());
    }
    // A name two brothers share names neither.
    let mut named = joined.into_iter().filter(|(_, n)| n == name);
    let (id, _) = named.next()?;
    named.next().is_none().then_some(id)
}

/// A joined brother's name by its id.
fn name_of(id: &str) -> Option<String> {
    joined().into_iter().find(|(i, _)| i == id).map(|(_, n)| n)
}

/// The roster as known here, for names.
fn names(me: &str) -> Roster {
    horadric_hooks::state_dir()
        .map(|d| horadric_core::rostering::known(&d, me, &pc_name()))
        .unwrap_or_default()
}

/// Every session's owner, as the roster holds them.
fn owners() -> Owners {
    horadric_hooks::state_dir()
        .map(|d| mesh::owners(&d))
        .unwrap_or_default()
}

/// Changes the owners the roster holds.
fn owning<R: Default>(f: impl FnOnce(&mut Owners) -> R) -> R {
    match horadric_hooks::state_dir() {
        Some(d) => mesh::update(&d, |r| f(&mut r.sessions)),
        None => R::default(),
    }
}

/// What the tray's keeping lines show: each joined brother (id, name),
/// the first keeper, and whether it takes over by itself.
pub(crate) fn tray_state() -> (Vec<(String, String)>, Option<String>, bool) {
    let book = horadric_hooks::state_dir()
        .map(|d| Book::load(&d))
        .unwrap_or_default();
    (
        joined(),
        book.keeper().map(str::to_string),
        book.auto_takeover.is_some(),
    )
}

/// The tray's picks, made outside the app's borrow and read back by the
/// UI thread at its next tick.
pub(crate) fn set_keeper(keeper: Option<String>) {
    let Some(dir) = horadric_hooks::state_dir() else {
        return;
    };
    let mut book = Book::load(&dir);
    book.set_keeper(keeper.as_deref());
    save(&book);
    STATE.with(|s| s.borrow_mut().loaded = false);
}

pub(crate) fn set_auto(on: bool) {
    let Some(dir) = horadric_hooks::state_dir() else {
        return;
    };
    let mut book = Book::load(&dir);
    book.auto_takeover = on.then_some(keeping::AUTO_AFTER);
    save(&book);
    STATE.with(|s| s.borrow_mut().loaded = false);
}

/// The brother of a brother's tile and its own id there.
fn remote_of(app: &App, id: &str) -> Option<(String, String)> {
    let r = app.shared.registry.lock().ok()?;
    let key = r.get(id)?.brother.as_ref()?.id.clone();
    let remote = horadric_core::afar::remote_id(id, &key)?.to_string();
    Some((key, remote))
}

/// The keeper's store and holding of brother `from`'s session `remote`,
/// with the name this PC knows the brother by.
fn held_of(from: &str, remote: &str) -> Option<(Store, Holding)> {
    let dir = horadric_hooks::state_dir()?;
    let joined = joining::Store::load(&dir).unwrap_or_default();
    keeper::held_of(&keeping::root(&dir), &joined, from, remote)
}

/// Says why session `name` kept in `store` is not taken over. One that
/// moved on since its snapshot goes from the store: it is behind for good.
fn untakeable(store: &Store, name: &str, why: &Untakeable) -> String {
    match why {
        Untakeable::Stuck => format!("{name} has a number that cannot go up."),
        Untakeable::MovedOn(o) => {
            let me = with(|s| s.me.clone());
            let owner = names(&me).name(&o.brother).to_string();
            debug(&format!("{name} is {owner}'s since, its old snapshots go"));
            if let Err(e) = store.prune() {
                eprintln!("horadric: cannot drop the snapshots of {name}: {e}");
            }
            format!("{name} is {owner}'s now, moved on since its last snapshot here.")
        }
    }
}

/// Drops everything this PC keeps of brother `from`, which named
/// another keeper.
fn let_go(from: &str) {
    let Some(dir) = horadric_hooks::state_dir() else {
        return;
    };
    debug(&format!(
        "{from} named another keeper, what is kept of it goes"
    ));
    keeper::let_go(&keeping::root(&dir), from);
}

impl App {
    /// Whether session `id` waits at start for its keeper to say it was
    /// not taken over, rather than resuming by itself. A click still
    /// resumes it: that is the human's call.
    pub(super) fn keeping_holds(&mut self, id: &str) -> bool {
        with(|s| {
            let hold = s.book.fenced(id).is_some()
                || (s.book.keeper().is_some() && s.book.sent.contains_key(id));
            if hold {
                s.held.insert(id.to_string());
            }
            hold
        })
    }

    /// Session `id` went for good here. Where the roster has it as this
    /// PC's own, its brothers hear it ended, so its owner can go from every
    /// roster once all have heard. One handed on is another's by now, and
    /// a brother's tile owns nothing here: neither changes anything.
    pub(super) fn keeping_ended(&mut self, id: &str) {
        let Some(dir) = horadric_hooks::state_dir() else {
            return;
        };
        let (me, handed) = with(|s| (s.me.clone(), s.handed.remove(id)));
        if !handed && !me.is_empty() && mesh::end(&dir, id, &me, unix_now()) {
            debug(&format!("{id} ended for good"));
        }
    }

    /// The roster number session `id` is handed off under, so the
    /// receiver owns it one higher.
    pub(super) fn keeping_number(&self, id: &str) -> u64 {
        mesh::number(&owners(), id)
    }

    /// A handoff moved session `id` to brother `to` (its name, None for
    /// this PC) under number `n`. Recorded as a takeover is, so the copy
    /// left behind is fenced. Not told to the brothers here: the receiver
    /// would beat its own `took` to the sender and fence the session it
    /// is letting go. They hear it when their links next come up.
    pub(super) fn keeping_handed(&mut self, id: &str, to: Option<&str>, n: u64) {
        if to.is_some() {
            with(|s| s.handed.insert(id.to_string()));
        }
        let to = match to {
            Some(name) => match id_of(name) {
                Some(b) => b,
                None => return,
            },
            None => with(|s| s.me.clone()),
        };
        with(|s| {
            if to != s.me {
                s.book.forget(id);
                save(&s.book);
            }
        });
        owning(|o| mesh::handed(o, id, &to, n, unix_now()));
    }

    /// The tile line of a session taken over elsewhere, which never
    /// resumes here.
    pub(super) fn keeping_fenced(&self, id: &str) -> Option<String> {
        with(|s| s.book.fenced(id).map(str::to_string))
    }

    /// What a brother's tile offers: "Take over on <this PC>" while its
    /// brother is silent and this PC keeps the session and is its keeper.
    pub(super) fn keeping_offer(&self, id: &str) -> Option<String> {
        let (name, remote) = remote_of(self, id)?;
        let from = id_of(&name)?;
        let (_, holding) = held_of(&from, &remote)?;
        takeable(&owners(), &remote, &from, holding.record.n).ok()?;
        let (heard, _) = self.far_presence(&name)?;
        if !self.keeping_turn_for(&from, SILENT_SECS) {
            return None;
        }
        keeping::offer(heard, &pc_name(), unix_now())
    }

    /// What a brother's tile offers when another brother keeps it: "Take
    /// over on <keeper>" while its owner is silent and its first keeper is
    /// linked here. A Linux node keeper has no tiles to click, so this PC
    /// asks it with a `handoff` to itself. Gives the keeper and the label.
    pub(super) fn keeping_offer_elsewhere(&self, id: &str) -> Option<(String, String)> {
        let (owner, _) = remote_of(self, id)?;
        let (heard, _) = self.far_presence(&owner)?;
        let keeper = with(|s| {
            let first = s.book.keepers_of.get(&owner)?.first()?.clone();
            (first != s.me).then_some(first)
        })?;
        if !self.far_takers().contains(&keeper) {
            return None;
        }
        let name = crate::project::brother_name(&keeper);
        keeping::offer(heard, &name, unix_now()).map(|label| (keeper, label))
    }

    /// Asks the keeper of brother's tile `id` to take it over, as
    /// [`App::keeping_offer_elsewhere`] offered.
    pub(super) fn keeping_ask_take_over(&mut self, id: &str) {
        let (Some((keeper, _)), Some((_, remote))) =
            (self.keeping_offer_elsewhere(id), remote_of(self, id))
        else {
            return;
        };
        let msg = serde_json::json!({"t": "handoff", "id": remote, "to": keeper});
        self.far_tell(&keeper, msg);
        self.toasts.show(
            Kind::Done,
            "Taking over",
            &format!(
                "Asked {} to take it over.",
                crate::project::brother_name(&keeper)
            ),
        );
    }

    /// Whether this PC is the keeper of brother `from` that takes over:
    /// its first, or the next once those before have been out of reach
    /// `after` seconds. So an old keeper, or a second one, never races
    /// the first for one session.
    fn keeping_turn_for(&self, from: &str, after: u64) -> bool {
        let heard: HashMap<String, Heard> = joined()
            .into_iter()
            .filter_map(|(id, _)| Some((id.clone(), self.far_presence(&id)?.0)))
            .collect();
        let now = unix_now();
        let Some(last) = heard.get(from).map(|h| h.last) else {
            return false;
        };
        with(|s| {
            let me = s.me.clone();
            waited(last, s.started, now)
                && s.book
                    .keeps_for(from, &me, |k| heard.get(k).copied(), after, now)
        })
    }

    /// Takes brother's tile `id` over: lands what is kept on a thread,
    /// then adopts it here at the next tick.
    pub(super) fn keeping_take_over(&mut self, id: &str) {
        let Some((name, remote)) = remote_of(self, id) else {
            return;
        };
        let Some(from) = id_of(&name) else {
            return;
        };
        if !self.keeping_turn_for(&from, SILENT_SECS) {
            debug(&format!("not the keeper of {name} that takes {id}"));
            return;
        }
        if !with(|s| s.taking.insert(id.to_string())) {
            return;
        }
        let Some((store, holding)) = held_of(&from, &remote) else {
            with(|s| s.taking.remove(id));
            return;
        };
        // Refused before landing, so nothing lands that cannot be owned,
        // nor a session that moved on since this snapshot: landed and told,
        // it would fence the brother it runs on now.
        if let Err(why) = takeable(&owners(), &remote, &from, holding.record.n) {
            with(|s| s.taking.remove(id));
            let why = untakeable(&store, &holding.record.name, &why);
            self.toasts.show(Kind::Done, "Not taken over", &why);
            return;
        }
        let checkout = self.checkout_of(&holding.record.repo);
        let projects = horadric_hooks::transcript::projects();
        let notify = self.notify.0 as isize;
        let tile = id.to_string();
        std::thread::spawn(move || {
            let landed = (|| {
                let repo = checkout.ok_or("no checkout of this project here")?;
                let projects = projects.ok_or("Claude Code's folder was not found")?;
                let package = keeping::package(&store, &holding, &remote)?;
                horadric_core::handing::land(&package, &repo, &projects)
            })();
            debug(&format!(
                "landed {tile}: {:?}",
                landed.as_ref().map(|l| &l.cwd)
            ));
            back(
                Back::Landed {
                    store: store.0.clone(),
                    holding,
                    landed,
                },
                notify,
            );
        });
    }

    /// Called every tick and when a link or thread wakes the app.
    pub(super) fn keeping_tick(&mut self) {
        let turns = std::mem::take(&mut *TURNS.lock().unwrap_or_else(|e| e.into_inner()));
        for (id, transcript) in turns {
            self.keeping_turn(&id, &transcript);
        }
        let backs = std::mem::take(&mut *BACK.lock().unwrap_or_else(|e| e.into_inner()));
        for b in backs {
            match b {
                Back::Cut { id, keeper, cut } => self.keeping_cut(&id, keeper, cut),
                Back::Landed {
                    store,
                    holding,
                    landed,
                } => self.keeping_landed(Store(store), holding, landed),
            }
        }
        self.keeping_restore();
        self.keeping_note();
        self.keeping_tell();
        self.keeping_lines();
        self.keeping_prune();
        self.keeping_auto();
    }

    /// Once at start: the sessions this PC keeps, of brothers not heard
    /// from yet, as their tiles, so they can be taken over while their
    /// owner stays dead.
    fn keeping_restore(&mut self) {
        if with(|s| std::mem::replace(&mut s.restored, true)) {
            return;
        }
        let Some(dir) = horadric_hooks::state_dir() else {
            return;
        };
        let mut by: BTreeMap<String, Vec<(String, Holding, u64)>> = BTreeMap::new();
        for (from, session, store) in stores(&keeping::root(&dir)) {
            if store.full() {
                continue;
            }
            let Some(h) = Holding::load(&store) else {
                continue;
            };
            by.entry(from)
                .or_default()
                .push((session, h, store.shelf().1));
        }
        for (from, kept) in by {
            let Some(name) = name_of(&from) else {
                continue;
            };
            let heard = restored_heard(kept.iter().map(|(_, h, at)| (h, *at)));
            let list = kept
                .iter()
                .map(|(session, h, at)| kept_entry(session, h, *at))
                .collect();
            debug(&format!("showing {} kept sessions of {name}", kept.len()));
            self.far_restore(&from, &name, heard, list);
        }
    }

    /// Writes beside each kept session what this keeper last heard of it
    /// and its owner, now and then, for [`App::keeping_restore`].
    fn keeping_note(&mut self) {
        let now = unix_now();
        if with(|s| {
            let early = now.saturating_sub(s.noted) < 10;
            if !early {
                s.noted = now;
            }
            early
        }) {
            return;
        }
        let Some(dir) = horadric_hooks::state_dir() else {
            return;
        };
        let tiles: HashMap<String, (String, String, Phase)> = match self.shared.registry.lock() {
            Ok(r) => r
                .all()
                .filter_map(|s| {
                    let project = s.brother.as_ref()?.project.clone();
                    Some((s.id.clone(), (s.name.clone(), project, s.phase.clone())))
                })
                .collect(),
            Err(_) => return,
        };
        for (from, session, store) in stores(&keeping::root(&dir)) {
            let Some((heard, _)) = self.far_presence(&from) else {
                continue;
            };
            let Some((name, project, phase)) =
                tiles.get(&horadric_core::afar::local_id(&from, &session))
            else {
                continue;
            };
            let Some(mut h) = Holding::load(&store) else {
                continue;
            };
            let seen = Seen {
                heard: heard.last,
                said: heard.said,
                name: name.clone(),
                project: project.clone(),
                phase: phase.clone(),
            };
            if !seen_due(h.seen.as_ref(), &seen) {
                continue;
            }
            // A snapshot being filed writes the holding too; this waits
            // for the next round rather than block the UI thread.
            let Ok(_one) = RECEIVING.try_lock() else {
                continue;
            };
            h.seen = Some(seen);
            if let Err(e) = h.save(&store) {
                eprintln!("horadric: cannot note what was heard of {session}: {e}");
            }
        }
    }

    /// A turn ended: a snapshot for the first keeper, cut on a thread.
    fn keeping_turn(&mut self, id: &str, transcript: &str) {
        let Some((keeper, me)) = with(|s| {
            let k = s.book.keeper()?.to_string();
            let fits = !s.book.full(id, &k);
            (fits && s.book.fenced(id).is_none() && !s.me.is_empty()).then(|| (k, s.me.clone()))
        }) else {
            return;
        };
        let Some(name) = joined()
            .into_iter()
            .find(|(i, _)| *i == keeper)
            .map(|(_, n)| n)
        else {
            return;
        };
        if !self.far_presence(&keeper).is_some_and(|(_, ready)| ready) {
            debug(&format!("{name} is not reached, no snapshot of {id}"));
            return;
        }
        if with(|s| {
            let busy = s.cutting.contains(id);
            if busy {
                s.again.insert(id.to_string(), transcript.to_string());
            }
            busy
        }) {
            return;
        }
        let record = self.shared.registry.lock().ok().and_then(|r| {
            let s = r.get(id).filter(|s| !s.shell && s.brother.is_none())?;
            Some(Record {
                session: s.claude_session_id.clone()?,
                id: id.to_string(),
                name: s.name.clone(),
                project: s
                    .worktree
                    .as_ref()
                    .map_or_else(|| s.cwd.clone(), |w| w.main.clone()),
                cwd: s.cwd.clone(),
                agent: "claude".into(),
                from: pc_name(),
                ..Record::default()
            })
        });
        let Some(record) = record else {
            return;
        };
        let owner_n = owning(|o| mesh::claim(o, id, &me, unix_now()));
        let next: Next = with(|s| {
            s.cutting.insert(id.to_string());
            let next = s.book.next(id, &keeper, owner_n);
            save(&s.book);
            next
        });
        let notify = self.notify.0 as isize;
        let id = id.to_string();
        let transcript = PathBuf::from(transcript);
        std::thread::spawn(move || {
            let cut = keeper::cut(&id, record, &next, &transcript);
            back(
                Back::Cut {
                    id,
                    keeper: (keeper, name),
                    cut,
                },
                notify,
            );
        });
    }

    fn keeping_cut(&mut self, id: &str, keeper: (String, String), cut: Result<Cut, String>) {
        let again = with(|s| {
            s.cutting.remove(id);
            s.again.remove(id)
        });
        match cut {
            Err(e) => eprintln!("horadric: no snapshot of {id} for {}: {e}", keeper.1),
            Ok(c) => {
                let (auto_takeover, keepers) =
                    with(|s| (s.book.auto_takeover, s.book.keepers.clone()));
                let holding = Holding {
                    from: String::new(),
                    from_name: pc_name(),
                    record: c.record,
                    auto_takeover,
                    keepers,
                    seen: None,
                };
                let msgs = keep_messages(id, &holding, &c.payload);
                debug(&format!(
                    "snapshot {} of {id} to {}: {} bytes in {} parts",
                    c.meta.n,
                    keeper.1,
                    c.payload.len(),
                    msgs.len()
                ));
                for m in msgs {
                    self.far_tell(&keeper.0, m);
                }
                with(|s| {
                    s.book.sent(id, &keeper.0, &c.meta, c.len);
                    save(&s.book);
                });
            }
        }
        if let Some(t) = again {
            self.keeping_turn(id, &t);
        }
    }

    /// Hears what a brother answered about keeping: `kept`. False for
    /// anything else. Its roster is read on the way past, for who it
    /// says keeps it, and still goes on to whatever else reads it.
    pub(super) fn keeping_heard(&mut self, name: &str, msg: &Value) -> bool {
        if msg["t"] == "roster" {
            self.keeping_roster(name, msg);
            return false;
        }
        let Some((id, n, ok)) = kept_of(msg) else {
            return false;
        };
        debug(&format!("{name} kept snapshot {n} of {id}: {ok}"));
        if kept_full(msg) {
            if let Some(keeper) = id_of(name) {
                eprintln!("horadric: {id} outgrew what {name} keeps of a session, no more snapshots of it go there");
                with(|s| {
                    s.book.filled(&id, &keeper);
                    save(&s.book);
                });
            }
            return true;
        }
        if !ok {
            with(|s| {
                s.book.refused(&id);
                save(&s.book);
            });
        }
        true
    }

    /// Brother `name` served its roster. Only its own entry is taken, for
    /// who keeps it; when that is no longer this PC first, what this PC
    /// keeps of it goes.
    fn keeping_roster(&mut self, name: &str, msg: &Value) {
        let Some(from) = id_of(name) else {
            return;
        };
        let Ok(roster) = serde_json::from_value::<Roster>(msg.clone()) else {
            return;
        };
        let keepers = roster
            .brother(&from)
            .map(|b| b.keepers.clone())
            .unwrap_or_default();
        self.keeping_keepers(&from, keepers);
        // Its owners were merged into the roster held before this, so a
        // keeper's roster is its word on what was taken over.
        self.keeping_release(&from);
    }

    fn keeping_keepers(&mut self, from: &str, keepers: Vec<String>) {
        let changed = with(|s| s.book.keepers_of.get(from) != Some(&keepers));
        if !changed {
            return;
        }
        let drop = with(|s| {
            let me = s.me.clone();
            let drop = s.book.keepers_said(from, keepers, &me);
            save(&s.book);
            drop
        });
        if drop {
            let_go(from);
        }
    }

    /// Serves a brother's `keep` part or `takeover` from `who`, its id: a
    /// job filing a whole snapshot, or None.
    pub(super) fn keeping_serve(&mut self, who: &str, msg: &Value) -> Option<Job> {
        if msg["t"] == "takeover" {
            self.keeping_owners(who, &owners_of(msg));
            return None;
        }
        let whole = with(|s| s.joins.add(who, msg, unix_now()));
        let whole = match whole {
            Ok(w) => w?,
            Err(e) => {
                debug(&e);
                return None;
            }
        };
        if let Some(keepers) = whole.2.as_ref().map(|h| h.keepers.clone()) {
            if !keepers.is_empty() {
                self.keeping_keepers(who, keepers);
            }
        }
        let root = keeping::root(&horadric_hooks::state_dir()?);
        let from = who.to_string();
        // The name this PC knows the brother by, not the one it claims.
        let from_name = name_of(who).unwrap_or_else(|| who.to_string());
        let caps = with(|s| s.book.caps());
        let awake = self.keeping_awake();
        Some(Box::new(move || {
            keeper::file(&root, &from, &from_name, whole, caps, &awake, &RECEIVING)
                .unwrap_or(Value::Null)
        }))
    }

    /// The ids of the joined brothers awake right now. A keeper's cap
    /// spares the sessions of every other, which may yet be taken over.
    fn keeping_awake(&self) -> HashSet<String> {
        let now = unix_now();
        joined()
            .into_iter()
            .filter(|(id, _)| {
                self.far_presence(id)
                    .is_some_and(|(h, _)| h.presence(now) == Presence::Awake)
            })
            .map(|(id, _)| id)
            .collect()
    }

    /// Owners a brother `who` told with `takeover`: sessions of this PC
    /// taken over meanwhile stay paused and keep their work on a branch;
    /// once the keeper has spoken, the sessions held back at start run on.
    fn keeping_owners(&mut self, who: &str, told: &Owners) {
        let (me, mine) = with(|s| (s.me.clone(), s.book.keepers.clone()));
        let moved = match horadric_hooks::state_dir() {
            Some(d) => mesh::update(&d, |r| mesh::told_in(r, &me, &mine, who, told)),
            None => Vec::new(),
        };
        self.keeping_moved(moved);
        self.keeping_release(who);
    }

    /// Sessions of this PC another owns now, as the roster or a brother's
    /// `takeover` said: each is fenced, paused with its work kept on a
    /// branch. One being handed to that very brother is the handoff's own
    /// doing, which its `took` finishes.
    pub(super) fn keeping_moved(&mut self, moved: Vec<(String, Owner)>) {
        let moved: Vec<(String, Owner)> = moved
            .into_iter()
            .filter(|(id, o)| {
                let to = self
                    .handing_out_to(id)
                    .and_then(|to| id_of(&to).or(Some(to)));
                to.as_deref() != Some(o.brother.as_str())
            })
            .collect();
        if moved.is_empty() {
            return;
        }
        let offset = crate::questlog::utc_offset(unix_now());
        let fenced = with(|s| {
            let roster = names(&s.me);
            let fenced = s.book.fence(&moved, &roster, offset);
            save(&s.book);
            fenced
        });
        for (id, line) in fenced {
            debug(&format!("{id} was {line}"));
            self.keeping_orphan(&id);
        }
    }

    /// Brother `who` spoke of owners: if it is this PC's keeper, the
    /// sessions held back at start, and not fenced, run on.
    fn keeping_release(&mut self, who: &str) {
        let release: Vec<String> = with(|s| {
            if s.book.keeper() == Some(who) {
                s.held
                    .drain()
                    .filter(|id| s.book.fenced(id).is_none())
                    .collect()
            } else {
                Vec::new()
            }
        });
        for id in release {
            let paused = self
                .shared
                .registry
                .lock()
                .is_ok_and(|r| r.get(&id).is_some_and(|s| s.phase == Phase::Paused));
            if paused {
                if let Err(e) = self.resume(&id, false) {
                    eprintln!("horadric: cannot resume {id}: {e}");
                }
            }
        }
    }

    /// Stops a session taken over elsewhere and keeps its work on its
    /// orphan branch, so nothing done here since is lost.
    fn keeping_orphan(&mut self, id: &str) {
        if let Some(c) = self.consoles.get(id) {
            if c.exit_code().is_none() {
                c.kill();
            }
        }
        let cwd = self
            .shared
            .registry
            .lock()
            .ok()
            .and_then(|r| r.get(id).map(|s| s.cwd.clone()))
            .or_else(|| self.paused.get(id).map(|p| p.cwd.clone()));
        let Some(cwd) = cwd else {
            return;
        };
        let id = id.to_string();
        std::thread::spawn(move || match keeping::keep_orphan(Path::new(&cwd), &id) {
            Ok(c) => debug(&format!("{} keeps {id}'s work at {c}", orphan_branch(&id))),
            Err(e) => eprintln!("horadric: cannot keep {id}'s orphaned work: {e}"),
        });
    }

    /// Tells every brother whose link came up since who owns what.
    fn keeping_tell(&mut self) {
        let peers = self.far_peer_names();
        let due: Vec<String> = with(|s| {
            s.told
                .retain(|n| peers.iter().any(|(p, ready)| p == n && *ready));
            peers
                .iter()
                .filter(|(n, ready)| *ready && !s.told.contains(n))
                .map(|(n, _)| n.clone())
                .collect()
        });
        if due.is_empty() {
            return;
        }
        let mine = mesh::mine(&owners(), &with(|s| s.me.clone()));
        let msg = with(|s| {
            for n in &due {
                s.told.insert(n.clone());
            }
            takeover_message(&mine)
        });
        for n in due {
            debug(&format!("telling {n} the owners"));
            self.far_tell(&n, msg.clone());
        }
    }

    /// Keeps the line of every fenced session on its tile, whatever the
    /// exit of its console wrote there.
    fn keeping_lines(&mut self) {
        let fenced = with(|s| s.book.fenced.clone());
        if fenced.is_empty() {
            return;
        }
        let mut changed = false;
        if let Ok(mut r) = self.shared.registry.lock() {
            for (id, line) in &fenced {
                if let Some(s) = r.get_mut(id) {
                    if &s.last_line != line {
                        s.last_line = line.clone();
                        changed = true;
                    }
                }
            }
        }
        for (id, line) in &fenced {
            if let Some(p) = self.paused.get_mut(id) {
                p.last_line = line.clone();
            }
        }
        if changed {
            self.reconcile(false);
        }
    }

    /// Drops what this keeper holds of a session its brother no longer
    /// lists while linked, and the owner's word of sessions that ended.
    fn keeping_prune(&mut self) {
        let Some(dir) = horadric_hooks::state_dir() else {
            return;
        };
        let listed: HashSet<(String, String)> = match self.shared.registry.lock() {
            Ok(r) => r
                .all()
                .filter_map(|s| {
                    let key = s.brother.as_ref()?.id.clone();
                    let remote = horadric_core::afar::remote_id(&s.id, &key)?.to_string();
                    Some((key, remote))
                })
                .collect(),
            Err(_) => return,
        };
        let mine: HashSet<String> = self
            .shared
            .registry
            .lock()
            .map(|r| {
                r.all()
                    .filter(|s| s.brother.is_none())
                    .map(|s| s.id.clone())
                    .collect()
            })
            .unwrap_or_default();
        let now = unix_now();
        // Hourly: an age is days long, and it walks every store.
        let age_due = with(|s| {
            let due = now.saturating_sub(s.aged) >= 3600;
            if due {
                s.aged = now;
            }
            due
        });
        for (from, session, store) in stores(&keeping::root(&dir)) {
            let gone = name_of(&from).and_then(|name| {
                let listed = listed.contains(&(from.clone(), session.clone()));
                let ready = self.far_presence(&from).is_some_and(|(_, r)| r);
                with(|s| {
                    s.pruning
                        .verdict(&from, &name, &session, listed, ready, now)
                })
            });
            let gone = gone.or_else(|| {
                (age_due && too_old(store.shelf().1, now))
                    .then(|| format!("{session} of {from} was kept a month"))
            });
            if let Some(why) = gone {
                debug(&format!("{why}, its snapshots go"));
                if let Err(e) = store.prune() {
                    eprintln!("horadric: cannot drop the snapshots of {session}: {e}");
                }
            }
        }
        with(|s| {
            let ended: Vec<String> = s
                .book
                .sent
                .keys()
                .chain(s.book.fenced.keys())
                .filter(|id| !mine.contains(*id))
                .cloned()
                .collect();
            if !ended.is_empty() && !mine.is_empty() {
                for id in &ended {
                    s.book.forget(id);
                }
                save(&s.book);
            }
        });
    }

    /// Takes over by itself the sessions of a brother silent past its
    /// switch, if this PC keeps them and is the keeper whose turn it is.
    fn keeping_auto(&mut self) {
        let Some(dir) = horadric_hooks::state_dir() else {
            return;
        };
        let now = unix_now();
        let due: Vec<String> = stores(&keeping::root(&dir))
            .into_iter()
            .filter_map(|(from, session, store)| {
                let h = Holding::load(&store)?;
                name_of(&from)?;
                let (heard, _) = self.far_presence(&from)?;
                let after = h.auto_takeover?.max(SILENT_SECS);
                if !(auto_due(heard, h.auto_takeover, now) && self.keeping_turn_for(&from, after)) {
                    return None;
                }
                // Asked of the roster before a takeover is tried, so an old
                // snapshot of a session that moved on is dropped rather
                // than tried every tick.
                if let Err(why @ Untakeable::MovedOn(_)) =
                    takeable(&owners(), &session, &from, h.record.n)
                {
                    untakeable(&store, &h.record.name, &why);
                    return None;
                }
                Some(horadric_core::afar::local_id(&from, &session))
            })
            .collect();
        for id in due {
            if !with(|s| s.taking.contains(&id)) {
                debug(&format!("taking {id} over by itself"));
                self.keeping_take_over(&id);
            }
        }
    }

    fn keeping_landed(
        &mut self,
        store: Store,
        holding: Holding,
        landed: Result<horadric_core::handing::Landed, String>,
    ) {
        let record = holding.record.clone();
        let tile = horadric_core::afar::local_id(&holding.from, &record.id);
        let fail = |app: &mut App, why: &str| {
            eprintln!("horadric: cannot take {} over: {why}", record.name);
            app.toasts.show(Kind::Done, "Not taken over", why);
        };
        let landed = match landed {
            Ok(l) => l,
            Err(e) => {
                with(|s| s.taking.remove(&tile));
                fail(self, &e);
                return;
            }
        };
        // The roster may have heard of a handoff while it landed.
        if let Err(why) = takeable(&owners(), &record.id, &holding.from, record.n) {
            with(|s| s.taking.remove(&tile));
            let why = untakeable(&store, &record.name, &why);
            crate::worktree::discard(landed.worktree);
            fail(self, &why);
            return;
        }
        let since = self
            .far_presence(&holding.from)
            .map(|(h, _)| h.last)
            .unwrap_or_else(unix_now);
        let offset = crate::questlog::utc_offset(since);
        let line = takeover_line(
            &holding.from_name,
            &pc_name(),
            &landed.worktree.path,
            &clock(since, offset),
        );
        // The brother's tile goes before the session comes, so the two are
        // never both shown for one session.
        if let Ok(mut r) = self.shared.registry.lock() {
            r.remove(&tile);
        }
        let id = match self.adopt_landed(&record, landed, line) {
            Ok(id) => id,
            Err(e) => {
                with(|s| s.taking.remove(&tile));
                fail(self, &e);
                return;
            }
        };
        let me = with(|s| {
            s.taking.remove(&tile);
            s.me.clone()
        });
        let taken = owning(|o| {
            Some(keeping::take_over_from(
                o,
                &id,
                &holding.from,
                record.n,
                &me,
                unix_now(),
            ))
        });
        let owner = match taken {
            Some(Ok(owner)) => owner,
            // It moved on while the session started: it stops again, its
            // work kept on its orphan branch, as any session of this PC
            // another owns.
            Some(Err(Untakeable::MovedOn(owner))) => {
                eprintln!("horadric: {} moved on while it was taken over", record.name);
                self.keeping_moved(vec![(id, owner)]);
                return;
            }
            // Checked before landing; only a number that rose meanwhile
            // ends here, and then the old owner is not fenced.
            _ => {
                eprintln!("horadric: {id} has no number to take over with");
                return;
            }
        };
        let msg = takeover_message(&BTreeMap::from([(id.clone(), owner)]));
        for (name, ready) in self.far_peer_names() {
            if ready {
                self.far_tell(&name, msg.clone());
            }
        }
        if let Err(e) = store.prune() {
            eprintln!("horadric: cannot drop the snapshots of {id}: {e}");
        }
        self.toasts.show(
            Kind::Done,
            "Taken over",
            &format!(
                "{} runs here now, from {}'s last turn.",
                record.name, holding.from_name
            ),
        );
    }
}
