//! Each project's [`Glow`]: what its agents touched lately, for the Arcane
//! Sanctuary pane to light (`horadric_core::glow`).
//!
//! The feeder thread fills it as hook events and lore touches arrive, and
//! the pane reads it on the UI thread, so it sits behind a lock of its own
//! rather than in `Shared`, held only for one touch or one read.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use horadric_core::glow::{self, Glow, Touch};
use horadric_core::{HookEvent, Registry};

use crate::project::project_key;

fn glows() -> &'static Mutex<HashMap<String, Glow>> {
    static GLOWS: OnceLock<Mutex<HashMap<String, Glow>>> = OnceLock::new();
    GLOWS.get_or_init(Default::default)
}

static STAMP: AtomicU64 = AtomicU64::new(0);

/// The clock touches are made on, in seconds: the pane asks the glow about
/// this moment.
pub fn now() -> f64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_secs_f64()
}

/// Changes whenever any project is touched, so a still pane can tell
/// cheaply whether it has light to draw again.
pub fn stamp() -> u64 {
    STAMP.load(Ordering::Acquire)
}

/// Reads the glow of the project `key`, None when nothing in it was
/// touched since the app started.
pub fn with<R>(key: &str, f: impl FnOnce(&Glow) -> R) -> Option<R> {
    glows().lock().ok()?.get(key).map(f)
}

/// Lights what the event `event` of the session `id` touched: a hook
/// event's files, or a [`HookEvent::GLOW`]'s nodes. True when anything
/// was lit.
pub fn hear(registry: &Mutex<Registry>, id: &str, event: &HookEvent) -> bool {
    let told = event.hook_event_name == HookEvent::GLOW;
    if !told && event.tool_name.is_none() {
        return false;
    }
    let Some((key, tree)) = registry.lock().ok().and_then(|r| {
        let s = r.get(id)?;
        Some((project_key(s), s.worktree.as_ref().map(|w| w.path.clone())))
    }) else {
        return false;
    };
    let nodes: Vec<(String, Touch)> = if told {
        glow::told(event)
    } else {
        let roots = [tree.as_deref().unwrap_or_default(), key.as_str()];
        glow::heard(event, |p| Path::new(p).exists())
            .into_iter()
            .filter_map(|(p, how)| Some((glow::node(&p, &roots)?, how)))
            .collect()
    };
    light(&key, id, &nodes)
}

/// Lights `nodes` of the project `key` in the colour of the session `id`.
/// True when there were any.
pub fn light(key: &str, id: &str, nodes: &[(String, Touch)]) -> bool {
    if nodes.is_empty() {
        return false;
    }
    let rgb = glow::session_rgb(id);
    let now = now();
    let Ok(mut all) = glows().lock() else {
        return false;
    };
    let glow = all.entry(key.to_string()).or_default();
    for (node, how) in nodes {
        glow.touch(node, *how, rgb, now);
    }
    glow.prune(now);
    STAMP.fetch_add(1, Ordering::AcqRel);
    true
}
