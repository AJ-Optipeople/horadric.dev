//! The app's side of the quest log: a project's quests built from the
//! chronicle, the journal and its quest list, the window kept up to date
//! as they change, and what its keys ask done with a quest's conversation.

use std::cell::{Cell, RefCell};
use std::fs;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::SystemTime;

use horadric_core::chronicle::{self, Quest};
use horadric_core::journal;
use horadric_core::tasks::Task;
use horadric_core::Agent;

use super::App;
use crate::questlog::{self, Ask, QuestLog};
use crate::store;
use crate::window::project_name;

/// A file's size and when it was written, which says whether it changed.
type Stamp = Option<(u64, SystemTime)>;

/// How many ticks between repaints that only move the ages on.
const AGES_EVERY: u32 = 30;

thread_local! {
    /// What the quest log was last built from: the chronicle, the journal
    /// and the project's quest list. A tick that finds them as they were
    /// builds nothing.
    static SEEN: RefCell<Option<(Stamp, Stamp, Vec<Task>)>> = const { RefCell::new(None) };
    static TICKS: Cell<u32> = const { Cell::new(0) };
}

fn stamp(file: &str) -> Stamp {
    let meta = fs::metadata(store::dir()?.join(file)).ok()?;
    Some((meta.len(), meta.modified().ok()?))
}

impl App {
    /// Does what the quest log asks, or opens it.
    pub(super) fn quest_log_asks(&mut self, ask: Ask) {
        match ask {
            Ask::Open(key) => self.open_quest_log(&key),
            Ask::Close => {
                if let Some(w) = self.quest_log.take() {
                    w.destroy();
                }
            }
            Ask::Read(key, id) => self.read_quest(&key, &id),
            Ask::Carry(key, id) => self.carry_on_quest(&key, &id),
        }
    }

    /// The project's quests, oldest first.
    fn quests_of(&self, key: &str) -> Vec<Quest> {
        let list = self.list_of(key);
        chronicle::quests(
            &store::chronicle_all(),
            &store::journal_since(0),
            key,
            &list,
        )
    }

    fn list_of(&self, key: &str) -> Vec<Task> {
        self.shared
            .boards
            .borrow()
            .get(key)
            .map(|b| b.tasks.clone())
            .unwrap_or_default()
    }

    /// Shows the quest log for the project `key`, raising the one open.
    fn open_quest_log(&mut self, key: &str) {
        let quests = self.quests_of(key);
        let name = project_name(key);
        SEEN.with(|s| s.replace(None));
        match &self.quest_log {
            Some(w) => {
                w.set(key.to_string(), name, quests);
                w.raise();
            }
            None => match QuestLog::open(Rc::clone(&self.shared), key.to_string(), name, quests) {
                Ok(w) => self.quest_log = Some(w),
                Err(e) => eprintln!("horadric: cannot open the quest log: {e}"),
            },
        }
    }

    /// Hands the quest log its project's quests again when the chronicle,
    /// the journal or the quest list changed, or when `force`d. Otherwise,
    /// now and then, it is only repainted, so its ages move on.
    pub(super) fn refresh_quest_log(&mut self, force: bool) {
        let Some(w) = &self.quest_log else {
            return;
        };
        let key = w.key();
        let now = (
            stamp(chronicle::FILE),
            stamp(journal::FILE),
            self.list_of(&key),
        );
        let changed = SEEN.with(|s| s.borrow().as_ref() != Some(&now));
        if changed || force {
            SEEN.with(|s| s.replace(Some(now)));
            w.set(key.clone(), project_name(&key), self.quests_of(&key));
        } else if TICKS.with(|t| {
            t.set(t.get().wrapping_add(1));
            t.get() % AGES_EVERY == 0
        }) {
            w.invalidate();
        }
    }

    fn quest(&self, key: &str, id: &str) -> Option<Quest> {
        self.quests_of(key).into_iter().find(|q| q.id == id)
    }

    /// Writes out the quest's conversation, its story on top, and shows it
    /// on the stage beside the project's sessions, read only.
    fn read_quest(&mut self, key: &str, id: &str) {
        let Some(q) = self.quest(key, id) else {
            return;
        };
        let Some((conversation, cwd)) = &q.conversation else {
            return;
        };
        let transcript = match horadric_hooks::transcript::path(cwd, conversation) {
            Some(p) => chronicle::transcript_text(&fs::read_to_string(p).unwrap_or_default()),
            None => "_The conversation is no longer on disk._\n".to_string(),
        };
        let Some(dir) = store::dir() else {
            return;
        };
        let rel = questlog::session_file(id);
        let path = dir.join(rel.replace('/', "\\"));
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Err(e) = fs::write(&path, questlog::session_doc(&q, &transcript)) {
            eprintln!("horadric: cannot write out the quest's session: {e}");
            return;
        }
        self.open_view(key, &dir, &rel);
    }

    /// Carries on the quest's conversation in a new tile, in the folder it
    /// was held in, or the project's when that is gone. A conversation a
    /// session still holds is shown instead: two agents on one
    /// conversation would write over each other.
    fn carry_on_quest(&mut self, key: &str, id: &str) {
        let Some(q) = self.quest(key, id) else {
            return;
        };
        let Some((conversation, cwd)) = q.conversation.clone() else {
            return;
        };
        let holder = self.shared.registry.lock().ok().and_then(|r| {
            r.all()
                .find(|s| s.claude_session_id.as_deref() == Some(conversation.as_str()))
                .map(|s| s.id.clone())
        });
        if let Some(h) = holder {
            self.reveal(&h, false);
            return;
        }
        let dir = Some(PathBuf::from(&cwd))
            .filter(|d| d.is_dir())
            .or_else(|| self.project_dir(key));
        let Some(dir) = dir else {
            return;
        };
        let name = Some(q.name.clone()).filter(|n| !n.trim().is_empty());
        let args = Agent::Claude.resume_args(Some(&conversation));
        let started = match self.start_in(name, dir, args, Agent::Claude, false) {
            Ok(id) => id,
            Err(e) => {
                eprintln!("horadric: cannot carry on the quest: {e}");
                return;
            }
        };
        // Known before any hook says so, as for the History menu: a pause
        // before the next prompt would otherwise resume nothing.
        if let Ok(mut r) = self.shared.registry.lock() {
            if let Some(s) = r.get_mut(&started) {
                s.claude_session_id = Some(conversation);
                s.prompted = true;
            }
        }
    }
}
