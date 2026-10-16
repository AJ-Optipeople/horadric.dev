//! The store reads every format it ever wrote. `formats/v<N>` holds a
//! store written by version N, frozen: when the format moves on, the new
//! version gets a folder and a test of its own, and these stay as they
//! are. A store kept for years is read by builds years newer.

use std::path::{Path, PathBuf};

use horadric_memory::store::{ChunkId, Kind, Store};

fn frozen(version: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/formats")
        .join(version)
}

#[test]
fn version_1_reads() {
    let store = Store::open(&frozen("v1")).unwrap();
    assert!(store.foreign().is_empty());
    assert_eq!(store.len(), 2);

    let note = store.chunk(ChunkId::parse("2026-10#0").unwrap()).unwrap();
    assert_eq!(note.kind, Kind::Note);
    assert_eq!(note.time, 1_792_151_940);
    assert_eq!(note.project, "horadric");
    assert_eq!(note.session, None);
    assert_eq!(note.files[0].path, "crates/horadric-memory/src/store.rs");
    assert_eq!(note.files[0].hash, 0x9e37_79b9_7f4a_7c15);
    assert_eq!(
        note.text,
        "The store keeps text verbatim; vectors are a cache."
    );

    let said = store.chunk(ChunkId::parse("2026-10#1").unwrap()).unwrap();
    assert_eq!(said.kind, Kind::Dialogue);
    assert_eq!(said.session.as_deref(), Some("6c78acaa"));
    assert_eq!(said.quest.as_deref(), Some("Memory: the store"));
    assert_eq!(said.commit.as_deref(), Some("dacb6ae"));
    assert_eq!(said.text, "Line one\nline two, with \"quotes\" and æøå.");

    let tag = "all-MiniLM-L6-v2@1110a243";
    assert_eq!(store.models(), [(tag, 2)]);
    let found = store.nearest(tag, &[0.0, 1.0, 0.0], 2);
    assert_eq!(found[0].0.to_string(), "2026-10#1");
    assert!((found[0].1 - 1.0).abs() < 0.01);
    let found = store.nearest(tag, &[0.6, 0.0, -0.8], 1);
    assert_eq!(found[0].0.to_string(), "2026-10#0");
}
