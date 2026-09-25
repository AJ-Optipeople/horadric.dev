//! `horadric serve`: the state stream as a live table in the terminal.
//!
//! Redraws when an event arrives and once a second so the age column moves.
//! This is the whole product with the tiles removed. If this is useful on its
//! own, the tiles are worth building.

use std::io::{self, Write};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, SystemTime};

use horadric_core::{format_age, Phase, Registry};
use horadric_hooks::listener::{self, Tagged};

pub fn serve() -> Result<(), String> {
    let port = horadric_hooks::port();
    let (tx, rx) = mpsc::channel::<Tagged>();

    // Bind on this thread so a port clash is reported before we clear the screen.
    let (ready_tx, ready_rx) = mpsc::channel::<io::Result<()>>();
    thread::spawn(move || {
        let r = listener::serve(port, tx, None);
        let _ = ready_tx.send(r);
    });
    if let Ok(Err(e)) = ready_rx.recv_timeout(Duration::from_millis(300)) {
        return Err(format!("cannot listen on 127.0.0.1:{port}: {e}"));
    }

    let mut registry = Registry::new();
    let mut log: Vec<String> = Vec::new();
    draw(&registry, &log, port)?;

    loop {
        match rx.recv_timeout(Duration::from_secs(1)) {
            // Nobody's conversation. The tiles ask whether it is a
            // background session; this table has no tiles.
            Ok(t) if t.horadric_id.is_empty() => {}
            Ok(t) => {
                let now = SystemTime::now();
                let changed = registry.apply(&t.horadric_id, &t.event, now);
                let detail = t
                    .event
                    .notification_type
                    .as_deref()
                    .or(t.event.tool_name.as_deref())
                    .unwrap_or("");
                log.push(format!(
                    "{:<8} {:<18} {:<20} {}",
                    stamp(now),
                    t.horadric_id,
                    t.event.hook_event_name,
                    if changed {
                        format!(
                            "{detail} -> {}",
                            registry
                                .get(&t.horadric_id)
                                .map(|s| s.phase.label())
                                .unwrap_or("?")
                        )
                    } else {
                        detail.to_string()
                    }
                ));
                if log.len() > 8 {
                    log.remove(0);
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return Err("listener stopped".into()),
        }
        draw(&registry, &log, port)?;
    }
}

fn draw(registry: &Registry, log: &[String], port: u16) -> Result<(), String> {
    let mut out = io::stdout().lock();
    let mut s = String::new();
    // Clear screen, home cursor.
    s.push_str("\x1b[2J\x1b[H");
    s.push_str(&format!(
        "horadric  listening on 127.0.0.1:{port}   {} sessions, {} waiting\n\n",
        registry.len(),
        registry.waiting().len()
    ));
    s.push_str(&format!(
        "  {:<22} {:<12} {:<10} {}\n",
        "SESSION", "STATE", "AGE", "LAST"
    ));
    for sess in registry.all() {
        let mark = match &sess.phase {
            Phase::Waiting(_) => "\x1b[33m*\x1b[0m",
            Phase::Working => "\x1b[36m~\x1b[0m",
            Phase::Done => "\x1b[32m+\x1b[0m",
            _ => " ",
        };
        s.push_str(&format!(
            "{mark} {:<22} {:<12} {:<10} {}\n",
            truncate(sess.label(), 22),
            sess.phase.label(),
            format_age(sess.age()),
            truncate(&sess.last_line, 60)
        ));
    }
    if registry.is_empty() {
        s.push_str(
            "  (no sessions yet: run `horadric run` in a project, or `horadric hooks install` first)\n",
        );
    }
    if !log.is_empty() {
        s.push_str("\nrecent events\n");
        for l in log {
            s.push_str("  ");
            s.push_str(l);
            s.push('\n');
        }
    }
    out.write_all(s.as_bytes()).map_err(|e| e.to_string())?;
    out.flush().map_err(|e| e.to_string())
}

fn stamp(t: SystemTime) -> String {
    let s = t
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Local time is not worth a dependency here; UTC clock is fine for a log.
    format!("{:02}:{:02}:{:02}", (s / 3600) % 24, (s / 60) % 60, s % 60)
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let cut: String = s.chars().take(n - 1).collect();
        format!("{cut}\u{2026}")
    }
}
