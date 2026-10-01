//! What the Discord presence says: the sessions and the setting, turned
//! into the [`Activity`] the pipe client sends. Pure, so every line of it
//! is tested; the app asks again whenever the registry or the setting
//! changes, and the client drops what did not change.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::discord::Activity;
use crate::saved::{Discord, SavedRun};
use crate::session::{Phase, Session};

/// The art. Discord takes an `https` URL where it would take the key of
/// an uploaded asset, so the images live in `docs/discord` and are named
/// by their URL on `main`, which nothing has to be uploaded for. See
/// `docs/discord/README.md` for going back to keys.
pub const LARGE_IMAGE: &str =
    "https://raw.githubusercontent.com/Mopra/horadric.dev/main/docs/discord/horadric.png";
pub const WAITS_IMAGE: &str =
    "https://raw.githubusercontent.com/Mopra/horadric.dev/main/docs/discord/waits.png";
pub const WORKING_IMAGE: &str =
    "https://raw.githubusercontent.com/Mopra/horadric.dev/main/docs/discord/working.png";
pub const IDLE_IMAGE: &str =
    "https://raw.githubusercontent.com/Mopra/horadric.dev/main/docs/discord/idle.png";

/// Discord refuses a `state` or `details` longer than this.
const MAX_TEXT: usize = 128;

/// No session has worked for this long: the run of work is over, and the
/// next one counts from zero. Shorter, and reading an answer or a quiet
/// moment between turns would restart the clock the plan says must hold.
pub const RUN_GAP: Duration = Duration::from_secs(10 * 60);

/// One session as the presence sees it: the session and the name of the
/// project it belongs to, which only the app can work out (it asks git).
#[derive(Debug, Clone, Copy)]
pub struct Seen<'a> {
    pub session: &'a Session,
    pub project: &'a str,
}

/// The lamp a session shows, most urgent first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Lamp {
    Waits,
    Working,
    Idle,
}

/// The lamp of a session that counts, or None for one that does not: a
/// plain shell is not an agent, and an ended or paused one runs nothing.
fn lamp(s: &Session) -> Option<Lamp> {
    if s.shell {
        return None;
    }
    match s.phase {
        Phase::Waiting(_) => Some(Lamp::Waits),
        Phase::Working => Some(Lamp::Working),
        Phase::Idle | Phase::Done => Some(Lamp::Idle),
        Phase::Ended | Phase::Paused => None,
    }
}

/// The presence for these sessions, or None with the setting off or
/// nothing running, which clears it rather than saying "0 agents". `stage`
/// is the name of the project on the stage, and `start` when the current
/// run of work began, from [`Run`].
pub fn presence(
    sessions: &[Seen],
    stage: Option<&str>,
    setting: Discord,
    start: Option<SystemTime>,
) -> Option<Activity> {
    if setting.is_off() {
        return None;
    }
    let lamps: Vec<(Lamp, &str)> = sessions
        .iter()
        .filter_map(|s| lamp(s.session).map(|l| (l, s.project)))
        .collect();
    let urgent = lamps.iter().map(|(l, _)| *l).min()?;
    let count = |want: Lamp| lamps.iter().filter(|(l, _)| *l == want).count();
    let (small_image, small_text) = match urgent {
        Lamp::Waits => (WAITS_IMAGE, "Waits for you"),
        Lamp::Working => (WORKING_IMAGE, "Working"),
        Lamp::Idle => (IDLE_IMAGE, "Idle"),
    };
    Some(Activity {
        details: clip(&details(
            count(Lamp::Working),
            count(Lamp::Waits),
            count(Lamp::Idle),
        )),
        state: setting
            .names()
            .then(|| project(&lamps, stage))
            .flatten()
            .map(|p| clip(&format!("in {p}"))),
        large_image: Some(LARGE_IMAGE.to_string()),
        large_text: Some("Horadric".to_string()),
        small_image: Some(small_image.to_string()),
        small_text: Some(small_text.to_string()),
        start: start
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs()),
    })
}

/// "3 agents working, 1 waits for you". Idle sessions are only counted
/// when nothing else is going on, since then they are all there is to say.
fn details(working: usize, waits: usize, idle: usize) -> String {
    let agents = |n: usize| if n == 1 { "agent" } else { "agents" };
    let wait = |n: usize| if n == 1 { "waits" } else { "wait" };
    match (working, waits) {
        (0, 0) => format!("{idle} {} idle", agents(idle)),
        (w, 0) => format!("{w} {} working", agents(w)),
        (0, n) => format!("{n} {} {} for you", agents(n), wait(n)),
        (w, n) => format!("{w} {} working, {n} {} for you", agents(w), wait(n)),
    }
}

/// The project to name: the one on the stage if it runs anything, else the
/// busiest, by most working, then most sessions, then name so it does not
/// flicker between two that tie.
fn project<'a>(lamps: &[(Lamp, &'a str)], stage: Option<&str>) -> Option<&'a str> {
    if let Some(p) = stage.and_then(|st| lamps.iter().find(|(_, p)| *p == st)) {
        return Some(p.1);
    }
    let mut names: Vec<&str> = lamps.iter().map(|(_, p)| *p).collect();
    names.sort_unstable();
    names.dedup();
    names.into_iter().max_by(|a, b| {
        let busy = |name: &str| {
            let of = lamps.iter().filter(|(_, p)| *p == name);
            let working = of.clone().filter(|(l, _)| *l == Lamp::Working).count();
            (working, of.count())
        };
        busy(a).cmp(&busy(b)).then_with(|| b.cmp(a))
    })
}

/// At most [`MAX_TEXT`] characters, cut on a character, not a byte.
fn clip(text: &str) -> String {
    match text.char_indices().nth(MAX_TEXT) {
        Some((at, _)) => text[..at].to_string(),
        None => text.to_string(),
    }
}

/// When the current run of work began. It starts when a session starts
/// working with no run going, holds while any session works and for
/// [`RUN_GAP`] after the last one stopped, so the count on the profile
/// does not go back to zero at every turn.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Run {
    start: Option<SystemTime>,
    last_work: Option<SystemTime>,
}

impl Run {
    /// Look at the sessions again and say when the run began, if one is
    /// going. A run picked up mid turn, as after a restart, counts from
    /// when the earliest working session began working.
    pub fn update(&mut self, sessions: &[Seen], now: SystemTime) -> Option<SystemTime> {
        let working = sessions
            .iter()
            .map(|s| s.session)
            .filter(|s| lamp(s) == Some(Lamp::Working))
            .map(|s| s.since)
            .min();
        let over = self
            .last_work
            .is_some_and(|t| now.duration_since(t).unwrap_or_default() >= RUN_GAP);
        if over {
            *self = Run::default();
        }
        if let Some(since) = working {
            self.start.get_or_insert(since.min(now));
            self.last_work = Some(now);
        }
        self.start
    }

    /// The run to write down as the app goes away, if one is going.
    pub fn to_saved(&self) -> Option<SavedRun> {
        let secs = |t: SystemTime| t.duration_since(UNIX_EPOCH).ok().map(|d| d.as_secs());
        Some(SavedRun {
            start: secs(self.start?)?,
            last_work: secs(self.last_work?)?,
        })
    }

    /// The run a reload handed over. One that is over by now ends at the
    /// next [`Run::update`], as it would have without the reload.
    pub fn from_saved(saved: Option<SavedRun>) -> Run {
        let at = |secs: u64| UNIX_EPOCH + Duration::from_secs(secs);
        saved.map_or_else(Run::default, |s| Run {
            start: Some(at(s.start)),
            last_work: Some(at(s.last_work)),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::WaitReason;

    fn at(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn session(phase: Phase) -> Session {
        let mut s = Session::new("id", "name", "c:/code/x");
        s.phase = phase;
        s.since = at(1000);
        s
    }

    fn seen<'a>(sessions: &'a [(Session, &'a str)]) -> Vec<Seen<'a>> {
        sessions
            .iter()
            .map(|(session, project)| Seen { session, project })
            .collect()
    }

    #[test]
    fn nothing_running_clears_it() {
        assert_eq!(presence(&[], None, Discord::Named, None), None);
        let gone = [(session(Phase::Ended), "a"), (session(Phase::Paused), "a")];
        assert_eq!(presence(&seen(&gone), None, Discord::Named, None), None);
    }

    #[test]
    fn off_says_nothing() {
        let all = [(session(Phase::Working), "a")];
        assert_eq!(presence(&seen(&all), None, Discord::Off, None), None);
    }

    #[test]
    fn shells_are_not_agents() {
        let mut shell = session(Phase::Idle);
        shell.shell = true;
        let only = [(shell, "a")];
        assert_eq!(presence(&seen(&only), None, Discord::Named, None), None);
    }

    #[test]
    fn details_count_working_and_waiting() {
        let all = [
            (session(Phase::Working), "a"),
            (session(Phase::Working), "a"),
            (session(Phase::Working), "b"),
            (session(Phase::Waiting(WaitReason::Permission)), "b"),
            (session(Phase::Idle), "c"),
        ];
        let a = presence(&seen(&all), None, Discord::Unnamed, None).unwrap();
        assert_eq!(a.details, "3 agents working, 1 waits for you");
    }

    #[test]
    fn details_read_right_for_one_and_many() {
        assert_eq!(details(1, 0, 0), "1 agent working");
        assert_eq!(details(0, 1, 0), "1 agent waits for you");
        assert_eq!(details(0, 2, 3), "2 agents wait for you");
        assert_eq!(details(2, 2, 0), "2 agents working, 2 wait for you");
        assert_eq!(details(0, 0, 1), "1 agent idle");
        assert_eq!(details(0, 0, 4), "4 agents idle");
    }

    #[test]
    fn done_counts_as_idle() {
        let all = [(session(Phase::Done), "a"), (session(Phase::Idle), "a")];
        let a = presence(&seen(&all), None, Discord::Unnamed, None).unwrap();
        assert_eq!(a.details, "2 agents idle");
        assert_eq!(a.small_image.as_deref(), Some(IDLE_IMAGE));
    }

    #[test]
    fn the_small_image_is_the_most_urgent_state() {
        let small = |phases: Vec<Phase>| {
            let all: Vec<(Session, &str)> = phases.into_iter().map(|p| (session(p), "a")).collect();
            presence(&seen(&all), None, Discord::Unnamed, None)
                .unwrap()
                .small_image
                .unwrap()
        };
        assert_eq!(small(vec![Phase::Idle, Phase::Done]), IDLE_IMAGE);
        assert_eq!(small(vec![Phase::Idle, Phase::Working]), WORKING_IMAGE);
        let error = Phase::Waiting(WaitReason::Error("boom".into()));
        assert_eq!(small(vec![Phase::Working, error, Phase::Idle]), WAITS_IMAGE);
    }

    #[test]
    fn the_large_image_is_horadrics_own() {
        let all = [(session(Phase::Working), "a")];
        let a = presence(&seen(&all), None, Discord::Unnamed, None).unwrap();
        assert_eq!(a.large_image.as_deref(), Some(LARGE_IMAGE));
        assert_eq!(a.large_text.as_deref(), Some("Horadric"));
        assert_eq!(a.small_text.as_deref(), Some("Working"));
    }

    #[test]
    fn project_names_only_when_allowed() {
        let all = [(session(Phase::Working), "secret")];
        let hidden = presence(&seen(&all), Some("secret"), Discord::Unnamed, None).unwrap();
        assert_eq!(hidden.state, None);
        let shown = presence(&seen(&all), Some("secret"), Discord::Named, None).unwrap();
        assert_eq!(shown.state.as_deref(), Some("in secret"));
    }

    #[test]
    fn the_stage_wins_over_the_busiest() {
        let all = [
            (session(Phase::Working), "busy"),
            (session(Phase::Working), "busy"),
            (session(Phase::Idle), "quiet"),
        ];
        let a = presence(&seen(&all), Some("quiet"), Discord::Named, None).unwrap();
        assert_eq!(a.state.as_deref(), Some("in quiet"));
    }

    #[test]
    fn an_empty_stage_names_the_busiest() {
        let all = [
            (session(Phase::Idle), "many"),
            (session(Phase::Idle), "many"),
            (session(Phase::Idle), "many"),
            (session(Phase::Working), "busy"),
            (session(Phase::Idle), "few"),
        ];
        let a = presence(&seen(&all), Some("ended"), Discord::Named, None).unwrap();
        assert_eq!(a.state.as_deref(), Some("in busy"));
        let b = presence(&seen(&all[..3]), None, Discord::Named, None).unwrap();
        assert_eq!(b.state.as_deref(), Some("in many"));
    }

    #[test]
    fn a_tie_names_the_first_by_name() {
        let all = [
            (session(Phase::Working), "zed"),
            (session(Phase::Working), "abe"),
        ];
        let a = presence(&seen(&all), None, Discord::Named, None).unwrap();
        assert_eq!(a.state.as_deref(), Some("in abe"));
    }

    #[test]
    fn long_text_is_cut_on_a_character() {
        let long = "ø".repeat(300);
        let cut = clip(&long);
        assert_eq!(cut.chars().count(), MAX_TEXT);
        assert_eq!(clip("short"), "short");
    }

    #[test]
    fn the_start_is_unix_seconds() {
        let all = [(session(Phase::Working), "a")];
        let a = presence(&seen(&all), None, Discord::Unnamed, Some(at(1234))).unwrap();
        assert_eq!(a.start, Some(1234));
        let b = presence(&seen(&all), None, Discord::Unnamed, None).unwrap();
        assert_eq!(b.start, None);
    }

    #[test]
    fn a_run_holds_across_turns() {
        let mut run = Run::default();
        let working = [(session(Phase::Working), "a")];
        let done = [(session(Phase::Done), "a")];
        assert_eq!(run.update(&seen(&done), at(900)), None);
        // Picked up mid turn, it counts from when the session began working.
        assert_eq!(run.update(&seen(&working), at(1100)), Some(at(1000)));
        assert_eq!(run.update(&seen(&done), at(1200)), Some(at(1000)));
        let mut next = session(Phase::Working);
        next.since = at(1300);
        let again = [(next, "a")];
        assert_eq!(run.update(&seen(&again), at(1300)), Some(at(1000)));
    }

    #[test]
    fn a_run_ends_after_the_gap() {
        let mut run = Run::default();
        let working = [(session(Phase::Working), "a")];
        let done = [(session(Phase::Done), "a")];
        run.update(&seen(&working), at(1000));
        let gap = RUN_GAP.as_secs();
        assert_eq!(run.update(&seen(&done), at(1000 + gap - 1)), Some(at(1000)));
        assert_eq!(run.update(&seen(&done), at(1000 + gap)), None);
        let mut next = session(Phase::Working);
        next.since = at(5000);
        let again = [(next, "a")];
        assert_eq!(run.update(&seen(&again), at(5000)), Some(at(5000)));
    }

    #[test]
    fn a_run_carries_over_a_reload() {
        let mut run = Run::default();
        let working = [(session(Phase::Working), "a")];
        run.update(&seen(&working), at(1500));
        let saved = run.to_saved();
        assert_eq!(
            saved,
            Some(SavedRun {
                start: 1000,
                last_work: 1500
            })
        );
        let mut back = Run::from_saved(saved);
        assert_eq!(back, run);
        // The new build sees the same session still working, started anew.
        let mut again = session(Phase::Working);
        again.since = at(1510);
        let after = [(again, "a")];
        assert_eq!(back.update(&seen(&after), at(1510)), Some(at(1000)));
    }

    #[test]
    fn a_run_handed_over_after_the_gap_is_over() {
        let saved = SavedRun {
            start: 1000,
            last_work: 1500,
        };
        let mut back = Run::from_saved(Some(saved));
        let done = [(session(Phase::Done), "a")];
        let late = at(1500 + RUN_GAP.as_secs());
        assert_eq!(back.update(&seen(&done), late), None);
    }

    #[test]
    fn no_run_hands_over_nothing() {
        assert_eq!(Run::default().to_saved(), None);
        assert_eq!(Run::from_saved(None), Run::default());
    }

    #[test]
    fn a_waiting_session_does_not_start_a_run() {
        let mut run = Run::default();
        let waits = [(session(Phase::Waiting(WaitReason::Input)), "a")];
        assert_eq!(run.update(&seen(&waits), at(1000)), None);
    }

    #[test]
    fn a_since_in_the_future_starts_now() {
        let mut run = Run::default();
        let mut s = session(Phase::Working);
        s.since = at(9000);
        let all = [(s, "a")];
        assert_eq!(run.update(&seen(&all), at(2000)), Some(at(2000)));
    }
}
