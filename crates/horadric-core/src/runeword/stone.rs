//! What a runeword's stone looks like, and the prompt of the Runesmith
//! who makes new ones.
//!
//! A stone's runeword name and the glyph cut into it both come from a hash
//! of its label, never its steps, so a stone keeps its look while its steps
//! are edited, and the same label looks the same on every machine.

use super::{Rune, Stone};
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
    smith(
        horadric,
        config,
        global,
        "First ask me what the stone should do, and whether it is for this project or \
         for every project.",
    )
}

/// What every Runesmith is told, with `ask` saying what it asks first.
fn smith(horadric: &str, config: &str, global: &str, ask: &str) -> String {
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
         Both have the same shape, with the label as the key and an \"about\" saying \
         in one sentence what the stone is for:\n\n\
         {{ \"runewords\": {{\n    \
         \"Fresh start\": {{ \"about\": \"Clears the conversation, then takes the next \
         quest\", \"steps\": [ {{ \"keys\": \"/clear{{Enter}}\" }}, \
         {{ \"say\": \"Read the plan and take the next quest\" }} ] }},\n    \
         \"Open the site\": {{ \"steps\": [ {{ \"run\": \"start http://localhost:3000\" }} ] }}\n\
         }} }}\n\n\
         A stone can also be an errand, cast on a clock with nobody clicking, when it \
         has \"every\": \"30m\", \"1h\", \"day 09:00\", \"weekday 08:30\" or a day's \
         name and a time such as \"sunday 12:00\", once a minute at most, or instead \
         \"on\": \"landed\", \"shipped\", \"away\" or \"back\" to be cast each time a \
         quest lands, the project ships, or I leave or come back. \"for\": \"1h\" \
         stops a cast that runs longer (30m when left out), and \"mode\": \"bypass\" lets \
         its session skip every permission prompt; add that only when I ask for it. An \
         errand has say, keys and run steps only, since test, review and merge need a \
         quest. One of only run steps runs hidden; one with say steps starts a fresh \
         session of its own in the project's main folder, which closes after the last \
         step. {{since}} in a step becomes the time, in UTC, of the last cast that \
         finished well, so a step can read only what is new. What an errand finds \
         becomes quests: a say step should tell it to file each finding with \
         `{horadric} quest add` and a notes line `From: <link or id>` naming the one \
         thing it came from, which is how it knows not to file it twice. An errand does \
         nothing until I arm it by clicking its stone in the Runetome, which shows me its \
         steps and its schedule, and changing its steps or its mode disarms it again. \
         You can not arm it, so tell me to. For example:\n\n\
         {{ \"Feedback to quests\": {{ \"about\": \"Turns new feedback into quests\", \
         \"every\": \"1h\", \"steps\": [ {{ \"say\": \"Read Slack #feedback since \
         {{since}}. File each new point as a quest with a From: line.\" }} ] }} }}\n\n\
         Keep everything else in the file as it is. {ask} Then write it, run \
         `{horadric} runeword list` to check it parses, and tell me the stone's runeword \
         name from that list."
    )
}

/// The first prompt of the Runesmith started to change a stone that is
/// there already, the one labelled `label` in `file`, rather than make a
/// new one. The rest is as [`smith_prompt`] says.
pub fn reforge_prompt(
    horadric: &str,
    config: &str,
    global: &str,
    label: &str,
    file: &str,
) -> String {
    smith(
        horadric,
        config,
        global,
        &format!(
            "This time I want to change the stone \"{label}\" in {file}, not make a new \
             one. Read it, tell me in a line what it does now, and ask me what should change."
        ),
    )
}

/// What hovering a stone says: its runeword name, what it is for when it
/// says, then its steps in order, so what a click does is never a guess. A cracked stone says why
/// it does not parse instead, and one marked `changed` that its steps are
/// not the ones last cast. An errand says when it runs, and `changed` that
/// it is not armed for these steps.
pub fn tip(stone: &Stone, changed: bool) -> String {
    let mut lines = vec![name(&stone.label)];
    if !stone.about.is_empty() {
        lines.push(stone.about.clone());
    }
    match &stone.steps {
        Ok(runes) => lines.extend(
            runes
                .iter()
                .enumerate()
                .map(|(i, r)| format!("{}. {}", i + 1, cut(&r.describe(), TIP_STEP_CHARS))),
        ),
        Err(why) => lines.push(format!("Cracked: {why}")),
    }
    match &stone.errand {
        Some(e) => {
            let mut when = super::describe(e.every);
            when[..1].make_ascii_uppercase();
            lines.push(format!("{when}, for {} at most", super::length(e.most)));
            if changed {
                lines.push("Not armed: a click arms it".into());
            }
        }
        None if changed => lines.push("Not cast since these steps came in".into()),
        None => {}
    }
    lines.join("\n")
}

/// What the human reads before a click casts a stone: what it is for,
/// what it is cast on, and every step, so nothing runs unseen. `changed`
/// says its steps are not the ones it was last cast with.
pub fn ask_text(stone: &Stone, on: &str, changed: bool) -> String {
    let mut out = String::new();
    if !stone.about.is_empty() {
        out.push_str(&stone.about);
        out.push_str("\n\n");
    }
    out.push_str(&format!("On {on}:"));
    for (i, r) in stone.runes().unwrap_or_default().iter().enumerate() {
        out.push_str(&format!(
            "\n{}. {}",
            i + 1,
            cut(&r.describe(), TIP_STEP_CHARS)
        ));
    }
    if changed {
        out.push_str("\n\nNot cast since these steps came in.");
    }
    out
}

/// What hovering the empty stone says.
pub const EMPTY_TIP: &str = "Make a new stone: the Runesmith asks what it should do";

/// The longest a step reads in a stone's tip.
const TIP_STEP_CHARS: usize = 120;

fn cut(text: &str, most: usize) -> String {
    if text.chars().count() <= most {
        return text.to_string();
    }
    let cut: String = text.chars().take(most).collect();
    format!("{}...", cut.trim_end())
}

/// A stone's steps as one number, kept when it is cast, so a project's
/// stone whose steps changed after (a pull, an agent's edit) is marked
/// until it is cast again. FNV-1a over the steps as JSON, the same on
/// every build, as it is saved.
pub fn fingerprint(runes: &[Rune]) -> u64 {
    let text = serde_json::to_string(runes).unwrap_or_default();
    text.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
    })
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

    #[test]
    fn the_smith_knows_errands_since_arming_and_from_lines() {
        let p = smith_prompt("horadric", "c.json", "g.json");
        for word in [
            "\"every\": \"30m\"",
            "\"day 09:00\"",
            "\"weekday 08:30\"",
            "\"sunday 12:00\"",
            "\"on\": \"landed\"",
            "\"for\"",
            "\"mode\": \"bypass\"",
            "{since} in a step",
            "since {since}.",
            "`horadric quest add`",
            "`From: <link or id>`",
            "until I arm it",
            "You can not arm it",
        ] {
            assert!(p.contains(word), "{word}");
        }
    }

    #[test]
    fn a_reforging_smith_is_told_which_stone_and_to_ask_what_changes() {
        let p = reforge_prompt(
            "horadric",
            ".horadric/config.json",
            "C:/g/runewords.json",
            "Deploy",
            "C:/app/.horadric/config.json",
        );
        assert!(p.contains("change the stone \"Deploy\" in C:/app/.horadric/config.json"));
        assert!(p.contains("ask me what should change"));
        assert!(!p.contains("First ask me what the stone should do"));
        assert!(p.contains("\"keys\""));
        assert!(p.contains("`horadric runeword list`"));
    }

    fn stone(label: &str, steps: Result<Vec<Rune>, String>) -> Stone {
        Stone {
            label: label.into(),
            steps,
            source: super::super::Source::Project,
            about: String::new(),
            errand: None,
        }
    }

    #[test]
    fn a_tip_names_the_stone_then_its_steps() {
        let s = stone(
            "Fresh start",
            Ok(vec![
                Rune::Keys("/clear{Enter}".into()),
                Rune::Say("Take the next quest".into()),
            ]),
        );
        assert_eq!(
            tip(&s, false),
            "Um Pul Zod Io\n1. keys /clear{Enter}\n2. say \"Take the next quest\""
        );
        assert!(tip(&s, true).ends_with("\nNot cast since these steps came in"));
        let about = Stone {
            about: "Clears it, then gives it the next quest".into(),
            ..s
        };
        assert!(tip(&about, false)
            .starts_with("Um Pul Zod Io\nClears it, then gives it the next quest\n1. keys"));
    }

    #[test]
    fn asking_before_a_cast_says_what_for_on_what_and_each_step() {
        let mut s = stone(
            "Ship",
            Ok(vec![Rune::Test, Rune::Say("Update the changelog".into())]),
        );
        assert_eq!(
            ask_text(&s, "fix-login", false),
            "On fix-login:\n1. test\n2. say \"Update the changelog\""
        );
        s.about = "Tests it and writes the changelog".into();
        assert_eq!(
            ask_text(&s, "fix-login", true),
            "Tests it and writes the changelog\n\nOn fix-login:\n1. test\n\
             2. say \"Update the changelog\"\n\nNot cast since these steps came in."
        );
    }

    #[test]
    fn a_cracked_stone_says_why_and_a_long_step_is_cut() {
        let s = stone("Fresh start", Err("step 2: \"say\" wants some text".into()));
        assert_eq!(
            tip(&s, false),
            "Um Pul Zod Io\nCracked: step 2: \"say\" wants some text"
        );
        let long = stone("Fresh start", Ok(vec![Rune::Say("x".repeat(400))]));
        let line = tip(&long, false).lines().nth(1).unwrap().to_string();
        assert!(
            line.ends_with("...") && line.chars().count() < 140,
            "{line}"
        );
    }

    #[test]
    fn a_fingerprint_changes_with_the_steps_and_only_with_them() {
        let a = vec![Rune::Test, Rune::Say("Tidy".into())];
        let b = vec![Rune::Test, Rune::Say("Tidy up".into())];
        assert_eq!(fingerprint(&a), fingerprint(&a.clone()));
        assert_ne!(fingerprint(&a), fingerprint(&b));
        // Pinned, as it is saved: a change to it would mark every stone.
        assert_eq!(fingerprint(&[]), 675_868_731_199_239_589);
    }
}
