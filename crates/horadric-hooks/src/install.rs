//! Writes Horadric's hook entries into Claude Code's user settings.
//!
//! Claude Code only reads hooks from settings files, so the entries have to
//! live in `~/.claude/settings.json`. They are inert for any `claude` not
//! started under Horadric: the session header comes out empty and the listener
//! drops the event. The installer only ever touches entries whose URL
//! contains `/horadric/`, so a user's own hooks are left exactly as they were.
//!
//! Grok Build reads hooks from a folder of files, so it gets one of its own,
//! `~/.grok/hooks/horadric.json`, a command hook for `horadric hook grok`.
//! Grok refuses an `http` hook to loopback. The command does nothing for a
//! `grok` not started under Horadric. Grok has no per session way to be
//! given an MCP server either: no flag, its `GROK_CONFIG` overlay drops
//! `mcp_servers` and `plugins`, and a plugin in `~/.grok/plugins` starts
//! out disabled. So `horadric mcp` goes in as a `[mcp_servers.horadric]`
//! table in its `config.toml`, the only part of that file Horadric
//! touches. It offers no tools to a `grok` not started under Horadric.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

use crate::{hook_url, HOOK_PATH, OWNER_ENV, SESSION_ENV};

/// Hook events Horadric needs. Everything that moves a session between
/// working, waiting and done.
pub const EVENTS: &[&str] = &[
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "PostToolUseFailure",
    "PermissionRequest",
    "Notification",
    "Stop",
    "StopFailure",
    "SessionEnd",
];

/// `~/.claude/settings.json`.
pub fn settings_path() -> Option<PathBuf> {
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))?;
    Some(Path::new(&home).join(".claude").join("settings.json"))
}

/// The handler Claude Code runs. One shape for every event.
fn handler(port: u16) -> Value {
    json!({
        "type": "http",
        "url": hook_url(port),
        "headers": {
            "X-Horadric-Session": format!("${SESSION_ENV}"),
            "X-Horadric-Port": format!("${OWNER_ENV}")
        },
        "allowedEnvVars": [SESSION_ENV, OWNER_ENV],
        "timeout": 5
    })
}

fn is_ours(handler: &Value) -> bool {
    handler
        .get("url")
        .and_then(Value::as_str)
        .is_some_and(|u| u.contains(HOOK_PATH))
}

/// Adds or refreshes Horadric's hooks in a settings document.
pub fn install_into(settings: &mut Value, port: u16) {
    remove_from(settings);
    let root = ensure_object(settings);
    let hooks = root
        .entry("hooks")
        .or_insert_with(|| Value::Object(Map::new()));
    let hooks = ensure_object(hooks);
    for event in EVENTS {
        let groups = hooks.entry(*event).or_insert_with(|| Value::Array(vec![]));
        if let Value::Array(groups) = groups {
            groups.push(json!({ "hooks": [handler(port)] }));
        }
    }
}

/// Removes every Horadric hook from a settings document, leaving the rest.
pub fn remove_from(settings: &mut Value) {
    let Some(hooks) = settings.get_mut("hooks").and_then(Value::as_object_mut) else {
        return;
    };
    for groups in hooks.values_mut() {
        let Some(groups) = groups.as_array_mut() else {
            continue;
        };
        for group in groups.iter_mut() {
            if let Some(list) = group.get_mut("hooks").and_then(Value::as_array_mut) {
                list.retain(|h| !is_ours(h));
            }
        }
        groups.retain(|g| {
            g.get("hooks")
                .and_then(Value::as_array)
                .is_none_or(|l| !l.is_empty())
        });
    }
    hooks.retain(|_, groups| groups.as_array().is_none_or(|g| !g.is_empty()));
    if hooks.is_empty() {
        settings.as_object_mut().map(|o| o.remove("hooks"));
    }
}

/// True when the document has a Horadric hook on every event we need.
pub fn is_installed(settings: &Value, port: u16) -> bool {
    let url = hook_url(port);
    EVENTS.iter().all(|event| {
        settings
            .pointer(&format!("/hooks/{event}"))
            .and_then(Value::as_array)
            .is_some_and(|groups| {
                groups.iter().any(|g| {
                    g.get("hooks").and_then(Value::as_array).is_some_and(|l| {
                        l.iter()
                            .any(|h| h.get("url").and_then(Value::as_str) == Some(&url))
                    })
                })
            })
    })
}

fn ensure_object(v: &mut Value) -> &mut Map<String, Value> {
    if !v.is_object() {
        *v = Value::Object(Map::new());
    }
    v.as_object_mut().expect("just made it an object")
}

fn read(path: &Path) -> io::Result<Value> {
    match fs::read(path) {
        Ok(bytes) if bytes.iter().all(u8::is_ascii_whitespace) => Ok(json!({})),
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(json!({})),
        Err(e) => Err(e),
    }
}

fn write(path: &Path, settings: &Value) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    // Write beside and rename, so a crash mid write cannot leave Claude Code
    // with half a settings file.
    let tmp = path.with_extension("json.horadric-tmp");
    let mut text = serde_json::to_string_pretty(settings)?;
    text.push('\n');
    fs::write(&tmp, text)?;
    fs::rename(&tmp, path)
}

/// Installs the hooks into the user's settings file. Idempotent.
pub fn install(path: &Path, port: u16) -> io::Result<()> {
    let mut settings = read(path)?;
    install_into(&mut settings, port);
    write(path, &settings)
}

/// Removes the hooks from the user's settings file. Idempotent.
pub fn uninstall(path: &Path) -> io::Result<()> {
    let mut settings = read(path)?;
    remove_from(&mut settings);
    write(path, &settings)
}

/// Reports whether the user's settings file has the hooks.
pub fn status(path: &Path, port: u16) -> io::Result<bool> {
    Ok(is_installed(&read(path)?, port))
}

/// The Grok events Horadric hears. `Notification` carries the permission
/// prompt that is waiting, `StopCancelled` a Ctrl+C or a rejected prompt.
pub const GROK_EVENTS: &[&str] = &[
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "PostToolUseFailure",
    "Notification",
    "Stop",
    "StopFailure",
    "StopCancelled",
    "SessionEnd",
];

/// Grok's home: `$GROK_HOME`, or `~/.grok`.
pub fn grok_home() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("GROK_HOME").filter(|h| !h.is_empty()) {
        return Some(PathBuf::from(home));
    }
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))?;
    Some(Path::new(&home).join(".grok"))
}

/// Horadric's hook file in Grok home `home`.
pub fn grok_hooks_path(home: &Path) -> PathBuf {
    home.join("hooks").join("horadric.json")
}

/// The hook file that runs `command` on every event Horadric needs. `Stop`
/// is a gate Grok waits up to ten minutes for by default, so every entry
/// says five seconds: `horadric hook` gives up sooner anyway.
pub fn grok_hooks(command: &str) -> Value {
    let hooks: Map<String, Value> = GROK_EVENTS
        .iter()
        .map(|e| {
            let handler = json!({ "type": "command", "command": command, "timeout": 5 });
            (e.to_string(), json!([{ "hooks": [handler] }]))
        })
        .collect();
    json!({ "hooks": hooks })
}

/// Grok's own settings in Grok home `home`.
pub fn grok_config_path(home: &Path) -> PathBuf {
    home.join("config.toml")
}

/// The header of the table that gives Grok `horadric mcp`.
const GROK_MCP_TABLE: &str = "[mcp_servers.horadric]";

/// The table that gives Grok `horadric mcp`, run from `exe`. A page that is
/// slow to load is given as long as Codex gives it.
pub fn grok_mcp(exe: &str) -> String {
    format!(
        "{GROK_MCP_TABLE}\ncommand = {}\nargs = [\"mcp\"]\ntool_timeout_sec = 120\n",
        horadric_core::agent::toml_string(exe)
    )
}

/// Grok's `config` with Horadric's table taken out, and `table` put in at
/// the end when there is one. Only that table goes: it runs from its
/// header to the next one, and a table under it, like
/// `[mcp_servers.horadric.env]`, goes with it.
pub fn with_grok_mcp(config: &str, table: Option<&str>) -> String {
    let ours = |line: &str| {
        let line = line.trim();
        line == GROK_MCP_TABLE || line.starts_with("[mcp_servers.horadric.")
    };
    let mut kept: Vec<&str> = Vec::new();
    let mut inside = false;
    for line in config.lines() {
        if line.trim_start().starts_with('[') {
            inside = ours(line);
        }
        if !inside {
            kept.push(line);
        }
    }
    while kept.last().is_some_and(|l| l.trim().is_empty()) {
        kept.pop();
    }
    let mut out = kept.join("\n");
    if !out.is_empty() {
        out.push('\n');
    }
    if let Some(table) = table {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(table);
    }
    out
}

/// Puts `table` into Grok's config in `home`, or takes Horadric's out with
/// None. A missing config is made only to put the table in.
fn set_grok_mcp(home: &Path, table: Option<&str>) -> io::Result<()> {
    let path = grok_config_path(home);
    let config = match fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) if e.kind() == io::ErrorKind::NotFound && table.is_none() => return Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e),
    };
    let text = with_grok_mcp(&config, table);
    if text == config {
        return Ok(());
    }
    // Beside and rename, as for Claude Code's settings.
    let tmp = path.with_extension("toml.horadric-tmp");
    fs::write(&tmp, text)?;
    fs::rename(&tmp, path)
}

/// Writes Horadric's hook file into Grok home `home`, and the table that
/// gives Grok `horadric mcp` from `exe`, when Grok is there: a machine
/// without Grok gets no `~/.grok`. True when it wrote them. Idempotent, and
/// the hook file is Horadric's alone.
pub fn install_grok(home: &Path, command: &str, exe: &str) -> io::Result<bool> {
    if !home.is_dir() {
        return Ok(false);
    }
    write(&grok_hooks_path(home), &grok_hooks(command))?;
    set_grok_mcp(home, Some(&grok_mcp(exe)))?;
    Ok(true)
}

/// Removes Horadric's hook file and MCP table from Grok home `home`.
/// Idempotent.
pub fn uninstall_grok(home: &Path) -> io::Result<()> {
    if let Err(e) = fs::remove_file(grok_hooks_path(home)) {
        if e.kind() != io::ErrorKind::NotFound {
            return Err(e);
        }
    }
    set_grok_mcp(home, None)
}

/// What Claude Code's `/model` and `/effort` save into the user's settings
/// as they switch a session: typed in, as Horadric does, they switch this
/// session and also make the pick the default for every new one. Effort is
/// saved for the model in use, under `modelSettings`.
pub const SWITCHED: [&str; 3] = ["model", "effortLevel", "modelSettings"];

/// What `keys` hold in `settings`, None where one is missing.
pub fn pick(settings: &Value, keys: &[&str]) -> Vec<Option<Value>> {
    keys.iter().map(|k| settings.get(*k).cloned()).collect()
}

/// Puts `keys` back as [`pick`] found them. True when anything changed.
pub fn put_back(settings: &mut Value, keys: &[&str], was: &[Option<Value>]) -> bool {
    if pick(settings, keys) == was {
        return false;
    }
    let map = ensure_object(settings);
    for (k, v) in keys.iter().zip(was) {
        match v {
            Some(v) => map.insert(k.to_string(), v.clone()),
            None => map.remove(*k),
        };
    }
    true
}

/// What `keys` hold in the user's settings file now.
pub fn snapshot(path: &Path, keys: &[&str]) -> io::Result<Vec<Option<Value>>> {
    Ok(pick(&read(path)?, keys))
}

/// Puts `keys` in the user's settings file back as a [`snapshot`] had
/// them, writing only when something changed.
pub fn restore(path: &Path, keys: &[&str], was: &[Option<Value>]) -> io::Result<()> {
    let mut settings = read(path)?;
    if put_back(&mut settings, keys, was) {
        write(path, &settings)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_switch_saved_as_the_default_is_put_back() {
        let mut s = json!({"model": "opus", "hooks": {}});
        let was = pick(&s, &SWITCHED);
        assert_eq!(was, [Some(json!("opus")), None, None]);
        assert!(!put_back(&mut s, &SWITCHED, &was));
        s["model"] = json!("claude-haiku-4-5-20251001");
        s["modelSettings"] = json!({"claude-haiku-4-5": {"effortLevel": "low"}});
        assert!(put_back(&mut s, &SWITCHED, &was));
        assert_eq!(s, json!({"model": "opus", "hooks": {}}));
    }

    #[test]
    fn install_is_idempotent_and_removable() {
        let mut s = json!({
            "model": "opus",
            "hooks": {
                "Stop": [ { "hooks": [ { "type": "command", "command": "echo mine" } ] } ]
            }
        });
        install_into(&mut s, 43117);
        install_into(&mut s, 43117);
        assert!(is_installed(&s, 43117));
        // The user's own Stop hook is untouched and ours sits beside it.
        let stop = s.pointer("/hooks/Stop").unwrap().as_array().unwrap();
        assert_eq!(stop.len(), 2);
        assert_eq!(stop[0]["hooks"][0]["command"], "echo mine");

        remove_from(&mut s);
        assert!(!is_installed(&s, 43117));
        assert_eq!(
            s.pointer("/hooks/Stop").unwrap().as_array().unwrap().len(),
            1
        );
        assert!(s.pointer("/hooks/Notification").is_none());
        assert_eq!(s["model"], "opus");
    }

    #[test]
    fn port_change_replaces_old_entries() {
        let mut s = json!({});
        install_into(&mut s, 1000);
        install_into(&mut s, 2000);
        assert!(!is_installed(&s, 1000));
        assert!(is_installed(&s, 2000));
        let stop = s.pointer("/hooks/Stop").unwrap().as_array().unwrap();
        assert_eq!(stop.len(), 1);
    }

    #[test]
    fn remove_on_empty_is_fine() {
        let mut s = json!({ "model": "x" });
        remove_from(&mut s);
        assert_eq!(s, json!({ "model": "x" }));
    }

    #[test]
    fn grok_gets_a_command_hook_on_every_event() {
        let f = grok_hooks("C:/h/horadric.exe hook grok");
        let hooks = f["hooks"].as_object().unwrap();
        assert_eq!(hooks.len(), GROK_EVENTS.len());
        let h = &f["hooks"]["StopCancelled"][0]["hooks"][0];
        assert_eq!(h["type"], "command");
        assert_eq!(h["command"], "C:/h/horadric.exe hook grok");
        assert_eq!(h["timeout"], 5);
        assert!(hooks.contains_key("Notification"));
    }

    #[test]
    fn grok_hooks_go_only_where_grok_is_and_come_out_again() {
        let dir = std::env::temp_dir().join(format!("horadric-grok-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        assert!(!install_grok(&dir, "h", "x").unwrap());
        assert!(!dir.exists());
        fs::create_dir_all(&dir).unwrap();
        fs::write(grok_config_path(&dir), "[ui]\nyolo = false\n").unwrap();
        assert!(install_grok(&dir, "h", "x").unwrap());
        assert!(install_grok(&dir, "h", "x").unwrap());
        let written: Value =
            serde_json::from_slice(&fs::read(grok_hooks_path(&dir)).unwrap()).unwrap();
        assert_eq!(written, grok_hooks("h"));
        let config = fs::read_to_string(grok_config_path(&dir)).unwrap();
        assert_eq!(config, format!("[ui]\nyolo = false\n\n{}", grok_mcp("x")));
        uninstall_grok(&dir).unwrap();
        uninstall_grok(&dir).unwrap();
        assert!(!grok_hooks_path(&dir).exists());
        let config = fs::read_to_string(grok_config_path(&dir)).unwrap();
        assert_eq!(config, "[ui]\nyolo = false\n");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn grok_mcp_runs_horadric_mcp() {
        assert_eq!(
            grok_mcp(r"C:\h\horadric.exe"),
            "[mcp_servers.horadric]\n\
             command = \"C:\\\\h\\\\horadric.exe\"\n\
             args = [\"mcp\"]\n\
             tool_timeout_sec = 120\n"
        );
    }

    #[test]
    fn grok_mcp_goes_in_once_and_leaves_the_rest() {
        let table = "[mcp_servers.horadric]\ncommand = \"h\"\n";
        assert_eq!(with_grok_mcp("", Some(table)), table);
        assert_eq!(with_grok_mcp("", None), "");
        let config = "[cli]\ninstaller = \"internal\"\n\n\
                      [mcp_servers.horadric]\ncommand = \"old\"\n\n\
                      [mcp_servers.horadric.env]\nA = \"1\"\n\n\
                      [mcp_servers.github]\ncommand = \"gh\"\n";
        let rest = "[cli]\ninstaller = \"internal\"\n\n[mcp_servers.github]\ncommand = \"gh\"\n";
        assert_eq!(with_grok_mcp(config, None), rest);
        let put = with_grok_mcp(config, Some(table));
        assert_eq!(put, format!("{rest}\n{table}"));
        assert_eq!(with_grok_mcp(&put, Some(table)), put);
        // A table whose name only starts the same is someone else's.
        let theirs = "[mcp_servers.horadric2]\ncommand = \"z\"\n";
        assert_eq!(with_grok_mcp(theirs, None), theirs);
    }

    #[test]
    fn handler_carries_session_header() {
        let h = handler(43117);
        assert_eq!(h["type"], "http");
        assert_eq!(h["headers"]["X-Horadric-Session"], "$HORADRIC_SESSION");
        assert_eq!(h["allowedEnvVars"][0], "HORADRIC_SESSION");
    }

    #[test]
    fn handler_carries_owner_port_header() {
        let h = handler(43117);
        assert_eq!(h["headers"]["X-Horadric-Port"], "$HORADRIC_OWNER_PORT");
        assert_eq!(h["allowedEnvVars"][1], "HORADRIC_OWNER_PORT");
    }
}
