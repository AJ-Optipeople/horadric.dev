//! The Arcane Sanctuary's graph and Tristram's sky, built from the lore
//! and the code index: the app's panes and `horadric sanctuary` and
//! `horadric sky`. Windows only, as the panes are; the lore tools beside
//! it run on a Linux node too.

use std::cell::OnceCell;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime};

use horadric_core::chronicle;
use horadric_core::pack::Touched;
use horadric_core::sanctuary::{self, Graph, Kind, Positions, QuestFiles};
use horadric_core::sky::{Project, Sky};
use horadric_hooks::lore::Place;
use horadric_hooks::tasks as tasks_file;

use super::{walk, with_index};
use crate::code;

/// The Arcane Sanctuary's graph of the project `place` is in: every file
/// of the tree, the calls between them from the code index, the notes and
/// the done quests.
pub fn graph(place: &Place) -> Result<Graph, String> {
    graph_with(place, &crate::task::finished(&place.top))
}

/// The graph of `place` with its done quests already found.
fn graph_with(place: &Place, done: &[(String, Touched)]) -> Result<Graph, String> {
    let calls = with_index(place, code::links)?;
    let files = place.listed().unwrap_or_else(|| {
        let mut found = Vec::new();
        walk(&place.top, "", &mut found);
        found
    });
    let notes = place.read()?.notes;
    let quests: Vec<QuestFiles> = done
        .iter()
        .map(|(title, touched)| QuestFiles { title, touched })
        .collect();
    Ok(Graph::build(&files, &calls, &notes, &quests))
}

/// The graph of the project `dir` is in, for the app's sanctuary pane.
pub fn graph_at(dir: &Path) -> Result<Graph, String> {
    graph(&Place::of(dir)?)
}

/// `horadric sanctuary`: lays the graph out from where it was left and
/// keeps it, saying how large it is and how long that took, so the layout
/// can be tried on a real project before the pane draws it.
pub fn sanctuary() -> Result<(), String> {
    let place = Place::here()?;
    let t = Instant::now();
    let g = graph(&place)?;
    let built = t.elapsed();
    let t = Instant::now();
    let at = sanctuary::layout(&g, &place.positions());
    let laid = t.elapsed();
    place.keep_positions(&at)?;
    let count = |k| g.nodes.iter().filter(|n| n.kind == k).count();
    let orphans = g.nodes.iter().filter(|n| n.weight == 0).count();
    println!(
        "{} files, {} notes, {} quests, {} links, {orphans} with none",
        count(Kind::File),
        count(Kind::Note),
        count(Kind::Quest),
        g.links.len()
    );
    println!("built in {built:?}, laid out in {laid:?}");
    println!("kept in {}", place.sanctuary_store().display());
    Ok(())
}

/// One project of Tristram's sky: its graph and where its nodes were left.
pub struct Charted {
    pub key: String,
    pub name: String,
    pub graph: Graph,
    pub at: Positions,
}

/// Tristram's sky: every project Horadric knows as a galaxy, `projects`
/// in the galaxies' order so a star's node can be looked up, and the
/// projects whose graph could not be built, with why.
pub struct TownSky {
    pub sky: Sky,
    pub projects: Vec<Arc<Charted>>,
    pub missed: Vec<(String, String)>,
}

/// A charted project as the cache keeps it: the files its graph is read
/// out of and their times when it was built. Edits the code index has not
/// read yet are not in them, so the sky shows a project as its index last
/// saw it, which is close enough at that distance; its own sanctuary reads
/// it fresh.
struct Kept {
    sources: Vec<PathBuf>,
    stamp: Vec<Option<SystemTime>>,
    charted: Arc<Charted>,
}

static CHARTED: Mutex<Option<HashMap<String, Kept>>> = Mutex::new(None);

/// Tristram's sky as it is now, from the same projects `horadric projects`
/// lists. A project whose sources have not changed since it was last
/// charted in this process is taken as it was, so opening the town again
/// costs only a look at a few file times per project.
pub fn town_sky() -> TownSky {
    let records = OnceCell::new();
    let mut projects = Vec::new();
    let mut missed = Vec::new();
    for dir in horadric_ui::town::known_dirs() {
        let key = horadric_ui::folder_key(&dir);
        match chart(&dir, &key, &records) {
            Ok(c) => projects.push(c),
            Err(e) => missed.push((horadric_ui::project_name(&key), e)),
        }
    }
    projects.sort_by(|a, b| a.key.cmp(&b.key));
    let sky = Sky::build(
        &projects
            .iter()
            .map(|c| Project {
                key: &c.key,
                name: &c.name,
                graph: &c.graph,
                at: &c.at,
            })
            .collect::<Vec<_>>(),
    );
    TownSky {
        sky,
        projects,
        missed,
    }
}

/// Tristram's sky as one graph for the town's sanctuary pane: each
/// project's nodes under its key, the links between them, every star where
/// the sky put it.
pub fn town_graph() -> Result<horadric_ui::sanctuary::Town, String> {
    use horadric_ui::sanctuary::{Galaxy, Town, SEP};
    let town = town_sky();
    let mut graph = Graph::default();
    let mut points = Vec::with_capacity(town.sky.stars.len());
    for star in &town.sky.stars {
        let c = &town.projects[star.galaxy];
        let mut n = c.graph.nodes[star.node].clone();
        n.id = format!("{}{SEP}{}", c.key, n.id);
        n.label = format!("{}: {}", c.name, n.label);
        graph.nodes.push(n);
        points.push(star.at);
    }
    graph.links = town
        .sky
        .links
        .iter()
        .map(|l| sanctuary::Link {
            a: l.a.min(l.b),
            b: l.a.max(l.b),
            weight: l.weight,
        })
        .collect();
    let galaxies = town
        .sky
        .galaxies
        .iter()
        .map(|g| Galaxy {
            key: g.key.clone(),
            name: g.name.clone(),
            centre: g.centre,
            radius: g.radius,
            nodes: g.stars.clone(),
        })
        .collect();
    Ok(Town {
        graph,
        points,
        galaxies,
    })
}

fn stamp(sources: &[PathBuf]) -> Vec<Option<SystemTime>> {
    sources
        .iter()
        .map(|p| fs::metadata(p).and_then(|m| m.modified()).ok())
        .collect()
}

/// One project of the sky, from the cache while its sources are as they
/// were, else built, and laid out and kept when it has nodes never placed.
fn chart(
    dir: &str,
    key: &str,
    records: &OnceCell<Vec<chronicle::Record>>,
) -> Result<Arc<Charted>, String> {
    // Asking git where the project is costs a process start, more than
    // the rest of a look at a charted project, so the cache is by key.
    if let Some(k) = CHARTED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .and_then(|m| m.get(key))
    {
        if stamp(&k.sources) == k.stamp {
            return Ok(k.charted.clone());
        }
    }
    let place = Place::of(Path::new(dir))?;
    let log = tasks_file::find_list(&place.top);
    let sources = vec![
        place.code_store(),
        place.notes_store().to_path_buf(),
        place.sanctuary_store(),
        place.top.join(".git").join("index"),
        log.as_deref().map(tasks_file::file).unwrap_or_default(),
        horadric_hooks::state_dir()
            .unwrap_or_default()
            .join(chronicle::FILE),
    ];
    let before = stamp(&sources);
    let done = match &log {
        Some(log) => crate::task::finished_in(log, records.get_or_init(horadric_ui::chronicle_all)),
        None => Vec::new(),
    };
    let graph = graph_with(&place, &done)?;
    let mut at = place.positions();
    if graph.nodes.iter().any(|n| !at.at.contains_key(&n.id)) {
        at = sanctuary::layout(&graph, &at);
        place.keep_positions(&at)?;
    } else {
        // Kept by an older build, the orphans may sit on a ring far out;
        // a layout with nothing new moves only them, back beside the rest.
        at = sanctuary::Layout::new(&graph, &at).kept(&graph);
    }
    // Building wrote the code index and keeping wrote the positions, so
    // their times are taken again; the rest as they were before reading,
    // so a change made meanwhile is seen next time.
    let mut stamp = before;
    let after = self::stamp(&sources);
    stamp[0] = after[0];
    stamp[2] = after[2];
    let charted = Arc::new(Charted {
        key: key.to_string(),
        name: horadric_ui::project_name(key),
        graph,
        at,
    });
    CHARTED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_insert_with(HashMap::new)
        .insert(
            key.to_string(),
            Kept {
                sources,
                stamp,
                charted: charted.clone(),
            },
        );
    Ok(charted)
}

/// `horadric sky`: charts every known project into Tristram's sky twice,
/// cold and then from the cache, saying what each galaxy holds and how
/// long it took, so the sky can be tried before the town's pane draws it.
pub fn sky() -> Result<(), String> {
    let t = Instant::now();
    let town = town_sky();
    let cold = t.elapsed();
    let t = Instant::now();
    let again = town_sky();
    let warm = t.elapsed();
    for (g, c) in town.sky.galaxies.iter().zip(&town.projects) {
        println!(
            "{:<28} {:>5} nodes {:>6} links  radius {:>4.0} at ({:>5.0}, {:>5.0})",
            g.name,
            c.graph.nodes.len(),
            c.graph.links.len(),
            g.radius,
            g.centre[0],
            g.centre[1]
        );
    }
    for (name, why) in &town.missed {
        println!("{name:<28} not charted: {why}");
    }
    let (_, r) = town.sky.bounds();
    println!(
        "{} galaxies, {} stars, {} links, {r:.0} across from the middle",
        town.sky.galaxies.len(),
        town.sky.stars.len(),
        town.sky.links.len()
    );
    println!(
        "charted in {cold:?}, again from the cache in {warm:?} ({} the same)",
        if again.sky == town.sky {
            "all"
        } else {
            "not all"
        }
    );
    Ok(())
}
