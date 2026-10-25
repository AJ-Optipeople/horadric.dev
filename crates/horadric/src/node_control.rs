//! The human at a node, who sits at a terminal rather than in front of it.
//! The node runs under systemd with no terminal, so whatever needs a human
//! (joining a brother, letting one in, casting one out) is a `horadric
//! node` command in a terminal that speaks to the running node over a
//! socket of its own, beside the viewers' one: lines of JSON both ways.
//!
//! Joining asks a human on both sides whether the two six digit codes
//! match, No by default. A node never lets anyone in by itself: a join
//! asked of it while no `horadric node let-in` is attached is refused, and
//! a code shown to a terminal that goes away is answered no.

use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use horadric_core::joining::Store;
use horadric_core::keeping::Book;
use horadric_core::standing::Standing;
use serde_json::{json, Value};

use super::{keeping as node_keeping, relay, signin};

/// The socket's name, beside the viewers' socket in the user's private
/// folder.
fn path() -> io::Result<PathBuf> {
    Ok(horadric_pty::pipe::socket_dir()?
        .join(format!("horadric-node-ctl-{}", horadric_hooks::instance())))
}

/// Who is attached and what each was shown.
#[derive(Default)]
struct Ctl {
    next: u64,
    conns: HashMap<u64, Sender<Value>>,
    /// The terminal running `horadric node let-in`.
    approver: Option<u64>,
    /// The terminal that asked a brother to let this node in.
    asker: Option<u64>,
    /// Codes shown and not yet answered: the channel, to the terminal.
    prompts: HashMap<u32, u64>,
}

static CTL: Mutex<Option<Ctl>> = Mutex::new(None);

/// Never held while the brothers' lock is taken: the brothers call
/// [`Human`] with theirs held.
fn ctl() -> MutexGuard<'static, Option<Ctl>> {
    CTL.lock().unwrap_or_else(|e| e.into_inner())
}

/// Says no on channel `ch`, from a thread of its own, since the brothers
/// ask with their lock held and take the answer under it.
fn no_later(ch: u32) {
    thread::spawn(move || {
        relay::with(|b| b.answer(ch, false));
    });
}

/// The human, as the brothers see it: whoever is attached.
pub(super) struct Human;

impl Human {
    fn show(&self, ch: u32, to: Option<u64>, msg: Value) -> bool {
        let mut held = ctl();
        let Some(c) = held.as_mut() else {
            return false;
        };
        let Some(conn) = to.filter(|t| c.conns.contains_key(t)) else {
            return false;
        };
        c.prompts.insert(ch, conn);
        c.conns.get(&conn).is_some_and(|tx| tx.send(msg).is_ok())
    }
}

impl horadric_core::brothers::Human for Human {
    fn let_in(&self, ch: u32, name: &str, code: &str) {
        let approver = ctl().as_ref().and_then(|c| c.approver);
        let msg = json!({"ev": "let_in", "ch": ch, "name": name, "code": code});
        if !self.show(ch, approver, msg) {
            eprintln!(
                "horadric node: {name} asked to join, and no one is at a terminal to say yes \
                 (`horadric node let-in`): refused"
            );
            no_later(ch);
        }
    }

    fn confirm(&self, ch: u32, name: &str, code: &str) {
        let asker = ctl().as_ref().and_then(|c| c.asker);
        let msg = json!({"ev": "confirm", "ch": ch, "name": name, "code": code});
        if !self.show(ch, asker, msg) {
            eprintln!("horadric node: {name} showed a code, and no one is here to compare it");
            no_later(ch);
        }
    }

    fn tell(&self, text: &str) {
        eprintln!("horadric node: {text}");
        if let Some(c) = ctl().as_ref() {
            for conn in [c.approver, c.asker].into_iter().flatten() {
                if let Some(tx) = c.conns.get(&conn) {
                    let _ = tx.send(json!({"ev": "tell", "text": text}));
                }
            }
        }
    }
}

/// Listens on the control socket for as long as the node runs.
pub(super) fn serve() -> Result<(), String> {
    let path = path().map_err(|e| format!("no folder for the control socket: {e}"))?;
    // The viewers' socket, bound first, holds the lock that says this is
    // the only node: what is here was left by one that is gone.
    let _ = std::fs::remove_file(&path);
    let listener =
        UnixListener::bind(&path).map_err(|e| format!("cannot serve {}: {e}", path.display()))?;
    let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    *ctl() = Some(Ctl::default());
    thread::Builder::new()
        .name("node-control".into())
        .spawn(move || {
            for stream in listener.incoming().flatten() {
                thread::spawn(move || attached(stream));
            }
        })
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// One terminal, from its first line to its last.
fn attached(stream: UnixStream) {
    let Ok(writer) = stream.try_clone() else {
        return;
    };
    let (tx, rx) = mpsc::channel::<Value>();
    let conn = {
        let mut held = ctl();
        let Some(c) = held.as_mut() else {
            return;
        };
        c.next += 1;
        c.conns.insert(c.next, tx.clone());
        c.next
    };
    thread::spawn(move || write_lines(writer, rx));
    for line in BufReader::new(stream).lines() {
        let Ok(line) = line else {
            break;
        };
        let Ok(req) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let reply = request(conn, &req);
        if tx.send(reply).is_err() {
            break;
        }
    }
    // Gone: what it was shown is answered no.
    let shown: Vec<u32> = {
        let mut held = ctl();
        let Some(c) = held.as_mut() else {
            return;
        };
        c.conns.remove(&conn);
        if c.approver == Some(conn) {
            c.approver = None;
        }
        if c.asker == Some(conn) {
            c.asker = None;
        }
        let shown = c
            .prompts
            .iter()
            .filter(|(_, t)| **t == conn)
            .map(|(ch, _)| *ch)
            .collect();
        c.prompts.retain(|_, t| *t != conn);
        shown
    };
    for ch in shown {
        no_later(ch);
    }
}

fn write_lines(mut w: UnixStream, rx: Receiver<Value>) {
    for msg in rx {
        let mut line = msg.to_string();
        line.push('\n');
        if w.write_all(line.as_bytes()).is_err() {
            return;
        }
    }
}

/// Answers one request from terminal `conn`.
fn request(conn: u64, req: &Value) -> Value {
    let text = |k: &str| req[k].as_str().unwrap_or_default().to_string();
    match req["do"].as_str().unwrap_or_default() {
        "status" => status(),
        "renew" => {
            relay::renew();
            json!({"re": "renew"})
        }
        "ask" => {
            let to = text("to");
            if let Some(c) = ctl().as_mut() {
                c.asker = Some(conn);
            }
            relay::with(|b| b.ask(&to));
            json!({"re": "ask"})
        }
        "let-in" => {
            if let Some(c) = ctl().as_mut() {
                c.approver = Some(conn);
            }
            json!({"re": "let-in"})
        }
        "answer" => {
            let ch = req["ch"].as_u64().unwrap_or(0) as u32;
            let mine = ctl()
                .as_mut()
                .is_some_and(|c| c.prompts.remove(&ch) == Some(conn));
            if mine {
                let yes = req["yes"] == true;
                relay::with(|b| b.answer(ch, yes));
            }
            json!({"re": "answer"})
        }
        "cast-out" => match cast_out(&text("id")) {
            Ok(()) => json!({"re": "cast-out"}),
            Err(e) => json!({"re": "cast-out", "error": e}),
        },
        "control" => match set_control(&text("id"), req["on"] == true) {
            Ok(()) => json!({"re": "control"}),
            Err(e) => json!({"re": "control", "error": e}),
        },
        "keeper" => {
            let change = node_keeping::Keepers {
                keeper: req.get("keeper").map(|k| k.as_str().map(str::to_string)),
                auto: req["auto"].as_bool(),
            };
            match node_keeping::ask_keepers(change) {
                Ok((keeper, auto)) => json!({"re": "keeper", "keeper": keeper, "auto": auto}),
                Err(e) => json!({"re": "keeper", "error": e}),
            }
        }
        other => json!({"re": other, "error": "not something a node does"}),
    }
}

/// What `horadric node brothers` shows, and what a terminal waiting on a
/// join reads to see it through.
fn status() -> Value {
    let store = relay::store();
    let listed: Vec<Value> = relay::listed()
        .unwrap_or_default()
        .into_iter()
        .map(|l| json!({"id": l.id, "name": l.name, "joined": l.joined, "linked": l.linked, "old": l.old}))
        .collect();
    let dir = horadric_hooks::state_dir();
    let standing = dir.as_deref().map(Standing::load).unwrap_or_default();
    let book = dir.as_deref().map(Book::load).unwrap_or_default();
    let joined: Vec<Value> = store
        .brothers
        .iter()
        .map(|b| {
            json!({
                "id": b.id, "name": b.name, "seen": b.seen,
                "control": standing.controls(&b.id),
            })
        })
        .collect();
    json!({
        "re": "status",
        "user": signin::user().map(|u| u.email),
        "up": relay::up(),
        "me": relay::me(),
        "listed": listed,
        "joined": joined,
        "keeper": book.keeper(),
        "auto": book.auto_takeover.is_some(),
    })
}

/// Casts brother `id` out: refused from now on here and, by the roster,
/// on every brother; its key gone, its links closed. Works with no relay
/// link up too, from the files alone.
fn cast_out(id: &str) -> Result<(), String> {
    let dir = horadric_hooks::state_dir().ok_or("cannot find the state folder")?;
    let store = Store::load(&dir).unwrap_or_default();
    if store.get(id).is_none() {
        return Err(format!("{id} is no brother of this node"));
    }
    if relay::with(|b| b.cast_out(id)).is_none() {
        horadric_core::brothers::cast_out_offline(&dir, id).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn set_control(id: &str, on: bool) -> Result<(), String> {
    let dir = horadric_hooks::state_dir().ok_or("cannot find the state folder")?;
    let mut standing = Standing::load(&dir);
    standing.set_control(id, on);
    standing.save(&dir).map_err(|e| e.to_string())
}

// The terminal's side.

/// A terminal attached to the running node.
struct Line {
    w: UnixStream,
    r: BufReader<UnixStream>,
}

impl Line {
    fn open() -> Result<Line, String> {
        let path = path().map_err(|e| e.to_string())?;
        let w = UnixStream::connect(&path).map_err(|_| {
            "no node runs here: start it with `systemctl --user start horadric`, \
             or `horadric node install`"
                .to_string()
        })?;
        let r = BufReader::new(w.try_clone().map_err(|e| e.to_string())?);
        Ok(Line { w, r })
    }

    /// Opens it, waiting up to `secs` for a node that is starting, as
    /// one just installed or restarted is.
    fn open_waiting(secs: u64) -> Result<Line, String> {
        let until = Instant::now() + Duration::from_secs(secs);
        loop {
            match Line::open() {
                Ok(line) => return Ok(line),
                Err(e) if Instant::now() > until => return Err(e),
                Err(_) => thread::sleep(Duration::from_millis(300)),
            }
        }
    }

    fn send(&mut self, req: Value) -> Result<(), String> {
        let mut line = req.to_string();
        line.push('\n');
        self.w
            .write_all(line.as_bytes())
            .map_err(|e| format!("the node went away: {e}"))
    }

    /// The next line the node says, None once it went.
    fn next(&mut self) -> Option<Value> {
        let mut line = String::new();
        match self.r.read_line(&mut line) {
            Ok(0) | Err(_) => None,
            Ok(_) => serde_json::from_str(&line).ok(),
        }
    }

    /// Sends `req` and gives the answer to it, telling on the way what
    /// came before it.
    fn ask(&mut self, req: Value) -> Result<Value, String> {
        let what = req["do"].clone();
        self.send(req)?;
        loop {
            let v = self.next().ok_or("the node went away")?;
            if v["re"] == what {
                return match v["error"].as_str() {
                    Some(e) => Err(e.to_string()),
                    None => Ok(v),
                };
            }
            if let Some(t) = v["text"].as_str() {
                println!("{t}");
            }
        }
    }
}

/// Asks the human at this terminal, No unless they say yes.
fn yes(question: &str) -> bool {
    print!("{question} [y/N] ");
    let _ = io::stdout().flush();
    let mut answer = String::new();
    if io::stdin().read_line(&mut answer).is_err() {
        return false;
    }
    matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

/// `horadric node signin [--to <name>]`: signs in, then asks a signed in
/// brother to let this node in.
pub(super) fn signin_command(args: &[String]) -> Result<(), String> {
    let to = flag(args, "--to");
    match signin::user() {
        Some(u) => println!("Signed in as {}.", u.email),
        None => {
            let u = signin::device()?;
            println!("Signed in as {}.", u.email);
        }
    }
    let mut line = Line::open_waiting(10)?;
    line.ask(json!({"do": "renew"}))?;
    let status = wait_for_relay(&mut line)?;
    let joined: Vec<(String, String)> = pairs(&status["joined"]);
    let listed: Vec<(String, String, bool)> = status["listed"]
        .as_array()
        .map(|l| {
            l.iter()
                .map(|b| {
                    (
                        b["id"].as_str().unwrap_or_default().to_string(),
                        b["name"].as_str().unwrap_or_default().to_string(),
                        b["joined"] == true,
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    let fresh: Vec<(String, String)> = listed
        .iter()
        .filter(|(_, _, j)| !j)
        .map(|(i, n, _)| (i.clone(), n.clone()))
        .collect();
    let pick = match &to {
        Some(name) => Some(by_name(name, &fresh).map_err(|e| {
            if by_name(name, &joined).is_ok() {
                format!("{name} and this node are brothers already.")
            } else {
                e
            }
        })?),
        None if !joined.is_empty() && fresh.is_empty() => {
            let names: Vec<&str> = joined.iter().map(|(_, n)| n.as_str()).collect();
            println!("This node is a brother of {} already.", names.join(", "));
            return Ok(());
        }
        None => match fresh.len() {
            0 if joined.is_empty() => {
                println!(
                    "No other machine of yours is signed in right now. Sign in on your PC \
                     (the tray's Sign in...), then run `horadric node signin` again, \
                     or let it ask this node to join (`horadric node let-in`)."
                );
                return Ok(());
            }
            0 => return Ok(()),
            1 => Some(fresh[0].clone()),
            _ => Some(choose(&fresh)?),
        },
    };
    let Some((id, name)) = pick else {
        return Ok(());
    };
    println!("Asking {name} to let this node in...");
    ask_and_follow(&mut line, &id, &name)
}

/// Waits up to twenty seconds for the node to reach the relay and hear
/// its list, and gives its status.
fn wait_for_relay(line: &mut Line) -> Result<Value, String> {
    let until = Instant::now() + Duration::from_secs(20);
    loop {
        let status = line.ask(json!({"do": "status"}))?;
        if status["up"] == true && status["listed"].is_array() {
            // The list comes a moment after the link.
            thread::sleep(Duration::from_millis(800));
            return line.ask(json!({"do": "status"}));
        }
        if Instant::now() > until {
            return Err(
                "the node could not reach the relay; see `journalctl --user -u horadric`".into(),
            );
        }
        thread::sleep(Duration::from_millis(500));
    }
}

/// Asks brother `id` and sees the join through: the code to compare here
/// and on the brother, the answer, then whether it holds.
fn ask_and_follow(line: &mut Line, id: &str, name: &str) -> Result<(), String> {
    line.ask(json!({"do": "ask", "to": id}))?;
    let until = Instant::now() + Duration::from_secs(600);
    while Instant::now() < until {
        let Some(ev) = line.next() else {
            return Err("the node went away".into());
        };
        match ev["ev"].as_str() {
            Some("confirm") => {
                let code = ev["code"].as_str().unwrap_or_default();
                println!();
                println!("{name} asks its human whether this node may join. It shows a code;");
                println!("so does this terminal:");
                println!();
                println!("    {code}");
                println!();
                println!("Say yes only if {name}'s screen shows exactly this code. If it shows");
                println!("another, or none, something between the two may be pretending to be");
                println!("{name}: say no.");
                let ok = yes(&format!("Does {name} show {code}?"));
                line.send(json!({"do": "answer", "ch": ev["ch"], "yes": ok}))?;
                if !ok {
                    return Err("not joined: the codes did not match".into());
                }
                println!("Waiting for {name}'s human to say yes...");
            }
            Some("tell") => {
                println!("{}", ev["text"].as_str().unwrap_or_default());
                let status = line.ask(json!({"do": "status"}))?;
                return if pairs(&status["joined"]).iter().any(|(i, _)| i == id) {
                    println!("This node is a brother of {name} now, and links to it by itself.");
                    Ok(())
                } else {
                    Err(format!("not joined to {name}"))
                };
            }
            _ => {}
        }
    }
    Err(format!("{name} did not answer"))
}

/// `horadric node let-in`: waits for a machine to ask this node, shows
/// its code and asks whether to let it in.
pub(super) fn let_in_command() -> Result<(), String> {
    let mut line = Line::open()?;
    line.ask(json!({"do": "let-in"}))?;
    println!("Waiting for one of your machines to ask this node to join (Ctrl+C stops)...");
    println!("On a PC: the tray's Brothers menu, this node, Ask to join.");
    loop {
        let Some(ev) = line.next() else {
            return Err("the node went away".into());
        };
        match ev["ev"].as_str() {
            Some("let_in") => {
                let name = ev["name"].as_str().unwrap_or("A machine");
                let code = ev["code"].as_str().unwrap_or_default();
                println!();
                println!("{name} wants to join. It shows a code; so does this terminal:");
                println!();
                println!("    {code}");
                println!();
                println!("Say yes only if {name} shows exactly this code. A brother sees this");
                println!("node's sessions and can type into them.");
                let ok = yes(&format!("Let {name} in?"));
                line.send(json!({"do": "answer", "ch": ev["ch"], "yes": ok}))?;
                if !ok {
                    println!("Not let in.");
                    return Ok(());
                }
            }
            Some("tell") => {
                println!("{}", ev["text"].as_str().unwrap_or_default());
                return Ok(());
            }
            _ => {}
        }
    }
}

/// `horadric node brothers`: who this node is signed in as, and every
/// brother with how it is linked.
pub(super) fn brothers_command() -> Result<(), String> {
    let status = match Line::open() {
        Ok(mut line) => line.ask(json!({"do": "status"}))?,
        Err(e) => {
            println!("{e}");
            return Ok(());
        }
    };
    match status["user"].as_str() {
        Some(u) => println!("Signed in as {u}."),
        None => println!("Not signed in: `horadric node signin`."),
    }
    println!(
        "The relay is {}.",
        if status["up"] == true {
            "reached"
        } else {
            "not reached"
        }
    );
    let listed = status["listed"].as_array().cloned().unwrap_or_default();
    let mut rows: Vec<(String, String)> = Vec::new();
    for b in status["joined"].as_array().cloned().unwrap_or_default() {
        let id = b["id"].as_str().unwrap_or_default();
        let linked = listed.iter().any(|l| l["id"] == id && l["linked"] == true);
        let old = listed.iter().any(|l| l["id"] == id && l["old"] == true);
        let mut state = match (linked, old) {
            (true, _) => "linked",
            (false, true) => "needs a newer Horadric",
            (false, false) => "out of reach",
        }
        .to_string();
        if b["control"] == false {
            state.push_str(", view only");
        }
        if status["keeper"].as_str() == Some(id) {
            state.push_str(", keeps this node's sessions");
            if status["auto"] == true {
                state.push_str(" and takes over by itself");
            }
        }
        rows.push((b["name"].as_str().unwrap_or(id).to_string(), state));
    }
    for l in listed.iter().filter(|l| l["joined"] != true) {
        rows.push((
            l["name"].as_str().unwrap_or_default().to_string(),
            "signed in, not a brother (`horadric node signin --to <name>`)".to_string(),
        ));
    }
    if rows.is_empty() {
        println!("No brothers yet.");
    }
    for (name, state) in rows {
        println!("  {name}\t{state}");
    }
    Ok(())
}

/// `horadric node cast-out <name>`: refuses a brother from now on.
pub(super) fn cast_out_command(args: &[String]) -> Result<(), String> {
    let name = args.first().ok_or("usage: horadric node cast-out <name>")?;
    let (id, name) = by_name(name, &joined_pairs())?;
    if !yes(&format!(
        "Cast {name} out? It loses its key here, and this node and every brother \
         refuse it from now on. Only a brother asking it to join again undoes it."
    )) {
        return Ok(());
    }
    match Line::open() {
        Ok(mut line) => {
            line.ask(json!({"do": "cast-out", "id": id}))?;
        }
        Err(_) => cast_out(&id)?,
    }
    println!("{name} is cast out.");
    Ok(())
}

/// `horadric node view-only <name> [on|off]`: whether a brother may only
/// watch this node's sessions.
pub(super) fn view_only_command(args: &[String]) -> Result<(), String> {
    let name = args
        .first()
        .ok_or("usage: horadric node view-only <name> [on|off]")?;
    let on = args.get(1).is_none_or(|a| a != "off");
    let (id, name) = by_name(name, &joined_pairs())?;
    match Line::open() {
        Ok(mut line) => {
            line.ask(json!({"do": "control", "id": id, "on": !on}))?;
        }
        Err(_) => set_control(&id, !on)?,
    }
    if on {
        println!("{name} may only watch this node's sessions now.");
    } else {
        println!("{name} may control this node's sessions now.");
    }
    Ok(())
}

/// `horadric node keeper [<name>|none] [--auto|--no-auto]`: who keeps a
/// snapshot of this node's sessions at every turn end, to take them over
/// if it falls silent, and whether that keeper does so by itself.
pub(super) fn keeper_command(args: &[String]) -> Result<(), String> {
    let dir = horadric_hooks::state_dir().ok_or("cannot find the state folder")?;
    let auto = if args.iter().any(|a| a == "--auto") {
        Some(true)
    } else if args.iter().any(|a| a == "--no-auto") {
        Some(false)
    } else {
        None
    };
    let pick = args.iter().find(|a| !a.starts_with("--"));
    let joined = joined_pairs();
    let keeper = match pick.map(String::as_str) {
        Some("none") => Some(None),
        Some(name) => Some(Some(by_name(name, &joined)?.0)),
        None => None,
    };
    if keeper.is_none() && auto.is_none() {
        let book = Book::load(&dir);
        let shown = book
            .keeper()
            .map(|k| name_of(k, &joined))
            .unwrap_or_else(|| "no one".into());
        println!("This node's sessions are kept by {shown}.");
        if book.auto_takeover.is_some() {
            println!("It takes them over by itself after 10 minutes of silence.");
        }
        return Ok(());
    }
    // A running node writes the book every turn, so it makes the change
    // itself; only with none running is the file this terminal's to write.
    let (now, on) = match Line::open() {
        Ok(mut line) => {
            let mut req = json!({"do": "keeper"});
            if let Some(k) = &keeper {
                req["keeper"] = json!(k);
            }
            if let Some(a) = auto {
                req["auto"] = json!(a);
            }
            let v = line.ask(req)?;
            (v["keeper"].as_str().map(str::to_string), v["auto"] == true)
        }
        Err(_) => {
            let mut book = Book::load(&dir);
            book.change_keepers(keeper.as_ref().map(Option::as_deref), auto);
            book.save(&dir)
                .map_err(|e| format!("cannot keep the keeper: {e}"))?;
            (
                book.keeper().map(str::to_string),
                book.auto_takeover.is_some(),
            )
        }
    };
    match now {
        Some(k) => println!(
            "{} keeps this node's sessions{}.",
            name_of(&k, &joined),
            if on {
                ", and takes them over by itself after 10 minutes of silence"
            } else {
                ""
            }
        ),
        None => println!("No one keeps this node's sessions."),
    }
    Ok(())
}

/// `horadric node signout`.
pub(super) fn signout_command() -> Result<(), String> {
    if signin::sign_out()? {
        println!("Signed out. Brothers already joined stay joined; the keys are what count.");
    } else {
        println!("Not signed in.");
    }
    if let Ok(mut line) = Line::open() {
        line.ask(json!({"do": "renew"}))?;
    }
    Ok(())
}

fn joined_pairs() -> Vec<(String, String)> {
    horadric_hooks::state_dir()
        .and_then(|d| Store::load(&d))
        .map(|s| {
            s.brothers
                .iter()
                .map(|b| (b.id.clone(), b.name.clone()))
                .collect()
        })
        .unwrap_or_default()
}

fn name_of(id: &str, joined: &[(String, String)]) -> String {
    joined
        .iter()
        .find(|(i, _)| i == id)
        .map_or_else(|| id.to_string(), |(_, n)| n.clone())
}

fn pairs(v: &Value) -> Vec<(String, String)> {
    v.as_array()
        .map(|l| {
            l.iter()
                .map(|b| {
                    (
                        b["id"].as_str().unwrap_or_default().to_string(),
                        b["name"].as_str().unwrap_or_default().to_string(),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The brother called `name` among `among` (id, name), by its name, any
/// case, or by its id.
fn by_name(name: &str, among: &[(String, String)]) -> Result<(String, String), String> {
    super::wire::pick_brother(name, among)
}

/// Lets the human pick one of `among` by its number.
fn choose(among: &[(String, String)]) -> Result<(String, String), String> {
    println!("Which machine should let this node in?");
    for (i, (_, name)) in among.iter().enumerate() {
        println!("  {}. {name}", i + 1);
    }
    print!("Number: ");
    let _ = io::stdout().flush();
    let mut answer = String::new();
    io::stdin()
        .read_line(&mut answer)
        .map_err(|e| e.to_string())?;
    answer
        .trim()
        .parse::<usize>()
        .ok()
        .and_then(|n| n.checked_sub(1))
        .and_then(|i| among.get(i).cloned())
        .ok_or_else(|| "no such machine".to_string())
}

/// The value after `name` in `args`, as `--to vps`.
fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
}
