//! Core model for Horadric: what a session is, what state it is in, and how
//! Claude Code hook events move it between states.
//!
//! This crate has no I/O. The hook listener feeds it [`HookEvent`]s, the UI
//! reads [`Session`]s out of a [`Registry`]. Everything here is testable
//! without a terminal or a network.

pub mod accounts;
pub mod afar;
pub mod agent;
pub mod aim;
pub mod ask;
pub mod background;
pub mod bench;
pub mod brotherhood;
pub mod brothers;
pub mod checkpoint;
pub mod chronicle;
pub mod cube;
pub mod diff;
pub mod discord;
pub mod escalate;
pub mod event;
pub mod experience;
pub mod fleet;
pub mod glow;
pub mod google;
pub mod guard;
pub mod handing;
pub mod handoff;
pub mod heavy;
pub mod history;
pub mod joining;
pub mod journal;
pub mod keepers;
pub mod keeping;
pub mod lookback;
pub mod lore;
pub mod merge;
pub mod node;
pub mod nodes;
pub mod overlap;
pub mod pack;
pub mod placing;
pub mod portal;
pub mod presence;
pub mod program_status;
pub mod rarity;
pub mod registry;
pub mod release;
pub mod runeword;
pub mod sanctuary;
pub mod saved;
pub mod serving;
pub mod session;
pub mod ship;
pub mod sky;
pub mod spend;
pub mod ssh;
pub mod standing;
pub mod tasks;
pub mod title;
pub mod tombs;
pub mod trust;
pub mod usage;
pub mod warriv;
pub mod webpush;
pub mod workos;
pub mod worktree;

pub use agent::Agent;
pub use event::{HookEvent, ModAgent};
pub use registry::{Registry, Route, STASH_SLOTS};
pub use saved::{Carry, SavedCluster, SavedPanel, SavedSession, SavedState};
pub use session::{format_age, session_id, Phase, Session, WaitReason};
pub use title::Title;
pub use usage::{Defaults, Limit, Limits, Setting, Status, Usage};
