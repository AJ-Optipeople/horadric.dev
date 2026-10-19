//! What the agents are doing in a project, as light on the Arcane
//! Sanctuary.
//!
//! A [`Glow`] keeps the recent touches of one project: a file read, edited,
//! made or deleted, a note recalled, a file packed for a quest. Each lights
//! its node in the colour of the session that touched it and fades, and a
//! session walking from one file to the next sends a [`Spark`] along the
//! way. All of it is a pure function of the touches and the clock, so the
//! pane asks [`Glow::light`], [`Glow::gone`] and [`Glow::sparks`] for the
//! frame it draws and stops drawing once [`Glow::moving`] says nothing
//! moves.
//!
//! The app keeps one `Glow` per project key in `horadric_ui::glows`. The
//! pane reads one with `glows::with(key, |glow| ...)`, which holds a lock
//! only for the closure, and times it with `glows::now()`, the clock the
//! touches were made on. `glows::stamp()` changes whenever any project is
//! touched, so a still pane woken by `WM_HORADRIC_EVENT` can tell whether
//! there is light to draw. Touches come from hook events ([`heard`]), from
//! the lore tools of `horadric mcp` over `/horadric/glow` ([`told`]), and
//! from a quest's context pack as the quest starts.
//!
//! Nodes are named as `sanctuary::Node::id` names them: a file by its path
//! relative to the top of the project's main tree, a note as `note:<id>`.

use std::collections::HashMap;

use serde_json::{json, Value};

use crate::pack::{command_paths, is_file_name, relative};
use crate::rarity::WRITERS;
use crate::HookEvent;

/// What happened to a node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Touch {
    Read,
    Edit,
    Create,
    Delete,
    /// A lore tool answered with it.
    Recall,
    /// A quest's context pack named it.
    Pack,
}

impl Touch {
    pub fn name(self) -> &'static str {
        match self {
            Touch::Read => "read",
            Touch::Edit => "edit",
            Touch::Create => "create",
            Touch::Delete => "delete",
            Touch::Recall => "recall",
            Touch::Pack => "pack",
        }
    }

    pub fn from_name(name: &str) -> Option<Touch> {
        [
            Touch::Read,
            Touch::Edit,
            Touch::Create,
            Touch::Delete,
            Touch::Recall,
            Touch::Pack,
        ]
        .into_iter()
        .find(|t| t.name() == name)
    }

    /// Seconds until it has faded out.
    fn life(self) -> f64 {
        match self {
            Touch::Read | Touch::Recall | Touch::Pack => READ_LIFE,
            Touch::Edit | Touch::Create => EDIT_LIFE,
            Touch::Delete => DELETE_FADE,
        }
    }

    /// How bright it is `age` seconds after, 0 to 1. A Delete gives no
    /// light: it is a fade out, which [`Glow::gone`] says.
    fn strength(self, age: f64) -> f32 {
        let age = age.max(0.0);
        let edit = || EDIT_PEAK * fade(age / EDIT_LIFE) + PULSE_PEAK * fade(age / PULSE);
        let s = match self {
            Touch::Read | Touch::Recall | Touch::Pack => READ_PEAK * fade(age / READ_LIFE),
            Touch::Edit => edit(),
            Touch::Create => edit().max(fade(age / FLASH)),
            Touch::Delete => 0.0,
        };
        s as f32
    }

    /// Whether a session's path through the code goes by it. A pack or a
    /// recall lights many nodes at once, which is no path.
    fn walks(self) -> bool {
        matches!(self, Touch::Read | Touch::Edit | Touch::Create)
    }
}

/// How long a read glows, in seconds, and how bright at first.
const READ_LIFE: f64 = 60.0;
const READ_PEAK: f64 = 0.35;
/// How long an edit glows, longer than a read since it changed something.
const EDIT_LIFE: f64 = 180.0;
const EDIT_PEAK: f64 = 0.7;
/// The pulse an edit starts with, on top of its glow.
const PULSE: f64 = 1.5;
const PULSE_PEAK: f64 = 0.15;
/// How long the flash of a new file takes to sink to an edit's glow.
const FLASH: f64 = 3.0;
/// How long a deleted node takes to fade out.
const DELETE_FADE: f64 = 4.0;
/// How long a deleted node is kept hidden: by then the graph has been
/// built again without it.
const DELETE_KEEP: f64 = 600.0;
/// How long a spark takes from one node to the next.
pub const SPARK_TIME: f64 = 1.2;
/// The longest a session may take between two files for a spark to go
/// from one to the other. Longer is a new thought, not a path.
const SPARK_GAP: f64 = 20.0;
/// At most this many nodes are kept lit, the oldest dropped first, so a
/// session reading a whole tree can not grow it without end.
const MOST_NODES: usize = 2000;
/// At most this many sparks travel at once.
const MOST_SPARKS: usize = 64;
/// At most this many marks are kept on one node: one per touch and
/// session, and few sessions touch one file in the same minutes.
const MOST_MARKS: usize = 8;

/// 1 at the start, 0 at `x` = 1, easing in and out, so a glow neither
/// drops at once nor ends with a jump.
fn fade(x: f64) -> f64 {
    if x <= 0.0 {
        return 1.0;
    }
    if x >= 1.0 {
        return 0.0;
    }
    1.0 - x * x * (3.0 - 2.0 * x)
}

/// A node's light: the colour of the session that lit it and how bright.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Lit {
    pub rgb: u32,
    pub strength: f32,
}

/// A spark on its way from one node to another, `t` from 0 at `from` to 1
/// at `to`.
#[derive(Debug, Clone, PartialEq)]
pub struct Spark {
    pub from: String,
    pub to: String,
    pub rgb: u32,
    pub t: f32,
}

#[derive(Debug, Clone, PartialEq)]
struct Mark {
    touch: Touch,
    rgb: u32,
    at: f64,
}

#[derive(Debug, Clone, PartialEq)]
struct Travel {
    from: String,
    to: String,
    rgb: u32,
    at: f64,
}

/// The recent touches of one project.
#[derive(Debug, Clone, Default)]
pub struct Glow {
    marks: HashMap<String, Vec<Mark>>,
    /// Where each session (by colour) last walked, and when.
    last: HashMap<u32, (String, f64)>,
    travels: Vec<Travel>,
}

impl Glow {
    /// `node` was touched by the session whose colour is `rgb` (0xRRGGBB),
    /// `now` seconds on the glow's clock.
    pub fn touch(&mut self, node: &str, touch: Touch, rgb: u32, now: f64) {
        if touch.walks() {
            if let Some((from, at)) = self.last.get(&rgb) {
                if from != node && now - at <= SPARK_GAP {
                    self.travels.push(Travel {
                        from: from.clone(),
                        to: node.to_string(),
                        rgb,
                        at: now,
                    });
                    if self.travels.len() > MOST_SPARKS {
                        self.travels.remove(0);
                    }
                }
            }
            self.last.insert(rgb, (node.to_string(), now));
        }
        let marks = self.marks.entry(node.to_string()).or_default();
        marks.retain(|m| !(m.touch == touch && m.rgb == rgb));
        marks.push(Mark {
            touch,
            rgb,
            at: now,
        });
        if marks.len() > MOST_MARKS {
            marks.remove(0);
        }
        if self.marks.len() > MOST_NODES {
            self.prune(now);
        }
        while self.marks.len() > MOST_NODES {
            let oldest = self
                .marks
                .iter()
                .min_by(|a, b| newest(a.1).total_cmp(&newest(b.1)))
                .map(|(k, _)| k.clone());
            match oldest {
                Some(k) => self.marks.remove(&k),
                None => break,
            };
        }
    }

    /// How `node` is lit at `now`, None when it is dark. Of several
    /// touches the brightest shows, and of two as bright the newest.
    pub fn light(&self, node: &str, now: f64) -> Option<Lit> {
        if self.gone(node, now).is_some() {
            return None;
        }
        let mut best: Option<(Lit, f64)> = None;
        for m in self.marks.get(node)? {
            let strength = m.touch.strength(now - m.at);
            if strength <= 0.0 {
                continue;
            }
            let better = best.is_none_or(|(b, at)| {
                strength > b.strength + 1e-6 || (strength >= b.strength - 1e-6 && m.at >= at)
            });
            if better {
                best = Some((
                    Lit {
                        rgb: m.rgb,
                        strength,
                    },
                    m.at,
                ));
            }
        }
        best.map(|(lit, _)| lit)
    }

    /// How much of a deleted `node` is still to be seen at `now`, from 1
    /// down to 0 and 0 after, or None when it was not deleted, or touched
    /// again since.
    pub fn gone(&self, node: &str, now: f64) -> Option<f32> {
        let marks = self.marks.get(node)?;
        let last = marks.iter().max_by(|a, b| a.at.total_cmp(&b.at))?;
        (last.touch == Touch::Delete).then(|| fade((now - last.at) / DELETE_FADE) as f32)
    }

    /// The sparks travelling at `now`.
    pub fn sparks(&self, now: f64) -> Vec<Spark> {
        self.travels
            .iter()
            .filter(|t| now - t.at < SPARK_TIME)
            .map(|t| Spark {
                from: t.from.clone(),
                to: t.to.clone(),
                rgb: t.rgb,
                t: ((now - t.at).max(0.0) / SPARK_TIME) as f32,
            })
            .collect()
    }

    /// Every node with a touch kept, lit or fading out, so a pane need not
    /// ask about every node of the graph.
    pub fn touched(&self) -> impl Iterator<Item = &str> {
        self.marks.keys().map(String::as_str)
    }

    /// Whether anything still fades or travels at `now`. When it does not,
    /// the next frame would be the same as this one.
    pub fn moving(&self, now: f64) -> bool {
        self.travels.iter().any(|t| now - t.at < SPARK_TIME)
            || self
                .marks
                .values()
                .flatten()
                .any(|m| now - m.at < m.touch.life())
    }

    /// Drops what has faded out at `now`. A deleted node is kept a while
    /// longer, so it stays hidden until the graph forgets it.
    pub fn prune(&mut self, now: f64) {
        self.travels.retain(|t| now - t.at < SPARK_TIME);
        self.last.retain(|_, (_, at)| now - *at <= SPARK_GAP);
        self.marks.retain(|_, marks| {
            marks.retain(|m| {
                let keep = match m.touch {
                    Touch::Delete => DELETE_KEEP,
                    t => t.life(),
                };
                now - m.at < keep
            });
            !marks.is_empty()
        });
    }

    /// Whether nothing is kept at all.
    pub fn is_empty(&self) -> bool {
        self.marks.is_empty() && self.travels.is_empty()
    }
}

fn newest(marks: &[Mark]) -> f64 {
    marks.iter().map(|m| m.at).fold(f64::MIN, f64::max)
}

/// Colours for sessions, bright enough to glow on the stage's dark and
/// light backgrounds. Panes give a session no colour of their own, so a
/// session is given one of these by its id.
pub const COLOURS: [u32; 8] = [
    0x60A5FA, 0xF472B6, 0x34D399, 0xFBBF24, 0xA78BFA, 0xF87171, 0x22D3EE, 0xA3E635,
];

/// The colour of the Horadric session `id`, the same in every process and
/// every run.
pub fn session_rgb(id: &str) -> u32 {
    // FNV-1a: std's hasher is seeded per process, and the MCP server and
    // the app must agree.
    let mut h: u32 = 0x811C_9DC5;
    for b in id.bytes() {
        h ^= u32::from(b);
        h = h.wrapping_mul(0x0100_0193);
    }
    COLOURS[h as usize % COLOURS.len()]
}

/// `path` as a full path: one an agent named relative to its shell's
/// folder `cwd` is joined to it.
pub fn full(path: &str, cwd: &str) -> String {
    let p = path.replace('\\', "/");
    let b = p.as_bytes();
    let absolute = p.starts_with('/') || (b.len() >= 2 && b[1] == b':');
    if absolute || cwd.is_empty() {
        return p;
    }
    let rest = p.trim_start_matches("./");
    format!("{}/{rest}", cwd.replace('\\', "/").trim_end_matches('/'))
}

/// The node of the full path `path`: relative to the first of `roots`
/// (the session's own tree, then the project's) that holds it, so a file
/// in a worktree is the same node as in the main tree.
pub fn node(path: &str, roots: &[&str]) -> Option<String> {
    roots
        .iter()
        .filter(|r| !r.is_empty())
        .find_map(|r| relative(path, r))
}

/// The files a hook event touched, as full paths, with how. `exists` says
/// whether a file is on disk now, which tells a Write that makes a file
/// before it runs, and a shell command that deleted one after.
///
/// Reads are the Read tool, a Grep of one file and a file a shell command
/// names, as `pack::touched` counts them. A Write is a Create when the
/// file is not there as it starts.
pub fn heard(event: &HookEvent, exists: impl Fn(&str) -> bool) -> Vec<(String, Touch)> {
    let (Some(tool), Some(input)) = (event.tool_name.as_deref(), event.tool_input.as_ref()) else {
        return Vec::new();
    };
    let field = |k: &str| input.get(k).and_then(Value::as_str);
    let at = |p: &str| full(p, &event.cwd);
    match event.hook_event_name.as_str() {
        "PreToolUse" if tool == "Write" => field("file_path")
            .map(at)
            .filter(|p| !exists(p))
            .map(|p| vec![(p, Touch::Create)])
            .unwrap_or_default(),
        "PostToolUse" => {
            if WRITERS.contains(&tool) {
                let p = field("file_path").or_else(|| field("notebook_path"));
                return p.map(|p| vec![(at(p), Touch::Edit)]).unwrap_or_default();
            }
            match tool {
                "Read" => field("file_path")
                    .map(|p| vec![(at(p), Touch::Read)])
                    .unwrap_or_default(),
                "Grep" => field("path")
                    .filter(|p| is_file_name(p))
                    .map(|p| vec![(at(p), Touch::Read)])
                    .unwrap_or_default(),
                "Bash" | "PowerShell" => {
                    let mut out: Vec<(String, Touch)> = Vec::new();
                    for p in command_paths(field("command").unwrap_or_default()) {
                        let p = at(p);
                        let how = if exists(&p) {
                            Touch::Read
                        } else {
                            Touch::Delete
                        };
                        if !out.iter().any(|(q, _)| *q == p) {
                            out.push((p, how));
                        }
                    }
                    out
                }
                _ => Vec::new(),
            }
        }
        _ => Vec::new(),
    }
}

/// The body of a [`HookEvent::GLOW`]: `nodes` touched as `touch`.
pub fn glow_body(nodes: &[String], touch: Touch) -> String {
    json!({ "nodes": nodes, "touch": touch.name() }).to_string()
}

/// The nodes a [`HookEvent::GLOW`] touched, with how. Its nodes are named
/// already, relative to the project's top.
pub fn told(event: &HookEvent) -> Vec<(String, Touch)> {
    if event.hook_event_name != HookEvent::GLOW {
        return Vec::new();
    }
    let Some(input) = event.tool_input.as_ref() else {
        return Vec::new();
    };
    let Some(touch) = input
        .get("touch")
        .and_then(Value::as_str)
        .and_then(Touch::from_name)
    else {
        return Vec::new();
    };
    input
        .get("nodes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter(|n| !n.is_empty() && !n.starts_with('/') && !n.contains(".."))
        .map(|n| (n.to_string(), touch))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const RED: u32 = 0xFF0000;
    const BLUE: u32 = 0x0000FF;

    fn strength(g: &Glow, node: &str, now: f64) -> f32 {
        g.light(node, now).map_or(0.0, |l| l.strength)
    }

    #[test]
    fn a_read_glows_softly_and_is_dark_after_a_minute() {
        let mut g = Glow::default();
        g.touch("a.rs", Touch::Read, RED, 100.0);
        let lit = g.light("a.rs", 100.0).unwrap();
        assert_eq!(lit.rgb, RED);
        assert!(lit.strength > 0.2 && lit.strength < 0.5, "{}", lit.strength);
        assert!(strength(&g, "a.rs", 130.0) < lit.strength);
        assert!(strength(&g, "a.rs", 130.0) > 0.0);
        assert_eq!(g.light("a.rs", 161.0), None);
        assert_eq!(g.light("b.rs", 100.0), None);
    }

    #[test]
    fn an_edit_is_brighter_than_a_read_and_lasts_longer() {
        let mut g = Glow::default();
        g.touch("r.rs", Touch::Read, RED, 0.0);
        g.touch("e.rs", Touch::Edit, RED, 0.0);
        assert!(strength(&g, "e.rs", 0.0) > strength(&g, "r.rs", 0.0) * 2.0);
        // The pulse is gone after a moment, the glow is not.
        assert!(strength(&g, "e.rs", 5.0) < strength(&g, "e.rs", 0.0));
        assert!(strength(&g, "e.rs", 120.0) > 0.0);
        assert_eq!(g.light("e.rs", 181.0), None);
    }

    #[test]
    fn a_new_file_flashes_brightest_and_sinks_to_an_edits_glow() {
        let mut g = Glow::default();
        g.touch("new.rs", Touch::Create, RED, 0.0);
        g.touch("old.rs", Touch::Edit, RED, 0.0);
        assert!((strength(&g, "new.rs", 0.0) - 1.0).abs() < 1e-6);
        assert!(strength(&g, "new.rs", 0.0) > strength(&g, "old.rs", 0.0));
        let later = |n| strength(&g, n, 60.0);
        assert!((later("new.rs") - later("old.rs")).abs() < 1e-6);
    }

    #[test]
    fn recalls_and_packs_light_like_reads() {
        let mut g = Glow::default();
        g.touch("note:7", Touch::Recall, RED, 0.0);
        g.touch("p.rs", Touch::Pack, BLUE, 0.0);
        g.touch("r.rs", Touch::Read, RED, 0.0);
        for t in [0.0, 30.0] {
            assert_eq!(strength(&g, "note:7", t), strength(&g, "r.rs", t));
            assert_eq!(strength(&g, "p.rs", t), strength(&g, "r.rs", t));
        }
    }

    #[test]
    fn the_brightest_touch_shows_and_of_equals_the_newest() {
        let mut g = Glow::default();
        g.touch("a.rs", Touch::Edit, RED, 0.0);
        g.touch("a.rs", Touch::Read, BLUE, 1.0);
        assert_eq!(g.light("a.rs", 2.0).unwrap().rgb, RED);
        g.touch("a.rs", Touch::Edit, BLUE, 3.0);
        assert_eq!(g.light("a.rs", 3.0).unwrap().rgb, BLUE);
        let mut g = Glow::default();
        g.touch("b.rs", Touch::Read, RED, 5.0);
        g.touch("b.rs", Touch::Read, BLUE, 5.0);
        assert_eq!(g.light("b.rs", 5.0).unwrap().rgb, BLUE);
    }

    #[test]
    fn touching_again_starts_the_glow_again() {
        let mut g = Glow::default();
        g.touch("a.rs", Touch::Read, RED, 0.0);
        g.touch("a.rs", Touch::Read, RED, 50.0);
        assert!(strength(&g, "a.rs", 70.0) > 0.0);
        assert_eq!(g.marks["a.rs"].len(), 1);
    }

    #[test]
    fn a_deleted_file_fades_out_and_stays_hidden() {
        let mut g = Glow::default();
        g.touch("a.rs", Touch::Edit, RED, 0.0);
        assert_eq!(g.gone("a.rs", 1.0), None);
        g.touch("a.rs", Touch::Delete, RED, 10.0);
        assert_eq!(g.gone("a.rs", 10.0), Some(1.0));
        let half = g.gone("a.rs", 12.0).unwrap();
        assert!(half > 0.0 && half < 1.0);
        assert_eq!(g.gone("a.rs", 20.0), Some(0.0));
        assert_eq!(g.light("a.rs", 11.0), None);
        g.prune(300.0);
        assert_eq!(g.gone("a.rs", 300.0), Some(0.0));
        g.prune(700.0);
        assert_eq!(g.gone("a.rs", 700.0), None);
    }

    #[test]
    fn a_file_made_again_after_a_delete_is_back() {
        let mut g = Glow::default();
        g.touch("a.rs", Touch::Delete, RED, 0.0);
        g.touch("a.rs", Touch::Create, RED, 5.0);
        assert_eq!(g.gone("a.rs", 5.0), None);
        assert!(g.light("a.rs", 5.0).is_some());
    }

    #[test]
    fn a_session_walking_from_file_to_file_sends_a_spark() {
        let mut g = Glow::default();
        g.touch("a.rs", Touch::Read, RED, 0.0);
        g.touch("b.rs", Touch::Edit, RED, 2.0);
        let s = g.sparks(2.6);
        assert_eq!(s.len(), 1);
        assert_eq!((s[0].from.as_str(), s[0].to.as_str()), ("a.rs", "b.rs"));
        assert_eq!(s[0].rgb, RED);
        assert!((s[0].t - 0.5).abs() < 1e-4);
        assert!(g.sparks(2.0 + SPARK_TIME).is_empty());
    }

    #[test]
    fn no_spark_between_sessions_after_a_pause_or_to_the_same_file() {
        let mut g = Glow::default();
        g.touch("a.rs", Touch::Read, RED, 0.0);
        g.touch("b.rs", Touch::Read, BLUE, 1.0);
        g.touch("b.rs", Touch::Read, BLUE, 2.0);
        g.touch("c.rs", Touch::Read, RED, 100.0);
        assert!(g.sparks(1.0).is_empty());
        assert!(g.sparks(2.0).is_empty());
        assert!(g.sparks(100.0).is_empty());
    }

    #[test]
    fn recalls_packs_and_deletes_send_no_spark() {
        let mut g = Glow::default();
        g.touch("a.rs", Touch::Read, RED, 0.0);
        g.touch("note:1", Touch::Recall, RED, 1.0);
        g.touch("p.rs", Touch::Pack, RED, 1.0);
        g.touch("d.rs", Touch::Delete, RED, 1.0);
        assert!(g.sparks(1.0).is_empty());
        g.touch("b.rs", Touch::Read, RED, 2.0);
        assert_eq!(g.sparks(2.0)[0].from, "a.rs");
    }

    #[test]
    fn it_moves_until_the_last_glow_and_spark_are_done() {
        let mut g = Glow::default();
        assert!(!g.moving(0.0));
        g.touch("a.rs", Touch::Read, RED, 0.0);
        assert!(g.moving(59.0));
        assert!(!g.moving(60.0));
        g.touch("d.rs", Touch::Delete, RED, 100.0);
        assert!(g.moving(103.0));
        assert!(!g.moving(104.0));
    }

    #[test]
    fn prune_drops_what_has_faded() {
        let mut g = Glow::default();
        g.touch("a.rs", Touch::Read, RED, 0.0);
        g.touch("b.rs", Touch::Edit, RED, 0.0);
        g.prune(61.0);
        assert_eq!(g.touched().collect::<Vec<_>>(), ["b.rs"]);
        g.prune(181.0);
        assert!(g.is_empty());
        assert!(g.last.is_empty());
    }

    #[test]
    fn memory_is_bounded() {
        let mut g = Glow::default();
        for i in 0..MOST_NODES + 50 {
            g.touch(&format!("f{i}.rs"), Touch::Read, RED, i as f64 * 0.01);
        }
        assert_eq!(g.marks.len(), MOST_NODES);
        assert!(!g.marks.contains_key("f0.rs"));
        assert!(g.marks.contains_key(&format!("f{}.rs", MOST_NODES + 49)));
        assert!(g.travels.len() <= MOST_SPARKS);
        for (i, &c) in COLOURS.iter().cycle().take(20).enumerate() {
            g.touch("busy.rs", Touch::Read, c + i as u32, 1000.0);
        }
        assert!(g.marks["busy.rs"].len() <= MOST_MARKS);
    }

    #[test]
    fn a_clock_running_back_does_not_break_anything() {
        let mut g = Glow::default();
        g.touch("a.rs", Touch::Edit, RED, 10.0);
        let lit = g.light("a.rs", 9.0).unwrap();
        assert!(lit.strength <= 1.0);
    }

    #[test]
    fn a_session_keeps_its_colour() {
        assert_eq!(session_rgb("fix-it-1"), session_rgb("fix-it-1"));
        assert!(COLOURS.contains(&session_rgb("x")));
        let all: std::collections::HashSet<u32> =
            (0..40).map(|i| session_rgb(&format!("s{i}"))).collect();
        assert!(all.len() > 4);
    }

    #[test]
    fn touches_have_names_both_ways() {
        for t in [
            Touch::Read,
            Touch::Edit,
            Touch::Create,
            Touch::Delete,
            Touch::Recall,
            Touch::Pack,
        ] {
            assert_eq!(Touch::from_name(t.name()), Some(t));
        }
        assert_eq!(Touch::from_name("poke"), None);
    }

    #[test]
    fn a_path_is_made_full_from_the_shells_folder() {
        assert_eq!(full(r"C:\p\a.rs", "c:/q"), "C:/p/a.rs");
        assert_eq!(full("/c/p/a.rs", "c:/q"), "/c/p/a.rs");
        assert_eq!(full("src/a.rs", r"C:\p\"), "C:/p/src/a.rs");
        assert_eq!(full("./a.rs", "C:/p"), "C:/p/a.rs");
        assert_eq!(full("a.rs", ""), "a.rs");
    }

    #[test]
    fn a_worktree_file_is_the_main_trees_node() {
        let roots = ["C:/code/app-wt/fix", "c:/code/app"];
        assert_eq!(
            node("C:/code/app-wt/fix/src/a.rs", &roots).as_deref(),
            Some("src/a.rs")
        );
        assert_eq!(
            node("C:/Code/App/src/a.rs", &roots).as_deref(),
            Some("src/a.rs")
        );
        assert_eq!(node("C:/elsewhere/a.rs", &roots), None);
        assert_eq!(node("C:/code/app/target/x.rs", &roots), None);
        assert_eq!(
            node("C:/code/app/a.rs", &["", "c:/code/app"]).as_deref(),
            Some("a.rs")
        );
    }

    fn event(name: &str, tool: &str, input: Value) -> HookEvent {
        HookEvent {
            hook_event_name: name.into(),
            tool_name: Some(tool.into()),
            tool_input: Some(input),
            cwd: "C:/p".into(),
            ..Default::default()
        }
    }

    #[test]
    fn reads_and_edits_are_heard_after_the_tool() {
        let yes = |_: &str| true;
        let e = event("PostToolUse", "Read", json!({"file_path": "C:/p/a.rs"}));
        assert_eq!(heard(&e, yes), [("C:/p/a.rs".to_string(), Touch::Read)]);
        let e = event("PreToolUse", "Read", json!({"file_path": "C:/p/a.rs"}));
        assert!(heard(&e, yes).is_empty());
        for tool in WRITERS {
            let e = event("PostToolUse", tool, json!({"file_path": "C:/p/a.rs"}));
            assert_eq!(heard(&e, yes), [("C:/p/a.rs".to_string(), Touch::Edit)]);
        }
        let e = event(
            "PostToolUse",
            "NotebookEdit",
            json!({"notebook_path": "n.ipynb"}),
        );
        assert_eq!(heard(&e, yes), [("C:/p/n.ipynb".to_string(), Touch::Edit)]);
    }

    #[test]
    fn a_grep_of_one_file_is_a_read_and_of_a_folder_is_not() {
        let yes = |_: &str| true;
        let e = event(
            "PostToolUse",
            "Grep",
            json!({"pattern": "x", "path": "src/a.rs"}),
        );
        assert_eq!(heard(&e, yes), [("C:/p/src/a.rs".to_string(), Touch::Read)]);
        let e = event(
            "PostToolUse",
            "Grep",
            json!({"pattern": "x", "path": "src"}),
        );
        assert!(heard(&e, yes).is_empty());
    }

    #[test]
    fn a_write_of_a_file_not_there_yet_is_a_create() {
        let e = event("PreToolUse", "Write", json!({"file_path": "C:/p/new.rs"}));
        assert_eq!(
            heard(&e, |_| false),
            [("C:/p/new.rs".to_string(), Touch::Create)]
        );
        assert!(heard(&e, |_| true).is_empty());
        let e = event("PreToolUse", "Edit", json!({"file_path": "C:/p/new.rs"}));
        assert!(heard(&e, |_| false).is_empty());
    }

    #[test]
    fn a_shell_command_reads_what_is_there_and_deleted_what_is_not() {
        let e = event(
            "PostToolUse",
            "Bash",
            json!({"command": "cat src/a.rs && rm src/old.rs && cat src/a.rs"}),
        );
        let got = heard(&e, |p| p.ends_with("a.rs"));
        assert_eq!(
            got,
            [
                ("C:/p/src/a.rs".to_string(), Touch::Read),
                ("C:/p/src/old.rs".to_string(), Touch::Delete),
            ]
        );
        let e = event("PostToolUse", "Bash", json!({"command": "cargo test"}));
        assert!(heard(&e, |_| true).is_empty());
    }

    #[test]
    fn other_events_and_tools_touch_nothing() {
        let e = event(
            "PostToolUse",
            "WebFetch",
            json!({"url": "https://x.dev/a.rs"}),
        );
        assert!(heard(&e, |_| true).is_empty());
        assert!(heard(&HookEvent::synthetic("Stop"), |_| true).is_empty());
    }

    #[test]
    fn a_glow_event_says_its_nodes_and_how() {
        let body = glow_body(&["note:3".into(), "src/a.rs".into()], Touch::Recall);
        let e = HookEvent {
            tool_input: serde_json::from_str(&body).ok(),
            ..HookEvent::synthetic(HookEvent::GLOW)
        };
        assert_eq!(
            told(&e),
            [
                ("note:3".to_string(), Touch::Recall),
                ("src/a.rs".to_string(), Touch::Recall),
            ]
        );
        let odd = HookEvent {
            tool_input: Some(json!({"nodes": ["../x", "/etc/y", ""], "touch": "pack"})),
            ..HookEvent::synthetic(HookEvent::GLOW)
        };
        assert!(told(&odd).is_empty());
        let poke = HookEvent {
            tool_input: Some(json!({"nodes": ["a.rs"], "touch": "poke"})),
            ..HookEvent::synthetic(HookEvent::GLOW)
        };
        assert!(told(&poke).is_empty());
        let hook = HookEvent {
            tool_input: serde_json::from_str(&body).ok(),
            ..HookEvent::synthetic("PostToolUse")
        };
        assert!(told(&hook).is_empty());
    }
}
