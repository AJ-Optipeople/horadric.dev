//! The app's side of the Horadric Cube: which sessions it holds, the
//! window that shows them, and the recipes run on them. Which recipe the
//! contents make is `horadric_core::cube`, pure and tested; this starts
//! the reviewer, merges the branch or closes the sessions.
//!
//! What the cube holds is not saved. It is a hand of tiles on the way to
//! a recipe, and a restart puts them back where they stand anyway.

use std::path::PathBuf;
use std::rc::Rc;

use horadric_core::cube::{self, Ingredient, Recipe, Subject};
use horadric_core::journal::{Entry, What};
use horadric_core::Session;

use super::Merge;
use crate::app::{unix_now, App, Run};
use crate::columns;
use crate::cube::{Contents, CubeWindow};
use crate::render::StashLook;
use crate::store;
use crate::theme;
use crate::toast::Kind;
use crate::window::{project_key, project_name};

/// Whether a session can go in the cube: an agent's, never a plain
/// terminal or a background session, which no recipe can act on.
fn fits(s: &Session) -> bool {
    !s.shell && s.background.is_none()
}

fn ingredient(s: &Session) -> Ingredient {
    Ingredient {
        name: s.label().to_string(),
        phase: s.phase.clone(),
        changed: s.loot.changed
            || s.loot.committed
            || s.diff.as_ref().is_some_and(|d| !d.is_empty()),
        branch: s.worktree.as_ref().map(|w| w.branch.clone()),
    }
}

/// A session as a reviewer is told of it.
pub(in crate::app) fn subject(s: &Session) -> Subject {
    Subject {
        name: s.label().to_string(),
        dir: s
            .worktree
            .as_ref()
            .map_or_else(|| s.cwd.clone(), |w| w.path.clone()),
        branch: s.worktree.as_ref().map(|w| w.branch.clone()),
    }
}

/// A session as the cube's slots and its swirl show it.
pub(in crate::app) fn look(s: &Session) -> StashLook {
    let key = project_key(s);
    StashLook {
        name: s.label().to_string(),
        project: project_name(&key),
        accent: theme::accent(&key),
        ink: theme::rarity_color(s.rarity()),
        last: s.last_line.clone(),
        branch: s.worktree.as_ref().map(|w| w.branch.clone()),
    }
}

impl App {
    /// Makes the cube window match what the cube holds: there while it is
    /// switched on and an agent session has a tile, the cube holds anything or a transmute is
    /// still playing, under the stash
    /// or the usage window when it first comes. True when it came or went,
    /// so the columns are laid out again.
    pub(in crate::app) fn sync_cube(&mut self) -> bool {
        let (contents, wanted) = {
            let Ok(r) = self.shared.registry.lock() else {
                return false;
            };
            let on = self.cube_on;
            self.cube.retain(|id| on && r.get(id).is_some_and(fits));
            let held: Vec<&Session> = self.cube.iter().filter_map(|id| r.get(id)).collect();
            let ingredients: Vec<Ingredient> = held.iter().map(|s| ingredient(s)).collect();
            let contents = Contents {
                items: held.iter().map(|s| (s.id.clone(), look(s))).collect(),
                main: self.cube_main,
                recipe: cube::recipe(&ingredients, self.cube_main).map(|r| r.name().to_string()),
                hint: cube::hint(&ingredients, self.cube_main),
            };
            let playing = self.cube_window.as_ref().is_some_and(|w| w.transmuting());
            (
                contents,
                (on && r.all().any(fits)) || !held.is_empty() || playing,
            )
        };
        if !wanted {
            self.cube_main = false;
            let Some(w) = self.cube_window.take() else {
                return false;
            };
            self.glides.borrow_mut().forget(w.hwnd.0 as isize);
            w.destroy();
            return true;
        }
        let mut changed = false;
        if self.cube_window.is_none() {
            match CubeWindow::create(Rc::clone(&self.shared), -10_000, -10_000) {
                // Born under whatever else stands there.
                Ok(w) => {
                    w.raise();
                    self.cube_window = Some(w);
                    changed = true;
                }
                Err(e) => {
                    eprintln!("horadric: cannot create the cube: {e}");
                    return false;
                }
            }
        }
        if !self.columns.contains(columns::CUBE) {
            let above = if self.columns.contains(columns::STASH) {
                columns::STASH
            } else {
                columns::USAGE
            };
            self.columns.add_under(columns::CUBE, above);
        }
        if let Some(w) = &self.cube_window {
            w.set_contents(contents);
        }
        changed
    }

    /// A tile or a pane let go over the cube: its session goes in, unless
    /// it cannot or the cube is full.
    pub(in crate::app) fn put_in_cube(&mut self, id: &str) {
        let fits = self
            .shared
            .registry
            .lock()
            .is_ok_and(|r| r.get(id).is_some_and(fits));
        if !self.cube_on || !fits || self.cube.iter().any(|c| c == id) {
            return;
        }
        if self.cube.len() >= cube::SLOTS {
            self.toasts.show(
                Kind::Failed,
                "The cube is full",
                "Take a session out with a click on its slot first.",
            );
            return;
        }
        self.cube.push(id.to_string());
    }

    /// A slot clicked: its session comes out.
    pub(in crate::app) fn out_of_cube(&mut self, id: &str) {
        self.cube.retain(|c| c != id);
    }

    /// Runs the recipe the cube's contents make, then empties it.
    pub(in crate::app) fn transmute(&mut self) {
        let sessions: Vec<Session> = match self.shared.registry.lock() {
            Ok(r) => self
                .cube
                .iter()
                .filter_map(|id| r.get(id).cloned())
                .collect(),
            Err(_) => return,
        };
        let ingredients: Vec<Ingredient> = sessions.iter().map(ingredient).collect();
        let Some(recipe) = cube::recipe(&ingredients, self.cube_main) else {
            return;
        };
        if let Some(w) = &self.cube_window {
            w.transmute(recipe.outcome(), recipe == Recipe::Cow);
        }
        self.cube.clear();
        self.cube_main = false;
        match recipe {
            Recipe::Review => self.review(&sessions[0], &sessions[1]),
            Recipe::Merge => {
                self.merge_session(&sessions[0]);
            }
            Recipe::Close => {
                for s in &sessions {
                    self.close_with_summary(s);
                }
            }
            // The portal is all it does: the leg goes back to its tile.
            Recipe::Cow => {}
        }
        self.reconcile(false);
    }

    /// Starts a session in the first one's main tree that reviews both
    /// diffs, and shows it on the stage.
    fn review(&mut self, a: &Session, b: &Session) {
        let main = self.main_tree(a);
        let base = crate::worktree::checked_out(&main).unwrap_or_else(|| "main".into());
        let prompt = cube::review_prompt(&subject(a), &subject(b), &base);
        let name = format!("Review: {} + {}", a.label(), b.label());
        let Some(id) = self.start_reviewer(&name, main, prompt) else {
            return;
        };
        if let Some(key) = self.project_of(&id) {
            if self.fill_stage(&key) {
                if let Some(stage) = &self.stage {
                    stage.focus_session(&id);
                }
            }
        }
    }

    /// Merges the branch of a session's own worktree, the merge rune in the
    /// cube and in a runeword. None when it has no branch of its own.
    pub(in crate::app) fn merge_session(&mut self, s: &Session) -> Option<bool> {
        let w = s.worktree.as_ref()?;
        Some(self.merge(&Merge {
            main: PathBuf::from(&w.main),
            branch: w.branch.clone(),
            title: s.label().to_string(),
        }))
    }

    /// The main working tree of a session's repository, where a reviewer
    /// of its work starts.
    pub(in crate::app) fn main_tree(&self, s: &Session) -> PathBuf {
        s.worktree
            .as_ref()
            .map(|w| PathBuf::from(&w.main))
            .or_else(|| self.project_dir(&project_key(s)))
            .unwrap_or_else(|| PathBuf::from(&s.cwd))
    }

    /// Starts a reviewer named `name` in `main`, told `prompt`. Its id, or
    /// None when it could not start, which a notification says.
    pub(in crate::app) fn start_reviewer(
        &mut self,
        name: &str,
        main: PathBuf,
        prompt: String,
    ) -> Option<String> {
        let id = self.unique_id("review");
        self.tasks.prompts.insert(id.clone(), prompt);
        if let Err(e) = self.launch(
            &id,
            name,
            main,
            Vec::new(),
            Run::Agent(horadric_core::Agent::Claude),
            false,
        ) {
            self.tasks.prompts.remove(&id);
            eprintln!("horadric: cannot start the reviewer: {e}");
            self.toasts
                .show(Kind::Failed, "Cannot start the reviewer", &e);
            return None;
        }
        Some(id)
    }

    /// Ends a session and keeps the one line it leaves in the journal, for
    /// the catch-up. Its end is not journaled as well: the line is its
    /// last word.
    fn close_with_summary(&mut self, s: &Session) {
        self.journaled.remove(&s.id);
        self.end(&s.id);
        store::journal(&Entry {
            at: unix_now(),
            session: s.id.clone(),
            name: s.label().to_string(),
            project: project_key(s),
            what: What::Closed {
                line: cube::summary(&s.last_line),
            },
        });
    }
}
