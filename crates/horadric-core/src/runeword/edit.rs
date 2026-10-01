//! Taking a stone out of the file it is written in, by hand, so the rest
//! of the file stays exactly as its owner wrote it: serde would put every
//! key in alphabetical order and lose the spacing, and a project's config
//! is a file people read and keep in git.

use crate::tasks::one_line;

/// `text` without the stone labelled `label` in its `runewords`. Every
/// other byte is kept, the comma between it and a neighbour included
/// once. An error when the text is not JSON or has no such stone.
pub fn unwrite(text: &str, label: &str) -> Result<String, String> {
    serde_json::from_str::<serde_json::Value>(text).map_err(|e| format!("not JSON: {e}"))?;
    let b = text.as_bytes();
    let top = members(b, skip_ws(b, 0)).ok_or("the file is not an object")?;
    let words = top
        .iter()
        .find(|m| key(text, m) == "runewords")
        .ok_or("the file has no runewords")?;
    if b.get(words.value) != Some(&b'{') {
        return Err("\"runewords\" is not an object".into());
    }
    let stones = members(b, words.value).ok_or("\"runewords\" does not close")?;
    let want = one_line(label);
    let i = stones
        .iter()
        .position(|m| one_line(&key(text, m)) == want)
        .ok_or_else(|| format!("no stone called \"{label}\""))?;
    let m = &stones[i];
    let (from, to) = match (i.checked_sub(1).map(|j| &stones[j]), stones.get(i + 1)) {
        // Up to the next one's key, which takes the comma after it.
        (_, Some(next)) => (m.key, next.key),
        // From the end of the one before, which takes the comma before it.
        (Some(before), None) => (before.end, m.end),
        // The only one: the object is left empty.
        (None, None) => (words.value + 1, close_of(b, m.end)),
    };
    Ok(format!("{}{}", &text[..from], &text[to..]))
}

/// One `"key": value` of an object, by byte offsets: where its key starts,
/// where its value starts, and just past the value.
struct Member {
    key: usize,
    value: usize,
    end: usize,
}

fn key(text: &str, m: &Member) -> String {
    let b = text.as_bytes();
    let end = skip_string(b, m.key).unwrap_or(m.key);
    serde_json::from_str(&text[m.key..end]).unwrap_or_default()
}

/// The members of the object whose `{` is at `at`. None for what is not
/// one, which the JSON check before rules out but for a broken offset.
fn members(b: &[u8], at: usize) -> Option<Vec<Member>> {
    if b.get(at) != Some(&b'{') {
        return None;
    }
    let mut out = Vec::new();
    let mut i = skip_ws(b, at + 1);
    while b.get(i) == Some(&b'"') {
        let key = i;
        i = skip_ws(b, skip_string(b, i)?);
        if b.get(i) != Some(&b':') {
            return None;
        }
        let value = skip_ws(b, i + 1);
        let end = skip_value(b, value)?;
        out.push(Member { key, value, end });
        i = skip_ws(b, end);
        if b.get(i) == Some(&b',') {
            i = skip_ws(b, i + 1);
        }
    }
    Some(out)
}

/// The offset of the `}` that closes the object a member ending at `at`
/// is in, leaving the whitespace before it out.
fn close_of(b: &[u8], at: usize) -> usize {
    let mut i = skip_ws(b, at);
    if b.get(i) == Some(&b',') {
        i = skip_ws(b, i + 1);
    }
    i
}

fn skip_ws(b: &[u8], mut i: usize) -> usize {
    while b.get(i).is_some_and(u8::is_ascii_whitespace) {
        i += 1;
    }
    i
}

/// Just past the string whose opening quote is at `i`.
fn skip_string(b: &[u8], mut i: usize) -> Option<usize> {
    i += 1;
    loop {
        match b.get(i)? {
            b'\\' => i += 2,
            b'"' => return Some(i + 1),
            _ => i += 1,
        }
    }
}

/// Just past the value starting at `i`.
fn skip_value(b: &[u8], i: usize) -> Option<usize> {
    match b.get(i)? {
        b'"' => skip_string(b, i),
        b'{' | b'[' => {
            let mut depth = 0usize;
            let mut j = i;
            loop {
                match b.get(j)? {
                    b'"' => {
                        j = skip_string(b, j)?;
                        continue;
                    }
                    b'{' | b'[' => depth += 1,
                    b'}' | b']' => {
                        depth -= 1;
                        if depth == 0 {
                            return Some(j + 1);
                        }
                    }
                    _ => {}
                }
                j += 1;
            }
        }
        _ => {
            let mut j = i;
            while b
                .get(j)
                .is_some_and(|c| !matches!(c, b',' | b'}' | b']') && !c.is_ascii_whitespace())
            {
                j += 1;
            }
            Some(j)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONFIG: &str = r#"{
  "tasks": { "mode": "auto" },
  "runewords": {
    "Ship": ["test", "Say \"hi\" {Enter}", "merge"],
    "Fresh start": { "steps": [ { "keys": "/clear{Enter}" } ] },
    "Open": { "steps": [ { "run": "start http://localhost:3000" } ] }
  },
  "hosts": ["box"]
}
"#;

    fn labels(text: &str) -> Vec<String> {
        crate::runeword::parse(text)
            .unwrap()
            .into_iter()
            .map(|(l, _)| l)
            .collect()
    }

    #[test]
    fn a_stone_in_the_middle_goes_with_one_comma() {
        let out = unwrite(CONFIG, "Fresh start").unwrap();
        assert_eq!(
            out,
            CONFIG.replace(
                "\"Fresh start\": { \"steps\": [ { \"keys\": \"/clear{Enter}\" } ] },\n    ",
                ""
            )
        );
        assert_eq!(labels(&out), ["Open", "Ship"]);
    }

    #[test]
    fn the_first_and_the_last_stone_go_and_the_rest_stays() {
        let first = unwrite(CONFIG, "Ship").unwrap();
        assert_eq!(labels(&first), ["Fresh start", "Open"]);
        assert!(first.contains("\"tasks\": { \"mode\": \"auto\" },"));
        let last = unwrite(CONFIG, "Open").unwrap();
        assert_eq!(labels(&last), ["Fresh start", "Ship"]);
        assert!(
            last.contains("/clear{Enter}\" } ] }\n  },\n  \"hosts\""),
            "{last}"
        );
    }

    #[test]
    fn the_only_stone_leaves_an_empty_object() {
        let text = "{ \"runewords\": { \"A\": [\"test\"] } }";
        assert_eq!(unwrite(text, "A").unwrap(), "{ \"runewords\": {} }");
    }

    #[test]
    fn a_label_matches_as_the_tome_shows_it() {
        let text = "{\"runewords\":{\"Fresh\\n  start\":[\"test\"],\"B\":[\"merge\"]}}";
        assert_eq!(
            unwrite(text, "Fresh start").unwrap(),
            "{\"runewords\":{\"B\":[\"merge\"]}}"
        );
    }

    #[test]
    fn what_cannot_be_removed_says_why() {
        assert!(unwrite("{ not json", "A")
            .unwrap_err()
            .starts_with("not JSON"));
        assert_eq!(
            unwrite("{\"tasks\":{}}", "A").unwrap_err(),
            "the file has no runewords"
        );
        assert_eq!(
            unwrite(CONFIG, "Deploy").unwrap_err(),
            "no stone called \"Deploy\""
        );
    }
}
