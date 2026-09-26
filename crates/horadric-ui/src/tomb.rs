//! The app's side of Tal Rasha's tombs: one item of the list started in
//! several sessions at once, each in a worktree of its own, until the human
//! picks the one to keep. The others end, fading out as any ended tile
//! does, and their worktrees and branches go with the work in them.
//!
//! The runner's fuses hold: never more than eight tombs, the first starts
//! at the click and each after it at most one every ten seconds in its
//! project, and none starts beside a paused tomb or once none is left.
//! A batch counts as one place of `parallel` for each tomb, so the runner
//! starts no other item past it.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Instant;

use horadric_core::tasks::{self, Holder, Mark, Task};
use horadric_core::tombs;
use horadric_hooks::tasks as file;

use super::{horadric_command, Merge, START_GAP};
use crate::app::{with_app, App, Run};
use crate::board::{Board, RowState};
use crate::dialog::{Dialog, Tone};
use crate::toast::Kind;

impl App {
    /// The tombs of `batch` there are, running, paused or stashed, by their
    /// number.
    pub(super) fn tombs_of(&self, batch: &str) -> Vec<String> {
        let Ok(r) = self.shared.registry.lock() else {
            return Vec::new();
        };
        let mut ids: Vec<(usize, String)> = r
            .all()
            .map(|s| s.id.as_str())
            .chain(r.stashed().iter().map(|s| s.id.as_str()))
            .filter_map(|id| match tombs::of(id) {
                Some((b, n)) if b == batch => Some((n, id.to_string())),
                _ => None,
            })
            .collect();
        ids.sort();
        ids.into_iter().map(|(_, id)| id).collect()
    }

    /// A batch holds its item as its tombs do between them: paused while
    /// one is, since only a click resumes it, live while one runs, and gone
    /// once none is left.
    pub(super) fn batch_holder(&self, batch: &str) -> Holder {
        let each: Vec<Holder> = self
            .tombs_of(batch)
            .iter()
            .map(|id| self.holder(id))
            .collect();
        if each.contains(&Holder::Paused) {
            Holder::Paused
        } else if each.contains(&Holder::Live) {
            Holder::Live
        } else {
            Holder::Gone
        }
    }

    /// Starts the open item on `line` titled `title` in `count` tombs: the
    /// first at once, the rest by the runner.
    pub(super) fn take_tombs(
        &mut self,
        key: &str,
        line: usize,
        title: &str,
        count: usize,
    ) -> Result<(), String> {
        let dir = self.project_dir(key).ok_or("the project has no folder")?;
        let own_trees = self
            .shared
            .boards
            .borrow()
            .get(key)
            .is_some_and(|b| b.own_trees);
        if !own_trees {
            return Err("tombs need a worktree for each session, and this project has none".into());
        }
        let task = tasks::parse(&file::read(&dir))
            .into_iter()
            .find(|t| t.line == line && t.title == title && t.mark == Mark::Open)
            .ok_or("the list changed, so that item is not there to take")?;
        let batch = tombs::batch(&self.unique_id(&tasks::slug(title)), count);
        let taken = file::update(&dir, |text| tasks::take(text, line, title, &batch))
            .map_err(|e| format!("cannot write {}: {e}", file::file(&dir).display()))?;
        if !taken {
            return Err("the list changed, so that item is not there to take".into());
        }
        self.refresh_boards(true);
        self.tasks.started.insert(key.to_string(), Instant::now());
        if let Err(e) = self.start_tomb(&dir, &task, &batch, 1, true) {
            self.tasks.tombs.remove(&batch);
            let _ = file::update(&dir, |text| tasks::set_mark(text, line, title, Mark::Open));
            self.refresh_boards(true);
            return Err(e);
        }
        Ok(())
    }

    /// Starts tomb `n` of `batch` on `task`, in a worktree of its own or
    /// not at all: tombs sharing a tree would edit the same files.
    fn start_tomb(
        &mut self,
        dir: &Path,
        task: &Task,
        batch: &str,
        n: usize,
        show: bool,
    ) -> Result<(), String> {
        self.tasks
            .tombs
            .entry(batch.to_string())
            .or_default()
            .insert(n);
        let id = tombs::tomb(batch, n);
        let cwd = self.own_tree(&id, &tasks::slug(&task.title), dir.to_path_buf(), &[]);
        if !self.new_trees.contains_key(&id) {
            return Err("git gave the tomb no worktree of its own".into());
        }
        self.tasks
            .prompts
            .insert(id.clone(), tasks::prompt(task, &horadric_command()));
        let name = tombs::name(&task.title, n);
        if let Err(e) = self.launch(&id, &name, cwd, Vec::new(), Run::Agent, false) {
            if let Some((w, _)) = self.new_trees.remove(&id) {
                crate::worktree::remove(w);
            }
            self.tasks.prompts.remove(&id);
            return Err(e);
        }
        if let Some(s) = self
            .shared
            .registry
            .lock()
            .ok()
            .as_mut()
            .and_then(|r| r.get_mut(&id))
        {
            s.loot.batch = true;
        }
        if show {
            let key = self.project_of(&id);
            if key.is_some_and(|k| self.fill_stage(&k, true)) {
                if let Some(stage) = &self.stage {
                    stage.focus_session(&id);
                }
            }
        }
        Ok(())
    }

    /// Starts the next tomb of an item in tombs, one per project every
    /// `START_GAP`. Never beside a paused tomb, and never for a batch with
    /// none left: after a restart that is the human's to start again. True
    /// when it started one, which uses the project's turn.
    pub(super) fn start_tombs(&mut self, key: &str, b: &Board) -> bool {
        if self
            .tasks
            .started
            .get(key)
            .is_some_and(|at| at.elapsed() < START_GAP)
        {
            return false;
        }
        for t in b.tasks.iter().filter(|t| t.mark == Mark::Working) {
            let Some(batch) = t.holder.as_deref().filter(|h| tombs::count(h).is_some()) else {
                continue;
            };
            let there = self.tombs_of(batch);
            if there.is_empty() || self.batch_holder(batch) == Holder::Paused {
                continue;
            }
            // A tomb once started stays started, even ended by the human.
            let started: &mut HashSet<usize> =
                self.tasks.tombs.entry(batch.to_string()).or_default();
            started.extend(there.iter().filter_map(|id| Some(tombs::of(id)?.1)));
            let started: Vec<usize> = started.iter().copied().collect();
            let Some(n) = tombs::next(batch, &started) else {
                continue;
            };
            let Some(dir) = self.project_dir(key) else {
                return false;
            };
            self.tasks.started.insert(key.to_string(), Instant::now());
            if let Err(e) = self.start_tomb(&dir, t, batch, n, false) {
                eprintln!("horadric: cannot start tomb {n} of \"{}\": {e}", t.title);
            }
            return true;
        }
        false
    }

    /// Every tomb of the batch has started and says it is done, so the
    /// human has something to pick from.
    pub(super) fn ready_to_pick(&self, batch: &str) -> bool {
        let started: Vec<usize> = self
            .tasks
            .tombs
            .get(batch)
            .map(|s| s.iter().copied().collect())
            .unwrap_or_default();
        let all_started = tombs::next(batch, &started).is_none();
        all_started && self.tombs_state(batch) == RowState::Pick
    }

    fn tombs_state(&self, batch: &str) -> RowState {
        let task = Task {
            line: 0,
            mark: Mark::Working,
            title: String::new(),
            holder: Some(batch.to_string()),
            reason: None,
            notes: Vec::new(),
        };
        self.row_state(&task)
    }

    /// A tomb said it is done, or blocked. Its item stays the batch's, so
    /// only the app keeps what it said: done in its loot, which is saved,
    /// and blocked as a notification.
    pub(in crate::app) fn tomb_reported(&mut self, id: &str, why: Option<&str>) {
        let name = {
            let Ok(mut r) = self.shared.registry.lock() else {
                return;
            };
            let Some(s) = r.get_mut(id) else {
                return;
            };
            if why.is_none() {
                s.loot.finished = true;
            }
            s.name.clone()
        };
        self.tasks.nudged.remove(id);
        if let (Some(why), false) = (why, self.quiet) {
            self.toasts.show(
                Kind::Waiting,
                &format!("Blocked: {name}"),
                &tasks::one_line(why),
            );
        }
        for c in &self.clusters {
            c.invalidate();
        }
    }

    /// The live tombs of the item a row holds, with their names, to pick
    /// from. None for an item not in tombs.
    pub(super) fn pickable(&self, holder: &str) -> Option<Vec<(String, String)>> {
        tombs::count(holder)?;
        let ids = self.tombs_of(holder);
        let r = self.shared.registry.lock().ok()?;
        Some(
            ids.into_iter()
                .filter_map(|id| Some((id.clone(), r.get(&id)?.name.clone())))
                .collect(),
        )
    }

    /// Whether `id` is a tomb whose item still waits for a pick.
    pub(in crate::app) fn can_pick(&self, id: &str) -> bool {
        let Some((batch, _)) = tombs::of(id) else {
            return false;
        };
        self.shared.boards.borrow().values().any(|b| {
            b.tasks
                .iter()
                .any(|t| t.mark == Mark::Working && t.holder.as_deref() == Some(batch))
        })
    }

    /// Keeps the tomb `winner`: its branch merged into the main tree with
    /// `merge`, or left for later. The other tombs end, their worktrees
    /// and branches deleted with the work in them, and the item is done,
    /// held by the winner, whose session closes as any finished one does.
    pub(super) fn pick_tomb(&mut self, winner: &str, merge: bool) {
        let Some((batch, _)) = tombs::of(winner) else {
            return;
        };
        let batch = batch.to_string();
        let Some(dir) = self.project_of(winner).and_then(|k| self.project_dir(&k)) else {
            return;
        };
        let Some(task) = tasks::parse(&file::read(&dir))
            .into_iter()
            .find(|t| t.holder.as_deref() == Some(batch.as_str()) && t.mark != Mark::Done)
        else {
            return;
        };
        for id in self.tombs_of(&batch).into_iter().filter(|id| id != winner) {
            self.discard_tomb(&id);
        }
        self.tasks.tombs.remove(&batch);
        let tree = self
            .shared
            .registry
            .lock()
            .ok()
            .and_then(|r| r.get(winner)?.worktree.clone());
        if let (true, Some(w)) = (merge, tree) {
            self.merge(&Merge {
                main: PathBuf::from(&w.main),
                branch: w.branch,
                title: task.title.clone(),
            });
        }
        if let Err(e) = file::update(&dir, |text| tasks::pick(text, &batch, winner)) {
            eprintln!("horadric: cannot write {}: {e}", file::file(&dir).display());
        }
        self.refresh_boards(true);
        self.reconcile(false);
        self.run_tasks();
    }

    /// Ends a tomb that lost, stashed or not, and throws its worktree away.
    /// Taken off the session first, so the plain removal an ending starts,
    /// which keeps what it would lose, does not get to it.
    fn discard_tomb(&mut self, id: &str) {
        let tree = self
            .shared
            .registry
            .lock()
            .ok()
            .and_then(|mut r| match r.get_mut(id) {
                Some(s) => s.worktree.take(),
                None => r.discard(id)?.worktree,
            });
        self.forget(id);
        if let Some(w) = tree {
            crate::worktree::discard(w);
        }
    }
}

/// Asks which way to keep the tomb `winner`, and keeps it.
pub(in crate::app) fn ask_pick(winner: &str) {
    let Some(asked) = with_app(|app| {
        let (batch, n) = tombs::of(winner)?;
        let others = app.tombs_of(batch).len().saturating_sub(1);
        let r = app.shared.registry.lock().ok()?;
        let w = r.get(winner)?.worktree.clone()?;
        let title = app
            .shared
            .boards
            .borrow()
            .values()
            .flat_map(|b| &b.tasks)
            .find(|t| t.holder.as_deref() == Some(batch))?
            .title
            .clone();
        let into =
            crate::worktree::checked_out(Path::new(&w.main)).unwrap_or_else(|| "main".into());
        Some(pick_question(n, others, &title, &w.branch, &into))
    })
    .flatten() else {
        return;
    };
    let pressed = crate::app::ask(&Dialog {
        tone: Tone::Warning,
        title: "Pick this tomb",
        text: &asked,
        buttons: &["Merge it", "Keep its branch", "Cancel"],
        default: 2,
    });
    if let Some(merge @ (0 | 1)) = pressed {
        with_app(|app| app.pick_tomb(winner, merge == 0));
    }
}

/// What to ask before keeping tomb `n` and throwing away the `others`.
fn pick_question(n: usize, others: usize, title: &str, branch: &str, into: &str) -> String {
    let rest = match others {
        0 => "No other tomb is left.".to_string(),
        1 => "The other tomb ends now, and its worktree and branch are deleted with \
              the work in them."
            .to_string(),
        k => format!(
            "The other {k} tombs end now, and their worktrees and branches are deleted \
             with the work in them."
        ),
    };
    format!(
        "Keep tomb {n} of \"{}\"? Merge puts {branch} into {into}; keeping the branch \
         leaves it to merge later.\n\n{rest}",
        tasks::one_line(title)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pick_question_says_what_is_thrown_away() {
        let q = pick_question(2, 2, "Fix the\nlogin", "fix-the-login-2", "main");
        assert!(q.starts_with(
            "Keep tomb 2 of \"Fix the login\"? Merge puts fix-the-login-2 into main;"
        ));
        assert!(q.ends_with(
            "\n\nThe other 2 tombs end now, and their worktrees and branches are deleted \
             with the work in them."
        ));
        assert!(pick_question(1, 1, "A", "a", "main").contains("The other tomb ends now"));
        assert!(pick_question(1, 0, "A", "a", "main").ends_with("No other tomb is left."));
    }
}
