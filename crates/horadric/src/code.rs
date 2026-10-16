//! The code index of the lore: each file's symbols and the calls in them,
//! read with tree-sitter, and the three questions asked of them (where is
//! a name defined, who calls it and what it calls, a file's outline).
//! Where the index is kept and how it is kept fresh is in `lore.rs`.
//!
//! Calls are matched by name, not by type: tree-sitter knows the shape of
//! the code, not what a method call's receiver is. So the callers of
//! `take` include every `.take()`. A `Type::name` question narrows what
//! it can: the definitions, and calls written with a path.

use std::collections::HashMap;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use tree_sitter::{Language, Node, Parser, Query, QueryCursor, StreamingIterator};

/// Bumped whenever what is read from a file changes, so an index kept by
/// an older build is read again rather than trusted.
pub const VERSION: u32 = 3;

/// A file larger than this is taken to be generated (a bundle, a minified
/// script) and left out.
pub const LARGEST: u64 = 1 << 20;

/// How many lines an answer holds before it says how many more there are.
const MOST: usize = 40;

/// One file's symbols and calls.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FileIndex {
    #[serde(rename = "s")]
    pub symbols: Vec<Symbol>,
    #[serde(rename = "c")]
    pub calls: Vec<Call>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Symbol {
    #[serde(rename = "n")]
    pub name: String,
    #[serde(rename = "k")]
    pub kind: String,
    /// First and last line, from 1.
    #[serde(rename = "l")]
    pub line: u32,
    #[serde(rename = "e")]
    pub end: u32,
    /// The symbol this one is inside, an index into the file's symbols.
    #[serde(rename = "p", default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<u32>,
    /// Its head: what comes before its body, on one line.
    #[serde(rename = "h")]
    pub head: String,
    /// Its doc comment as one line, cut short: what it is for, which the
    /// head does not say.
    #[serde(rename = "d", default, skip_serializing_if = "String::is_empty")]
    pub doc: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Call {
    /// What is called, `name!` for a macro.
    #[serde(rename = "n")]
    pub name: String,
    #[serde(rename = "l")]
    pub line: u32,
    /// The symbol the call is in; None at the top of a file.
    #[serde(rename = "f", default, skip_serializing_if = "Option::is_none")]
    pub from: Option<u32>,
    /// The path a call was written with, `Lock` in `Lock::take()`.
    #[serde(rename = "q", default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Called on a value (`x.take()`), whose type the index does not know.
    #[serde(rename = "m", default, skip_serializing_if = "std::ops::Not::not")]
    pub method: bool,
}

/// A language the index reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    Rust,
    Python,
    JavaScript,
    TypeScript,
    Tsx,
}

impl Lang {
    pub fn of(path: &str) -> Option<Lang> {
        let name = path.rsplit('/').next().unwrap_or(path).to_lowercase();
        if name.ends_with(".min.js") || name.ends_with(".d.ts") {
            return None;
        }
        Some(match name.rsplit_once('.')?.1 {
            "rs" => Lang::Rust,
            "py" | "pyi" => Lang::Python,
            "js" | "mjs" | "cjs" | "jsx" => Lang::JavaScript,
            "ts" | "mts" | "cts" => Lang::TypeScript,
            "tsx" => Lang::Tsx,
            _ => return None,
        })
    }

    pub fn tag(self) -> &'static str {
        match self {
            Lang::Rust => "rs",
            Lang::Python => "py",
            Lang::JavaScript => "js",
            Lang::TypeScript => "ts",
            Lang::Tsx => "tsx",
        }
    }

    fn grammar(self) -> &'static (Language, Query) {
        static GRAMMARS: [OnceLock<(Language, Query)>; 5] = [const { OnceLock::new() }; 5];
        GRAMMARS[self as usize].get_or_init(|| {
            let (language, query): (Language, String) = match self {
                Lang::Rust => (tree_sitter_rust::LANGUAGE.into(), RUST.into()),
                Lang::Python => (tree_sitter_python::LANGUAGE.into(), PYTHON.into()),
                Lang::JavaScript => (tree_sitter_javascript::LANGUAGE.into(), JAVASCRIPT.into()),
                Lang::TypeScript => (
                    tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
                    format!("{JAVASCRIPT}\n{TYPESCRIPT}"),
                ),
                Lang::Tsx => (
                    tree_sitter_typescript::LANGUAGE_TSX.into(),
                    format!("{JAVASCRIPT}\n{TYPESCRIPT}"),
                ),
            };
            let query = Query::new(&language, &query)
                .unwrap_or_else(|e| panic!("the {self:?} query does not compile: {e}"));
            (language, query)
        })
    }
}

/// A definition is a `@def.<kind>` with its `@name`; a call is a `@call`
/// (or a `@call.value`, one made on a value, or a `@macro`) with its `@callee`
/// and maybe the `@path` it was written with.
const RUST: &str = r#"
(function_item name: (identifier) @name) @def.fn
(function_signature_item name: (identifier) @name) @def.fn
(struct_item name: (type_identifier) @name) @def.struct
(enum_item name: (type_identifier) @name) @def.enum
(union_item name: (type_identifier) @name) @def.union
(type_item name: (type_identifier) @name) @def.type
(trait_item name: (type_identifier) @name) @def.trait
(impl_item type: (_) @name) @def.impl
(mod_item name: (identifier) @name) @def.mod
(macro_definition name: (identifier) @name) @def.macro
(const_item name: (identifier) @name) @def.const
(static_item name: (identifier) @name) @def.static
(call_expression function: (identifier) @callee) @call
(call_expression function: (field_expression field: (field_identifier) @callee)) @call.value
(call_expression function: (field_expression value: (self) @path field: (field_identifier) @callee)) @call
(call_expression function: (scoped_identifier path: (_) @path name: (identifier) @callee)) @call
(call_expression function: (generic_function function: (identifier) @callee)) @call
(call_expression function: (generic_function function: (field_expression field: (field_identifier) @callee))) @call.value
(call_expression function: (generic_function function: (scoped_identifier path: (_) @path name: (identifier) @callee))) @call
(macro_invocation macro: (identifier) @callee) @macro
"#;

const PYTHON: &str = r#"
(class_definition name: (identifier) @name) @def.class
(function_definition name: (identifier) @name) @def.def
(call function: (identifier) @callee) @call
(call function: (attribute attribute: (identifier) @callee)) @call.value
(call function: (attribute object: (identifier) @path attribute: (identifier) @callee) (#eq? @path "self")) @call
"#;

const JAVASCRIPT: &str = r#"
(function_declaration name: (identifier) @name) @def.function
(generator_function_declaration name: (identifier) @name) @def.function
(class_declaration name: (_) @name) @def.class
(method_definition name: (_) @name) @def.method
(variable_declarator name: (identifier) @name value: [(arrow_function) (function_expression)]) @def.function
(pair key: (property_identifier) @name value: [(arrow_function) (function_expression)]) @def.method
(call_expression function: (identifier) @callee) @call
(call_expression function: (member_expression property: (property_identifier) @callee)) @call.value
(call_expression function: (member_expression object: (this) @path property: (property_identifier) @callee)) @call
(new_expression constructor: (identifier) @callee) @call
"#;

/// Added to the JavaScript query for TypeScript, whose grammar builds on
/// JavaScript's.
const TYPESCRIPT: &str = r#"
(abstract_class_declaration name: (_) @name) @def.class
(interface_declaration name: (_) @name) @def.interface
(type_alias_declaration name: (_) @name) @def.type
(enum_declaration name: (_) @name) @def.enum
(method_signature name: (_) @name) @def.method
(abstract_method_signature name: (_) @name) @def.method
(function_signature name: (identifier) @name) @def.function
(internal_module name: (_) @name) @def.namespace
"#;

/// Reads one file's symbols and calls. A file tree-sitter can not make
/// sense of still gives what it could read.
pub fn read(lang: Lang, src: &[u8]) -> FileIndex {
    let (language, query) = lang.grammar();
    let mut parser = Parser::new();
    if parser.set_language(language).is_err() {
        return FileIndex::default();
    }
    let Some(tree) = parser.parse(src, None) else {
        return FileIndex::default();
    };
    let names = query.capture_names();
    let text = |n: Node| String::from_utf8_lossy(&src[n.byte_range()]).into_owned();

    // (start, end, symbol) and (start, call) before parents are known.
    let mut defs: Vec<(usize, usize, Symbol)> = Vec::new();
    let mut calls: Vec<(usize, Call)> = Vec::new();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, tree.root_node(), src);
    while let Some(m) = matches.next() {
        let mut node = None;
        let mut kind = "";
        let mut name = None;
        let mut path = None;
        for c in m.captures {
            match names[c.index as usize] {
                "name" | "callee" => name = Some(c.node),
                "path" => path = Some(c.node),
                "call" => (node, kind) = (Some(c.node), "call"),
                "call.value" => (node, kind) = (Some(c.node), "call.value"),
                "macro" => (node, kind) = (Some(c.node), "macro"),
                k => {
                    if let Some(k) = k.strip_prefix("def.") {
                        (node, kind) = (Some(c.node), k);
                    }
                }
            }
        }
        let (Some(node), Some(name)) = (node, name) else {
            continue;
        };
        let line = node.start_position().row as u32 + 1;
        match kind {
            "call" | "call.value" | "macro" => {
                let mut name = text(name);
                if kind == "macro" {
                    name.push('!');
                }
                calls.push((
                    node.start_byte(),
                    Call {
                        name,
                        line,
                        from: None,
                        path: path.map(text).map(|p| match p.as_str() {
                            "self" | "this" => "Self".to_string(),
                            _ => p,
                        }),
                        method: kind == "call.value",
                    },
                ));
            }
            _ => {
                let mut name = text(name);
                if kind == "impl" {
                    name = bare_type(&name);
                }
                defs.push((
                    node.start_byte(),
                    node.end_byte(),
                    Symbol {
                        name,
                        kind: kind.to_string(),
                        line,
                        end: node.end_position().row as u32 + 1,
                        parent: None,
                        head: head(node, src),
                        doc: doc(lang, node, src),
                    },
                ));
            }
        }
    }

    // Outer before inner, so a stack of the open ones gives each its parent.
    defs.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)));
    defs.dedup_by(|a, b| a.0 == b.0 && a.1 == b.1);
    let mut open: Vec<usize> = Vec::new();
    let mut ranges: Vec<(usize, usize)> = Vec::with_capacity(defs.len());
    let mut symbols = Vec::with_capacity(defs.len());
    for (i, (start, end, mut s)) in defs.into_iter().enumerate() {
        while open.last().is_some_and(|&o| ranges[o].1 <= start) {
            open.pop();
        }
        s.parent = open.last().map(|&o| o as u32);
        open.push(i);
        ranges.push((start, end));
        symbols.push(s);
    }

    // A call on `self` is matched both as one on a value and as one on
    // its own type; the second, which says more, is kept.
    calls.sort_by_key(|(at, c)| (*at, c.path.is_none()));
    calls.dedup_by(|a, b| a.0 == b.0 && a.1.name == b.1.name);
    let mut out: Vec<Call> = Vec::new();
    for (at, mut c) in calls {
        // The innermost symbol around it: the last one starting before it
        // that has not ended yet.
        c.from = ranges
            .iter()
            .rposition(|&(s, e)| s <= at && at < e)
            .map(|i| i as u32);
        let seen = out
            .iter()
            .any(|o| o.from == c.from && o.name == c.name && o.path == c.path);
        if !seen {
            out.push(c);
        }
    }
    FileIndex {
        symbols,
        calls: out,
    }
}

/// `Server` for `Server<A>` or `crate::x::Server`: the name an impl block
/// is for, so its methods are found by it.
fn bare_type(ty: &str) -> String {
    let ty = ty.split('<').next().unwrap_or(ty).trim();
    let ty = ty.trim_start_matches('&').trim_start_matches("mut ").trim();
    ty.rsplit("::").next().unwrap_or(ty).to_string()
}

/// The longest doc comment kept, in characters.
const LONGEST_DOC: usize = 300;

/// A definition's doc comment on one line: the comments right above it
/// (Rust `///`, JavaScript and TypeScript `/** */` or `//`), or a Python
/// docstring.
fn doc(lang: Lang, node: Node, src: &[u8]) -> String {
    let text = |n: Node| String::from_utf8_lossy(&src[n.byte_range()]).into_owned();
    let mut found = if lang == Lang::Python {
        node.child_by_field_name("body")
            .and_then(|b| b.named_child(0))
            .filter(|s| s.kind() == "expression_statement")
            .and_then(|s| s.named_child(0))
            .filter(|s| s.kind() == "string")
            .map(|s| clean_doc(text(s).trim_matches(['"', '\''])))
            .unwrap_or_default()
    } else {
        let mut at = node;
        if let Some(p) = at.parent().filter(|p| p.kind() == "export_statement") {
            at = p;
        }
        let mut next = at.start_position().row;
        let mut parts: Vec<String> = Vec::new();
        let mut prev = at.prev_sibling();
        while let Some(p) = prev {
            if p.end_position().row + 1 < next {
                break;
            }
            match p.kind() {
                "attribute_item" => {}
                "line_comment" | "block_comment" | "comment" => {
                    let t = text(p);
                    let doc = if lang == Lang::Rust {
                        t.starts_with("///") || t.starts_with("/**")
                    } else {
                        t.starts_with("/**") || t.starts_with("//")
                    };
                    if !doc {
                        break;
                    }
                    parts.push(clean_doc(&t));
                }
                _ => break,
            }
            next = p.start_position().row;
            prev = p.prev_sibling();
        }
        parts.reverse();
        parts.join(" ")
    };
    found = found.split_whitespace().collect::<Vec<_>>().join(" ");
    if found.chars().count() > LONGEST_DOC {
        found = found.chars().take(LONGEST_DOC - 1).collect::<String>() + "…";
    }
    found
}

/// A comment's words without its markers.
fn clean_doc(raw: &str) -> String {
    raw.lines()
        .map(|l| {
            let l = l.trim();
            let l = l
                .trim_start_matches("/**")
                .trim_start_matches("///")
                .trim_start_matches("//")
                .trim_start_matches('*')
                .trim_end_matches("*/");
            l.trim().to_string()
        })
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// What comes before a definition's body, on one line: its signature.
fn head(node: Node, src: &[u8]) -> String {
    let body = node.child_by_field_name("body").or_else(|| {
        node.child_by_field_name("value")
            .and_then(|v| v.child_by_field_name("body"))
    });
    let end = match body {
        Some(b) if b.start_byte() > node.start_byte() => b.start_byte(),
        _ => {
            let from = node.start_byte();
            src[from..node.end_byte()]
                .iter()
                .position(|&b| b == b'\n')
                .map_or(node.end_byte(), |i| from + i)
        }
    };
    let raw = String::from_utf8_lossy(&src[node.start_byte()..end]);
    let one: Vec<&str> = raw.split_whitespace().collect();
    let mut head = one.join(" ");
    if head.chars().count() > 200 {
        head = head.chars().take(199).collect::<String>() + "…";
    }
    head
}

/// The files of a project and what each holds, by path relative to it.
pub type Files<'a> = [(String, &'a FileIndex)];

/// A name asked about, maybe with the type or module it is in:
/// `Lock::take`, `Lock.take` or `take`.
struct Asked<'a> {
    within: Option<&'a str>,
    name: &'a str,
}

impl<'a> Asked<'a> {
    fn parse(q: &'a str) -> Asked<'a> {
        let q = q.trim().trim_end_matches("()").trim_end_matches('!');
        let split = q
            .rfind("::")
            .map(|i| (i, 2))
            .or_else(|| q.rfind('.').map(|i| (i, 1)));
        match split {
            Some((i, n)) if i > 0 && i + n < q.len() => Asked {
                within: Some(q[..i].rsplit(['.', ':']).next().unwrap_or(&q[..i])),
                name: &q[i + n..],
            },
            _ => Asked {
                within: None,
                name: q,
            },
        }
    }

    fn fits(&self, f: &FileIndex, s: &Symbol) -> bool {
        s.name == self.name
            && self
                .within
                .is_none_or(|w| s.parent.is_some_and(|p| f.symbols[p as usize].name == w))
    }
}

/// `Type::name` for a symbol in a type or module, else its name.
fn full(f: &FileIndex, s: &Symbol) -> String {
    match s.parent {
        Some(p) => format!("{}::{}", f.symbols[p as usize].name, s.name),
        None => s.name.clone(),
    }
}

fn more(out: &mut String, shown: usize, all: usize) {
    if all > shown {
        out.push_str(&format!("…and {} more.\n", all - shown));
    }
}

/// Where a name is defined. An exact match first; when there is none, one
/// that differs only in case; then names that hold it.
pub fn find(files: &Files, query: &str) -> String {
    let asked = Asked::parse(query);
    if asked.name.is_empty() {
        return "Say what to find: name.".into();
    }
    let all: Vec<(&str, &FileIndex, &Symbol)> = files
        .iter()
        .flat_map(|(p, f)| f.symbols.iter().map(move |s| (p.as_str(), *f, s)))
        .collect();
    type Try<'t> = &'t dyn Fn(&FileIndex, &Symbol) -> bool;
    let low = asked.name.to_lowercase();
    let tries: [Try; 3] = [
        &|f, s| asked.fits(f, s),
        &|_, s| s.name.to_lowercase() == low,
        &|_, s| s.name.to_lowercase().contains(&low),
    ];
    for (i, t) in tries.iter().enumerate() {
        let hits: Vec<_> = all.iter().filter(|(_, f, s)| t(f, s)).collect();
        if hits.is_empty() {
            continue;
        }
        let mut out = String::new();
        if i > 0 {
            out.push_str(&format!("No `{query}` exactly; the nearest:\n"));
        }
        for (path, f, s) in hits.iter().take(MOST) {
            out.push_str(&format!(
                "{path}:{}-{} {} {}\n    {}\n",
                s.line,
                s.end,
                s.kind,
                full(f, s),
                s.head
            ));
        }
        more(&mut out, MOST, hits.len());
        return out;
    }
    format!(
        "Nothing named `{query}` in the {} files read (Rust, TypeScript, JavaScript, Python).",
        files.len()
    )
}

/// Who calls a name, and what it calls; `which` is "callers", "callees"
/// or anything else for both.
pub fn calls(files: &Files, query: &str, which: &str) -> String {
    let asked = Asked::parse(query);
    if asked.name.is_empty() {
        return "Say whose calls: name.".into();
    }
    let mut out = String::new();
    if which != "callees" {
        callers(files, &asked, &mut out);
    }
    if which != "callers" {
        if !out.is_empty() {
            out.push('\n');
        }
        callees(files, &asked, &mut out);
    }
    out
}

fn callers(files: &Files, asked: &Asked, out: &mut String) {
    let mut hits = Vec::new();
    for (path, f) in files {
        for c in &f.calls {
            if c.name.trim_end_matches('!') != asked.name {
                continue;
            }
            // A call written with a path names its type; one on a value
            // (`x.take()`) could be any type's, so it is kept.
            if let (Some(w), Some(p)) = (asked.within, &c.path) {
                let last = p.rsplit("::").next().unwrap_or(p);
                let own = c
                    .from
                    .and_then(|i| f.symbols[i as usize].parent)
                    .map(|i| f.symbols[i as usize].name.as_str());
                if last != w && !(last == "Self" && own == Some(w)) {
                    continue;
                }
            }
            let caller = match c.from {
                Some(i) => {
                    let s = &f.symbols[i as usize];
                    format!("{} {}", s.kind, full(f, s))
                }
                None => "the top of the file".into(),
            };
            hits.push(format!("{path}:{} in {caller}\n", c.line));
        }
    }
    if hits.is_empty() {
        out.push_str(&format!("Nothing calls `{}` by that name.\n", asked.name));
        return;
    }
    out.push_str(&format!("Called by name `{}` from:\n", asked.name));
    for h in hits.iter().take(MOST) {
        out.push_str(h);
    }
    more(out, MOST, hits.len());
}

/// A place a name is defined, as a call is resolved to it.
#[derive(Clone, Copy)]
struct Def<'a> {
    path: &'a str,
    line: u32,
    /// The type or module it is in.
    parent: Option<&'a str>,
    /// Whether it is in a type, so called on a value or with the type's
    /// path rather than by its bare name.
    member: bool,
}

/// `lore` for `src/lore.rs`, `lore` for `src/lore/mod.rs`: the module a
/// file is, as a call's path names it.
fn module(path: &str) -> &str {
    let mut parts = path.rsplit('/');
    let file = parts.next().unwrap_or(path);
    let stem = file.split('.').next().unwrap_or(file);
    match stem {
        "mod" | "lib" | "main" | "index" | "__init__" => parts.next().unwrap_or(stem),
        _ => stem,
    }
}

/// Where in the project a call made in the file `here`, in a symbol of
/// type `own`, may go: those the way it was written allows, and of them
/// the ones in its own file when there are any.
fn resolve<'a>(c: &Call, here: &str, own: Option<&str>, defs: &[Def<'a>]) -> Vec<Def<'a>> {
    let mut at = defs.to_vec();
    match &c.path {
        Some(p) => {
            let last = p.rsplit("::").next().unwrap_or(p);
            let last = if last == "Self" {
                own.unwrap_or(last)
            } else {
                last
            };
            if !matches!(last, "crate" | "super") {
                at.retain(|d| d.parent == Some(last) || module(d.path) == last);
            }
        }
        None if c.method => at.retain(|d| d.member),
        None => at.retain(|d| !d.member),
    }
    if at.iter().any(|d| d.path == here) {
        at.retain(|d| d.path == here);
    }
    at
}

/// Every place each name is defined, by the name.
fn definitions<'a>(files: &'a Files) -> HashMap<&'a str, Vec<Def<'a>>> {
    const TYPES: &[&str] = &["impl", "class", "trait", "interface", "struct", "enum"];
    let mut defined: HashMap<&str, Vec<Def>> = HashMap::new();
    for (path, f) in files {
        for s in &f.symbols {
            if matches!(s.kind.as_str(), "impl" | "mod" | "namespace") {
                continue;
            }
            let parent = s.parent.map(|p| &f.symbols[p as usize]);
            defined.entry(s.name.as_str()).or_default().push(Def {
                path: path.as_str(),
                line: s.line,
                parent: parent.map(|p| p.name.as_str()),
                member: parent.is_some_and(|p| TYPES.contains(&p.kind.as_str())),
            });
        }
    }
    defined
}

/// File to file, how many calls the first makes to symbols the second
/// defines, for the Arcane Sanctuary's lines. Only calls that resolve to
/// one file count: a call on a value or one that could go to several
/// files is a guess, and guesses would tie every file to every other.
pub fn links(files: &Files) -> Vec<(String, String, u32)> {
    let defined = definitions(files);
    let mut count: HashMap<(&str, &str), u32> = HashMap::new();
    for (path, f) in files {
        for c in f.calls.iter().filter(|c| !c.method) {
            let Some(defs) = defined.get(c.name.trim_end_matches('!')) else {
                continue;
            };
            let own = c
                .from
                .and_then(|i| f.symbols[i as usize].parent)
                .map(|p| f.symbols[p as usize].name.as_str());
            let at = resolve(c, path, own, defs);
            let Some(first) = at.first() else {
                continue;
            };
            if first.path != path && at.iter().all(|d| d.path == first.path) {
                *count.entry((path.as_str(), first.path)).or_default() += 1;
            }
        }
    }
    let mut out: Vec<(String, String, u32)> = count
        .into_iter()
        .map(|((a, b), n)| (a.to_string(), b.to_string(), n))
        .collect();
    out.sort();
    out
}

fn callees(files: &Files, asked: &Asked, out: &mut String) {
    let defined = definitions(files);
    let mut any = false;
    for (path, f) in files {
        for (i, s) in f.symbols.iter().enumerate() {
            if !asked.fits(f, s) {
                continue;
            }
            any = true;
            out.push_str(&format!(
                "{} {} ({path}:{}) calls:\n",
                s.kind,
                full(f, s),
                s.line
            ));
            let own = s.parent.map(|p| f.symbols[p as usize].name.as_str());
            // In the order they are made. A call on a value is only maybe
            // the project's method of that name, so those are kept apart.
            let mut inside: Vec<(&Call, Vec<Def>)> = Vec::new();
            let mut maybe: Vec<String> = Vec::new();
            let mut outside: Vec<String> = Vec::new();
            for c in f.calls.iter().filter(|c| c.from == Some(i as u32)) {
                let shown = match &c.path {
                    _ if c.method => format!(".{}", c.name),
                    Some(p) => format!("{p}::{}", c.name),
                    None => c.name.clone(),
                };
                let defs = defined
                    .get(c.name.trim_end_matches('!'))
                    .map_or(&[][..], Vec::as_slice);
                let at = resolve(c, path, own, defs);
                if at.is_empty() {
                    if !outside.contains(&shown) {
                        outside.push(shown);
                    }
                } else if c.method {
                    let n = at.len();
                    let said = format!("{shown} ({n} {})", if n == 1 { "place" } else { "places" });
                    if !maybe.contains(&said) {
                        maybe.push(said);
                    }
                } else if !inside
                    .iter()
                    .any(|(o, _)| o.name == c.name && o.path == c.path)
                {
                    inside.push((c, at));
                }
            }
            for (c, at) in &inside {
                let wheres: Vec<String> = at
                    .iter()
                    .take(3)
                    .map(|d| format!("{}:{}", d.path, d.line))
                    .collect();
                let rest = if at.len() > 3 {
                    format!(" and {} more", at.len() - 3)
                } else {
                    String::new()
                };
                out.push_str(&format!(
                    "  line {}: {}, defined at {}{rest}\n",
                    c.line,
                    c.name,
                    wheres.join(", ")
                ));
            }
            if !maybe.is_empty() {
                out.push_str(&format!(
                    "  on values, maybe the project's: {}\n",
                    maybe.join(", ")
                ));
            }
            if !outside.is_empty() {
                out.push_str(&format!(
                    "  and outside the project: {}\n",
                    outside.join(", ")
                ));
            }
            if inside.is_empty() && maybe.is_empty() && outside.is_empty() {
                out.push_str("  nothing\n");
            }
        }
    }
    if !any {
        out.push_str(&format!("No `{}` is defined in the project.\n", asked.name));
    }
}

/// What one file's symbols say for the memory to find them by meaning: one
/// text each, its place, head and doc comment, never its body. An `impl`
/// says nothing its methods do not.
pub fn passages(path: &str, f: &FileIndex) -> Vec<String> {
    f.symbols
        .iter()
        .filter(|s| s.kind != "impl")
        .map(|s| {
            let mut text = format!("{path}\n{} {}: {}", s.kind, full(f, s), s.head);
            if !s.doc.is_empty() {
                text.push('\n');
                text.push_str(&s.doc);
            }
            text
        })
        .collect()
}

/// A file's symbols with their heads and lines, nested as in the file,
/// without their bodies.
pub fn outline(path: &str, f: &FileIndex) -> String {
    if f.symbols.is_empty() {
        return format!("{path} defines nothing the index reads.\n");
    }
    let mut out = format!("{path}\n");
    let mut depth: Vec<usize> = Vec::with_capacity(f.symbols.len());
    for s in &f.symbols {
        let d = s.parent.map_or(0, |p| depth[p as usize] + 1);
        depth.push(d);
        out.push_str(&format!(
            "{:>5}-{:<5}{}{}\n",
            s.line,
            s.end,
            "  ".repeat(d),
            s.head
        ));
    }
    out
}

/// What the index keeps on disk: each file read, by its language and the
/// hash of its bytes, so a file is read once whichever worktree has it.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Store {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub files: HashMap<String, Kept>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Kept {
    /// When a session last had a file with these bytes, in seconds.
    pub seen: u64,
    pub index: FileIndex,
}

/// How long a file's index is kept after no working tree has it any more.
pub const FORGET: u64 = 30 * 24 * 60 * 60;

/// The key a file is kept by.
pub fn key(lang: Lang, bytes: &[u8]) -> String {
    format!("{}:{:016x}", lang.tag(), horadric_core::lore::hash(bytes))
}

impl Store {
    /// Takes in what one session read and had, and forgets what nobody has
    /// had for a month. A store from another version starts again.
    pub fn merge(&mut self, read: Vec<(String, FileIndex)>, had: &[&str], now: u64) {
        if self.version != VERSION {
            *self = Store {
                version: VERSION,
                files: HashMap::new(),
            };
        }
        for (k, index) in read {
            self.files.insert(k, Kept { seen: now, index });
        }
        for k in had {
            if let Some(kept) = self.files.get_mut(*k) {
                kept.seen = now;
            }
        }
        self.files
            .retain(|_, kept| now.saturating_sub(kept.seen) < FORGET);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(f: &FileIndex) -> Vec<String> {
        f.symbols.iter().map(|s| full(f, s)).collect()
    }

    #[test]
    fn every_language_has_a_query_that_compiles() {
        for l in [
            Lang::Rust,
            Lang::Python,
            Lang::JavaScript,
            Lang::TypeScript,
            Lang::Tsx,
        ] {
            l.grammar();
        }
    }

    #[test]
    fn a_language_is_known_by_its_extension() {
        assert_eq!(Lang::of("crates/a/src/main.rs"), Some(Lang::Rust));
        assert_eq!(Lang::of("x/App.TSX"), Some(Lang::Tsx));
        assert_eq!(Lang::of("x/y.py"), Some(Lang::Python));
        assert_eq!(Lang::of("x/y.mjs"), Some(Lang::JavaScript));
        assert_eq!(Lang::of("dist/app.min.js"), None);
        assert_eq!(Lang::of("types/x.d.ts"), None);
        assert_eq!(Lang::of("README.md"), None);
        assert_eq!(Lang::of("Makefile"), None);
    }

    const RS: &str = r#"
struct Lock(PathBuf);

impl<A> Server<A> {
    pub fn tool(&mut self, name: &str)
        -> Result<(), String> {
        let l = Lock::take(name)?;
        helper(1);
        self.page(2);
        println!("x");
        Self::tool(self, "y")
    }
}

fn helper(x: u32) -> u32 {
    x
}

mod tests {
    fn helper() {}
}
"#;

    #[test]
    fn rust_gives_symbols_nested_in_their_impl_or_module() {
        let f = read(Lang::Rust, RS.as_bytes());
        assert_eq!(
            names(&f),
            [
                "Lock",
                "Server",
                "Server::tool",
                "helper",
                "tests",
                "tests::helper"
            ]
        );
        let tool = &f.symbols[2];
        assert_eq!(tool.kind, "fn");
        assert_eq!((tool.line, tool.end), (5, 12));
        assert_eq!(
            tool.head,
            "pub fn tool(&mut self, name: &str) -> Result<(), String>"
        );
        assert_eq!(f.symbols[1].kind, "impl");
    }

    #[test]
    fn rust_calls_know_the_symbol_they_are_in() {
        let f = read(Lang::Rust, RS.as_bytes());
        let calls: Vec<(&str, u32, Option<u32>, Option<&str>)> = f
            .calls
            .iter()
            .map(|c| (c.name.as_str(), c.line, c.from, c.path.as_deref()))
            .collect();
        assert_eq!(
            calls,
            [
                ("take", 7, Some(2), Some("Lock")),
                ("helper", 8, Some(2), None),
                ("page", 9, Some(2), Some("Self")),
                ("println!", 10, Some(2), None),
                ("tool", 11, Some(2), Some("Self")),
            ]
        );
    }

    #[test]
    fn a_call_made_twice_from_one_symbol_is_kept_once() {
        let f = read(Lang::Rust, b"fn a() { b(); b(); }\nfn b() {}\n");
        assert_eq!(f.calls.len(), 1);
    }

    #[test]
    fn python_gives_classes_methods_and_calls() {
        let src = "class Shop:\n    def buy(self, n):\n        return self.pay(total(n))\n\ndef total(n):\n    return n\n\nmain()\n";
        let f = read(Lang::Python, src.as_bytes());
        assert_eq!(names(&f), ["Shop", "Shop::buy", "total"]);
        assert_eq!(f.symbols[1].head, "def buy(self, n):");
        let calls: Vec<(&str, Option<u32>)> =
            f.calls.iter().map(|c| (c.name.as_str(), c.from)).collect();
        assert_eq!(
            calls,
            [("pay", Some(1)), ("total", Some(1)), ("main", None)]
        );
    }

    #[test]
    fn javascript_gives_functions_classes_and_arrow_functions() {
        let src = "export function go(a) { return run(a); }\nclass Car { drive() { this.go(); } }\nconst stop = (x) => new Car(x);\n";
        let f = read(Lang::JavaScript, src.as_bytes());
        assert_eq!(names(&f), ["go", "Car", "Car::drive", "stop"]);
        assert_eq!(f.symbols[3].head, "stop = (x) =>");
        let calls: Vec<&str> = f.calls.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(calls, ["run", "go", "Car"]);
    }

    #[test]
    fn typescript_adds_interfaces_types_and_enums() {
        let src = "interface Shape { area(): number }\ntype Id = string;\nenum Colour { Red }\nclass Sq implements Shape { area(): number { return calc(1); } }\n";
        for lang in [Lang::TypeScript, Lang::Tsx] {
            let f = read(lang, src.as_bytes());
            assert_eq!(
                names(&f),
                ["Shape", "Shape::area", "Id", "Colour", "Sq", "Sq::area"],
                "{lang:?}"
            );
            assert_eq!(f.symbols[0].kind, "interface");
        }
    }

    #[test]
    fn a_file_that_does_not_parse_still_gives_what_it_can() {
        let f = read(Lang::Rust, b"fn good() {}\nfn broken( {{{\n");
        assert_eq!(f.symbols[0].name, "good");
    }

    fn project() -> Vec<(String, FileIndex)> {
        vec![
            ("src/lore.rs".into(), read(Lang::Rust, RS.as_bytes())),
            (
                "src/lock.rs".into(),
                read(
                    Lang::Rust,
                    b"impl Lock {\n    pub fn take(p: &str) -> Lock { make() }\n}\nfn other() { x.take(); Cell::take(); }\n",
                ),
            ),
        ]
    }

    fn files(p: &[(String, FileIndex)]) -> Vec<(String, &FileIndex)> {
        p.iter().map(|(n, f)| (n.clone(), f)).collect()
    }

    #[test]
    fn links_count_calls_that_go_to_one_other_file() {
        let p = vec![
            (
                "src/a.rs".to_string(),
                read(
                    Lang::Rust,
                    b"fn main() { helper(); helper(); b::other(); x.take(); twice(); }
",
                ),
            ),
            (
                "src/b.rs".to_string(),
                read(
                    Lang::Rust,
                    b"pub fn helper() {}
pub fn other() { helper(); }
pub fn twice() {}
impl T { fn take(&self) {} }
",
                ),
            ),
            (
                "src/c.rs".to_string(),
                read(
                    Lang::Rust,
                    b"pub fn twice() {}
",
                ),
            ),
        ];
        let got = links(&files(&p));
        // helper (kept once for main) and b::other; .take() is on a value,
        // twice() could be either file and b's own helper() stays in b.
        assert_eq!(got, [("src/a.rs".into(), "src/b.rs".into(), 2)]);
    }

    #[test]
    fn find_answers_where_a_name_is_defined() {
        let p = project();
        let said = find(&files(&p), "helper");
        assert!(
            said.contains("src/lore.rs:15-17 fn helper\n    fn helper(x: u32) -> u32"),
            "{said}"
        );
        assert!(
            said.contains("src/lore.rs:20-20 fn tests::helper"),
            "{said}"
        );
        let said = find(&files(&p), "tests::helper");
        assert!(!said.contains("15-17"), "{said}");
    }

    #[test]
    fn find_falls_back_to_case_and_then_to_part_of_a_name() {
        let p = project();
        let said = find(&files(&p), "HELPER");
        assert!(said.starts_with("No `HELPER` exactly"), "{said}");
        assert!(said.contains("fn helper"), "{said}");
        assert!(find(&files(&p), "elp").contains("fn helper"));
        assert!(find(&files(&p), "zzz").starts_with("Nothing named `zzz`"));
    }

    #[test]
    fn callers_of_a_qualified_name_leave_out_calls_through_other_types() {
        let p = project();
        let said = calls(&files(&p), "Lock::take", "callers");
        assert!(said.contains("src/lore.rs:7 in fn Server::tool"), "{said}");
        // On a value, so it could be a Lock's.
        assert!(said.contains("src/lock.rs:4 in fn other"), "{said}");
        assert_eq!(said.matches("src/lock.rs:4").count(), 1, "{said}");
        let said = calls(&files(&p), "take", "callers");
        assert_eq!(said.matches("src/lock.rs:4").count(), 2, "{said}");
    }

    #[test]
    fn a_call_through_self_counts_for_its_own_type() {
        let p = project();
        let said = calls(&files(&p), "Server::tool", "callers");
        assert!(said.contains("src/lore.rs:11 in fn Server::tool"), "{said}");
    }

    #[test]
    fn callees_say_where_each_is_defined_and_what_is_outside() {
        let p = project();
        let said = calls(&files(&p), "Server::tool", "callees");
        assert!(
            said.starts_with("fn Server::tool (src/lore.rs:5) calls:"),
            "{said}"
        );
        assert!(
            said.contains("line 8: helper, defined at src/lore.rs:15, src/lore.rs:20"),
            "{said}"
        );
        assert!(
            said.contains("line 7: take, defined at src/lock.rs:2"),
            "{said}"
        );
        assert!(
            said.contains("outside the project: Self::page, println!"),
            "{said}"
        );
    }

    #[test]
    fn a_call_goes_where_its_path_its_file_and_its_shape_allow() {
        let p = vec![
            (
                "src/main.rs".to_string(),
                read(
                    Lang::Rust,
                    b"fn go() { code::read(); HashMap::new(); new(); x.read(); }\nfn new() {}\n",
                ),
            ),
            (
                "src/code/mod.rs".to_string(),
                read(Lang::Rust, b"pub fn read() {}\n"),
            ),
            (
                "src/store.rs".to_string(),
                read(
                    Lang::Rust,
                    b"impl Store { fn new() {} fn read(&self) {} }\nfn read() {}\n",
                ),
            ),
        ];
        let said = calls(&files(&p), "go", "callees");
        assert!(
            said.contains("read, defined at src/code/mod.rs:1\n"),
            "{said}"
        );
        assert!(said.contains("new, defined at src/main.rs:2\n"), "{said}");
        assert!(
            said.contains("maybe the project's: .read (1 place)"),
            "{said}"
        );
        assert!(said.contains("outside the project: HashMap::new"), "{said}");
    }

    #[test]
    fn calls_of_an_unknown_name_say_so() {
        let p = project();
        let said = calls(&files(&p), "nope", "both");
        assert!(said.contains("Nothing calls `nope`"), "{said}");
        assert!(said.contains("No `nope` is defined"), "{said}");
    }

    #[test]
    fn an_outline_nests_heads_under_their_parents() {
        let f = read(Lang::Rust, RS.as_bytes());
        let said = outline("src/lore.rs", &f);
        assert!(said.contains("    4-13   impl<A> Server<A>\n"), "{said}");
        assert!(said.contains("    5-12     pub fn tool("), "{said}");
        assert!(said.contains("   19-21   mod tests\n"), "{said}");
        assert_eq!(
            outline("a.rs", &FileIndex::default()),
            "a.rs defines nothing the index reads.\n"
        );
    }

    #[test]
    fn a_store_keeps_what_was_read_and_forgets_what_nobody_has() {
        let mut s = Store::default();
        s.merge(vec![("rs:1".into(), FileIndex::default())], &[], 100);
        assert_eq!(s.version, VERSION);
        s.merge(
            vec![("rs:2".into(), FileIndex::default())],
            &["rs:1"],
            FORGET + 50,
        );
        assert_eq!(s.files.len(), 2);
        s.merge(vec![], &["rs:2"], 2 * FORGET + 60);
        assert_eq!(s.files.keys().collect::<Vec<_>>(), ["rs:2"]);
    }

    #[test]
    fn a_store_from_another_version_starts_again() {
        let mut s = Store {
            version: VERSION + 1,
            ..Store::default()
        };
        s.files.insert(
            "rs:1".into(),
            Kept {
                seen: 100,
                index: FileIndex::default(),
            },
        );
        s.merge(vec![], &["rs:1"], 100);
        assert!(s.files.is_empty());
    }

    #[test]
    fn a_key_holds_the_language_and_the_bytes() {
        assert_ne!(key(Lang::Rust, b"a"), key(Lang::Rust, b"b"));
        assert_ne!(key(Lang::JavaScript, b"a"), key(Lang::TypeScript, b"a"));
    }

    #[test]
    fn an_index_survives_being_written_and_read() {
        let f = read(Lang::Rust, RS.as_bytes());
        let back: FileIndex = serde_json::from_str(&serde_json::to_string(&f).unwrap()).unwrap();
        assert_eq!(back, f);
    }

    #[test]
    fn a_doc_comment_is_read_past_attributes_and_exports() {
        let f = read(
            Lang::Rust,
            b"/// Rolls back
/// a failed reload.
#[derive(Debug)]
pub struct Roll;

// plain
fn plain() {}
",
        );
        assert_eq!(f.symbols[0].doc, "Rolls back a failed reload.");
        assert_eq!(f.symbols[1].doc, "");
        let f = read(
            Lang::TypeScript,
            b"/** Opens the door. */
export function open() {}
",
        );
        assert_eq!(f.symbols[0].doc, "Opens the door.");
        let f = read(
            Lang::Python,
            b"def run():
    \"\"\"Starts the run.\"\"\"
    pass
",
        );
        assert_eq!(f.symbols[0].doc, "Starts the run.");
    }

    #[test]
    fn a_passage_says_where_what_and_what_for_but_not_the_body() {
        let f = read(
            Lang::Rust,
            b"impl T {
    /// Rolls back.
    fn undo(&self) { secret(); }
}
",
        );
        let got = passages("src/a.rs", &f);
        assert_eq!(
            got,
            ["src/a.rs
fn T::undo: fn undo(&self)
Rolls back."]
        );
    }
}
