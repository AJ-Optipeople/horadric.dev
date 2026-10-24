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
//! Everything but cutting, landing and committing runs on the UI thread.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use horadric_core::brotherhood::{clock, orphan_branch, Heard, Presence, Roster, SILENT_SECS};
use horadric_core::handoff::{takeover_line, Joiner, Record};
use horadric_core::keepers::{decode, over_cap, too_old, unlisted_gone, Shelf, Store, FULL};
use horadric_core::keeping::{
    self, auto_due, full_message, keep_messages, keep_part, kept_full, kept_message, kept_of,
    owners_of, store_of, stores, takeover_message, Book, Holding, Next,
};
use horadric_core::serving::Job;
use horadric_core::Phase;
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

struct Cut {
    meta: horadric_core::keepers::Meta,
    payload: Vec<u8>,
    len: u64,
    record: Record,
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
    joins: HashMap<(String, String), (Joiner, Option<Holding>)>,
    /// Brothers told the owners since their link last came up.
    told: HashSet<String>,
    /// Sessions held back at start until the keeper is heard from.
    held: HashSet<String>,
    /// Brothers' sessions this keeper holds that were seen listed, as
    /// (brother name, session), so one gone from the list is pruned.
    seen: HashSet<(String, String)>,
    /// Kept sessions their linked brother does not list, since when, so
    /// one never seen listed goes too once that has lasted.
    unlisted: HashMap<(String, String), u64>,
    /// When the stores were last looked at for their age.
    aged: u64,
    /// Takeovers under way, by the brother's tile id.
    taking: HashSet<String>,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::default();
}

fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
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
    joined()
        .into_iter()
        .find(|(_, n)| n == name)
        .map(|(i, _)| i)
}

/// A joined brother's name by its id.
fn name_of(id: &str) -> Option<String> {
    joined().into_iter().find(|(i, _)| i == id).map(|(_, n)| n)
}

fn names(me: &str) -> Roster {
    let store = horadric_hooks::state_dir()
        .and_then(|d| joining::Store::load(&d))
        .unwrap_or_default();
    crate::portal::joined_roster(me, &pc_name(), &store)
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
    let name = r.get(id)?.brother.as_ref()?.name.clone();
    let remote = horadric_core::afar::remote_id(id, &name)?.to_string();
    Some((name, remote))
}

/// The keeper's store and holding of brother `from`'s session `remote`,
/// found by the folder its link filed it under, never by the name the
/// holding claims. The holding comes back with that id and the name
/// this PC knows the brother by.
fn held_of(from: &str, remote: &str) -> Option<(Store, Holding)> {
    let store = store_of(&keeping::root(&horadric_hooks::state_dir()?), from, remote)?;
    let h = Holding::load(&store)?;
    let from_name = name_of(from)?;
    Some((
        store,
        Holding {
            from: from.to_string(),
            from_name,
            ..h
        },
    ))
}

/// Drops everything this PC keeps of brother `from`, which named
/// another keeper.
fn let_go(from: &str) {
    let Some(dir) = horadric_hooks::state_dir() else {
        return;
    };
    for (_, session, store) in stores(&keeping::root(&dir))
        .into_iter()
        .filter(|(f, _, _)| f == from)
    {
        debug(&format!("{from} named another keeper, {session} goes"));
        if let Err(e) = store.prune() {
            eprintln!("horadric: cannot drop the snapshots of {session}: {e}");
        }
    }
}

/// Drops the oldest stores until what is kept under `root` fits in
/// `cap` bytes, sparing those of brothers not in `awake`.
fn fit(root: &Path, cap: u64, awake: &HashSet<String>) {
    let all = stores(root);
    let shelves: Vec<Shelf> = all
        .iter()
        .map(|(from, session, store)| {
            let (bytes, at) = store.shelf();
            Shelf {
                from: from.clone(),
                session: session.clone(),
                bytes,
                at,
            }
        })
        .collect();
    for s in over_cap(&shelves, cap, |s| !awake.contains(&s.from)) {
        debug(&format!(
            "past the cap, {} of {} goes ({} bytes)",
            s.session, s.from, s.bytes
        ));
        if let Some((_, _, store)) = all.iter().find(|(f, n, _)| *f == s.from && *n == s.session) {
            if let Err(e) = store.prune() {
                eprintln!("horadric: cannot drop the snapshots of {}: {e}", s.session);
            }
        }
    }
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

    /// The roster number session `id` is handed off under, so the
    /// receiver owns it one higher.
    pub(super) fn keeping_number(&self, id: &str) -> u64 {
        with(|s| s.book.number(id))
    }

    /// A handoff moved session `id` to brother `to` (its name, None for
    /// this PC) under number `n`. Recorded as a takeover is, so the copy
    /// left behind is fenced. Not told to the brothers here: the receiver
    /// would beat its own `took` to the sender and fence the session it
    /// is letting go. They hear it when their links next come up.
    pub(super) fn keeping_handed(&mut self, id: &str, to: Option<&str>, n: u64) {
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
            }
            if s.book.handed(id, &to, n, unix_now()) {
                save(&s.book);
            }
        });
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
        held_of(&from, &remote)?;
        let (heard, _) = self.far_presence(&name)?;
        if !self.keeping_turn_for(&from, SILENT_SECS) {
            return None;
        }
        keeping::offer(heard, &pc_name(), unix_now())
    }

    /// Whether this PC is the keeper of brother `from` that takes over:
    /// its first, or the next once those before have been out of reach
    /// `after` seconds. So an old keeper, or a second one, never races
    /// the first for one session.
    fn keeping_turn_for(&self, from: &str, after: u64) -> bool {
        let heard: HashMap<String, Heard> = joined()
            .into_iter()
            .filter_map(|(id, name)| Some((id, self.far_presence(&name)?.0)))
            .collect();
        let now = unix_now();
        with(|s| {
            let me = s.me.clone();
            s.book
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
        // Refused before landing, so nothing lands that cannot be owned.
        if with(|s| s.book.owner_after(&remote, holding.record.n)).is_none() {
            with(|s| s.taking.remove(id));
            self.toasts.show(
                Kind::Done,
                "Not taken over",
                &format!(
                    "{name} gave {} a number that cannot go up.",
                    holding.record.name
                ),
            );
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
        self.keeping_tell();
        self.keeping_lines();
        self.keeping_prune();
        self.keeping_auto();
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
        if !self.far_presence(&name).is_some_and(|(_, ready)| ready) {
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
        let Some(mut record) = record else {
            return;
        };
        let next: Next = with(|s| {
            s.cutting.insert(id.to_string());
            let next = s.book.next(id, &keeper, &me, unix_now());
            save(&s.book);
            next
        });
        record.n = next.owner_n;
        let notify = self.notify.0 as isize;
        let id = id.to_string();
        let transcript = PathBuf::from(transcript);
        std::thread::spawn(move || {
            let cut = (|| {
                let cwd = PathBuf::from(&record.cwd);
                let top = keeping::describe(&mut record, &cwd, &transcript)?;
                let (meta, payload, len) = keeping::snapshot(&top, &id, &next, &transcript)?;
                Ok(Cut {
                    meta,
                    payload,
                    len,
                    record,
                })
            })();
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
                    self.far_tell(&keeper.1, m);
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
        let (part, holding) = keep_part(msg)?;
        let key = (who.to_string(), part.id.clone());
        let id = part.id.clone();
        let whole = with(|s| {
            let (joiner, held) = s
                .joins
                .entry(key.clone())
                .or_insert_with(|| (Joiner::new(&id), None));
            if part.part == 0 {
                *joiner = Joiner::new(&id);
                *held = holding;
            }
            let whole = joiner.add(&part);
            match whole {
                Ok(Some(bytes)) => s.joins.remove(&key).map(|(_, h)| (bytes, h)),
                Ok(None) => None,
                Err(e) => {
                    s.joins.remove(&key);
                    debug(&format!(
                        "a keep part of {id} from {who} was refused: {e:?}"
                    ));
                    None
                }
            }
        });
        let (payload, holding) = whole?;
        if let Some(keepers) = holding.as_ref().map(|h| h.keepers.clone()) {
            if !keepers.is_empty() {
                self.keeping_keepers(who, keepers);
            }
        }
        let root = keeping::root(&horadric_hooks::state_dir()?);
        let store = store_of(&root, who, &id)?;
        let from = who.to_string();
        // The name this PC knows the brother by, not the one it claims.
        let from_name = name_of(who).unwrap_or_else(|| who.to_string());
        let caps = with(|s| s.book.caps());
        let awake = self.keeping_awake();
        Some(Box::new(move || {
            let n = decode(&payload).map_or(0, |s| s.meta.n);
            let _one = RECEIVING.lock();
            let taken = store.receive(&payload).and_then(|_| match holding {
                Some(h) => Holding {
                    from,
                    from_name,
                    ..h
                }
                .save(&store),
                None => Ok(()),
            });
            let full = match &taken {
                Err(e) if e == FULL => true,
                Err(e) => {
                    eprintln!("horadric: snapshot {n} of {id} not kept: {e}");
                    false
                }
                Ok(()) if store.shelf().0 > caps.1 => {
                    eprintln!("horadric: {id} outgrew the cap of one session, its snapshots go");
                    if let Err(e) = store.mark_full() {
                        eprintln!("horadric: cannot drop the snapshots of {id}: {e}");
                    }
                    true
                }
                Ok(()) => false,
            };
            fit(&root, caps.0, &awake);
            if full {
                full_message(&id, n)
            } else {
                kept_message(&id, n, taken.is_ok())
            }
        }))
    }

    /// The ids of the joined brothers awake right now. A keeper's cap
    /// spares the sessions of every other, which may yet be taken over.
    fn keeping_awake(&self) -> HashSet<String> {
        let now = unix_now();
        joined()
            .into_iter()
            .filter(|(_, name)| {
                self.far_presence(name)
                    .is_some_and(|(h, _)| h.presence(now) == Presence::Awake)
            })
            .map(|(id, _)| id)
            .collect()
    }

    /// Owners a brother `who` told: sessions of this PC taken over
    /// meanwhile stay paused and keep their work on a branch; once the
    /// keeper has spoken, the sessions held back at start run on.
    fn keeping_owners(
        &mut self,
        who: &str,
        owners: &BTreeMap<String, horadric_core::brotherhood::Owner>,
    ) {
        let offset = crate::questlog::utc_offset(unix_now());
        let (fenced, release) = with(|s| {
            let roster = names(&s.me);
            let me = s.me.clone();
            let fenced = s.book.hear(&me, who, owners, &roster, offset);
            save(&s.book);
            let release: Vec<String> = if s.book.keeper() == Some(who) {
                s.held
                    .drain()
                    .filter(|id| s.book.fenced(id).is_none())
                    .collect()
            } else {
                Vec::new()
            };
            (fenced, release)
        });
        for (id, line) in fenced {
            debug(&format!("{id} was {line}"));
            self.keeping_orphan(&id);
        }
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
        let msg = with(|s| {
            for n in &due {
                s.told.insert(n.clone());
            }
            takeover_message(&s.book.mine(&s.me))
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
                    let name = s.brother.as_ref()?.name.clone();
                    let remote = horadric_core::afar::remote_id(&s.id, &name)?.to_string();
                    Some((name, remote))
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
            let gone = match name_of(&from) {
                Some(name) => {
                    let key = (name.clone(), session.clone());
                    let ready = self.far_presence(&name).is_some_and(|(_, r)| r);
                    with(|s| {
                        if listed.contains(&key) {
                            s.seen.insert(key.clone());
                            s.unlisted.remove(&key);
                            None
                        } else if !ready {
                            s.unlisted.remove(&key);
                            None
                        } else if s.seen.remove(&key) {
                            s.unlisted.remove(&key);
                            Some(format!("{name} no longer lists {session}"))
                        } else {
                            let since = *s.unlisted.entry(key.clone()).or_insert(now);
                            unlisted_gone(since, now).then(|| {
                                s.unlisted.remove(&key);
                                format!("{name} has not listed {session} for ten minutes")
                            })
                        }
                    })
                }
                None => None,
            };
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
                let name = name_of(&from)?;
                let (heard, _) = self.far_presence(&name)?;
                let after = h.auto_takeover?.max(SILENT_SECS);
                (auto_due(heard, h.auto_takeover, now) && self.keeping_turn_for(&from, after))
                    .then(|| horadric_core::afar::local_id(&name, &session))
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
        let tile = horadric_core::afar::local_id(&holding.from_name, &record.id);
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
        let since = self
            .far_presence(&holding.from_name)
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
        let owner = with(|s| {
            s.taking.remove(&tile);
            let me = s.me.clone();
            let o = s.book.take_over(&id, record.n, &me, unix_now());
            save(&s.book);
            o
        });
        // Checked before landing; only a number that rose meanwhile ends
        // here, and then the old owner is not fenced.
        let Some(owner) = owner else {
            eprintln!("horadric: {id} has no number to take over with");
            return;
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
