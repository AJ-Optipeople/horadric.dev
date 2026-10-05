//! Core model for Horadric: what a session is, what state it is in, and how
//! Claude Code hook events move it between states.
//!
//! This crate has no I/O. The hook listener feeds it [`HookEvent`]s, the UI
//! reads [`Session`]s out of a [`Registry`]. Everything here is testable
//! without a terminal or a network.

pub mod accounts;
pub mod agent;
pub mod background;
pub mod chronicle;
pub mod cube;
pub mod diff;
pub mod discord;
pub mod event;
pub mod experience;
pub mod fleet;
pub mod journal;
pub mod overlap;
pub mod presence;
pub mod rarity;
pub mod registry;
pub mod release;
pub mod runeword;
pub mod saved;
pub mod session;
pub mod ssh;
pub mod tasks;
pub mod title;
pub mod tombs;
pub mod usage;
pub mod worktree;

pub use agent::Agent;
pub use event::HookEvent;
pub use registry::{Registry, Route, STASH_SLOTS};
pub use saved::{Carry, SavedCluster, SavedPanel, SavedSession, SavedState};
pub use session::{format_age, session_id, Phase, Session, WaitReason};
pub use title::Title;
pub use usage::{Defaults, Limit, Limits, Setting, Status, Usage};
