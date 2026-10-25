//! A seeded, deterministic simulation of a brotherhood under attack,
//! checking the invariants of docs/plan/brotherhood-ownership.md after
//! every step. Brothers start, hand, take over and end sessions, change
//! their keepers, sleep, restart and cast each other out; one of them may
//! be stolen; the relay drops, delays, reorders, duplicates and replays
//! what they send. Each brother is the real core ([`crate::mesh`] and
//! [`crate::said`]) behind [`Core`], so the simulation tests the code
//! that runs, not a model of it.

use std::collections::{BTreeMap, BTreeSet};

use crate::brotherhood::Roster;
use crate::said::Word;

/// A deterministic generator: splitmix64.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed ^ 0x9E37_79B9_7F4A_7C15)
    }

    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    pub fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next() % n as u64) as usize
        }
    }

    pub fn chance(&mut self, percent: u64) -> bool {
        self.next() % 100 < percent
    }

    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> Option<&'a T> {
        if items.is_empty() {
            None
        } else {
            Some(&items[self.below(items.len())])
        }
    }
}

/// One brother as the core sees it: what it can be asked to do, and what
/// it holds. Session keys are `<origin>/<id>`.
pub trait Core: Sized {
    /// Brother `me` among `all`, kept by `keepers`.
    fn node(me: &'static str, all: &[&'static str], keepers: Vec<String>, tag: &str) -> Self;
    /// Starts session `id`, as its creator.
    fn start(&mut self, id: &str, now: u64);
    /// Hands its session `key` to `to`: the number handed under and the
    /// words the bundle carries, None when it is not its own.
    fn hand_out(&mut self, key: &str, to: &str, now: u64) -> Option<(u64, Vec<Word>)>;
    /// A bundle of session `key` handed here under `n` lands, if allowed.
    fn arrive(&mut self, key: &str, n: u64, words: &[Word], now: u64) -> Result<(), String>;
    /// The receiver said it took the handoff.
    fn took(&mut self, key: &str, to: &str, n: u64, now: u64) -> bool;
    /// The handoff failed.
    fn fail(&mut self, key: &str, to: &str, n: u64, now: u64);
    /// The process restarts: what it held only in memory is gone.
    fn restart(&mut self, now: u64);
    /// Takes `key` over from `from`, as a keeper with what it holds.
    fn take_over(&mut self, key: &str, from: &str, now: u64) -> bool;
    /// Ends its own session `key`.
    fn end(&mut self, key: &str, now: u64) -> bool;
    /// Its human names other keepers.
    fn set_keepers(&mut self, keepers: Vec<String>, now: u64);
    /// Its human casts out `id`.
    fn cast(&mut self, id: &str, now: u64);
    /// The roster it serves.
    fn serve(&mut self, now: u64) -> Roster;
    /// Takes in a roster `from` served.
    fn hear(&mut self, from: &str, r: &Roster, now: u64);
    /// The owner of `key` and its number as held.
    fn owner_of(&self, key: &str) -> Option<(String, u64)>;
    fn ended(&self, key: &str) -> bool;
    /// Every session key held.
    fn sessions(&self) -> Vec<String>;
    /// The keys out or contested, as held.
    fn out_keys(&self) -> BTreeSet<String>;
    /// Every word held.
    fn words(&self) -> Vec<Word>;
    /// The sessions it let go after they were over.
    fn let_go(&self) -> Vec<String>;
    /// The signatures of the handoffs taken back that it holds word of.
    fn recalled(&self) -> BTreeSet<String>;
    /// How many words it holds on the sessions each other brother
    /// started, and how many cast outs each signer signed.
    fn counts(&self) -> (BTreeMap<String, usize>, BTreeMap<String, usize>);
    fn prune(&mut self, now: u64);
    /// Whether it lets brother `id` act on it.
    fn controls(&self, id: &str) -> bool;

    // What a thief holding this brother's key can sign.

    /// A takeover of `key` from `from` at `n`, as if a keeper.
    fn forge_takes(&self, key: &str, from: &str, n: u64, now: u64) -> Option<Word>;
    /// `count` sessions made up and ended at once, from number `first`,
    /// with a far future clock.
    fn forge_flood(&self, first: usize, count: usize) -> Vec<Word>;
    /// A cast out of `id`.
    fn forge_cast(&self, id: &str, now: u64) -> Option<Word>;
}

/// The test brothers' key pairs and keys, the same every run.
pub mod keys {
    use crate::joining::public_of;
    use crate::portal::{b64url, sha256};

    pub fn pair(id: &str) -> [u8; 96] {
        let mut p = [0u8; 96];
        for (i, chunk) in p.chunks_mut(32).enumerate() {
            chunk.copy_from_slice(&sha256(format!("{id}{i}").as_bytes()));
        }
        p
    }

    pub fn key(id: &str) -> String {
        b64url(&public_of(&pair(id)))
    }
}

/// What the simulation is told went wrong, with the step and seed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Broken {
    pub seed: u64,
    pub step: usize,
    pub invariant: &'static str,
    pub what: String,
    /// What happened just before, for finding out why.
    pub trail: Vec<String>,
}

/// A frame in the relay: a roster from one brother to another.
#[derive(Clone)]
struct Frame {
    from: usize,
    to: usize,
    roster: Roster,
    due: usize,
}

/// A handoff on its way.
#[derive(Clone)]
struct Flight {
    from: usize,
    to: usize,
    key: String,
    n: u64,
    words: Vec<Word>,
    due: usize,
    /// Lost on the way: the sender is told it failed.
    lost: bool,
    /// Lands after the sender was told it failed.
    late: bool,
}

/// Who each brother is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Honest,
    /// Stolen at some step, then a thief.
    Stolen,
}

/// How hard the relay and the thief push.
#[derive(Debug, Clone)]
pub struct Plan {
    pub brothers: usize,
    pub steps: usize,
    pub thief: bool,
    pub relay_drop: u64,
    pub relay_dup: u64,
    pub relay_delay: usize,
    pub flood: usize,
}

impl Plan {
    /// A plan for seed's generator `rng`. `floods` is how often in a
    /// hundred the thief floods: floods are what a run spends its time on.
    pub fn of(rng: &mut Rng, floods: u64) -> Plan {
        Plan {
            brothers: 3 + rng.below(3),
            steps: 60 + rng.below(60),
            thief: rng.chance(70),
            relay_drop: rng.below(30) as u64,
            relay_dup: rng.below(20) as u64,
            relay_delay: rng.below(6),
            flood: if rng.chance(floods) {
                100 + rng.below(300)
            } else {
                0
            },
        }
    }
}

pub const IDS: [&str; 6] = ["ash", "birch", "cedar", "dogwood", "elm", "fir"];

/// The bound one origin's words may reach on an honest brother, and the
/// bound on cast outs, from the spec.
pub const WORDS_PER_ORIGIN: usize = 16_384;
pub const CASTS_PER_SIGNER: usize = 32;

/// The whole simulated brotherhood.
pub struct Sim<C: Core> {
    seed: u64,
    rng: Rng,
    plan: Plan,
    ids: Vec<&'static str>,
    nodes: Vec<C>,
    roles: Vec<Role>,
    /// Each brother's keepers as its human set them.
    keepers: Vec<Vec<String>>,
    asleep: BTreeSet<usize>,
    relay: Vec<Frame>,
    /// Frames seen, for replays.
    sent: Vec<Frame>,
    flights: Vec<Flight>,
    /// Which brothers hold a copy of which session that can run.
    copies: BTreeMap<String, BTreeSet<usize>>,
    /// Sessions started, in order.
    started: Vec<String>,
    /// Signatures of what the thief signed without the right to.
    wrong: BTreeSet<String>,
    stolen: Option<usize>,
    /// The thief was cast out, by whom.
    cast_by: Option<usize>,
    /// Handoffs refused in a row, by (sender, session, receiver).
    refused: BTreeMap<(usize, String, usize), usize>,
    /// Handoffs that failed, which their sender tries again to the same
    /// brother, as a human would.
    retries: Vec<(usize, String, usize)>,
    now: u64,
    step: usize,
    flood_next: usize,
    /// What happened, the last few hundred events.
    log: Vec<String>,
    pub broken: Vec<Broken>,
}

impl<C: Core> Sim<C> {
    pub fn new(seed: u64, floods: u64) -> Sim<C> {
        let mut rng = Rng::new(seed);
        let plan = Plan::of(&mut rng, floods);
        Sim::with(seed, plan)
    }

    pub fn with(seed: u64, plan: Plan) -> Sim<C> {
        let mut rng = Rng::new(seed.wrapping_mul(31).wrapping_add(7));
        let ids: Vec<&'static str> = IDS[..plan.brothers].to_vec();
        let mut keepers = Vec::new();
        let mut nodes = Vec::new();
        for (i, id) in ids.iter().enumerate() {
            let others: Vec<String> = ids
                .iter()
                .enumerate()
                .filter(|(j, _)| *j != i)
                .map(|(_, o)| o.to_string())
                .collect();
            let k = vec![others[rng.below(others.len())].clone()];
            nodes.push(C::node(id, &ids, k.clone(), &format!("{seed}-{i}")));
            keepers.push(k);
        }
        let mut roles = vec![Role::Honest; plan.brothers];
        if plan.thief {
            let t = rng.below(plan.brothers);
            roles[t] = Role::Stolen;
        }
        Sim {
            seed,
            rng,
            plan,
            ids,
            nodes,
            roles,
            keepers,
            asleep: BTreeSet::new(),
            relay: Vec::new(),
            sent: Vec::new(),
            flights: Vec::new(),
            copies: BTreeMap::new(),
            started: Vec::new(),
            wrong: BTreeSet::new(),
            stolen: None,
            cast_by: None,
            refused: BTreeMap::new(),
            retries: Vec::new(),
            now: 1_800_000_000,
            step: 0,
            flood_next: 0,
            log: Vec::new(),
            broken: Vec::new(),
        }
    }

    fn honest(&self, i: usize) -> bool {
        self.stolen != Some(i)
    }

    fn honest_awake(&self) -> Vec<usize> {
        (0..self.nodes.len())
            .filter(|i| self.honest(*i) && !self.asleep.contains(i))
            .collect()
    }

    fn index(&self, id: &str) -> Option<usize> {
        self.ids.iter().position(|i| *i == id)
    }

    fn fail(&mut self, invariant: &'static str, what: String) {
        if self.broken.iter().any(|b| b.invariant == invariant) {
            return;
        }
        let from = self.log.len().saturating_sub(60);
        self.broken.push(Broken {
            seed: self.seed,
            step: self.step,
            invariant,
            what,
            trail: self.log[from..].to_vec(),
        });
    }

    fn note(&mut self, what: String) {
        if self.log.len() > 600 {
            self.log.drain(..300);
        }
        self.log.push(format!("{}: {what}", self.step));
    }

    /// Runs the plan, checking after every step and at every flush.
    pub fn run(&mut self) {
        for _ in 0..self.plan.steps {
            self.step += 1;
            self.now += 60;
            self.act();
            self.deliver();
            self.check_bounds();
            if self.step.is_multiple_of(15) {
                self.flush();
                self.check_all();
            }
        }
        self.finish();
    }

    /// One random action.
    fn act(&mut self) {
        let roll = self.rng.below(100);
        let awake = self.honest_awake();
        let Some(&me) = self.rng.pick(&awake) else {
            return;
        };
        match roll {
            0..=14 => self.start(me),
            15..=34 => self.hand(me),
            35..=42 => self.takeover(me),
            43..=50 => self.end(me),
            51..=55 => self.rekeep(me),
            56..=60 => self.sleep_or_wake(),
            61..=63 => self.restart(me),
            64..=82 => self.gossip(me),
            83..=86 => self.steal(),
            87..=89 => self.cast_thief(me),
            _ => self.thief_acts(),
        }
    }

    fn start(&mut self, me: usize) {
        let id = format!("s{}", self.started.len());
        self.nodes[me].start(&id, self.now);
        let key = format!("{}/{id}", self.ids[me]);
        self.note(format!("{} starts {key}", self.ids[me]));
        self.copies.entry(key.clone()).or_default().insert(me);
        self.started.push(key);
    }

    /// The sessions `me` holds as its own, live, with a copy to run.
    fn mine(&self, me: usize) -> Vec<String> {
        let id = self.ids[me];
        self.nodes[me]
            .sessions()
            .into_iter()
            .filter(|k| {
                self.nodes[me].owner_of(k).is_some_and(|(o, _)| o == id)
                    && !self.nodes[me].ended(k)
                    && self.copies.get(k).is_some_and(|c| c.contains(&me))
                    && !self.flights.iter().any(|f| f.key == *k)
            })
            .collect()
    }

    fn hand(&mut self, me: usize) {
        let mine = self.mine(me);
        let again = self
            .retries
            .iter()
            .position(|(f, k, _)| *f == me && mine.contains(k));
        let (key, to) = match again {
            Some(i) if self.rng.chance(80) => {
                let (_, k, to) = self.retries.remove(i);
                (k, to)
            }
            _ => {
                let Some(key) = self.rng.pick(&mine).cloned() else {
                    return;
                };
                let others: Vec<usize> = (0..self.nodes.len())
                    .filter(|i| *i != me && self.honest(*i))
                    .collect();
                let Some(&to) = self.rng.pick(&others) else {
                    return;
                };
                (key, to)
            }
        };
        let Some((n, words)) = self.nodes[me].hand_out(&key, self.ids[to], self.now) else {
            return;
        };
        let lost = self.rng.chance(self.plan.relay_drop.max(10));
        let late = lost && self.rng.chance(40);
        let what = format!(
            "{} hands {key} to {} under {n}{}",
            self.ids[me],
            self.ids[to],
            if lost {
                if late {
                    ", lost, lands late"
                } else {
                    ", lost"
                }
            } else {
                ""
            }
        );
        self.note(what);
        if std::env::var_os("HORADRIC_SIM_DEBUG").is_some() {
            let held = self.words_on(me, &key);
            self.note(format!("  {} holds {held}", self.ids[me]));
            let h2 = self.words_on(to, &key);
            self.note(format!("  {} holds {h2}", self.ids[to]));
        }
        let due = self.step + 1 + self.rng.below(self.plan.relay_delay + 1);
        self.flights.push(Flight {
            from: me,
            to,
            key,
            n,
            words,
            due,
            lost,
            late,
        });
    }

    /// Handoffs due: landed, refused, lost, or landing late.
    fn land(&mut self, all: bool) {
        let mut keep = Vec::new();
        for f in std::mem::take(&mut self.flights) {
            if !all && f.due > self.step {
                keep.push(f);
                continue;
            }
            let (from, to) = (f.from, f.to);
            if f.lost {
                self.note(format!(
                    "{} is told its handoff of {} failed",
                    self.ids[from], f.key
                ));
                self.nodes[from].fail(&f.key, self.ids[to], f.n, self.now);
                self.retries.push((from, f.key.clone(), to));
                if f.late {
                    // The bundle still lands, after the sender gave up.
                    let got = self.nodes[to].arrive(&f.key, f.n, &f.words, self.now);
                    self.note(format!("{} lands {} late: {got:?}", self.ids[to], f.key));
                    if got.is_ok() {
                        self.copies.entry(f.key.clone()).or_default().insert(to);
                    }
                }
                continue;
            }
            if self.asleep.contains(&to) || !self.honest(to) {
                self.note(format!(
                    "{} asleep, {} told it failed",
                    self.ids[to], self.ids[from]
                ));
                self.nodes[from].fail(&f.key, self.ids[to], f.n, self.now);
                continue;
            }
            let got = self.nodes[to].arrive(&f.key, f.n, &f.words, self.now);
            if got.is_ok() {
                self.copies.entry(f.key.clone()).or_default().insert(to);
                let took = self.nodes[from].took(&f.key, self.ids[to], f.n, self.now);
                self.note(format!(
                    "{} runs {}, {} took it: {took}",
                    self.ids[to], f.key, self.ids[from]
                ));
                if took {
                    if let Some(c) = self.copies.get_mut(&f.key) {
                        c.remove(&from);
                    }
                }
                self.refused.remove(&(from, f.key.clone(), to));
            } else {
                self.note(format!("{} refuses {}: {got:?}", self.ids[to], f.key));
                self.nodes[from].fail(&f.key, self.ids[to], f.n, self.now);
                self.retries.push((from, f.key.clone(), to));
                // Only a refusal of a handoff its sender had the right to
                // make, to a brother in good standing, counts against I6.
                if !self.rightful(&f) {
                    continue;
                }
                let tries = self.refused.entry((from, f.key.clone(), to)).or_default();
                *tries += 1;
                if *tries >= 3 {
                    let what = format!(
                        "{} could not hand {} to {} three times in a row",
                        self.ids[from], f.key, self.ids[to]
                    );
                    self.fail("I6", what);
                }
            }
        }
        self.flights = keep;
    }

    /// Whether handoff `f` is one every honest brother's words together,
    /// with the bundle's, say its sender had the right to make: the
    /// session its own under the number before, sender and receiver in.
    fn rightful(&self, f: &Flight) -> bool {
        let mut seer = C::node(
            "zed",
            &self.ids,
            Vec::new(),
            &format!("{}-right", self.seed),
        );
        for i in 0..self.nodes.len() {
            if self.honest(i) {
                let r = Roster {
                    said: self.nodes[i].words(),
                    ..Roster::default()
                };
                seer.hear(self.ids[i], &r, self.now);
            }
        }
        let r = Roster {
            said: f.words.clone(),
            ..Roster::default()
        };
        seer.hear(self.ids[f.from], &r, self.now);
        let out = seer.out_keys();
        let owns = seer.owner_of(&f.key) == Some((self.ids[f.from].to_string(), f.n - 1));
        owns && !out.contains(&keys::key(self.ids[f.from]))
            && !out.contains(&keys::key(self.ids[f.to]))
    }

    fn takeover(&mut self, me: usize) {
        let id = self.ids[me];
        let candidates: Vec<(String, usize)> = self.nodes[me]
            .sessions()
            .into_iter()
            .filter_map(|k| {
                let (o, _) = self.nodes[me].owner_of(&k)?;
                let oi = self.index(&o)?;
                let asleep = self.asleep.contains(&oi) || !self.honest(oi);
                let keeps = self.keepers[oi].iter().any(|x| x == id);
                (oi != me && asleep && keeps && !self.nodes[me].ended(&k)).then_some((k, oi))
            })
            .collect();
        let Some((key, from)) = self.rng.pick(&candidates).cloned() else {
            return;
        };
        if self.flights.iter().any(|f| f.key == key) {
            return;
        }
        let ok = self.nodes[me].take_over(&key, self.ids[from], self.now);
        self.note(format!(
            "{} takes {key} over from {}: {ok}",
            self.ids[me], self.ids[from]
        ));
        if ok {
            self.copies.entry(key).or_default().insert(me);
        }
    }

    fn end(&mut self, me: usize) {
        let mine = self.mine(me);
        if let Some(key) = self.rng.pick(&mine).cloned() {
            self.note(format!("{} ends {key}", self.ids[me]));
            self.nodes[me].end(&key, self.now);
        }
    }

    fn rekeep(&mut self, me: usize) {
        let others: Vec<String> = self
            .ids
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != me)
            .map(|(_, o)| o.to_string())
            .collect();
        let mut k = Vec::new();
        for o in others {
            if self.rng.chance(40) {
                k.push(o);
            }
        }
        self.keepers[me] = k.clone();
        self.note(format!("{} names keepers {k:?}", self.ids[me]));
        self.nodes[me].set_keepers(k, self.now);
    }

    fn sleep_or_wake(&mut self) {
        let i = self.rng.below(self.nodes.len());
        if self.asleep.remove(&i) {
            self.note(format!("{} wakes", self.ids[i]));
        } else if self.asleep.len() + 1 < self.nodes.len() {
            self.note(format!("{} sleeps", self.ids[i]));
            self.asleep.insert(i);
        }
    }

    fn restart(&mut self, me: usize) {
        self.note(format!("{} restarts", self.ids[me]));
        self.nodes[me].restart(self.now);
    }

    /// `me` serves its roster to another brother through the relay.
    fn gossip(&mut self, me: usize) {
        let to = self.rng.below(self.nodes.len());
        if to == me {
            return;
        }
        let roster = self.nodes[me].serve(self.now);
        self.post(me, to, roster);
    }

    fn post(&mut self, from: usize, to: usize, roster: Roster) {
        if self.rng.chance(self.plan.relay_drop) {
            return;
        }
        let due = self.step + self.rng.below(self.plan.relay_delay + 1);
        let f = Frame {
            from,
            to,
            roster,
            due,
        };
        if self.rng.chance(self.plan.relay_dup) {
            self.relay.push(f.clone());
        }
        if self.sent.len() < 200 {
            self.sent.push(f.clone());
        }
        self.relay.push(f);
    }

    /// Frames due arrive, in an order the relay picks; it may replay an
    /// old one too.
    fn deliver(&mut self) {
        if self.rng.chance(self.plan.relay_dup) && !self.sent.is_empty() {
            let i = self.rng.below(self.sent.len());
            let mut old = self.sent[i].clone();
            old.due = self.step;
            self.relay.push(old);
        }
        let mut due: Vec<Frame> = Vec::new();
        let mut later = Vec::new();
        for f in std::mem::take(&mut self.relay) {
            if f.due <= self.step {
                due.push(f);
            } else {
                later.push(f);
            }
        }
        self.relay = later;
        // The relay's own order.
        for i in (1..due.len()).rev() {
            let j = self.rng.below(i + 1);
            due.swap(i, j);
        }
        for f in due {
            if self.asleep.contains(&f.to) {
                continue;
            }
            let from = self.ids[f.from];
            self.nodes[f.to].hear(from, &f.roster, self.now);
        }
        self.land(false);
    }

    fn steal(&mut self) {
        if self.stolen.is_some() {
            return;
        }
        if let Some(t) = self.roles.iter().position(|r| *r == Role::Stolen) {
            self.stolen = Some(t);
            self.note(format!("{} is stolen", self.ids[t]));
            self.asleep.remove(&t);
        }
    }

    /// A human on honest brother `me` casts the thief out.
    fn cast_thief(&mut self, me: usize) {
        let Some(t) = self.stolen else {
            return;
        };
        if self.cast_by.is_some() || me == t {
            return;
        }
        let thief = self.ids[t];
        self.note(format!("{}'s human casts {thief} out", self.ids[me]));
        self.nodes[me].cast(thief, self.now);
        self.cast_by = Some(me);
    }

    /// The thief signs what it likes and sends it wherever.
    fn thief_acts(&mut self) {
        let Some(t) = self.stolen else {
            return;
        };
        let thief = self.ids[t];
        let roll = self.rng.below(5);
        let mut words: Vec<Word> = Vec::new();
        match roll {
            // Takes over a session it may not: it is no keeper of the
            // owner, or no more, or it is cast out.
            0 | 1 => {
                let held = self.nodes[t].sessions();
                if let Some(k) = self.rng.pick(&held).cloned() {
                    if let Some((o, n)) = self.nodes[t].owner_of(&k) {
                        let oi = self.index(&o);
                        let keeper =
                            oi.is_some_and(|oi| self.keepers[oi].iter().any(|x| x == thief));
                        let allowed = keeper && self.cast_by.is_none();
                        if o != thief {
                            if let Some(w) = self.nodes[t].forge_takes(&k, &o, n + 1, self.now) {
                                if !allowed {
                                    self.wrong.insert(w.sig.clone());
                                }
                                self.note(format!(
                                    "{thief} signs a takeover of {k} (allowed: {allowed})"
                                ));
                                words.push(w);
                            }
                        }
                    }
                }
            }
            // Floods every brother with sessions made up and ended.
            2 if self.plan.flood > 0 => {
                words = self.nodes[t].forge_flood(self.flood_next, self.plan.flood);
                self.flood_next += self.plan.flood;
                for w in &words {
                    self.wrong.insert(w.sig.clone());
                }
                self.note(format!("{thief} floods {} sessions", self.plan.flood));
            }
            // Casts out an honest brother, maybe the very one that cast it
            // out, maybe every one.
            3 => {
                let targets: Vec<usize> = if self.rng.chance(50) {
                    (0..self.nodes.len()).filter(|i| *i != t).collect()
                } else {
                    self.cast_by.into_iter().collect()
                };
                for i in targets {
                    if let Some(w) = self.nodes[t].forge_cast(self.ids[i], self.now) {
                        // Before a human cast it out, a stolen brother casts
                        // out as any brother may.
                        if self.cast_by.is_some() {
                            self.wrong.insert(w.sig.clone());
                        }
                        self.note(format!("{thief} casts {} out", self.ids[i]));
                        words.push(w);
                    }
                }
            }
            _ => {}
        }
        // The thief's own roster carries them, to every brother.
        let mut roster = self.nodes[t].serve(self.now);
        roster.said.extend(words);
        for to in 0..self.nodes.len() {
            if to != t {
                self.post(t, to, roster.clone());
            }
        }
    }

    /// Every handoff lands or fails, and the awake honest brothers pass
    /// their rosters to each other, reliably, until nothing changes.
    fn flush(&mut self) {
        self.land(true);
        let awake = self.honest_awake();
        for _round in 0..6 {
            let mut changed = false;
            for &a in &awake {
                let r = self.nodes[a].serve(self.now);
                for &b in &awake {
                    if a == b {
                        continue;
                    }
                    let before = self.nodes[b].words().len();
                    let owners: Vec<_> = self.nodes[b]
                        .sessions()
                        .iter()
                        .map(|k| self.nodes[b].owner_of(k))
                        .collect();
                    let from = self.ids[a];
                    self.nodes[b].hear(from, &r, self.now);
                    let after: Vec<_> = self.nodes[b]
                        .sessions()
                        .iter()
                        .map(|k| self.nodes[b].owner_of(k))
                        .collect();
                    changed |= before != self.nodes[b].words().len() || owners != after;
                }
            }
            if !changed {
                break;
            }
        }
    }

    /// I5 after every step: what each honest brother holds stays bounded,
    /// and no roster it serves needs a frame past the limit.
    fn check_bounds(&mut self) {
        for i in 0..self.nodes.len() {
            if !self.honest(i) {
                continue;
            }
            let (per, casts) = self.nodes[i].counts();
            let me = self.ids[i];
            for (o, n) in &per {
                if o != me && *n > WORDS_PER_ORIGIN {
                    let what = format!("{me} holds {n} words of sessions {o} started");
                    self.fail("I5", what);
                }
            }
            for (s, n) in &casts {
                if *n > CASTS_PER_SIGNER {
                    let what = format!("{me} holds {n} cast outs by {s}");
                    self.fail("I5", what);
                }
            }
        }
    }

    fn check_all(&mut self) {
        self.check_agreement();
        self.check_no_fencing();
        self.check_one_owner();
        self.check_cast_out();
        self.check_one_hand();
    }

    /// I1: awake honest brothers, having passed every word, agree.
    fn check_agreement(&mut self) {
        let awake = self.honest_awake();
        let Some(&first) = awake.first() else {
            return;
        };
        type View = (BTreeMap<String, Option<(String, u64)>>, BTreeSet<String>);
        let view = |n: &C| -> View {
            let s = n
                .sessions()
                .into_iter()
                .map(|k| (k.clone(), n.owner_of(&k)))
                .collect();
            (s, n.out_keys())
        };
        let base = view(&self.nodes[first]);
        for &b in &awake[1..] {
            let other = view(&self.nodes[b]);
            if other.1 != base.1 {
                let what = format!(
                    "{} holds {:?} out, {} holds {:?}",
                    self.ids[first], base.1, self.ids[b], other.1
                );
                self.fail("I1", what);
            }
            for (k, o) in &base.0 {
                if let Some(p) = other.0.get(k) {
                    if p != o {
                        let what = format!(
                            "{k}: {} says {:?}, {} says {:?}",
                            self.ids[first], o, self.ids[b], p
                        );
                        self.fail("I1", what);
                    }
                }
            }
        }
    }

    /// I2: what the thief signed without the right to changes no honest
    /// owner: a brother hearing every word agrees with one hearing every
    /// word but those.
    fn check_no_fencing(&mut self) {
        let awake = self.honest_awake();
        if awake.is_empty() || self.wrong.is_empty() {
            return;
        }
        let mut all = C::node("zed", &self.ids, Vec::new(), &format!("{}-all", self.seed));
        let mut clean = C::node(
            "zed",
            &self.ids,
            Vec::new(),
            &format!("{}-clean", self.seed),
        );
        let ids = self.ids.clone();
        let rosters: Vec<(usize, Roster)> = awake
            .iter()
            .map(|&a| {
                let mut r = self.nodes[a].serve(self.now);
                unflooded(&mut r.said);
                (a, r)
            })
            .collect();
        for _ in 0..2 {
            for (a, r) in &rosters {
                all.hear(ids[*a], r, self.now);
                let mut c = r.clone();
                c.said.retain(|w| !self.wrong.contains(&w.sig));
                clean.hear(ids[*a], &c, self.now);
            }
        }
        for k in clean.sessions() {
            let Some((o, n)) = clean.owner_of(&k) else {
                continue;
            };
            // A live session run by an honest owner: awake, holding a copy,
            // and owning it as it holds it itself.
            let Some(oi) = self.index(&o) else {
                continue;
            };
            let runs = self.honest(oi)
                && !self.asleep.contains(&oi)
                && self.copies.get(&k).is_some_and(|c| c.contains(&oi))
                && self.nodes[oi].owner_of(&k).is_some_and(|x| x.0 == o);
            if !runs || clean.ended(&k) {
                continue;
            }
            let got = all.owner_of(&k);
            if got.as_ref().map(|g| &g.0) != Some(&o) {
                let what = format!("{k} is {o}'s at {n}, but the thief's word makes it {got:?}");
                self.fail("I2", what);
            }
        }
    }

    /// I3: every live session an honest brother owns, as every honest
    /// brother's words together say, has a copy there to run; and no two
    /// awake honest brothers, having heard each other, both run one.
    fn check_one_owner(&mut self) {
        let mut seer = C::node("zed", &self.ids, Vec::new(), &format!("{}-seer", self.seed));
        for i in 0..self.nodes.len() {
            if self.honest(i) {
                let mut r = Roster {
                    said: self.nodes[i].words(),
                    ..Roster::default()
                };
                unflooded(&mut r.said);
                seer.hear(self.ids[i], &r, self.now);
            }
        }
        for k in seer.sessions() {
            if seer.ended(&k) {
                continue;
            }
            let Some((o, _)) = seer.owner_of(&k) else {
                continue;
            };
            let Some(oi) = self.index(&o) else {
                continue;
            };
            let copy = self.copies.get(&k).is_some_and(|c| c.contains(&oi));
            // Over on its owner, which let it go: a brother that slept
            // through it learns so from the owner.
            let over = self.nodes[oi].let_go().contains(&k);
            if self.honest(oi) && !copy && !over {
                let mut words = Vec::new();
                for i in 0..self.nodes.len() {
                    if self.honest(i) {
                        words.push(format!("{}: {}", self.ids[i], self.words_on(i, &k)));
                    }
                }
                let what = format!(
                    "{k} is {o}'s, which holds no copy to run; held: {}",
                    words.join(" | ")
                );
                self.fail("I3", what);
            }
        }
        let mut runs: BTreeMap<String, BTreeSet<usize>> = BTreeMap::new();
        for a in self.honest_awake() {
            for k in self.nodes[a].sessions() {
                let mine = self.nodes[a]
                    .owner_of(&k)
                    .is_some_and(|(o, _)| o == self.ids[a]);
                let copy = self.copies.get(&k).is_some_and(|c| c.contains(&a));
                if mine && copy && !self.nodes[a].ended(&k) {
                    runs.entry(k).or_default().insert(a);
                }
            }
        }
        for (k, r) in runs {
            if r.len() > 1 {
                let names: Vec<_> = r.iter().map(|i| self.ids[*i]).collect();
                self.fail("I3", format!("{k} runs on {names:?}"));
            }
        }
    }

    /// The words brother `a` holds on session `k`, short.
    fn words_on(&self, a: usize, k: &str) -> String {
        self.nodes[a]
            .words()
            .iter()
            .filter(|w| w.what.key().as_deref() == Some(k))
            .map(|w| {
                let kind = match &w.what {
                    crate::said::What::Owns { v, keepers, .. } => format!("owns v{v} {keepers:?}"),
                    crate::said::What::Hands { to, .. } => format!("hands to {to}"),
                    crate::said::What::Recalls { .. } => "recalls".to_string(),
                    crate::said::What::Takes { from, .. } => format!("takes from {from}"),
                    crate::said::What::Ended { .. } => "ended".to_string(),
                    crate::said::What::CastOut { .. } => "cast".to_string(),
                };
                format!("[{} {} {kind} @{}]", w.n, w.by, w.at)
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// I4: once an honest human cast the thief out, no honest brother
    /// lets it act.
    fn check_cast_out(&mut self) {
        let (Some(t), Some(c)) = (self.stolen, self.cast_by) else {
            return;
        };
        let thief = self.ids[t];
        let caster = self.ids[c];
        for a in self.honest_awake() {
            let holds = self.nodes[a].words().iter().any(|w| {
                w.by == caster
                    && matches!(&w.what, crate::said::What::CastOut { id, .. } if id == thief)
            });
            if holds && self.nodes[a].controls(thief) {
                let what = format!("{} still lets {thief} act on it", self.ids[a]);
                self.fail("I4", what);
            }
        }
    }

    /// One handoff, one word: no brother signs two live handoffs of one
    /// session at one number to one brother.
    fn check_one_hand(&mut self) {
        for a in self.honest_awake() {
            let mut seen: BTreeMap<(String, u64, String, String), usize> = BTreeMap::new();
            let words = self.nodes[a].words();
            let recalled = self.nodes[a].recalled();
            for w in words.iter().filter(|w| !recalled.contains(&w.sig)) {
                if let crate::said::What::Hands { to, .. } = &w.what {
                    let key = w.what.key().unwrap_or_default();
                    *seen
                        .entry((key, w.n, w.by.clone(), to.clone()))
                        .or_default() += 1;
                }
            }
            if let Some(((k, n, by, to), c)) = seen.into_iter().find(|(_, c)| *c > 1) {
                let what = format!("{by} signed {c} handoffs of {k} to {to} at {n}");
                self.fail("one hand", what);
            }
        }
    }

    /// The end: everyone wakes, every session ends, time passes, and
    /// every ended session must be gone.
    fn finish(&mut self) {
        self.asleep.clear();
        self.flush();
        self.check_all();
        for i in 0..self.nodes.len() {
            if !self.honest(i) {
                continue;
            }
            for k in self.mine(i) {
                self.nodes[i].end(&k, self.now);
            }
        }
        for day in [1u64, 40, 100, 100] {
            self.now += day * 86_400;
            self.flush();
            for i in 0..self.nodes.len() {
                self.nodes[i].prune(self.now);
            }
        }
        self.flush();
        for i in self.honest_awake() {
            let held: Vec<String> = self.nodes[i]
                .sessions()
                .into_iter()
                .filter(|k| self.nodes[i].ended(k))
                .collect();
            if !held.is_empty() {
                let what = format!(
                    "{} still holds {} ended sessions after a hundred days, {:?}",
                    self.ids[i],
                    held.len(),
                    held.iter().take(3).collect::<Vec<_>>()
                );
                self.fail("I6", what);
            }
        }
    }
}

/// Leaves out the sessions a thief made up by the hundred (named `f...`),
/// which [`Sim::check_bounds`] watches: the other checks judge the rest
/// the quicker.
fn unflooded(said: &mut Vec<Word>) {
    said.retain(|w| w.what.session().is_none_or(|(_, id)| !id.starts_with('f')));
}

/// Runs seeds `from..to`, the thief flooding in `floods` of a hundred,
/// and gives every invariant broken.
pub fn sweep<C: Core>(from: u64, to: u64, floods: u64) -> Vec<Broken> {
    let mut out = Vec::new();
    for seed in from..to {
        let mut s: Sim<C> = Sim::new(seed, floods);
        s.run();
        out.extend(s.broken);
    }
    out
}

/// The core as it is: each brother a [`crate::mesh::Held`] in memory,
/// changed by the same functions the app and the node call.
pub mod core {
    use std::collections::{BTreeMap, BTreeSet};

    use super::keys::{key, pair};
    use super::Core;
    use crate::brotherhood::{Brother, Link, Roster};
    use crate::joining::fake::Fake;
    use crate::mesh::{self, Held, Trust};
    use crate::said::{self, Signer, What, Word};

    static C: Fake = Fake::new();

    pub struct Node {
        me: &'static str,
        held: Held,
        keys: BTreeMap<String, String>,
        cast: BTreeMap<String, String>,
        keepers: Vec<String>,
        entry_n: u64,
        /// This brother's process, told apart from the one before a
        /// restart.
        process: u64,
    }

    impl Node {
        fn trust(&self) -> Trust<'static> {
            let r = &self.held.roster;
            let control = self
                .keys
                .iter()
                .filter(|(id, k)| {
                    **id != self.me && !r.outcast.contains(*k) && !r.contested.contains(*k)
                })
                .map(|(id, _)| id.clone())
                .collect();
            Trust {
                me: self.me.to_string(),
                keys: self.keys.clone(),
                cast: self.cast.clone(),
                control,
                view_only: BTreeSet::new(),
                keepers: self.keepers.clone(),
                crypto: &C,
            }
        }

        fn signer(&self) -> Signer<'static> {
            Signer {
                me: self.me,
                private: pair(self.me),
                crypto: &C,
            }
        }

        fn id(key: &str) -> String {
            said::split(key).1.to_string()
        }

        /// What the brothers code does on a roster that puts a brother
        /// out: its key is kept apart.
        fn drop_out(&mut self) {
            for (id, k) in self.keys.clone() {
                if id != self.me && self.held.roster.outcast.contains(&k) {
                    self.keys.remove(&id);
                    self.cast.insert(id, k);
                }
            }
        }
    }

    impl Core for Node {
        fn node(me: &'static str, all: &[&'static str], keepers: Vec<String>, tag: &str) -> Self {
            let mut keys: BTreeMap<String, String> =
                all.iter().map(|i| (i.to_string(), key(i))).collect();
            keys.insert(me.to_string(), key(me));
            Node {
                me,
                held: Held {
                    version: mesh::VERSION,
                    ..Held::default()
                },
                keys,
                cast: BTreeMap::new(),
                keepers,
                entry_n: 1,
                process: crate::lore::hash(tag.as_bytes()),
            }
        }

        fn start(&mut self, id: &str, now: u64) {
            let (s, t) = (self.signer(), self.trust());
            mesh::own_in(&mut self.held, Some(&s), &t, id, now);
        }

        fn hand_out(&mut self, key: &str, to: &str, now: u64) -> Option<(u64, Vec<Word>)> {
            let o = self.held.roster.sessions.get(key).cloned()?;
            if o.brother != self.me {
                return None;
            }
            let n = o.n + 1;
            let s = self.signer();
            let words = mesh::handing_word_in(&mut self.held, &s, &Node::id(key), to, n, now);
            // The handing process is this one.
            if let Some(p) = self.held.pending.get_mut(key) {
                p.process = self.process;
            }
            Some((n, words))
        }

        fn arrive(&mut self, key: &str, n: u64, words: &[Word], now: u64) -> Result<(), String> {
            let (s, t) = (self.signer(), self.trust());
            let id = Node::id(key);
            mesh::may_arrive_in(&self.held, &t, words, &id, n)?;
            if mesh::arrived_in(&mut self.held, Some(&s), words, &t, &id, n, now) {
                Ok(())
            } else {
                Err("ran, but it is not this brother's".into())
            }
        }

        fn took(&mut self, key: &str, to: &str, n: u64, now: u64) -> bool {
            let _ = now;
            let t = self.trust();
            mesh::hand_in(&mut self.held, &t, &Node::id(key), to, n)
        }

        fn fail(&mut self, key: &str, _to: &str, _n: u64, now: u64) {
            let (s, t) = (self.signer(), self.trust());
            mesh::recall_in(&mut self.held, &s, &t, &Node::id(key), now);
        }

        fn restart(&mut self, now: u64) {
            self.process = self.process.wrapping_add(1);
            let (s, t) = (self.signer(), self.trust());
            mesh::recall_stale_in(&mut self.held, &s, &t, self.process, now);
        }

        fn take_over(&mut self, key: &str, from: &str, now: u64) -> bool {
            let id = Node::id(key);
            let Some(o) = self.held.roster.sessions.get(key).cloned() else {
                return false;
            };
            let carried = mesh::chain(&self.held, &id, from);
            let (s, t) = (self.signer(), self.trust());
            mesh::take_in(&mut self.held, &s, &t, &id, from, o.n, &carried, &[], now).is_ok()
        }

        fn end(&mut self, key: &str, now: u64) -> bool {
            let (s, t) = (self.signer(), self.trust());
            mesh::end_in(&mut self.held, Some(&s), &t, &Node::id(key), now)
        }

        fn set_keepers(&mut self, keepers: Vec<String>, now: u64) {
            self.keepers = keepers;
            self.entry_n += 1;
            let (s, t) = (self.signer(), self.trust());
            mesh::rekeep_in(&mut self.held, &s, &t, now);
        }

        fn cast(&mut self, id: &str, now: u64) {
            let s = self.signer();
            mesh::cast_out_in(&mut self.held, Some(&s), id, id, &key(id), now);
            if let Some(k) = self.keys.remove(id) {
                self.cast.insert(id.to_string(), k);
            }
            self.drop_out();
        }

        fn serve(&mut self, now: u64) -> Roster {
            let (s, t) = (self.signer(), self.trust());
            mesh::tend(&mut self.held, &s, &t, self.process, now);
            let mut r = self.held.roster.clone();
            let mut b = Brother::new(self.me, self.me, &key(self.me), Link::Relay);
            b.n = self.entry_n;
            b.keepers = self.keepers.clone();
            assert!(said::sign_entry(&C, &pair(self.me), &mut b));
            r.brothers.retain(|x| x.id != self.me);
            r.insert(b);
            r
        }

        fn hear(&mut self, from: &str, r: &Roster, now: u64) {
            let t = self.trust();
            let (h, _) = mesh::merged(&self.held, from, r, &t, now);
            self.held = h;
            self.drop_out();
        }

        fn owner_of(&self, key: &str) -> Option<(String, u64)> {
            self.held
                .roster
                .sessions
                .get(key)
                .map(|o| (o.brother.clone(), o.n))
        }

        fn ended(&self, key: &str) -> bool {
            self.held.roster.ended.contains_key(key)
        }

        fn sessions(&self) -> Vec<String> {
            self.held.roster.sessions.keys().cloned().collect()
        }

        fn out_keys(&self) -> BTreeSet<String> {
            let r = &self.held.roster;
            r.outcast.union(&r.contested).cloned().collect()
        }

        fn words(&self) -> Vec<Word> {
            self.held.roster.said.clone()
        }

        fn let_go(&self) -> Vec<String> {
            self.held.roster.let_go.clone()
        }

        fn recalled(&self) -> BTreeSet<String> {
            self.held
                .roster
                .said
                .iter()
                .filter_map(|w| match &w.what {
                    What::Recalls { hand, .. } => Some(hand.clone()),
                    _ => None,
                })
                .collect()
        }

        fn counts(&self) -> (BTreeMap<String, usize>, BTreeMap<String, usize>) {
            let mut per: BTreeMap<String, usize> = BTreeMap::new();
            let mut casts: BTreeMap<String, usize> = BTreeMap::new();
            for w in &self.held.roster.said {
                match w.what.session() {
                    Some((o, _)) => *per.entry(o.to_string()).or_default() += 1,
                    None => *casts.entry(w.by.clone()).or_default() += 1,
                }
            }
            (per, casts)
        }

        fn prune(&mut self, now: u64) {
            mesh::prune(&mut self.held, self.me, now);
        }

        fn controls(&self, id: &str) -> bool {
            self.trust().controls(id)
        }

        fn forge_takes(&self, key: &str, from: &str, n: u64, now: u64) -> Option<Word> {
            let (origin, session) = said::split(key);
            let what = What::Takes {
                origin: origin?.to_string(),
                session: session.to_string(),
                from: from.to_string(),
                keepers: Vec::new(),
            };
            // On the owner's tenure as this brother holds it.
            let made = mesh::resolved(&self.held.roster, key).made;
            self.signer().sign_after(n, now, &made, what)
        }

        fn forge_flood(&self, first: usize, count: usize) -> Vec<Word> {
            let s = self.signer();
            let far = 4_000_000_000_000;
            let mut out = Vec::new();
            for i in first..first + count {
                let session = format!("f{i}");
                let origin = self.me.to_string();
                let owns = What::Owns {
                    origin: origin.clone(),
                    session: session.clone(),
                    keepers: Vec::new(),
                    v: 0,
                };
                let Some(first) = s.sign(1, far, owns) else {
                    continue;
                };
                out.extend(s.sign_after(1, far, &first.sig, What::Ended { origin, session }));
                out.push(first);
            }
            out
        }

        fn forge_cast(&self, id: &str, now: u64) -> Option<Word> {
            self.signer().sign(
                0,
                now,
                What::CastOut {
                    id: id.to_string(),
                    key: key(id),
                },
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::core::Node;
    use super::*;

    fn report(broken: &[Broken]) -> String {
        let mut by: BTreeMap<&str, Vec<&Broken>> = BTreeMap::new();
        for b in broken {
            by.entry(b.invariant).or_default().push(b);
        }
        by.iter()
            .map(|(inv, list)| {
                let b = list[0];
                format!(
                    "{inv}: {} seeds, first seed {} step {}: {}
      {}",
                    list.len(),
                    b.seed,
                    b.step,
                    b.what,
                    b.trail.join(
                        "
      "
                    )
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Every invariant holds over a thousand seeds, in CI time: floods
    /// in few of them. `HORADRIC_SIM_SEEDS` runs another count.
    #[test]
    fn the_brotherhood_holds_under_attack() {
        let seeds = std::env::var("HORADRIC_SIM_SEEDS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(1000);
        let t = std::time::Instant::now();
        let broken = sweep::<Node>(0, seeds, 5);
        eprintln!("{seeds} seeds in {:?}", t.elapsed());
        assert!(broken.is_empty(), "{}", report(&broken));
    }

    /// A thief flooding every brother with sessions, in every seed, up to
    /// and past every bound.
    #[test]
    fn the_brotherhood_holds_under_floods() {
        let broken = sweep::<Node>(5_000, 5_012, 100);
        assert!(broken.is_empty(), "{}", report(&broken));
    }

    /// The long run: `cargo test -p horadric-core --release
    /// brotherhood_sim -- --ignored`.
    #[test]
    #[ignore = "long: twenty thousand seeds"]
    fn the_brotherhood_holds_under_attack_long() {
        let broken = sweep::<Node>(10_000, 30_000, 30);
        assert!(broken.is_empty(), "{}", report(&broken));
    }

    /// One seed, its trail told: `HORADRIC_SIM_SEED` and
    /// `HORADRIC_SIM_FLOODS` (in a hundred).
    #[test]
    #[ignore = "for looking into one seed"]
    fn one_seed() {
        let var = |n: &str, d: u64| {
            std::env::var(n)
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(d)
        };
        let seed = var("HORADRIC_SIM_SEED", 0);
        let broken = sweep::<Node>(seed, seed + 1, var("HORADRIC_SIM_FLOODS", 30));
        assert!(broken.is_empty(), "{}", report(&broken));
    }

    /// The same seed runs the same.
    #[test]
    fn a_seed_is_a_replay() {
        let run = |seed| {
            let mut s: Sim<Node> = Sim::new(seed, 30);
            s.run();
            let views: Vec<_> = s
                .nodes
                .iter()
                .map(|n| (n.sessions(), n.out_keys()))
                .collect();
            (s.broken.clone(), views)
        };
        assert_eq!(run(7), run(7));
    }
}
