//! `horadric hook <agent>`: the command hook Codex and Grok post through.
//!
//! Neither can post to Horadric itself. Codex has command hooks only, and
//! Grok refuses every `http://` URL, loopback included. So each runs this
//! once per event with the payload on stdin, and it posts that on to the
//! Horadric that owns the session, tagged and named, for the listener to
//! read in the agent's shape. It is the one place Horadric costs a process
//! per event, so it does as little as it can and never fails the agent.

use std::io::Read;

use horadric_core::Agent;
use horadric_hooks::{client, AGENT_HEADER, HOOK_PATH, OWNER_ENV, SESSION_ENV, SESSION_HEADER};

pub fn run(args: &[String]) -> Result<(), String> {
    // Read to the end even when there is nothing to do, so the agent never
    // meets a closed pipe.
    let mut body = Vec::new();
    let _ = std::io::stdin().read_to_end(&mut body);
    let session = std::env::var(SESSION_ENV).unwrap_or_default();
    if session.is_empty() {
        // An agent started outside Horadric.
        return Ok(());
    }
    let Some(agent) = args.first().and_then(|a| Agent::from_name(a)) else {
        // Failing here would fail the agent's hook, which is worse than a
        // tile that does not move. Said on stderr, where the agent logs it.
        eprintln!("horadric hook: usage: horadric hook codex|grok");
        return Ok(());
    };
    let owner = std::env::var(OWNER_ENV)
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or_else(horadric_hooks::port);
    // Nobody listening, or a slow reply, costs the agent nothing but the
    // client's timeouts.
    let headers = [
        (SESSION_HEADER, session.as_str()),
        (AGENT_HEADER, agent.program()),
    ];
    let _ = client::post(owner, HOOK_PATH, &headers, &String::from_utf8_lossy(&body));
    Ok(())
}
