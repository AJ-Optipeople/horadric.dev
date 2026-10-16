//! Measures Horadrim on a real store, as part 4 keeps dialogue: how many
//! tokens its passages are word for word and inscribed, by this crate's
//! estimate. The yardstick's dialogue predates part 4 (no tool lines), so
//! this is the other half of the measure.
//!
//! `cargo run --release -p horadric-memory --example tongue [memory folder]`,
//! by default the installed Horadric's `%APPDATA%\Horadric\memory`. It
//! only reads: the store is opened, never written, and the Lexicon is
//! grown in memory.

use std::collections::BTreeMap;
use std::path::PathBuf;

use horadric_memory::horadrim::{self, Hit, Lexicon};
use horadric_memory::store::{Kind, Store};

/// How many passages one answer shows, as `lore_recall` does.
const ANSWER: usize = 10;

fn main() {
    let dir = std::env::args().nth(1).map(PathBuf::from).unwrap_or_else(|| {
        PathBuf::from(std::env::var("APPDATA").expect("APPDATA")).join("Horadric/memory")
    });
    let store = Store::open(&dir).expect("store");
    let mut by_project: BTreeMap<&str, Vec<Hit>> = BTreeMap::new();
    for (id, c) in store.chunks() {
        by_project.entry(&c.project).or_default().push(Hit {
            head: format!("{id} ({:?}, a day ago)", c.kind).to_lowercase(),
            kind: c.kind,
            text: c.text.clone(),
            files: c.files.iter().map(|f| f.path.clone()).collect(),
        });
    }
    for (project, hits) in by_project {
        if hits.len() < 200 {
            continue;
        }
        let mut lex = Lexicon::default();
        lex.grow(hits.iter().map(|h| h.text.as_str()), |_| true, 0);
        let dialogue = hits.iter().filter(|h| h.kind == Kind::Dialogue).count();
        let (mut plain, mut stripped, mut alone, mut session) = (0, 0, 0, 0);
        let mut known: Vec<String> = Vec::new();
        // Answers of ten passages that sit together, as a recall about one
        // thing finds; a session asks five of them.
        for (n, group) in hits.chunks(ANSWER).enumerate() {
            plain += horadrim::tokens(&horadrim::plain(group));
            let only_strip: Vec<Hit> = group
                .iter()
                .map(|h| Hit {
                    text: if h.kind == Kind::Dialogue {
                        horadrim::strip(&h.text)
                    } else {
                        h.text.clone()
                    },
                    ..h.clone()
                })
                .collect();
            stripped += horadrim::tokens(&horadrim::plain(&only_strip));
            alone += horadrim::tokens(&horadrim::inscribe(&mut lex.clone(), &[], group, 0).text);
            if n % 5 == 0 {
                known.clear();
            }
            let a = horadrim::inscribe(&mut lex, &known, group, 0);
            known.extend(a.given);
            session += horadrim::tokens(&a.text);
        }
        let x = |t: usize| plain as f64 / t.max(1) as f64;
        println!(
            "{project}: {} passages ({dialogue} dialogue), {} sigils, {:.0} tokens a passage\n  \
             stripped only {:.2}x, inscribed alone {:.2}x, in a session of 5 {:.2}x",
            hits.len(),
            lex.sigils.len(),
            plain as f64 / hits.len() as f64,
            x(stripped),
            x(alone),
            x(session),
        );
    }
}
