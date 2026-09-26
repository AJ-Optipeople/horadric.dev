//! The desktop face of Horadric.
//!
//! One frameless window per project cluster. Tiles are drawn
//! inside it with Direct2D and DirectWrite. There is no toolkit and no web
//! view: the window manager fight is the whole project, so nothing sits
//! between us and Win32.
//!
//! An expanded session is a second kind of window, a real terminal: the
//! agent runs in a ConPTY, `alacritty_terminal` keeps the grid, and the grid
//! is drawn with DirectWrite glyph runs. The sessions share one terminal,
//! the stage, which shows one project at a time with each of its sessions
//! as a pane in a grid. A click on a tile switches it to that project.
//!
//! One more window belongs to no project: the usage window, with the
//! account's Claude limits and the settings for sessions ([`usage`]), whose
//! lists drop down in a window of their own (`dropdown`).
//! Another stands in for the first project while none is open, so an empty
//! desktop says how to begin (`start`).
//!
//! A file clicked in a files tile is shown on the stage too, read only, as a
//! pane with no program behind it ([`viewer`], [`highlight`]).
//!
//! A project can also have plain terminals, a shell with no agent, for
//! whatever else the work needs. They are sessions with a tile and a pane
//! like the rest ([`shell`]).
//!
//! Each project has a task list, `.horadric/tasks.md`, shown in a tasks
//! tile in its cluster. A click on an item starts a session on it, and the
//! app can work down the list by itself ([`board`], `horadric_core::tasks`).
//!
//! [`anim`], [`board`], [`columns`], [`field`], [`files`], [`find`], [`layout`], [`theme`], [`palette`], [`paste`],
//! [`keys`], [`frame`], [`icon`], [`inbox`], [`motion`], [`paths`], [`viewer`],
//! [`highlight`], [`history`], [`screens`] and [`shell`] are pure and tested, and so is
//! the list logic in `recent`.
//! `update` is the updater's crypto through CNG, tested with keys made in
//! the tests.
//! The rest is Windows only and verified on screen.

pub mod anim;
pub mod board;
pub mod columns;
pub mod field;
pub mod files;
pub mod find;
pub mod frame;
pub mod highlight;
pub mod history;
pub mod icon;
pub mod inbox;
pub mod keys;
pub mod layout;
pub mod motion;
pub mod palette;
pub mod paste;
pub mod paths;
pub mod screens;
pub mod shell;
pub mod theme;
pub mod viewer;

#[cfg(windows)]
mod agents;
#[cfg(windows)]
pub mod app;
#[cfg(windows)]
mod ask;
#[cfg(windows)]
pub mod autostart;
#[cfg(windows)]
mod backdrop;
#[cfg(windows)]
mod browsers;
#[cfg(windows)]
mod caption;
#[cfg(windows)]
mod clipboard;
#[cfg(windows)]
mod console;
#[cfg(windows)]
mod dialog;
#[cfg(windows)]
pub use dialog::error_alone;
#[cfg(windows)]
mod dropdown;
#[cfg(windows)]
mod glyphs;
#[cfg(windows)]
mod menu;
#[cfg(windows)]
mod net;
#[cfg(windows)]
mod pane;
#[cfg(windows)]
mod picker;
#[cfg(windows)]
mod project;
#[cfg(windows)]
mod recent;
#[cfg(windows)]
mod render;
#[cfg(windows)]
mod snapping;
#[cfg(windows)]
mod start;
#[cfg(windows)]
mod store;
#[cfg(windows)]
mod terminal;
#[cfg(windows)]
mod toast;
#[cfg(windows)]
mod tray;
#[cfg(windows)]
pub mod update;
#[cfg(windows)]
mod usage;
#[cfg(windows)]
mod watch;
#[cfg(windows)]
mod window;
#[cfg(windows)]
mod worktree;
