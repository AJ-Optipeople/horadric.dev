//! Errands: stones cast on a clock. A stone with `"every"` is cast
//! unattended once the human has armed it, at most one a project at a
//! time, and stopped when it runs past `"for"`. When each is due and
//! which one goes now is decided here, from Unix seconds and the local
//! clock's offset, so the app only reads the clock and carries it out.

use serde::{Deserialize, Serialize};

use super::{Rune, Stone};
use crate::warriv::Full;

/// How often an errand is cast.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Every {
    /// So many seconds after the last cast began.
    Span(u64),
    /// Each day at this minute of the local day.
    Day(u32),
    /// Monday to Friday at this minute.
    Weekday(u32),
    /// One day a week, 0 for Monday, at this minute.
    Week(u8, u32),
}

/// A stone's clock: how often, and how long a cast may run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Errand {
    pub every: Every,
    /// Seconds a cast may run before it is stopped.
    pub most: u64,
    /// `"mode": "bypass"`: its session skips every permission prompt
    /// rather than stop at one as the project's sessions do.
    pub bypass: bool,
}

/// What an errand session's id starts with.
pub const ID: &str = "errand";

/// Whether the session `id` is one an errand started for itself.
pub fn is_errand(id: &str) -> bool {
    id.strip_prefix(ID)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('-'))
}

/// The steps of a cast with `{since}` put as `at`, the last cast that
/// finished, in UTC, so an errand reads only what came after it.
pub fn since(runes: &[Rune], at: u64) -> Vec<Rune> {
    let when = crate::tasks::utc(at);
    let put = |t: &str| t.replace("{since}", &when);
    runes
        .iter()
        .map(|r| match r {
            Rune::Say(t) => Rune::Say(put(t)),
            Rune::Keys(t) => Rune::Keys(put(t)),
            Rune::Run { command, show } => Rune::Run {
                command: put(command),
                show: *show,
            },
            other => other.clone(),
        })
        .collect()
}

/// How long a cast may run when the stone does not say `"for"`.
pub const FOR_DEFAULT: u64 = 30 * 60;

/// The shortest span an errand may be cast at, so a slip of the pen does
/// not start something every second.
const SHORTEST: u64 = 60;

const DAYS: [&str; 7] = [
    "monday",
    "tuesday",
    "wednesday",
    "thursday",
    "friday",
    "saturday",
    "sunday",
];

/// A length of time as a stone writes it: a number and `s`, `m` or `h`,
/// one or several (`1h30m`). In seconds.
pub fn span(text: &str) -> Result<u64, String> {
    let t = text.trim().to_lowercase();
    let wrong = || format!("\"{text}\" is not a time like 30m, 1h or 1h30m");
    let mut total = 0u64;
    let mut number = String::new();
    for c in t.chars() {
        if c.is_ascii_digit() {
            number.push(c);
            continue;
        }
        let unit = match c {
            's' => 1,
            'm' => 60,
            'h' => 3600,
            _ => return Err(wrong()),
        };
        let n: u64 = number.parse().map_err(|_| wrong())?;
        total = total.saturating_add(n.saturating_mul(unit));
        number.clear();
    }
    if !number.is_empty() || total == 0 {
        return Err(wrong());
    }
    Ok(total)
}

/// What `"every"` says: a span (`30m`, `1h`), or `day`, `weekday` or a
/// day's name (`sunday`, `sun`) and a time on the 24 hour clock.
pub fn every(text: &str) -> Result<Every, String> {
    let words: Vec<String> = text.split_whitespace().map(str::to_lowercase).collect();
    match words.as_slice() {
        [one] => {
            let s = span(one)?;
            if s < SHORTEST {
                return Err(format!("\"{text}\" is too often: once a minute at most"));
            }
            Ok(Every::Span(s))
        }
        [day, time] => {
            let at = clock(time)?;
            Ok(match day.as_str() {
                "day" | "daily" => Every::Day(at),
                "weekday" | "weekdays" => Every::Weekday(at),
                d => {
                    let d = d
                        .strip_suffix('s')
                        .filter(|d| d.ends_with("day"))
                        .unwrap_or(d);
                    let n = DAYS
                        .iter()
                        .position(|name| *name == d || (d.len() == 3 && name.starts_with(d)))
                        .ok_or_else(|| {
                            format!("\"{day}\" is not day, weekday or the name of a day")
                        })?;
                    Every::Week(n as u8, at)
                }
            })
        }
        _ => Err(format!(
            "\"{text}\" is not like 30m, 1h, day 09:00, weekday 08:30 or sunday 12:00"
        )),
    }
}

/// A time of day, `9:00` or `09:00`, as minutes since midnight.
fn clock(text: &str) -> Result<u32, String> {
    let wrong = || format!("\"{text}\" is not a time of day like 09:00");
    let (h, m) = text.split_once(':').ok_or_else(wrong)?;
    let (h, m): (u32, u32) = (
        h.parse().map_err(|_| wrong())?,
        m.parse().map_err(|_| wrong())?,
    );
    if h > 23 || m > 59 || text.len() > 5 {
        return Err(wrong());
    }
    Ok(h * 60 + m)
}

/// When an errand cast last at `last` (Unix seconds) is due again, the
/// local clock being `offset` seconds ahead of UTC. A clock time is the
/// first such moment after `last`, so a cast missed while the app was off
/// is due at once, and only once.
pub fn due(every: Every, last: u64, offset: i64) -> u64 {
    let (minute, on): (u32, Box<dyn Fn(u8) -> bool>) = match every {
        Every::Span(s) => return last.saturating_add(s),
        Every::Day(m) => (m, Box::new(|_| true)),
        Every::Weekday(m) => (m, Box::new(|day| day < 5)),
        Every::Week(d, m) => (m, Box::new(move |day| day == d)),
    };
    let today = (last as i64 + offset).div_euclid(86_400);
    // A week and a day always holds the next one.
    (today..today + 9)
        .filter(|&day| on(weekday(day)))
        .map(|day| day * 86_400 + i64::from(minute) * 60 - offset)
        .find(|&t| t > last as i64)
        .unwrap_or(last as i64 + 86_400) as u64
}

/// The day of the week of a day counted from 1 January 1970, a Thursday,
/// 0 for Monday.
fn weekday(day: i64) -> u8 {
    (day + 3).rem_euclid(7) as u8
}

/// How a schedule reads to the human: "every 1h", "every day at 09:00".
pub fn describe(every: Every) -> String {
    let at = |m: u32| format!("{:02}:{:02}", m / 60, m % 60);
    match every {
        Every::Span(s) => format!("every {}", length(s)),
        Every::Day(m) => format!("every day at {}", at(m)),
        Every::Weekday(m) => format!("every weekday at {}", at(m)),
        Every::Week(d, m) => {
            let name = DAYS[usize::from(d.min(6))];
            let mut name = name.to_string();
            name[..1].make_ascii_uppercase();
            format!("every {name} at {}", at(m))
        }
    }
}

/// A span in the fewest words: "90s", "30m", "1h", "1h30m".
pub fn length(secs: u64) -> String {
    let (h, m, s) = (secs / 3600, secs % 3600 / 60, secs % 60);
    let mut out = String::new();
    if h > 0 {
        out.push_str(&format!("{h}h"));
    }
    if m > 0 {
        out.push_str(&format!("{m}m"));
    }
    if s > 0 || out.is_empty() {
        out.push_str(&format!("{s}s"));
    }
    out
}

/// A stone's `"every"` and `"for"`, when it has `"every"`. A stone with
/// it whose steps need a quest's session (test, review, merge) cannot be
/// an errand, which has no quest.
pub(super) fn errand_of(
    value: &serde_json::Value,
    runes: &[Rune],
) -> Result<Option<Errand>, String> {
    let Some(every_text) = value.get("every") else {
        return Ok(None);
    };
    let every_text = every_text
        .as_str()
        .ok_or("\"every\" is text like \"1h\" or \"day 09:00\"")?;
    let every = every(every_text)?;
    let most = match value.get("for") {
        None => FOR_DEFAULT,
        Some(v) => span(v.as_str().ok_or("\"for\" is text like \"30m\"")?)?,
    };
    let bypass = match value.get("mode").map(|m| m.as_str()) {
        None => false,
        Some(Some("bypass")) => true,
        Some(_) => return Err("\"mode\" is \"bypass\", or left out for the project's".into()),
    };
    if runes
        .iter()
        .any(|r| matches!(r, Rune::Test | Rune::Review | Rune::Merge))
    {
        return Err(
            "test, review and merge need a quest's session, which an errand has none of".into(),
        );
    }
    Ok(Some(Errand {
        every,
        most,
        bypass,
    }))
}

/// What the app keeps of an armed errand, by project and label.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Armed {
    /// The steps it was armed with, and whether it bypasses prompts, as
    /// [`Armed::print`] makes it. Steps or a mode that are not these
    /// disarm it.
    pub steps: u64,
    /// When its last cast began, or it was armed, in Unix seconds: when
    /// it is next due is counted from this.
    pub last: u64,
    /// When its last cast finished well, for `{since}`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished: Option<u64>,
    /// Its last cast failed: the stone shows red until one succeeds, and
    /// a failure again is not told again.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub failed: bool,
    /// The clock cast it and it has not ended yet.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub running: bool,
    /// The session the running cast has, for steps that need one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
}

impl Armed {
    /// Armed at `now` with the steps and the mode the stone has.
    pub fn new(stone: &Stone, now: u64) -> Armed {
        Armed {
            steps: Armed::print(stone),
            last: now,
            finished: None,
            failed: false,
            running: false,
            session: None,
        }
    }

    /// Whether it is armed for the steps and the mode the stone has now.
    pub fn fits(&self, stone: &Stone) -> bool {
        stone.runes().is_some() && self.steps == Armed::print(stone)
    }

    /// What arming keeps of a stone: its steps, and whether it bypasses
    /// prompts, so one turned to bypass after it was armed is not the one
    /// that was. One that does not is its steps' fingerprint alone, as
    /// arming was before modes.
    fn print(stone: &Stone) -> u64 {
        let steps = super::fingerprint(stone.runes().unwrap_or_default());
        match &stone.errand {
            Some(e) if e.bypass => steps.rotate_left(1) ^ 0x6279_7061_7373,
            _ => steps,
        }
    }

    /// What `{since}` is for its next cast: its last good finish, or, when
    /// none has finished well yet, when it was armed or last cast.
    pub fn since(&self) -> u64 {
        self.finished.unwrap_or(self.last)
    }
}

/// One of a project's armed errands as the clock looks at it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Clocked {
    pub label: String,
    pub errand: Errand,
    pub armed: Armed,
}

/// What the clock does with an errand now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tick {
    /// Cast it.
    Cast(String),
    /// It is due while the account is too near its limit: it is passed
    /// over until its next time, not held to run later.
    Skip(String),
    /// It ran past its `"for"`: stop it.
    Overdue(String),
}

/// What the clock does with one project's armed errands at `now`. One
/// runs at a time: while one does, the others wait, and it is stopped
/// once it runs past its `"for"`. Otherwise the one due longest is cast,
/// unless `full` says the account's fullest limit is at 90 % or more:
/// then every due one is skipped, or with `Full::Wait` they all stay due
/// and the one due longest is cast once the limit has reset.
pub fn tick(errands: &[Clocked], now: u64, offset: i64, full: Option<Full>) -> Vec<Tick> {
    let running: Vec<&Clocked> = errands.iter().filter(|e| e.armed.running).collect();
    if !running.is_empty() {
        return running
            .into_iter()
            .filter(|e| now >= e.armed.last.saturating_add(e.errand.most))
            .map(|e| Tick::Overdue(e.label.clone()))
            .collect();
    }
    let mut due: Vec<(u64, &Clocked)> = errands
        .iter()
        .map(|e| (due(e.errand.every, e.armed.last, offset), e))
        .filter(|(at, _)| *at <= now)
        .collect();
    due.sort_by_key(|(at, _)| *at);
    match full {
        None => {}
        Some(Full::Wait) => return Vec::new(),
        Some(Full::Skip) => {
            return due
                .into_iter()
                .map(|(_, e)| Tick::Skip(e.label.clone()))
                .collect();
        }
    }
    due.first()
        .map(|(_, e)| Tick::Cast(e.label.clone()))
        .into_iter()
        .collect()
}

/// What the human reads before arming an errand: what it is for, when it
/// runs and for how long at most, on what, and every step.
pub fn arm_text(stone: &Stone, on: &str) -> String {
    let mut out = String::new();
    if !stone.about.is_empty() {
        out.push_str(&stone.about);
        out.push_str("\n\n");
    }
    if let Some(e) = &stone.errand {
        out.push_str(&format!(
            "Runs {} without asking, stopped after {}. A cast it misses while Horadric is off runs once when it starts.\n\n",
            describe(e.every),
            length(e.most)
        ));
        if e.bypass && !stone.sessionless() {
            out.push_str(
                "It skips every permission prompt: its session runs any command and edits any file without asking.\n\n",
            );
        }
    }
    out.push_str(&super::stone::ask_text(stone, on, false));
    out.push_str("\n\nIf its steps or its mode change it is disarmed until you arm it again.");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-10-05 00:00 UTC, a Monday.
    const MONDAY: u64 = 1_791_158_400;
    const HOUR: u64 = 3600;

    #[test]
    fn a_span_reads_seconds_minutes_and_hours() {
        assert_eq!(span("30m"), Ok(1800));
        assert_eq!(span("1h"), Ok(3600));
        assert_eq!(span("1h30m"), Ok(5400));
        assert_eq!(span(" 90S "), Ok(90));
        for bad in ["", "m", "30", "1x", "0m", "1.5h"] {
            assert!(span(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn every_reads_a_span_a_day_a_weekday_and_a_day_name() {
        assert_eq!(every("30m"), Ok(Every::Span(1800)));
        assert_eq!(every("1h"), Ok(Every::Span(3600)));
        assert_eq!(every("day 09:00"), Ok(Every::Day(540)));
        assert_eq!(every("Weekday 8:30"), Ok(Every::Weekday(510)));
        assert_eq!(every("sunday 12:00"), Ok(Every::Week(6, 720)));
        assert_eq!(every("Sundays 12:00"), Ok(Every::Week(6, 720)));
        assert_eq!(every("tue 07:05"), Ok(Every::Week(1, 425)));
        assert!(every("30s").unwrap_err().contains("once a minute"));
        assert!(every("someday 09:00").is_err());
        assert!(every("day 24:00").is_err());
        assert!(every("day 9").is_err());
        assert!(every("day 09:00 sharp").is_err());
    }

    #[test]
    fn a_span_is_due_that_long_after_the_last_cast() {
        assert_eq!(due(Every::Span(3600), 1000, 7200), 4600);
    }

    #[test]
    fn a_day_is_due_at_the_next_local_time_after_the_last_cast() {
        // Two hours ahead of UTC: 09:00 local is 07:00 UTC.
        let nine = MONDAY + 7 * HOUR;
        assert_eq!(due(Every::Day(540), MONDAY, 2 * HOUR as i64), nine);
        // Cast at nine, the next is tomorrow at nine.
        assert_eq!(
            due(Every::Day(540), nine, 2 * HOUR as i64),
            nine + 24 * HOUR
        );
        // Behind UTC the local day starts later.
        assert_eq!(
            due(Every::Day(540), MONDAY, -5 * HOUR as i64),
            MONDAY + 14 * HOUR
        );
    }

    #[test]
    fn a_weekday_skips_the_weekend_and_a_day_name_waits_for_its_day() {
        let friday_ten = MONDAY + 4 * 24 * HOUR + 10 * HOUR;
        let monday_next = MONDAY + 7 * 24 * HOUR + 8 * HOUR;
        assert_eq!(due(Every::Weekday(480), friday_ten, 0), monday_next);
        assert_eq!(
            due(Every::Week(6, 720), MONDAY, 0),
            MONDAY + 6 * 24 * HOUR + 12 * HOUR
        );
        let sunday_noon = MONDAY + 6 * 24 * HOUR + 12 * HOUR;
        assert_eq!(
            due(Every::Week(6, 720), sunday_noon, 0),
            sunday_noon + 7 * 24 * HOUR
        );
    }

    #[test]
    fn a_cast_missed_for_days_is_due_once_and_then_from_now() {
        let last = MONDAY + 9 * HOUR;
        let now = last + 3 * 24 * HOUR;
        let e = clocked("Nightly", Every::Day(540), last);
        assert_eq!(
            tick(std::slice::from_ref(&e), now, 0, None),
            [Tick::Cast("Nightly".into())]
        );
        // Cast now, it is due tomorrow at nine, not three more times.
        assert_eq!(due(Every::Day(540), now + 60, 0), now + 24 * HOUR);
    }

    fn clocked(label: &str, every: Every, last: u64) -> Clocked {
        Clocked {
            label: label.into(),
            errand: Errand {
                every,
                most: FOR_DEFAULT,
                bypass: false,
            },
            armed: Armed {
                steps: 0,
                last,
                finished: None,
                failed: false,
                running: false,
                session: None,
            },
        }
    }

    #[test]
    fn one_runs_at_a_time_the_one_due_longest_first() {
        let a = clocked("A", Every::Span(600), 1000);
        let b = clocked("B", Every::Span(600), 500);
        let c = clocked("C", Every::Span(6000), 1000);
        assert_eq!(
            tick(&[a.clone(), b.clone(), c.clone()], 2000, 0, None),
            [Tick::Cast("B".into())]
        );
        assert_eq!(tick(&[a.clone(), c.clone()], 1500, 0, None), []);
        let mut running = b;
        running.armed.running = true;
        running.armed.last = 1900;
        assert_eq!(tick(&[a, running, c], 2000, 0, None), []);
    }

    #[test]
    fn near_the_limit_every_due_errand_is_skipped() {
        let a = clocked("A", Every::Span(600), 1000);
        let b = clocked("B", Every::Span(600), 500);
        let c = clocked("C", Every::Span(6000), 1000);
        assert_eq!(
            tick(&[a, b, c], 2000, 0, Some(Full::Skip)),
            [Tick::Skip("B".into()), Tick::Skip("A".into())]
        );
    }

    #[test]
    fn while_warriv_drives_a_due_errand_waits_out_the_hold() {
        let a = clocked("A", Every::Span(600), 1000);
        let b = clocked("B", Every::Span(600), 500);
        assert_eq!(tick(&[a.clone(), b.clone()], 2000, 0, Some(Full::Wait)), []);
        // Its clock did not move, so the reset finds it due at once.
        assert_eq!(tick(&[a, b], 2100, 0, None), [Tick::Cast("B".into())]);
    }

    #[test]
    fn a_cast_running_past_its_for_is_overdue() {
        let mut e = clocked("A", Every::Span(600), 1000);
        e.armed.running = true;
        assert_eq!(
            tick(std::slice::from_ref(&e), 1000 + FOR_DEFAULT - 1, 0, None),
            []
        );
        assert_eq!(
            tick(&[e], 1000 + FOR_DEFAULT, 0, Some(Full::Skip)),
            [Tick::Overdue("A".into())]
        );
    }

    #[test]
    fn a_schedule_reads_in_words() {
        assert_eq!(describe(Every::Span(3600)), "every 1h");
        assert_eq!(describe(Every::Span(5400)), "every 1h30m");
        assert_eq!(describe(Every::Day(540)), "every day at 09:00");
        assert_eq!(describe(Every::Weekday(510)), "every weekday at 08:30");
        assert_eq!(describe(Every::Week(6, 720)), "every Sunday at 12:00");
        assert_eq!(length(90), "1m30s");
    }

    fn stone(steps: Vec<Rune>, bypass: bool) -> Stone {
        Stone {
            label: "Errand".into(),
            steps: Ok(steps),
            source: super::super::Source::Project,
            about: String::new(),
            errand: Some(Errand {
                every: Every::Span(3600),
                most: FOR_DEFAULT,
                bypass,
            }),
        }
    }

    #[test]
    fn arming_follows_the_steps_and_the_mode() {
        let clean = || Rune::Run {
            command: "cargo clean".into(),
            show: false,
        };
        let armed = Armed::new(&stone(vec![clean()], false), 10);
        assert!(armed.fits(&stone(vec![clean()], false)));
        // Armed before modes, a stone keeps its arming.
        assert_eq!(armed.steps, super::super::fingerprint(&[clean()]));
        let doc = Rune::Run {
            command: "cargo clean --doc".into(),
            show: false,
        };
        assert!(!armed.fits(&stone(vec![doc], false)));
        // Turned to bypass after arming, it is not what was armed.
        assert!(!armed.fits(&stone(vec![clean()], true)));
        let bypassing = Armed::new(&stone(vec![clean()], true), 10);
        assert!(bypassing.fits(&stone(vec![clean()], true)));
        assert!(!bypassing.fits(&stone(vec![clean()], false)));
    }

    #[test]
    fn a_mode_reads_bypass_or_nothing() {
        let steps = [Rune::Say("Look".into())];
        let of = |v: &str| errand_of(&serde_json::from_str(v).unwrap(), &steps);
        assert!(!of(r#"{"every":"1h"}"#).unwrap().unwrap().bypass);
        assert!(
            of(r#"{"every":"1h","mode":"bypass"}"#)
                .unwrap()
                .unwrap()
                .bypass
        );
        assert!(of(r#"{"every":"1h","mode":"auto"}"#)
            .unwrap_err()
            .contains("bypass"));
    }

    #[test]
    fn arming_a_bypassing_session_errand_says_so() {
        let say = vec![Rune::Say("Look".into())];
        let skips = "skips every permission prompt";
        assert!(arm_text(&stone(say.clone(), true), "x").contains(skips));
        assert!(!arm_text(&stone(say, false), "x").contains(skips));
    }

    #[test]
    fn an_errand_session_is_known_by_its_id() {
        assert!(is_errand("errand"));
        assert!(is_errand("errand-51234"));
        assert!(!is_errand("errands-1"));
        assert!(!is_errand("fix-errand-1"));
    }

    #[test]
    fn since_is_put_in_every_step_that_has_it() {
        let runes = [
            Rune::Say("Read mail since {since}.".into()),
            Rune::Run {
                command: "log --since {since}".into(),
                show: true,
            },
            Rune::Keys("{Enter}".into()),
        ];
        assert_eq!(
            since(&runes, MONDAY + 9 * HOUR),
            [
                Rune::Say("Read mail since 2026-10-05T09:00Z.".into()),
                Rune::Run {
                    command: "log --since 2026-10-05T09:00Z".into(),
                    show: true,
                },
                Rune::Keys("{Enter}".into()),
            ]
        );
    }

    #[test]
    fn since_is_the_last_good_finish_or_the_last_start() {
        let mut a = clocked("A", Every::Span(600), 1000).armed;
        assert_eq!(a.since(), 1000);
        a.finished = Some(900);
        a.last = 2000;
        assert_eq!(a.since(), 900);
    }
}
