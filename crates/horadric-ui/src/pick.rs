//! Something the human picked on the stage, handed to an agent's prompt.
//!
//! A pick goes in as one short line, a reference, and its whole payload
//! waits in a file the agent can read. A long terminal selection or a
//! page element's markup would otherwise flood the prompt box, and the
//! human could no longer see what they were about to send.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// How long a pick's files are kept, as long as a pasted image's.
const KEEP: Duration = Duration::from_secs(24 * 60 * 60);
/// How much of the picked text the reference quotes.
const PREVIEW: usize = 40;
/// How much of a page element's text and markup the payload keeps.
const TEXT_MAX: usize = 4000;
const HTML_MAX: usize = 8000;

/// What was picked.
#[derive(Debug, Clone, PartialEq)]
pub enum Pick {
    /// An element of the page in the browser pane.
    Element(Element),
    /// Text selected in a terminal, with the name of its pane.
    Terminal { from: String, text: String },
    /// Lines selected in the file viewer, counted from one.
    File {
        path: PathBuf,
        first: usize,
        last: usize,
    },
}

/// A page element, as the page's pick script describes it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Element {
    pub url: String,
    pub title: String,
    pub selector: String,
    pub tag: String,
    pub text: String,
    pub html: String,
    /// Where it is in the page, in CSS pixels from the document's corner,
    /// for the crop: x, y, width, height.
    pub rect: [f64; 4],
    /// A picture of it, once saved.
    pub crop: Option<PathBuf>,
}

impl Pick {
    /// Whether the pick needs a payload file: a file range is on disk
    /// already.
    pub fn has_payload(&self) -> bool {
        !matches!(self, Pick::File { .. })
    }

    /// What the payload file says, in Markdown.
    pub fn payload(&self) -> String {
        match self {
            Pick::Element(e) => {
                let mut out = format!("# Picked in the browser\n\nPage: {}\n", e.url);
                if !e.title.is_empty() {
                    out += &format!("Title: {}\n", e.title);
                }
                out += &format!("Element: <{}>\nSelector: `{}`\n", e.tag, e.selector);
                if let Some(crop) = &e.crop {
                    out += &format!("Picture: {}\n", crop.display());
                }
                let text = clip(e.text.trim(), TEXT_MAX);
                if !text.is_empty() {
                    out += &format!("\n## Text\n\n{}\n", fenced(&text, ""));
                }
                out += &format!(
                    "\n## Markup\n\n{}\n",
                    fenced(&clip(&e.html, HTML_MAX), "html")
                );
                out
            }
            Pick::Terminal { from, text } => {
                format!(
                    "# Picked from the terminal \"{from}\"\n\n{}\n",
                    fenced(text, "")
                )
            }
            Pick::File { path, first, last } => {
                format!("{} {}\n", path.display(), lines(*first, *last))
            }
        }
    }

    /// The one line the prompt gets. `payload` is the file holding the
    /// rest, for a pick that has one.
    pub fn reference(&self, payload: Option<&Path>) -> String {
        let full = || {
            payload
                .map(|p| format!(", full pick in {}", quoted(p)))
                .unwrap_or_default()
        };
        match self {
            Pick::Element(e) => {
                let quote = preview(&e.text)
                    .map(|q| format!(" \"{q}\""))
                    .unwrap_or_default();
                format!(
                    "[picked in the browser: <{}>{quote} on {}{}] ",
                    e.tag,
                    site(&e.url),
                    full()
                )
            }
            Pick::Terminal { from, text } => {
                let n = text.lines().count().max(1);
                let count = if n == 1 {
                    String::new()
                } else {
                    format!(", {n} lines")
                };
                let quote = preview(text)
                    .map(|q| format!(" \"{q}\""))
                    .unwrap_or_default();
                format!(
                    "[picked from terminal \"{from}\":{quote}{count}{}] ",
                    full()
                )
            }
            Pick::File { path, first, last } => format!(
                "[picked in the file viewer: {} {}] ",
                quoted(path),
                lines(*first, *last)
            ),
        }
    }
}

/// The first line with something on it, cut short, with no quote marks
/// to confuse the reference's own.
pub fn preview(text: &str) -> Option<String> {
    let line = text.lines().map(str::trim).find(|l| !l.is_empty())?;
    let line = line.replace('"', "'");
    let cut: String = line.chars().take(PREVIEW).collect();
    Some(if cut.len() < line.len() {
        format!("{}...", cut.trim_end())
    } else {
        cut
    })
}

/// A page's address without its scheme and query, which is enough to
/// say where an element came from.
pub fn site(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let rest = rest.split(['?', '#']).next().unwrap_or(rest);
    rest.trim_end_matches('/').to_string()
}

fn lines(first: usize, last: usize) -> String {
    if first == last {
        format!("line {first}")
    } else {
        format!("lines {first} to {last}")
    }
}

fn quoted(p: &Path) -> String {
    crate::paste::quote_paths(&[p.to_string_lossy()])
}

/// Cut at a character boundary, saying so.
pub fn clip(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n(cut, {} bytes in all)", &text[..end], text.len())
}

/// A code fence long enough that no run of backticks in `text` closes it.
fn fenced(text: &str, lang: &str) -> String {
    let mut longest = 0;
    let mut run = 0;
    for c in text.chars() {
        run = if c == '`' { run + 1 } else { 0 };
        longest = longest.max(run);
    }
    let fence = "`".repeat((longest + 1).max(3));
    format!("{fence}{lang}\n{}\n{fence}", text.trim_end_matches('\n'))
}

/// Where picks are kept: beside pasted images.
pub fn dir() -> PathBuf {
    std::env::temp_dir().join("Horadric")
}

/// A new pick's file name with this extension, unique by the clock.
pub fn new_path(ext: &str) -> PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or_default();
    dir().join(format!("pick-{stamp}.{ext}"))
}

/// Writes the pick's payload, when it has one, and gives the reference
/// for the prompt. Old picks go first.
pub fn save(pick: &Pick) -> Option<String> {
    if !pick.has_payload() {
        return Some(pick.reference(None));
    }
    let dir = dir();
    std::fs::create_dir_all(&dir).ok()?;
    forget_old(&dir);
    let path = new_path("md");
    std::fs::write(&path, pick.payload()).ok()?;
    Some(pick.reference(Some(&path)))
}

fn forget_old(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let ours = entry.file_name().to_string_lossy().starts_with("pick-");
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > KEEP);
        if ours && old {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// The page script that lets the human pick an element: it outlines what
/// is under the mouse, takes the next click for itself and answers with
/// the element as JSON, or null for Esc. Run with `awaitPromise`.
pub const SCRIPT: &str = r#"new Promise(done => {
  const glow = '#c9a24a';
  const box = document.createElement('div');
  box.style.cssText = `position:fixed;z-index:2147483647;pointer-events:none;border:2px solid ${glow};background:rgba(201,162,74,.14);border-radius:3px;display:none`;
  const tag = document.createElement('div');
  tag.style.cssText = `position:fixed;z-index:2147483647;pointer-events:none;font:12px/1.4 system-ui,sans-serif;background:#16130e;color:${glow};padding:2px 7px;border-radius:3px;display:none;white-space:nowrap`;
  document.documentElement.append(box, tag);
  let at = null;
  const sel = el => {
    if (el.id && document.querySelectorAll('#' + CSS.escape(el.id)).length === 1) return '#' + CSS.escape(el.id);
    const parts = [];
    for (let e = el; e && e.nodeType === 1 && e !== document.documentElement; e = e.parentElement) {
      if (e.id && document.querySelectorAll('#' + CSS.escape(e.id)).length === 1) { parts.unshift('#' + CSS.escape(e.id)); break; }
      let part = e.localName;
      const same = e.parentElement ? [...e.parentElement.children].filter(c => c.localName === e.localName) : [];
      if (same.length > 1) part += `:nth-of-type(${same.indexOf(e) + 1})`;
      parts.unshift(part);
    }
    return parts.join(' > ');
  };
  const show = e => {
    const el = document.elementFromPoint(e.clientX, e.clientY);
    if (!el || el === box || el === tag) return;
    at = el;
    const r = el.getBoundingClientRect();
    Object.assign(box.style, { display: 'block', left: r.left + 'px', top: r.top + 'px', width: r.width + 'px', height: r.height + 'px' });
    tag.textContent = el.localName + (el.id ? '#' + el.id : '') + '  click to hand it over, Esc to stop';
    const below = r.bottom + 24 < innerHeight;
    Object.assign(tag.style, { display: 'block', left: Math.max(0, r.left) + 'px', top: (below ? r.bottom + 4 : Math.max(0, r.top - 24)) + 'px' });
  };
  const swallow = e => { e.preventDefault(); e.stopPropagation(); e.stopImmediatePropagation(); };
  const kinds = ['pointerdown', 'pointerup', 'mousedown', 'mouseup', 'auxclick', 'contextmenu'];
  const end = v => {
    removeEventListener('mousemove', show, true);
    removeEventListener('click', click, true);
    removeEventListener('keydown', key, true);
    kinds.forEach(k => removeEventListener(k, swallow, true));
    box.remove(); tag.remove();
    done(v);
  };
  const click = e => {
    swallow(e);
    if (!at) return;
    const r = at.getBoundingClientRect();
    end(JSON.stringify({
      url: location.href, title: document.title, selector: sel(at), tag: at.localName,
      text: (at.innerText || at.textContent || '').slice(0, 20000),
      html: at.outerHTML.slice(0, 40000),
      rect: [r.left + scrollX, r.top + scrollY, r.width, r.height],
    }));
  };
  const key = e => { if (e.key === 'Escape') { swallow(e); end(null); } };
  addEventListener('mousemove', show, true);
  addEventListener('click', click, true);
  addEventListener('keydown', key, true);
  kinds.forEach(k => addEventListener(k, swallow, true));
})"#;

/// The element out of `Runtime.evaluate`'s answer to [`SCRIPT`]: None when
/// the human pressed Esc or the page went away.
pub fn element_of(answer: &str) -> Option<Element> {
    let v: serde_json::Value = serde_json::from_str(answer).ok()?;
    let json = v.get("result")?.get("value")?.as_str()?;
    let e: serde_json::Value = serde_json::from_str(json).ok()?;
    let text = |k: &str| {
        e.get(k)
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string()
    };
    let mut rect = [0.0; 4];
    if let Some(r) = e.get("rect").and_then(|r| r.as_array()) {
        for (to, from) in rect.iter_mut().zip(r) {
            *to = from.as_f64().unwrap_or_default();
        }
    }
    Some(Element {
        url: text("url"),
        title: text("title"),
        selector: text("selector"),
        tag: text("tag"),
        text: text("text"),
        html: text("html"),
        rect,
        crop: None,
    })
}

/// The `Page.captureScreenshot` call's parameters for an element at
/// `rect`, with a margin round it and no larger than a screen or two.
pub fn crop_params(rect: [f64; 4]) -> String {
    const MARGIN: f64 = 8.0;
    const MAX: f64 = 1600.0;
    let [x, y, w, h] = rect;
    let x = (x - MARGIN).max(0.0);
    let y = (y - MARGIN).max(0.0);
    let w = (w + 2.0 * MARGIN).clamp(1.0, MAX);
    let h = (h + 2.0 * MARGIN).clamp(1.0, MAX);
    serde_json::json!({
        "format": "png",
        "clip": { "x": x, "y": y, "width": w, "height": h, "scale": 1 },
        "captureBeyondViewport": true,
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn element() -> Element {
        Element {
            url: "http://localhost:3000/login?next=/#top".into(),
            title: "Sign in".into(),
            selector: "#go".into(),
            tag: "button".into(),
            text: "  \n Sign in with \"Google\" and keep going for a while longer\n".into(),
            html: "<button id=\"go\">Sign in</button>".into(),
            rect: [10.0, 20.0, 100.0, 30.0],
            crop: None,
        }
    }

    #[test]
    fn an_element_is_referenced_by_its_tag_text_and_site() {
        let r = Pick::Element(element()).reference(Some(Path::new(r"C:\t\pick-1.md")));
        assert_eq!(
            r,
            "[picked in the browser: <button> \"Sign in with 'Google' and keep going for...\" \
             on localhost:3000/login, full pick in C:\\t\\pick-1.md] "
        );
        assert!(!r.contains('\n'));
    }

    #[test]
    fn a_terminal_pick_counts_its_lines_and_quotes_the_first() {
        let pick = Pick::Terminal {
            from: "dev".into(),
            text: "\nerror: no\nat main.rs\n".into(),
        };
        assert_eq!(
            pick.reference(Some(Path::new("/t/p.md"))),
            "[picked from terminal \"dev\": \"error: no\", 3 lines, full pick in /t/p.md] "
        );
        let one = Pick::Terminal {
            from: "dev".into(),
            text: "ok".into(),
        };
        assert_eq!(
            one.reference(None),
            "[picked from terminal \"dev\": \"ok\"] "
        );
    }

    #[test]
    fn a_file_range_needs_no_payload_and_quotes_a_spaced_path() {
        let pick = Pick::File {
            path: PathBuf::from(r"C:\my code\a.rs"),
            first: 12,
            last: 40,
        };
        assert!(!pick.has_payload());
        assert_eq!(
            pick.reference(None),
            "[picked in the file viewer: \"C:\\my code\\a.rs\" lines 12 to 40] "
        );
        let one = Pick::File {
            path: PathBuf::from("a.rs"),
            first: 3,
            last: 3,
        };
        assert!(one.reference(None).ends_with("a.rs line 3] "));
    }

    #[test]
    fn the_payload_fences_text_that_holds_backticks() {
        let pick = Pick::Terminal {
            from: "x".into(),
            text: "a ``` b".into(),
        };
        assert!(pick.payload().contains("````\na ``` b\n````"));
        let e = Pick::Element(element()).payload();
        assert!(e.contains("Selector: `#go`"));
        assert!(e.contains("```html\n<button id=\"go\">Sign in</button>\n```"));
    }

    #[test]
    fn clip_cuts_at_a_character_and_says_so() {
        assert_eq!(clip("abc", 5), "abc");
        assert_eq!(clip("aéb", 2), "a\n(cut, 4 bytes in all)");
    }

    #[test]
    fn preview_skips_blank_lines_and_has_none_for_nothing() {
        assert_eq!(preview("\n  hi \n"), Some("hi".into()));
        assert_eq!(preview(" \n "), None);
    }

    #[test]
    fn the_site_drops_scheme_query_and_fragment() {
        assert_eq!(site("https://a.dev/x/?q=1#y"), "a.dev/x");
        assert_eq!(site("about:blank"), "about:blank");
    }

    #[test]
    fn the_element_comes_out_of_the_evaluate_answer() {
        let answer = serde_json::json!({
            "result": { "type": "string", "value": "{\"tag\":\"a\",\"rect\":[1,2,3,4]}" }
        })
        .to_string();
        let e = element_of(&answer).unwrap();
        assert_eq!((e.tag.as_str(), e.rect), ("a", [1.0, 2.0, 3.0, 4.0]));
        let esc = r#"{"result":{"type":"object","subtype":"null","value":null}}"#;
        assert_eq!(element_of(esc), None);
    }

    #[test]
    fn the_crop_keeps_a_margin_inside_the_page() {
        let p: serde_json::Value =
            serde_json::from_str(&crop_params([4.0, 100.0, 5000.0, 10.0])).unwrap();
        let clip = &p["clip"];
        assert_eq!(clip["x"], 0.0);
        assert_eq!(clip["y"], 92.0);
        assert_eq!(clip["width"], 1600.0);
        assert_eq!(clip["height"], 26.0);
    }
}
