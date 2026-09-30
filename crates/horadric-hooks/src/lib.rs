//! How Claude Code tells Horadric what a session is doing.
//!
//! Claude Code supports `http` hooks: on every lifecycle event it POSTs the
//! event payload to a URL. Horadric listens on localhost for those posts. The
//! hook is configured to send the `HORADRIC_SESSION` environment variable as a
//! header, so a `claude` started by Horadric (or inside a Horadric terminal) is
//! tagged with the tile it belongs to, and a `claude` started anywhere else
//! sends an empty header and is ignored.
//!
//! No script runs, no process is spawned per event. Connection refused when
//! Horadric is not running costs Claude Code a few microseconds.

pub mod client;
pub mod codex;
pub mod grok;
pub mod install;
pub mod listener;
pub mod tasks;
pub mod transcript;

/// Environment variable Horadric sets on every `claude` it spawns.
pub const SESSION_ENV: &str = "HORADRIC_SESSION";

/// Header the hook carries the session id in.
pub const SESSION_HEADER: &str = "x-horadric-session";

/// Environment variable Horadric sets beside [`SESSION_ENV`]: the port of the
/// Horadric that owns the session. The hook URL is fixed at install time, so
/// a session started by a dev instance still posts to the installed one,
/// which reads this from [`OWNER_HEADER`] and passes the event on.
pub const OWNER_ENV: &str = "HORADRIC_OWNER_PORT";

/// Environment variable Horadric sets on a session in a worktree of its
/// own: the project folder in the main working tree, where its task list
/// is. The worktree has none, or an old copy.
pub const TASKS_ENV: &str = "HORADRIC_TASKS";

/// Header `horadric hook` names the agent that sent the event in, as
/// [`horadric_core::Agent::from_name`] reads it. Without it the event is
/// Claude Code's.
pub const AGENT_HEADER: &str = "x-horadric-agent";

/// Header the hook carries [`OWNER_ENV`] in.
pub const OWNER_HEADER: &str = "x-horadric-port";

/// Default port. Overridable with `HORADRIC_PORT`.
pub const DEFAULT_PORT: u16 = 43117;

/// Set to anything but `0` to run a development instance beside the
/// installed Horadric: its own port, its own state, and nothing that changes
/// the machine. It is how Horadric gets developed from inside Horadric.
pub const DEV_ENV: &str = "HORADRIC_DEV";

/// A dev instance's port, unless `HORADRIC_PORT` says otherwise.
pub const DEV_PORT: u16 = 43118;

/// Path the hook posts to. The word `horadric` in the URL is how the installer
/// recognises its own entries when updating or removing them.
pub const HOOK_PATH: &str = "/horadric/hook";

/// Path `horadric status` posts to: what Claude Code gave the status line of
/// a session Horadric started.
pub const STATUS_PATH: &str = "/horadric/status";

/// Path `horadric new` posts to, asking the running app to start a session in
/// a terminal of its own.
pub const NEW_PATH: &str = "/horadric/new";

/// Path `horadric reload` posts to, asking the running app to hand over to a
/// new build.
pub const RELOAD_PATH: &str = "/horadric/reload";

/// Path `horadric quest` posts to after it changed a project's task list, so
/// the app reads it at once rather than on its next look.
pub const TASKS_PATH: &str = "/horadric/tasks";

/// Header a command request must carry. A browser can not send a custom
/// header to another origin without a preflight we never answer, so this is
/// what stops a web page from starting processes through localhost.
pub const COMMAND_HEADER: &str = "x-horadric-command";

/// Whether this is a development instance, from [`DEV_ENV`].
pub fn dev() -> bool {
    is_dev(std::env::var(DEV_ENV).ok().as_deref())
}

fn is_dev(value: Option<&str>) -> bool {
    value.is_some_and(|v| !v.is_empty() && v != "0")
}

/// Port to listen on and to write into the hook URL.
pub fn port() -> u16 {
    port_from(std::env::var("HORADRIC_PORT").ok().as_deref(), dev())
}

fn port_from(horadric_port: Option<&str>, dev: bool) -> u16 {
    horadric_port
        .and_then(|p| p.parse().ok())
        .unwrap_or(if dev { DEV_PORT } else { DEFAULT_PORT })
}

/// Names this Horadric among the others on the machine, in the pipes of
/// its session hosts: the port, which no two running instances share.
pub fn instance() -> String {
    port().to_string()
}

pub fn hook_url(port: u16) -> String {
    format!("http://127.0.0.1:{port}{HOOK_PATH}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dev_is_any_value_but_empty_or_zero() {
        assert!(is_dev(Some("1")));
        assert!(is_dev(Some("yes")));
        assert!(!is_dev(Some("0")));
        assert!(!is_dev(Some("")));
        assert!(!is_dev(None));
    }

    #[test]
    fn dev_moves_the_default_port_but_not_an_explicit_one() {
        assert_eq!(port_from(None, false), DEFAULT_PORT);
        assert_eq!(port_from(None, true), DEV_PORT);
        assert_eq!(port_from(Some("5000"), true), 5000);
        assert_eq!(port_from(Some("junk"), false), DEFAULT_PORT);
    }
}
