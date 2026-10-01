//! What a runeword's stone looks like, and the prompt of the Runesmith
//! who makes new ones.
//!
//! A stone's runeword name and the glyph cut into it both come from a hash
//! of its label, never its steps, so a stone keeps its look while its steps
//! are edited, and the same label looks the same on every machine.

use crate::tasks::one_line;

/// The 33 runes, in the order the game ranks them.
pub const RUNES: [&str; 33] = [
    "El", "Eld", "Tir", "Nef", "Eth", "Ith", "Tal", "Ral", "Ort", "Thul", "Amn", "Sol", "Shael",
    "Dol", "Hel", "Io", "Lum", "Ko", "Fal", "Lem", "Pul", "Um", "Mal", "Ist", "Gul", "Vex", "Ohm",
    "Lo", "Sur", "Ber", "Jah", "Cham", "Zod",
];

/// A stone's runeword name: two to four runes, none twice ("Tal Eth Ko").
pub fn name(label: &str) -> String {
    let mut dice = Dice::new(label, 1);
    let count = 2 + dice.below(3);
    let mut picked: Vec<usize> = Vec::new();
    while picked.len() < count {
        let rune = dice.below(RUNES.len());
        if !picked.contains(&rune) {
            picked.push(rune);
        }
    }
    picked
        .iter()
        .map(|&i| RUNES[i])
        .collect::<Vec<_>>()
        .join(" ")
}

/// One straight cut of a glyph, from and to a point in the unit square,
/// `y` down.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Stroke {
    pub from: (f32, f32),
    pub to: (f32, f32),
}

/// What is cut into a stone and how rough its edge is.
#[derive(Debug, Clone, PartialEq)]
pub struct Carving {
    /// The glyph: one or two upright staves and the branches off them.
    pub strokes: Vec<Stroke>,
    /// How far each of the slab's corners and sides sits out or in, from
    /// -1 to 1, clockwise from the top left. The painter scales it.
    pub edge: [f32; EDGE_POINTS],
}

/// The points round a stone's edge that are roughened: a corner and the
/// middle of each side, twice over.
pub const EDGE_POINTS: usize = 16;

/// Where the cuts of a glyph start and end, top to bottom. Snapping to a
/// grid is what makes random strokes read as a rune and not a scribble.
const LEVELS: [f32; 4] = [0.1, 0.37, 0.63, 0.9];

/// The glyph and the edge of the stone labelled `label`.
pub fn carve(label: &str) -> Carving {
    let mut dice = Dice::new(label, 2);
    // Most runes stand on one stave; one in five on two, as ᚺ and ᛗ do.
    let staves: &[f32] = if dice.below(5) == 0 {
        &[0.25, 0.75]
    } else {
        &[0.5]
    };
    let mut strokes: Vec<Stroke> = staves
        .iter()
        .map(|&x| Stroke {
            from: (x, LEVELS[0]),
            to: (x, LEVELS[3]),
        })
        .collect();
    let branches = branches(staves);
    let wanted = 1 + dice.below(3);
    let mut cut = 0;
    while cut < wanted {
        let b = branches[dice.below(branches.len())];
        if !strokes.contains(&b) {
            strokes.push(b);
            cut += 1;
        }
        // Many runes are mirrored about their stave, as ᛉ and ᛏ are.
        let m = mirrored(b);
        if staves.len() == 1 && dice.below(5) < 2 && !strokes.contains(&m) {
            strokes.push(m);
            cut += 1;
        }
    }
    let mut edge = [0.0; EDGE_POINTS];
    for e in &mut edge {
        *e = dice.below(2001) as f32 / 1000.0 - 1.0;
    }
    Carving { strokes, edge }
}

/// Every branch a glyph on `staves` may have: from a level of a stave to
/// the same or the next level of the column beside it.
fn branches(staves: &[f32]) -> Vec<Stroke> {
    let columns: &[f32] = if staves.len() == 1 {
        &[0.15, 0.85]
    } else {
        &[0.75]
    };
    let mut out = Vec::new();
    let x = staves[0];
    for &side in columns {
        for (i, &y) in LEVELS.iter().enumerate() {
            let near = LEVELS.iter().enumerate().skip(i.saturating_sub(1)).take(3);
            for (j, &to) in near.take_while(|(j, _)| *j <= i + 1) {
                // A flat branch off a single stave reads as a cross, not a
                // rune; between two staves it is the bar of ᚺ.
                if i == j && staves.len() == 1 {
                    continue;
                }
                out.push(Stroke {
                    from: (x, y),
                    to: (side, to),
                });
            }
        }
    }
    out
}

fn mirrored(s: Stroke) -> Stroke {
    let flip = |(x, y): (f32, f32)| (1.0 - x, y);
    Stroke {
        from: flip(s.from),
        to: flip(s.to),
    }
}

/// A run of numbers from a label, the same on every machine and in every
/// build, unlike std's hasher. `stream` keeps the name and the glyph from
/// using the same numbers.
struct Dice(u64);

impl Dice {
    fn new(label: &str, stream: u64) -> Dice {
        // FNV-1a over the label as it shows, so spacing does not count.
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for b in one_line(label).bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
        Dice(h ^ stream.wrapping_mul(0x9E37_79B9_7F4A_7C15))
    }

    /// splitmix64, which spreads even neighbouring seeds well.
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// The first prompt of the Runesmith, the session the empty stone starts
/// to make a new stone. `horadric` is how to run this Horadric from the
/// agent's shell, `config` the project's config file and `global` the
/// file of stones every project has.
pub fn smith_prompt(horadric: &str, config: &str, global: &str) -> String {
    format!(
        "You are the Runesmith for this project. You make rune stones: buttons in \
         Horadric's Runetome that do something when clicked. A stone has a label and a \
         list of steps, cast in order, each one waiting for the one before:\n\
         - {{\"say\": \"text\"}} types the text to the session as a prompt and waits for \
         its turn to end.\n\
         - {{\"keys\": \"/clear{{Enter}}\"}} types keys into the session's terminal at \
         once, such as \"Esc\", \"Ctrl+C\" or text with {{Enter}} in it.\n\
         - {{\"run\": \"command\"}} runs a command with cmd /c in the project's folder, \
         hidden, and stops the stone if it fails. Add \"show\": true to run it in a \
         terminal pane the human can watch. Anything outside Horadric, such as opening \
         a page or a program, is a run step.\n\
         - \"test\", \"review\" and \"merge\" are the runes: the session runs the tests \
         and fixes them, a reviewer reads its work and it answers the review, its branch \
         is merged.\n\n\
         Stones for this project go in {config}, stones for every project in {global}. \
         Both have the same shape, with the label as the key:\n\n\
         {{ \"runewords\": {{\n    \
         \"Fresh start\": {{ \"steps\": [ {{ \"keys\": \"/clear{{Enter}}\" }}, \
         {{ \"say\": \"Read the plan and take the next quest\" }} ] }},\n    \
         \"Open the site\": {{ \"steps\": [ {{ \"run\": \"start http://localhost:3000\" }} ] }}\n\
         }} }}\n\n\
         Keep everything else in the file as it is. First ask me what the stone should \
         do, and whether it is for this project or for every project. Then write it, run \
         `{horadric} runeword list` to check it parses, and tell me the stone's runeword \
         name from that list."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const LABELS: [&str; 24] = [
        "Fresh start",
        "Open the site",
        "Ship",
        "Test, merge",
        "Test, review, merge",
        "Review, merge",
        "Deploy",
        "Clear",
        "Lint",
        "Build docs",
        "Release notes",
        "Run the dev server",
        "Format",
        "Restart",
        "Open in VS Code",
        "Bump the version",
        "Stop",
        "Explain this",
        "Commit",
        "Push",
        "Pull",
        "Update deps",
        "Screenshot",
        "Benchmark",
    ];

    #[test]
    fn a_name_is_two_to_four_runes_none_twice() {
        for label in LABELS {
            let n = name(label);
            let runes: Vec<&str> = n.split(' ').collect();
            assert!((2..=4).contains(&runes.len()), "{label}: {n}");
            assert!(runes.iter().all(|r| RUNES.contains(r)), "{label}: {n}");
            let mut seen = runes.clone();
            seen.sort();
            seen.dedup();
            assert_eq!(seen.len(), runes.len(), "{label}: {n}");
        }
    }

    #[test]
    fn the_same_label_gives_the_same_stone_and_spacing_does_not_count() {
        assert_eq!(name("Fresh start"), name("Fresh start"));
        assert_eq!(name("Fresh start"), name("  Fresh\n start "));
        assert_eq!(carve("Ship"), carve("Ship"));
        assert_eq!(carve("Ship"), carve(" Ship"));
        // Pinned, so a change to the hash that would give every stone a new
        // look and name is a test that fails, not a surprise.
        assert_eq!(name("Fresh start"), "Um Pul Zod Io");
    }

    #[test]
    fn two_stones_rarely_match() {
        let mut names: Vec<String> = LABELS.iter().map(|l| name(l)).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), LABELS.len());
        let glyphs: Vec<Vec<Stroke>> = LABELS.iter().map(|l| carve(l).strokes).collect();
        let distinct = glyphs
            .iter()
            .enumerate()
            .filter(|(i, g)| !glyphs[..*i].contains(g))
            .count();
        assert!(
            distinct >= LABELS.len() - 2,
            "{distinct} of {}",
            LABELS.len()
        );
    }

    #[test]
    fn a_glyph_stands_on_its_staves_inside_the_stone() {
        for label in LABELS {
            let c = carve(label);
            let upright = c.strokes.iter().filter(|s| s.from.0 == s.to.0).count();
            assert!((1..=2).contains(&upright), "{label}");
            let branches = c.strokes.len() - upright;
            assert!((1..=4).contains(&branches), "{label}: {branches}");
            for (i, s) in c.strokes.iter().enumerate() {
                for (x, y) in [s.from, s.to] {
                    assert!(
                        (0.0..=1.0).contains(&x) && (0.0..=1.0).contains(&y),
                        "{label}"
                    );
                }
                assert_ne!(s.from, s.to, "{label}");
                assert!(!c.strokes[..i].contains(s), "{label}: a stroke twice");
            }
            assert!(c.edge.iter().all(|e| (-1.0..=1.0).contains(e)), "{label}");
            assert!(c.edge.iter().any(|&e| e != 0.0), "{label}: a smooth edge");
        }
    }

    #[test]
    fn the_smith_is_told_the_steps_the_files_and_to_ask_first() {
        let p = smith_prompt(
            "horadric",
            ".horadric/config.json",
            "C:/Users/me/AppData/Roaming/Horadric/runewords.json",
        );
        for step in [
            "\"say\"",
            "\"keys\"",
            "\"run\"",
            "\"show\": true",
            "\"test\"",
        ] {
            assert!(p.contains(step), "{step}");
        }
        assert!(p.contains("go in .horadric/config.json"));
        assert!(p.contains("every project in C:/Users/me/AppData/Roaming/Horadric/runewords.json"));
        assert!(p.contains("\"runewords\""));
        assert!(p.contains("{ \"keys\": \"/clear{Enter}\" }"));
        assert!(p.contains("First ask me what the stone should do"));
        assert!(p.contains("for this project or for every project"));
        assert!(p.contains("`horadric runeword list`"));
        assert!(p.contains("runeword name"));
    }
}
