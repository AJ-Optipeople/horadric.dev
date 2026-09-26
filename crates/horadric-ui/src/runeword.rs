//! The app's side of runewords: giving one to a session, and casting its
//! runes one turn after another. What comes next is decided by
//! `horadric_core::runeword`, pure and tested; this types to the session,
//! starts its reviewer and merges its branch, the cube's own actions.

use std::time::{Instant, SystemTime};

use horadric_core::runeword::{self, Act, Offered, Rune, Runeword, Seen, Step};
use horadric_core::tasks::one_line;
use horadric_core::Session;

use super::transmute::subject;
use crate::app::App;
use crate::store;
use crate::toast::Kind;
use crate::window::project_key;

fn seen(s: &Session) -> Seen {
    Seen {
        phase: s.phase.clone(),
        since: s.since,
        prompted: s.prompted_at,
    }
}

impl App {
    /// The runewords a session can be given, and the one it has. None for
    /// what cannot take one: a plain terminal or a background session.
    pub(in crate::app) fn runewords_of(&self, id: &str) -> Option<(Offered, Option<Runeword>)> {
        let (key, word) = {
            let r = self.shared.registry.lock().ok()?;
            let s = r.get(id).filter(|s| !s.shell && s.background.is_none())?;
            (project_key(s), s.runeword.clone())
        };
        let offered = self
            .project_dir(&key)
            .map(|d| horadric_hooks::tasks::runewords(&d))
            .unwrap_or_else(|| runeword::offered(""));
        Some((offered, word))
    }

    /// Gives a session a runeword. Its first rune is cast at once if the
    /// session is at rest, or when its turn ends.
    pub(in crate::app) fn give_runeword(&mut self, id: &str, name: &str, runes: Vec<Rune>) {
        self.set_runeword(id, Some(Runeword::new(name, runes)));
        self.tick_runewords();
    }

    /// Takes a session's runeword away where it stands. A reviewer it
    /// started keeps going, on its own.
    pub(in crate::app) fn stop_runeword(&mut self, id: &str) {
        self.set_runeword(id, None);
    }

    fn set_runeword(&mut self, id: &str, word: Option<Runeword>) {
        if let Ok(mut r) = self.shared.registry.lock() {
            if let Some(s) = r.get_mut(id) {
                s.runeword = word;
            }
        }
        self.save();
        self.redraw_tiles();
    }

    fn change_runeword(&mut self, id: &str, change: impl FnOnce(&mut Runeword)) {
        if let Ok(mut r) = self.shared.registry.lock() {
            if let Some(w) = r.get_mut(id).and_then(|s| s.runeword.as_mut()) {
                change(w);
            }
        }
        self.save();
        self.redraw_tiles();
    }

    /// Once a second, and when one is given: the next step of every
    /// runeword whose session or reviewer moved on.
    pub(in crate::app) fn tick_runewords(&mut self) {
        let words: Vec<(Session, Runeword, Act)> = {
            let Ok(r) = self.shared.registry.lock() else {
                return;
            };
            r.all()
                .filter_map(|s| {
                    let w = s.runeword.clone()?;
                    let reviewer = match &w.step {
                        Step::Reviewing { reviewer, .. } => r.get(reviewer).map(seen),
                        _ => None,
                    };
                    let act = runeword::act(&w, &seen(s), reviewer.as_ref());
                    (act != Act::Wait).then(|| (s.clone(), w, act))
                })
                .collect()
        };
        for (s, w, act) in words {
            match act {
                Act::Wait => {}
                Act::Cast(rune) => self.cast(&s, &w, rune),
                Act::Heard(p) => self.change_runeword(&s.id, |w| {
                    if let Step::Told { heard, .. } = &mut w.step {
                        *heard = Some(p);
                    }
                }),
                Act::Next => self.change_runeword(&s.id, Runeword::advance),
                Act::Answer => {
                    let Step::Reviewing { reviewer, file, .. } = &w.step else {
                        continue;
                    };
                    if self.tell(&s.id, &runeword::answer_prompt(file)) {
                        let reviewer = reviewer.clone();
                        self.change_runeword(&s.id, |w| {
                            w.step = Step::Told {
                                at: SystemTime::now(),
                                heard: None,
                            }
                        });
                        // Its review is in the file, which is all it was for.
                        self.end(&reviewer);
                    }
                }
                Act::Complete => {
                    self.set_runeword(&s.id, None);
                    self.toasts.show(
                        Kind::Done,
                        &format!("{} is complete", w.name),
                        &format!("Every rune is cast on {}.", s.label()),
                    );
                }
                Act::Stop(why) => {
                    self.set_runeword(&s.id, None);
                    self.stopped(&s, &w, &why);
                }
            }
        }
    }

    /// Casts the rune a runeword is at on its session.
    fn cast(&mut self, s: &Session, w: &Runeword, rune: Rune) {
        let told = |app: &mut App| {
            app.change_runeword(&s.id, |w| {
                w.step = Step::Told {
                    at: SystemTime::now(),
                    heard: None,
                }
            })
        };
        match rune {
            Rune::Test => {
                if self.tell(&s.id, &runeword::test_prompt()) {
                    told(self);
                }
            }
            Rune::Say(text) => {
                if self.tell(&s.id, &text) {
                    told(self);
                }
            }
            Rune::Review => match self.start_review(s, w.at) {
                Some((reviewer, file)) => self.change_runeword(&s.id, |w| {
                    w.step = Step::Reviewing {
                        reviewer,
                        file,
                        at: SystemTime::now(),
                    }
                }),
                None => {
                    self.set_runeword(&s.id, None);
                    self.stopped(s, w, "the reviewer could not start");
                }
            },
            // Without a branch of its own its commits are already in the
            // main tree, so there is nothing to merge.
            Rune::Merge => match self.merge_session(s) {
                Some(false) => {
                    self.set_runeword(&s.id, None);
                    self.stopped(s, w, "the merge failed");
                }
                _ => self.change_runeword(&s.id, Runeword::advance),
            },
        }
    }

    /// Starts a reviewer of one session's work that writes its review to
    /// a file of its own. The reviewer's id and the file.
    fn start_review(&mut self, s: &Session, at: usize) -> Option<(String, String)> {
        let dir = store::dir()?.join("reviews");
        std::fs::create_dir_all(&dir).ok()?;
        let path = dir.join(format!("{}-{}.md", s.id, at + 1));
        let _ = std::fs::remove_file(&path);
        // Forward slashes read as a path in every shell the agent may use.
        let file = path.to_string_lossy().replace('\\', "/");
        let main = self.main_tree(s);
        let base = crate::worktree::checked_out(&main).unwrap_or_else(|| "main".into());
        let prompt = runeword::review_prompt(&subject(s), &base, &file);
        let name = format!("Review: {}", s.label());
        let reviewer = self.start_reviewer(&name, main, prompt)?;
        Some((reviewer, file))
    }

    /// Types one line to a live session, its Enter a moment after so the
    /// agent takes it as typed. False when there is no console to type to.
    fn tell(&mut self, id: &str, text: &str) -> bool {
        let Some(c) = self.consoles.get(id).filter(|c| c.exit_code().is_none()) else {
            return false;
        };
        c.write(one_line(text).into_bytes());
        self.tasks.enters.push((id.to_string(), Instant::now()));
        true
    }

    fn stopped(&mut self, s: &Session, w: &Runeword, why: &str) {
        self.toasts.show(
            Kind::Failed,
            &format!("{} stopped", w.name),
            &format!("{} at {}: {why}.", s.label(), w.progress()),
        );
    }

    fn redraw_tiles(&self) {
        for c in &self.clusters {
            c.invalidate();
        }
    }
}
