//! Runewords: runes cast on one session one after another, a turn each,
//! the way runes in the right order make a runeword. "Test, review, merge"
//! given to a session tells it to test, has a reviewer read its work and
//! tells it to answer the review, then merges its branch.
//!
//! Runes and the cube's recipes are one system. A rune is an action on a
//! session; a recipe casts one on what the cube holds at once, a runeword
//! casts several on one session over time. Review and merge are the same
//! actions in both.
//!
//! Which step comes next is decided here from the session's phase, and
//! the reviewer's while one reads, so the app only carries it out.

use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::cube::{self, Subject};
use crate::session::Phase;
use crate::tasks::one_line;

mod stone;
pub use stone::{carve, name, smith_prompt, Carving, Stroke, EDGE_POINTS, RUNES};

/// One action a runeword casts on its session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Rune {
    /// The session runs the tests, fixes what fails and commits.
    Test,
    /// A new session reviews its work, then the session answers the review.
    Review,
    /// Its branch is merged into what the main tree has checked out.
    Merge,
    /// Anything else in a config's runeword: told to the session as it is.
    Say(String),
}

impl Rune {
    /// A word from a config: the three runes by name, anything else said
    /// to the session. None for an empty word.
    pub fn parse(word: &str) -> Option<Rune> {
        let word = one_line(word);
        Some(match word.to_lowercase().as_str() {
            "" => return None,
            "test" => Rune::Test,
            "review" => Rune::Review,
            "merge" => Rune::Merge,
            _ => Rune::Say(word),
        })
    }

    /// Its word on a tile and in a runeword's name.
    pub fn word(&self) -> String {
        match self {
            Rune::Test => "test".into(),
            Rune::Review => "review".into(),
            Rune::Merge => "merge".into(),
            Rune::Say(text) => cut(text, WORD_CHARS),
        }
    }
}

/// How long a said rune's word may be on a tile.
const WORD_CHARS: usize = 24;

fn cut(text: &str, most: usize) -> String {
    if text.chars().count() <= most {
        return text.to_string();
    }
    let cut: String = text.chars().take(most).collect();
    format!("{}...", cut.trim_end())
}

/// Where a runeword is in its current rune.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Step {
    /// Waiting for the session to be at rest, to cast the rune.
    Due,
    /// Told the session at `at`; the rune is done when that turn ends.
    /// `heard` is when its prompt went in, so a later one is the human's.
    Told {
        at: SystemTime,
        #[serde(default)]
        heard: Option<SystemTime>,
    },
    /// A reviewer started at `at` writes its review to `file`.
    Reviewing {
        reviewer: String,
        file: String,
        at: SystemTime,
    },
}

/// A runeword given to a session, and how far it has got.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Runeword {
    pub name: String,
    pub runes: Vec<Rune>,
    /// The rune being cast.
    pub at: usize,
    pub step: Step,
}

impl Runeword {
    pub fn new(name: &str, runes: Vec<Rune>) -> Runeword {
        Runeword {
            name: name.to_string(),
            runes,
            at: 0,
            step: Step::Due,
        }
    }

    /// What its tile says: the rune being cast and how far along it is.
    pub fn progress(&self) -> String {
        match self.runes.get(self.at) {
            Some(r) => format!("{} {}/{}", r.word(), self.at + 1, self.runes.len()),
            None => "done".into(),
        }
    }

    /// The same where there is no room for the rune's word.
    pub fn progress_short(&self) -> String {
        match self.runes.get(self.at) {
            Some(_) => format!("rune {}/{}", self.at + 1, self.runes.len()),
            None => "done".into(),
        }
    }

    /// On to the next rune.
    pub fn advance(&mut self) {
        self.at += 1;
        self.step = Step::Due;
    }
}

/// A runeword's name made from its runes: "Test, review, merge".
pub fn named(runes: &[Rune]) -> String {
    let words: Vec<String> = runes.iter().map(Rune::word).collect();
    let mut name = words.join(", ");
    if let Some(first) = name.get(..1) {
        name = first.to_uppercase() + &name[1..];
    }
    name
}

/// Runewords by name, as a project offers them.
pub type Offered = Vec<(String, Vec<Rune>)>;

/// The runewords every project offers.
const BUILT_IN: [&[Rune]; 3] = [
    &[Rune::Test, Rune::Merge],
    &[Rune::Test, Rune::Review, Rune::Merge],
    &[Rune::Review, Rune::Merge],
];

/// The runewords a project offers: its config's own first, then the built
/// in ones it has not already listed.
///
/// ```json
/// { "runewords": { "Ship": ["test", "Update the changelog", "merge"] } }
/// ```
pub fn offered(config: &str) -> Offered {
    let v = serde_json::from_str::<Value>(config).unwrap_or(Value::Null);
    let mut out: Offered = v
        .get("runewords")
        .and_then(Value::as_object)
        .map(|m| {
            m.iter()
                .filter_map(|(name, runes)| {
                    let runes: Vec<Rune> = runes
                        .as_array()?
                        .iter()
                        .filter_map(Value::as_str)
                        .filter_map(Rune::parse)
                        .collect();
                    let name = one_line(name);
                    (!runes.is_empty() && !name.is_empty()).then_some((name, runes))
                })
                .collect()
        })
        .unwrap_or_default();
    for runes in BUILT_IN {
        if !out.iter().any(|(_, r)| r == runes) {
            out.push((named(runes), runes.to_vec()));
        }
    }
    out
}

/// A session as the next step looks at it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seen {
    pub phase: Phase,
    /// When its phase began.
    pub since: SystemTime,
    /// When its last prompt went in, if one did since the app started.
    pub prompted: Option<SystemTime>,
}

impl Seen {
    /// A turn that ended after `at`.
    fn finished_after(&self, at: SystemTime) -> bool {
        self.phase == Phase::Done && self.since > at
    }
}

/// What the app does next for a runeword.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Act {
    /// Nothing yet: the session or its reviewer is not there yet.
    Wait,
    /// Cast this rune now.
    Cast(Rune),
    /// The told prompt went in at this time.
    Heard(SystemTime),
    /// The rune is done: on to the next.
    Next,
    /// The review is written: tell the session to answer it.
    Answer,
    /// Every rune is cast.
    Complete,
    /// It cannot go on, and why.
    Stop(String),
}

/// The next step for `word` on a session seen as `session`, with its
/// reviewer seen as `reviewer` while one reads (None when it is gone).
pub fn act(word: &Runeword, session: &Seen, reviewer: Option<&Seen>) -> Act {
    match session.phase {
        Phase::Ended => return Act::Stop("the session ended".into()),
        Phase::Paused => return Act::Wait,
        _ => {}
    }
    match &word.step {
        Step::Due => match word.runes.get(word.at) {
            None => Act::Complete,
            // At rest, and not waiting on the human: telling it now would
            // land in the middle of what it is doing.
            Some(rune) if matches!(session.phase, Phase::Done | Phase::Idle) => {
                Act::Cast(rune.clone())
            }
            Some(_) => Act::Wait,
        },
        // A turn the human cuts short (Esc, or No to a permission) sends
        // no Stop, so the next turn to end would be one the human asked
        // for. A prompt after the told one says the human took over, and
        // the runeword stops rather than count that turn as the rune or
        // tell the rune again over what the human is doing.
        Step::Told { heard: Some(h), .. } if session.prompted.is_some_and(|p| p > *h) => {
            Act::Stop("you took over from it".into())
        }
        Step::Told { at, .. } if session.finished_after(*at) => Act::Next,
        Step::Told { at, heard: None } => match session.prompted {
            Some(p) if p > *at => Act::Heard(p),
            _ => Act::Wait,
        },
        Step::Told { .. } => Act::Wait,
        Step::Reviewing { at, .. } => match reviewer {
            None => Act::Stop("the reviewer ended before it wrote its review".into()),
            Some(r) if r.phase == Phase::Ended => {
                Act::Stop("the reviewer ended before it wrote its review".into())
            }
            Some(r) if r.finished_after(*at) => Act::Answer,
            Some(_) => Act::Wait,
        },
    }
}

/// What the test rune tells the session.
pub fn test_prompt() -> String {
    "Run this project's tests. Fix whatever fails, and commit once they all pass. \
     If there are no tests, say so and change nothing."
        .into()
}

/// What the reviewer of one session is asked: to read its diff and write
/// the review to `file`.
pub fn review_prompt(subject: &Subject, base: &str, file: &str) -> String {
    format!(
        "Review the work of a session. {} Read the diff. Say what it does, what is wrong \
         or risky with file and line, and what is missing. Write your review to \"{file}\" \
         and change no other file.",
        cube::diff_of(subject, base)
    )
}

/// What the session is told once its review is written.
pub fn answer_prompt(file: &str) -> String {
    format!(
        "A reviewer read your work and wrote its review to \"{file}\". Read it, fix what \
         you agree with, and commit. Say which points you left alone and why."
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::WaitReason;
    use std::time::Duration;

    fn t(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn seen(phase: Phase, since: u64) -> Seen {
        Seen {
            phase,
            since: t(since),
            prompted: None,
        }
    }

    fn word(runes: &[Rune]) -> Runeword {
        Runeword::new(&named(runes), runes.to_vec())
    }

    #[test]
    fn words_parse_to_runes_and_the_rest_is_said() {
        assert_eq!(Rune::parse(" Test "), Some(Rune::Test));
        assert_eq!(Rune::parse("REVIEW"), Some(Rune::Review));
        assert_eq!(Rune::parse("merge"), Some(Rune::Merge));
        assert_eq!(Rune::parse("   "), None);
        assert_eq!(
            Rune::parse("Update\nthe changelog"),
            Some(Rune::Say("Update the changelog".into()))
        );
    }

    #[test]
    fn a_name_and_progress_come_from_the_runes() {
        let mut w = word(&[Rune::Test, Rune::Review, Rune::Merge]);
        assert_eq!(w.name, "Test, review, merge");
        assert_eq!(w.progress(), "test 1/3");
        w.advance();
        assert_eq!(w.progress(), "review 2/3");
        assert_eq!(w.progress_short(), "rune 2/3");
        assert_eq!(w.step, Step::Due);
        w.advance();
        w.advance();
        assert_eq!(w.progress(), "done");
        let said = Rune::Say("Write the release notes for this version".into());
        assert_eq!(said.word(), "Write the release notes...");
        assert_eq!(
            named(&[said, Rune::Merge]),
            "Write the release notes..., merge"
        );
    }

    #[test]
    fn a_project_offers_its_own_runewords_before_the_built_in_ones() {
        let config = r#"{ "runewords": {
            "Ship": ["test", "Update the changelog", "merge"],
            "Empty": [],
            "Again": ["test", "merge"]
        } }"#;
        let offered = offered(config);
        let names: Vec<&str> = offered.iter().map(|(n, _)| n.as_str()).collect();
        // Its own "Again" is the built in test and merge, which is not
        // offered twice.
        assert_eq!(
            names,
            ["Again", "Ship", "Test, review, merge", "Review, merge"]
        );
        assert_eq!(
            offered[1].1,
            [
                Rune::Test,
                Rune::Say("Update the changelog".into()),
                Rune::Merge
            ]
        );
        let plain = super::offered("");
        assert_eq!(plain.len(), 3);
        assert_eq!(plain[0].0, "Test, merge");
    }

    #[test]
    fn a_rune_is_cast_only_on_a_session_at_rest() {
        let w = word(&[Rune::Test, Rune::Merge]);
        assert_eq!(act(&w, &seen(Phase::Done, 5), None), Act::Cast(Rune::Test));
        assert_eq!(act(&w, &seen(Phase::Idle, 5), None), Act::Cast(Rune::Test));
        assert_eq!(act(&w, &seen(Phase::Working, 5), None), Act::Wait);
        let asking = Phase::Waiting(WaitReason::Permission);
        assert_eq!(act(&w, &seen(asking, 5), None), Act::Wait);
        assert_eq!(act(&w, &seen(Phase::Paused, 5), None), Act::Wait);
    }

    #[test]
    fn a_told_rune_is_done_when_a_later_turn_ends() {
        let mut w = word(&[Rune::Test, Rune::Merge]);
        w.step = Step::Told {
            at: t(10),
            heard: None,
        };
        // The turn it was told in has not ended yet.
        assert_eq!(act(&w, &seen(Phase::Done, 5), None), Act::Wait);
        assert_eq!(act(&w, &seen(Phase::Working, 11), None), Act::Wait);
        assert_eq!(act(&w, &seen(Phase::Done, 20), None), Act::Next);
    }

    #[test]
    fn a_prompt_after_the_told_one_stops_the_runeword() {
        let mut w = word(&[Rune::Test, Rune::Merge]);
        w.step = Step::Told {
            at: t(10),
            heard: None,
        };
        let working = |prompted: u64| Seen {
            prompted: Some(t(prompted)),
            ..seen(Phase::Working, prompted)
        };
        // A prompt from before the telling is not the told one.
        assert_eq!(act(&w, &working(8), None), Act::Wait);
        assert_eq!(act(&w, &working(11), None), Act::Heard(t(11)));
        w.step = Step::Told {
            at: t(10),
            heard: Some(t(11)),
        };
        assert_eq!(act(&w, &working(11), None), Act::Wait);
        // The human cut the turn short and asked for something else.
        assert_eq!(
            act(&w, &working(40), None),
            Act::Stop("you took over from it".into())
        );
        // Even once that turn has ended.
        let done = Seen {
            prompted: Some(t(40)),
            ..seen(Phase::Done, 50)
        };
        assert!(matches!(act(&w, &done, None), Act::Stop(_)));
        // The told turn ending is the rune done, heard or not.
        let own = Seen {
            prompted: Some(t(11)),
            ..seen(Phase::Done, 30)
        };
        assert_eq!(act(&w, &own, None), Act::Next);
    }

    #[test]
    fn a_told_step_saved_before_heard_existed_still_reads() {
        let w: Step = serde_json::from_str(
            r#"{"told":{"at":{"secs_since_epoch":10,"nanos_since_epoch":0}}}"#,
        )
        .unwrap();
        assert_eq!(
            w,
            Step::Told {
                at: t(10),
                heard: None
            }
        );
    }

    #[test]
    fn a_review_is_answered_once_the_reviewer_finished() {
        let mut w = word(&[Rune::Review]);
        w.step = Step::Reviewing {
            reviewer: "r".into(),
            file: "review.md".into(),
            at: t(10),
        };
        let s = seen(Phase::Done, 5);
        assert_eq!(act(&w, &s, Some(&seen(Phase::Idle, 10))), Act::Wait);
        assert_eq!(act(&w, &s, Some(&seen(Phase::Working, 12))), Act::Wait);
        assert_eq!(act(&w, &s, Some(&seen(Phase::Done, 30))), Act::Answer);
        assert!(matches!(act(&w, &s, None), Act::Stop(_)));
        assert!(matches!(
            act(&w, &s, Some(&seen(Phase::Ended, 30))),
            Act::Stop(_)
        ));
    }

    #[test]
    fn it_completes_after_the_last_rune_and_stops_with_its_session() {
        let mut w = word(&[Rune::Merge]);
        w.advance();
        assert_eq!(act(&w, &seen(Phase::Done, 5), None), Act::Complete);
        assert_eq!(
            act(&w, &seen(Phase::Ended, 5), None),
            Act::Stop("the session ended".into())
        );
    }

    #[test]
    fn it_survives_a_save() {
        let mut w = word(&[Rune::Test, Rune::Say("Tidy up".into())]);
        w.step = Step::Reviewing {
            reviewer: "r".into(),
            file: "C:/r.md".into(),
            at: t(10),
        };
        let json = serde_json::to_string(&w).unwrap();
        assert_eq!(serde_json::from_str::<Runeword>(&json).unwrap(), w);
    }

    #[test]
    fn the_prompts_name_the_diff_and_the_review_file() {
        let s = Subject {
            name: "login".into(),
            dir: "C:/p.login".into(),
            branch: Some("login".into()),
        };
        let p = review_prompt(&s, "main", "C:/reviews/login.md");
        assert!(p.contains("diff main...login"), "{p}");
        assert!(p.contains("\"C:/reviews/login.md\""));
        assert!(!p.contains('\n'));
        assert!(answer_prompt("C:/r.md").contains("\"C:/r.md\""));
        assert!(!test_prompt().contains('\n'));
    }
}
