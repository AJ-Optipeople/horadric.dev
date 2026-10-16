//! The models against numbers their makers published, so a change to the
//! layers written here cannot quietly make different vectors. Skipped
//! when the model files have not been fetched into the dev state folder.

use std::path::PathBuf;

use horadric_memory::{similarity, Embedder, Kind};

const QUERY: &str = "Which planet is known as the Red Planet?";
const DOCS: [&str; 4] = [
    "Venus is often called Earth's twin because of its similar size and proximity.",
    "Mars, known for its reddish appearance, is often referred to as the Red Planet.",
    "Jupiter, the largest planet in our solar system, has a prominent red spot.",
    "Saturn, famous for its rings, is sometimes mistaken for the Red Planet.",
];

fn load(kind: Kind) -> Option<Embedder> {
    let dir = PathBuf::from(std::env::var_os("APPDATA")?)
        .join("Horadric-dev/models")
        .join(kind.folder());
    if !dir.join("model.safetensors").exists() {
        eprintln!("skipped: no {} in {}", kind.folder(), dir.display());
        return None;
    }
    Some(Embedder::load(kind, &dir).expect("model loads"))
}

fn scores(e: &Embedder) -> Vec<f32> {
    let q = e.query(QUERY).unwrap();
    DOCS.iter()
        .map(|d| similarity(&q, &e.document(d).unwrap()))
        .collect()
}

#[test]
fn embedding_gemma_matches_its_model_card() {
    let Some(e) = load(Kind::EmbeddingGemma) else {
        return;
    };
    // From the model card on Hugging Face, made with sentence-transformers.
    let card = [0.3011, 0.6359, 0.4930, 0.4889];
    for (got, want) in scores(&e).iter().zip(card) {
        assert!((got - want).abs() < 0.002, "{got} against {want}");
    }
}

#[test]
fn mini_lm_finds_mars() {
    let Some(e) = load(Kind::MiniLm) else {
        return;
    };
    let s = scores(&e);
    let best = (0..4).max_by(|a, b| s[*a].total_cmp(&s[*b])).unwrap();
    assert_eq!(best, 1, "{s:?}");
    assert_eq!(e.query("x").unwrap().len(), 384);
}
