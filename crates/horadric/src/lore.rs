//! The lore tools of `horadric mcp`. Where the notes are kept is in
//! `horadric_hooks::lore`, what a note is and how one is found in
//! `horadric_core::lore`.

use horadric_core::lore;
use horadric_hooks::lore::{now, Place};
use serde_json::Value;

/// Answers a lore tool, or None when `name` is not one.
pub fn tool(name: &str, args: &Value) -> Option<Result<String, String>> {
    match name {
        "lore_note" => Some(note(args)),
        "lore_recall" => Some(recall(args)),
        _ => None,
    }
}

fn note(args: &Value) -> Result<String, String> {
    let text = args.get("text").and_then(Value::as_str).unwrap_or("");
    let replaces: Vec<u64> = list(args, "replaces")
        .iter()
        .filter_map(Value::as_u64)
        .collect();
    let place = Place::here()?;
    let marks = place.marks(files(&place, args)?);
    let commit = place.commit();
    let id = place.change(|l| l.add(text, marks, &commit, now(), &replaces))?;
    Ok(format!(
        "Kept as #{id}. Every session of this project can find it with lore_recall."
    ))
}

fn recall(args: &Value) -> Result<String, String> {
    let query = args.get("query").and_then(Value::as_str).unwrap_or("");
    let place = Place::here()?;
    let files = files(&place, args)?;
    let lore = place.read()?;
    let found = lore.recall(query, &files, |p| place.hash(p));
    Ok(lore::describe(&found, now()))
}

/// The `files` a tool was given, relative to the top.
fn files(place: &Place, args: &Value) -> Result<Vec<String>, String> {
    place.relative(list(args, "files").iter().filter_map(Value::as_str))
}

fn list<'a>(args: &'a Value, key: &str) -> &'a [Value] {
    args.get(key)
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}
