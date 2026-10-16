//! Tristram's sky, the pure part (docs/plan/the-arcane-sanctuary.md, step
//! 4): every project's sanctuary, each laid out in its own coordinates,
//! set apart as a galaxy in one picture so the town's pane can draw them
//! all at once and a click can go into one.
//!
//! A galaxy's size grows with the fourth root of its node count, so a
//! project of thousands of files is a few times larger than a small one
//! rather than swallowing the sky. Galaxies are placed largest first, each
//! on a ray of its own picked from its key, slid out from the middle until
//! it touches nothing placed before it. A project that comes or goes moves
//! only the smaller ones its place blocks, so the sky keeps its shape.

use std::ops::Range;

use crate::sanctuary::{Graph, Positions};

/// The radius of a galaxy of one node, in sky units. Only ratios matter
/// to a pane that scales to fit.
pub const UNIT: f32 = 60.0;
/// The space kept between two galaxies, as a share of `UNIT`.
const GAP: f32 = 0.5;

/// One project as the sky takes it: laid out already, in its own
/// coordinates.
pub struct Project<'a> {
    /// What it is ordered and placed by, stable across runs: its folder
    /// key, say.
    pub key: &'a str,
    /// What is shown.
    pub name: &'a str,
    pub graph: &'a Graph,
    pub at: &'a Positions,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Galaxy {
    pub key: String,
    pub name: String,
    pub centre: [f32; 2],
    pub radius: f32,
    /// Sky units per unit of the project's own layout.
    pub scale: f32,
    /// Its stars in `Sky::stars`, in its graph's node order, so star
    /// `stars.start + i` is the graph's node `i`.
    pub stars: Range<usize>,
    /// Its links in `Sky::links`.
    pub links: Range<usize>,
}

/// A node of some project, placed in the sky.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Star {
    pub galaxy: usize,
    /// The node's index in its project's graph.
    pub node: usize,
    pub at: [f32; 2],
}

/// A link between two stars, by their index in `Sky::stars`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SkyLink {
    pub a: usize,
    pub b: usize,
    pub weight: u32,
}

/// Every project as a galaxy, flat so a pane draws the whole sky in two
/// loops: the links, then the stars.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Sky {
    pub galaxies: Vec<Galaxy>,
    pub stars: Vec<Star>,
    pub links: Vec<SkyLink>,
}

impl Sky {
    /// Places `projects` apart in one sky. Their order does not matter: the
    /// galaxies come out ordered by key.
    pub fn build(projects: &[Project]) -> Sky {
        let mut order: Vec<&Project> = projects.iter().collect();
        order.sort_by(|a, b| a.key.cmp(b.key));
        let radii: Vec<f32> = order.iter().map(|p| radius(p.graph.nodes.len())).collect();
        let centres = pack(&order.iter().map(|p| p.key).collect::<Vec<_>>(), &radii);
        let mut sky = Sky::default();
        for (g, p) in order.iter().enumerate() {
            let own: Vec<[f32; 2]> = p
                .graph
                .nodes
                .iter()
                .map(|n| p.at.at.get(&n.id).copied().unwrap_or([0.0; 2]))
                .collect();
            let (mid, extent) = extent(&own);
            let scale = radii[g] / extent;
            let first = sky.stars.len();
            sky.stars
                .extend(own.iter().enumerate().map(|(node, q)| Star {
                    galaxy: g,
                    node,
                    at: [
                        centres[g][0] + (q[0] - mid[0]) * scale,
                        centres[g][1] + (q[1] - mid[1]) * scale,
                    ],
                }));
            let first_link = sky.links.len();
            sky.links.extend(p.graph.links.iter().map(|l| SkyLink {
                a: first + l.a,
                b: first + l.b,
                weight: l.weight,
            }));
            sky.galaxies.push(Galaxy {
                key: p.key.to_string(),
                name: p.name.to_string(),
                centre: centres[g],
                radius: radii[g],
                scale,
                stars: first..sky.stars.len(),
                links: first_link..sky.links.len(),
            });
        }
        sky
    }

    /// The galaxy a click at `point` lands in.
    pub fn hit(&self, point: [f32; 2]) -> Option<usize> {
        self.galaxies
            .iter()
            .position(|g| dist(g.centre, point) <= g.radius)
    }

    /// Where a pane looks to fill itself with galaxy `g`: its centre and
    /// radius.
    pub fn frame(&self, g: usize) -> ([f32; 2], f32) {
        let g = &self.galaxies[g];
        (g.centre, g.radius)
    }

    /// The circle around the whole sky, for a pane to fit it all.
    pub fn bounds(&self) -> ([f32; 2], f32) {
        if self.galaxies.is_empty() {
            return ([0.0; 2], UNIT);
        }
        let (mut lo, mut hi) = ([f32::MAX; 2], [f32::MIN; 2]);
        for g in &self.galaxies {
            for k in 0..2 {
                lo[k] = lo[k].min(g.centre[k] - g.radius);
                hi[k] = hi[k].max(g.centre[k] + g.radius);
            }
        }
        let mid = [(lo[0] + hi[0]) / 2.0, (lo[1] + hi[1]) / 2.0];
        let r = self
            .galaxies
            .iter()
            .map(|g| dist(mid, g.centre) + g.radius)
            .fold(0.0, f32::max);
        (mid, r)
    }
}

/// A galaxy's radius for `n` nodes: its area grows as the square root of
/// the count, so ten times the files is about three times the disc.
pub fn radius(n: usize) -> f32 {
    UNIT * ((n.max(1)) as f32).sqrt().sqrt()
}

/// The middle of the box around `pts` and how far the furthest point is
/// from it, never less than a small floor so one node does not divide by
/// nothing.
fn extent(pts: &[[f32; 2]]) -> ([f32; 2], f32) {
    if pts.is_empty() {
        return ([0.0; 2], 1.0);
    }
    let (mut lo, mut hi) = ([f32::MAX; 2], [f32::MIN; 2]);
    for p in pts {
        for k in 0..2 {
            lo[k] = lo[k].min(p[k]);
            hi[k] = hi[k].max(p[k]);
        }
    }
    let mid = [(lo[0] + hi[0]) / 2.0, (lo[1] + hi[1]) / 2.0];
    let far = pts.iter().map(|p| dist(mid, *p)).fold(0.0, f32::max);
    (mid, far.max(1.0))
}

/// The centres of circles of `radii`, keyed by `keys`, packed apart around
/// the middle. The largest first (by key among equals), each on the ray of
/// its key's angle, at the nearest distance out where it overlaps none
/// placed before it.
fn pack(keys: &[&str], radii: &[f32]) -> Vec<[f32; 2]> {
    let mut order: Vec<usize> = (0..keys.len()).collect();
    order.sort_by(|&a, &b| radii[b].total_cmp(&radii[a]).then(keys[a].cmp(keys[b])));
    let mut centres = vec![[0.0f32; 2]; keys.len()];
    let mut placed: Vec<usize> = Vec::new();
    for &i in &order {
        let angle = angle(keys[i]);
        let dir = [angle.cos(), angle.sin()];
        // Along the ray, the stretches where it would overlap each placed
        // circle; the answer is the first distance in none of them.
        let mut blocked: Vec<(f32, f32)> = placed
            .iter()
            .filter_map(|&j| {
                let reach = radii[i] + radii[j] + GAP * UNIT;
                let c = centres[j];
                let along = c[0] * dir[0] + c[1] * dir[1];
                let off = (c[0] * c[0] + c[1] * c[1]) - along * along;
                let half = reach * reach - off;
                (half > 0.0).then(|| (along - half.sqrt(), along + half.sqrt()))
            })
            .collect();
        blocked.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut d = 0.0f32;
        for (from, to) in blocked {
            if from < d && to > d {
                // A hair past the end, so rounding cannot leave it touching.
                d = to + 1e-3 * UNIT;
            }
        }
        centres[i] = [dir[0] * d, dir[1] * d];
        placed.push(i);
    }
    centres
}

/// A key's own direction from the middle, the same every run.
fn angle(key: &str) -> f32 {
    // FNV-1a: stable across builds, unlike the std hasher.
    let mut h: u64 = 0xcbf29ce484222325;
    for b in key.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    (h % 3600) as f32 / 3600.0 * std::f32::consts::TAU
}

fn dist(a: [f32; 2], b: [f32; 2]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sanctuary::{Kind, Link, Node};

    fn graph(n: usize) -> (Graph, Positions) {
        let nodes = (0..n)
            .map(|i| Node {
                id: format!("f{i}"),
                kind: Kind::File,
                label: format!("f{i}"),
                weight: 1,
            })
            .collect();
        let links = (1..n)
            .map(|i| Link {
                a: i - 1,
                b: i,
                weight: 1,
            })
            .collect();
        let g = Graph { nodes, links };
        // A line far from the middle, so placing must move and scale it.
        let at = Positions {
            at: (0..n)
                .map(|i| (format!("f{i}"), [500.0 + i as f32 * 10.0, -300.0]))
                .collect(),
        };
        (g, at)
    }

    fn sky(sizes: &[(&str, usize)]) -> Sky {
        let made: Vec<(String, Graph, Positions)> = sizes
            .iter()
            .map(|(k, n)| {
                let (g, at) = graph(*n);
                (k.to_string(), g, at)
            })
            .collect();
        let projects: Vec<Project> = made
            .iter()
            .map(|(k, g, at)| Project {
                key: k,
                name: k,
                graph: g,
                at,
            })
            .collect();
        Sky::build(&projects)
    }

    #[test]
    fn galaxies_do_not_overlap() {
        let names: Vec<String> = (0..30).map(|i| format!("c:/p/project{i}")).collect();
        let sizes: Vec<(&str, usize)> = names
            .iter()
            .enumerate()
            .map(|(i, k)| (k.as_str(), 1 + i * i * 7))
            .collect();
        let s = sky(&sizes);
        for (i, a) in s.galaxies.iter().enumerate() {
            for b in &s.galaxies[i + 1..] {
                assert!(
                    dist(a.centre, b.centre) >= a.radius + b.radius,
                    "{} overlaps {}",
                    a.key,
                    b.key
                );
            }
        }
    }

    #[test]
    fn stars_fall_inside_their_galaxy() {
        let s = sky(&[("a", 40), ("b", 3), ("c", 1)]);
        for st in &s.stars {
            let g = &s.galaxies[st.galaxy];
            assert!(dist(st.at, g.centre) <= g.radius * 1.001);
        }
        // The line's ends reach the rim, so it fills its disc.
        let a = &s.galaxies[0];
        let first = s.stars[a.stars.start].at;
        assert!((dist(first, a.centre) - a.radius).abs() < 0.01);
    }

    #[test]
    fn stars_and_links_keep_their_project_and_node() {
        let s = sky(&[("b", 3), ("a", 2)]);
        assert_eq!(s.galaxies[0].key, "a");
        assert_eq!(s.galaxies[1].stars, 2..5);
        assert_eq!(s.stars[3].galaxy, 1);
        assert_eq!(s.stars[3].node, 1);
        let l = s.links[s.galaxies[1].links.start];
        assert_eq!((l.a, l.b), (2, 3));
    }

    #[test]
    fn a_huge_project_does_not_dwarf_the_rest() {
        assert!(radius(3000) / radius(30) < 4.0);
        assert!(radius(3000) > radius(30));
        assert_eq!(radius(0), radius(1));
    }

    #[test]
    fn the_largest_sits_in_the_middle() {
        let s = sky(&[("small", 5), ("big", 500)]);
        let big = s.galaxies.iter().find(|g| g.key == "big").unwrap();
        assert_eq!(big.centre, [0.0, 0.0]);
    }

    #[test]
    fn a_project_keeps_its_place_when_a_smaller_one_goes() {
        let before = sky(&[("a", 900), ("b", 400), ("c", 3)]);
        let after = sky(&[("a", 900), ("b", 400)]);
        for g in &after.galaxies {
            let was = before.galaxies.iter().find(|w| w.key == g.key).unwrap();
            assert_eq!(was.centre, g.centre);
        }
    }

    #[test]
    fn the_same_projects_make_the_same_sky_in_any_order() {
        assert_eq!(sky(&[("a", 9), ("b", 4)]), sky(&[("b", 4), ("a", 9)]));
    }

    #[test]
    fn a_click_finds_its_galaxy() {
        let s = sky(&[("a", 50), ("b", 10)]);
        for (i, g) in s.galaxies.iter().enumerate() {
            assert_eq!(s.hit(g.centre), Some(i));
            assert_eq!(s.frame(i), (g.centre, g.radius));
        }
        let (mid, r) = s.bounds();
        assert_eq!(s.hit([mid[0] + r * 2.0, mid[1]]), None);
    }

    #[test]
    fn the_bounds_hold_every_galaxy() {
        let s = sky(&[("a", 50), ("b", 10), ("c", 200)]);
        let (mid, r) = s.bounds();
        for g in &s.galaxies {
            assert!(dist(mid, g.centre) + g.radius <= r + 0.01);
        }
        assert_eq!(Sky::default().bounds(), ([0.0; 2], UNIT));
    }

    #[test]
    fn an_empty_or_unplaced_project_still_gets_a_galaxy() {
        let g = Graph::default();
        let at = Positions::default();
        let s = Sky::build(&[Project {
            key: "x",
            name: "x",
            graph: &g,
            at: &at,
        }]);
        assert_eq!(s.galaxies.len(), 1);
        assert!(s.stars.is_empty());
        assert!(s.galaxies[0].scale.is_finite());
    }

    #[test]
    fn every_key_has_its_own_steady_angle() {
        assert_eq!(angle("c:/a"), angle("c:/a"));
        assert_ne!(angle("c:/a"), angle("c:/b"));
        assert!((0.0..std::f32::consts::TAU).contains(&angle("x")));
    }
}
