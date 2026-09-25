//! Claude Code's background sessions, asked of `claude agents`. Its daemon
//! runs them, so Horadric only lists them, attaches to them and stops them.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use horadric_core::background::{self, Asked, Background};
use horadric_core::{HookEvent, Registry};

use crate::console;

/// Every background session running now. None when `claude` is not there
/// or did not answer, which says nothing about whether any run.
pub fn list() -> Option<Vec<Background>> {
    let out = console::claude_command(&["agents", "--json"])?
        .output()
        .ok()?;
    out.status.success().then(|| background::parse(&out.stdout))
}

/// Stops a background session. Not waited for: its `SessionEnd`, or the
/// tile going, says it happened.
pub fn stop(short: &str) {
    if let Some(mut c) = console::claude_command(&["stop", short]) {
        let _ = c.stdout(std::process::Stdio::null()).spawn();
    }
}

/// A tile for each background session no tile holds yet. True when one
/// was added.
pub fn adopt_all(registry: &Arc<Mutex<Registry>>, list: &[Background]) -> bool {
    let Ok(mut r) = registry.lock() else {
        return false;
    };
    let mut added = false;
    for b in list {
        let held = r
            .all()
            .any(|s| s.claude_session_id.as_deref() == Some(&b.session_id));
        if !held && r.get(&background::tile_id(&b.id)).is_none() {
            r.add(b.session());
            added = true;
        }
    }
    added
}

/// The tile for the conversation of an event no tile holds, when it is a
/// background session. Asks `claude agents` at most once a minute for the
/// same conversation, since every plain `claude` on the machine posts here
/// too.
pub fn adopt(
    registry: &Arc<Mutex<Registry>>,
    event: &HookEvent,
    asked: &mut Asked,
) -> Option<String> {
    let id = event.session_id.as_str();
    if id.is_empty() || event.hook_event_name == HookEvent::STATUS || !asked.due(id, Instant::now())
    {
        return None;
    }
    let list = list()?;
    let b = list.iter().find(|b| b.session_id == id)?;
    let tile = background::tile_id(&b.id);
    let mut r = registry.lock().ok()?;
    if r.get(&tile).is_none() {
        r.add(b.session());
    }
    Some(tile)
}

/// Whether the background session with this short id still runs. None
/// when `claude` could not say.
pub fn running(short: &str) -> Option<bool> {
    Some(list()?.iter().any(|b| b.id == short))
}
