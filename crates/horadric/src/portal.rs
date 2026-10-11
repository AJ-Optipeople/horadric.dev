//! `horadric portal`: Town Portal, Horadric from a phone. Turns it on or
//! off, shows a one-time pairing code, and lists and forgets the paired
//! phones. The running app looks at `portal.json` and connects to the
//! relay to match. See `docs/PORTAL.md`.

use std::path::Path;
use std::time::Duration;

use horadric_core::portal::{Config, Subs, TICKET_SECS};
use horadric_ui::portal_pair::{now, pair_a_phone, pc_name, random32};

pub fn run(args: &[String]) -> Result<(), String> {
    let dir = horadric_hooks::state_dir().ok_or("cannot find %APPDATA%")?;
    let config = Config::load(&dir);
    let save = |c: &Config| {
        c.save(&dir)
            .map_err(|e| format!("cannot write {}: {e}", dir.join("portal.json").display()))
    };
    match args.first().map(String::as_str) {
        None | Some("pair") => {
            let c = pair_a_phone(&dir)?;
            println!("Town Portal is on, through {}.", c.relay);
            println!(
                "Scan the QR code in the browser with your phone, or open this link on it. \
                 It works once, for {} minutes:\n",
                TICKET_SECS / 60
            );
            println!("{}\n", c.link(&pc_name()).unwrap_or_default());
            wait_for_phone(&dir, c.phones.len());
        }
        Some("off") => {
            if let Some(mut c) = config {
                c.enabled = false;
                c.ticket = None;
                save(&c)?;
            }
            println!("Town Portal is off.");
        }
        Some("phones") => {
            let c = config.ok_or("Town Portal was never turned on")?;
            if c.phones.is_empty() {
                println!("No phone is paired. Pair one with `horadric portal`.");
            }
            let now = now();
            for (i, p) in c.phones.iter().enumerate() {
                println!(
                    "{}. {}, paired {}, last seen {}",
                    i + 1,
                    p.name,
                    ago(now, p.paired),
                    ago(now, p.seen)
                );
            }
        }
        Some("forget") => {
            let mut c = config.ok_or("Town Portal was never turned on")?;
            let which = args
                .get(1)
                .ok_or("usage: horadric portal forget NUMBER|all (see horadric portal phones)")?;
            let gone = forget(&mut c, which)?;
            save(&c)?;
            if which == "all" {
                Subs::clear(&dir).map_err(|e| format!("cannot forget the phones' push: {e}"))?;
            }
            println!("Forgotten: {gone}. It is cut off now, if Horadric is running.");
        }
        // The name from before phones had keys of their own.
        Some("rekey") => {
            let mut c = config.ok_or("Town Portal was never turned on")?;
            forget(&mut c, "all")?;
            save(&c)?;
            Subs::clear(&dir).map_err(|e| format!("cannot forget the phones' push: {e}"))?;
            println!("Every phone is forgotten. Pair again with `horadric portal`.");
        }
        Some("relay") => {
            let mut c = config.unwrap_or_else(fresh);
            let url = args
                .get(1)
                .ok_or("usage: horadric portal relay wss://HOST")?;
            if !(url.starts_with("wss://") || url.starts_with("ws://")) {
                return Err(format!("`{url}` is not a ws:// or wss:// address"));
            }
            c.relay = url.trim_end_matches('/').to_string();
            // Each phone remembers the relay it paired through.
            c.phones.clear();
            c.ticket = None;
            save(&c)?;
            Subs::clear(&dir).map_err(|e| format!("cannot forget the phones' push: {e}"))?;
            println!(
                "Town Portal goes through {} now. Pair your phones again with `horadric portal`.",
                c.relay
            );
        }
        Some(other) => return Err(format!("unknown portal command `{other}`")),
    }
    Ok(())
}

/// Takes the phones out that `which` names, a number from `horadric
/// portal phones` or `all`, and says which went.
fn forget(c: &mut Config, which: &str) -> Result<String, String> {
    if which == "all" {
        let n = c.phones.len();
        c.phones.clear();
        c.ticket = None;
        return Ok(format!("{n} phone{}", if n == 1 { "" } else { "s" }));
    }
    let i = which
        .parse::<usize>()
        .ok()
        .filter(|&i| i >= 1 && i <= c.phones.len())
        .ok_or_else(|| format!("no phone {which}; see horadric portal phones"))?;
    Ok(c.phones.remove(i - 1).name)
}

/// Waits while the code holds and says when a phone paired, so the human
/// sees at once if a phone they do not know took it.
fn wait_for_phone(dir: &Path, before: usize) {
    println!("Waiting for a phone...");
    loop {
        std::thread::sleep(Duration::from_secs(1));
        let Some(c) = Config::load(dir) else {
            return;
        };
        if c.phones.len() > before {
            if let Some(p) = c.phones.last() {
                println!("Paired: {}.", p.name);
                println!("Not yours? `horadric portal phones`, then `horadric portal forget N`.");
            }
            return;
        }
        match &c.ticket {
            Some(t) if now() < t.until => {}
            _ => {
                println!("The code ran out unused. Run `horadric portal` for a new one.");
                return;
            }
        }
    }
}

/// How long ago `then` was, in words.
fn ago(now: u64, then: u64) -> String {
    let secs = now.saturating_sub(then);
    let (n, unit) = match secs {
        0..120 => return "just now".into(),
        120..7200 => (secs / 60, "minutes"),
        7200..172_800 => (secs / 3600, "hours"),
        _ => (secs / 86_400, "days"),
    };
    format!("{n} {unit} ago")
}

fn fresh() -> Config {
    Config::fresh(&random32())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ago_says_it_in_the_largest_unit_that_fits() {
        assert_eq!(ago(100, 100), "just now");
        assert_eq!(ago(100, 200), "just now");
        assert_eq!(ago(600, 0), "10 minutes ago");
        assert_eq!(ago(3 * 3600, 0), "3 hours ago");
        assert_eq!(ago(5 * 86_400, 0), "5 days ago");
    }

    #[test]
    fn forget_takes_one_by_number_or_all() {
        let mut c = Config::fresh(&[0; 32]);
        for (i, name) in ["a", "b", "c"].iter().enumerate() {
            c.open_ticket(&[9; 32], 0);
            c.pair(name, &[i as u8; 40], 0);
        }
        assert_eq!(forget(&mut c, "2"), Ok("b".to_string()));
        assert_eq!(c.phones.len(), 2);
        assert!(forget(&mut c, "0").is_err());
        assert!(forget(&mut c, "3").is_err());
        assert!(forget(&mut c, "x").is_err());
        assert_eq!(forget(&mut c, "all"), Ok("2 phones".to_string()));
        assert!(c.phones.is_empty() && c.ticket.is_none());
    }
}
