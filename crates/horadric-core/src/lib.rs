//! Core model for Horadric: what a session is, what state it is in, and how
//! Claude Code hook events move it between states.
//!
//! This crate has no I/O. The hook listener feeds it [`HookEvent`]s, the UI
//! reads [`Session`]s out of a [`Registry`]. Everything here is testable
//! without a terminal or a network.

pub mod accounts;
pub mod afar;
pub mod afar_files;
pub mod afar_tiles;
pub mod agent;
pub mod aim;
pub mod ask;
pub mod asking;
pub mod background;
pub mod brotherhood;
#[cfg(test)]
mod brotherhood_sim;
#[cfg(test)]
mod brotherhood_wired;
pub mod brothers;
pub mod changes;
pub mod checkpoint;
pub mod chronicle;
pub mod context;
pub mod diff;
pub mod discord;
pub mod escalate;
pub mod event;
pub mod experience;
pub mod fleet;
pub mod folio;
pub mod forge;
pub mod gitfiles;
pub mod glow;
pub mod google;
pub mod handing;
pub mod handoff;
pub mod handoffs;
pub mod heavy;
pub mod hireling;
pub mod history;
pub mod joining;
pub mod journal;
pub mod keeper;
pub mod keepers;
pub mod keeping;
pub mod lineage;
pub mod lore;
pub mod markdown;
pub mod merge;
pub mod mesh;
pub mod naming;
pub mod node;
pub mod nodes;
pub mod overlap;
pub mod pack;
pub mod phoning;
pub mod placing;
pub mod portal;
pub mod presence;
pub mod program_status;
pub mod rarity;
pub mod registry;
pub mod relaying;
pub mod release;
pub mod remote;
pub mod reopen;
pub mod retitle;
pub mod rostering;
pub mod runeword;
pub mod said;
pub mod sanctuary;
pub mod saved;
pub mod scope;
pub mod screen_share;
pub mod serving;
pub mod session;
pub mod ship;
pub mod sky;
pub mod spend;
pub mod ssh;
pub mod standing;
pub mod sworn;
pub mod tasks;
pub mod team;
pub mod title;
pub mod towns;
pub mod trust;
pub mod usage;
pub mod vault;
pub mod warriv;
pub mod webpush;
pub mod workos;
pub mod worktree;

pub use agent::Agent;
pub use event::{HookEvent, ModAgent};
pub use registry::{Registry, Route};
pub use saved::{Carry, SavedCluster, SavedPanel, SavedSession, SavedState};
pub use session::{format_age, session_id, Phase, Session, WaitReason};
pub use title::Title;
pub use usage::{Defaults, Limit, Limits, Setting, Status, Usage};
