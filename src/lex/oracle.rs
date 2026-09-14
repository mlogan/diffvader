//! Tree-sitter as the source of truth for the lexers.
//!
//! tree-sitter-highlight runs each grammar's stock `highlights.scm` over the same bytes;
//! its captures are mapped onto `Class` (innermost capture wins) so both sides produce
//! the same per-byte format, and the two arrays are compared. Deliberate differences
//! between "what a parser knows" and "what a lexer can know" are encoded here, once, as
//! the capture mapping plus a few normalizations, not scattered through the lexers:
//!
//! - Operator characters are always `Punct` (the queries capture only some of them, and
//!   `<`/`>` only in generics).
//! - Loop labels are colored like lifetimes (tree-sitter-rust captures only lifetimes).
//! - Shebang lines are comments.
//! - TypeScript: `this` and `super` are keywords, but the other `variable.builtin` names
//!   (`console`, `window`, `module`, ...) are ordinary identifiers.
//! - Bytes inside tree-sitter `ERROR` nodes have no truth and are skipped, like
//!   unparseable macro bodies.
//! - Macro arguments are unparsed token trees to tree-sitter; the oracle re-highlights
//!   each invocation's token tree as an expression list so the lexer is checked there too.
//! - Newline and carriage return bytes are never compared (the renderer draws neither
//!   with a class color).
//!
//! `cargo test lex_oracle -- --nocapture` reports mismatches on this repo's sources.
//! `DIFFVADER_LEX_CORPUS=dir` runs over every supported file under `dir` instead and
//! prints statistics; `DIFFVADER_LEX_SHOW=n` caps the mismatch lines printed per file.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use tree_sitter::{Language, Node, Parser};
use tree_sitter_highlight::{HighlightConfiguration, HighlightEvent, Highlighter};

use super::{Class, Lang};

const PLAIN: u8 = Class::Plain as u8;
const ATTRIBUTE: u8 = Class::Attribute as u8;
const LIFETIME: u8 = Class::Lifetime as u8;
const PUNCT: u8 = Class::Punct as u8;
/// Oracle-only marker for `variable.builtin` in languages where only some of those names
/// are keywords; resolved by text in `classify`.
const BUILTIN: u8 = 254;
/// Oracle-only marker for bytes with no truth available (macro bodies that parse in no
/// form, such as `json!` or `quote!` with interpolations); `compare` skips them.
pub const UNKNOWN: u8 = 255;

/// Maps a tree-sitter capture name onto our class set.
fn class_for_capture(name: &str) -> Class {
    let head = name.split('.').next().unwrap_or(name);
    match head {
        "comment" => Class::Comment,
        "string" => Class::String,
        "escape" => Class::Escape,
        "keyword" => Class::Keyword,
        "number" => Class::Number,
        "type" | "constructor" => Class::Type,
        "function" => Class::Function,
        "attribute" => Class::Attribute,
        "label" => Class::Lifetime,
        "property" => Class::Property,
        "punctuation" | "operator" => Class::Punct,
        "variable" => match name {
            // `self`
            "variable.builtin" => Class::Keyword,
            _ => Class::Plain,
        },
        "constant" => match name {
            // Number and boolean literals.
            "constant.builtin" => Class::Number,
            _ => Class::Constant,
        },
        _ => Class::Plain,
    }
}

/// Characters that tree-sitter's queries leave uncaptured or capture inconsistently
/// but that the lexers always mark as `Punct`. `$` is an identifier character in
/// TypeScript.
fn is_operator_char(lang: Lang, b: u8) -> bool {
    if b == b'$' && lang == Lang::TypeScript {
        return false;
    }
    matches!(
        b,
        b'+' | b'-'
            | b'*'
            | b'/'
            | b'%'
            | b'^'
            | b'!'
            | b'&'
            | b'|'
            | b'='
            | b'<'
            | b'>'
            | b'?'
            | b'@'
            | b'~'
            | b'$'
            | b'.'
            | b':'
            | b';'
            | b','
    )
}

pub struct Oracle {
    lang: Lang,
    language: Language,
    config: HighlightConfiguration,
    /// Class per capture index of `config.query`.
    classes: Vec<u8>,
}

impl Oracle {
    pub fn new(lang: Lang) -> Oracle {
        let (language, name, highlights): (Language, &str, String) = match lang {
            Lang::Rust => (
                tree_sitter_rust::LANGUAGE.into(),
                "rust",
                tree_sitter_rust::HIGHLIGHTS_QUERY.to_string(),
            ),
            // Composed as upstream's tree-sitter.json does: TypeScript's query, then
            // JavaScript's. For the same node the later pattern wins.
            Lang::TypeScript => (
                tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
                "typescript",
                format!(
                    "{}\n{}",
                    tree_sitter_typescript::HIGHLIGHTS_QUERY,
                    tree_sitter_javascript::HIGHLIGHT_QUERY
                ),
            ),
        };
        let mut config = HighlightConfiguration::new(language.clone(), name, &highlights, "", "")
            .expect("highlight query");
        let names: Vec<String> = config
            .query
            .capture_names()
            .iter()
            .map(|s| s.to_string())
            .collect();
        config.configure(&names);
        let classes = names
            .iter()
            .map(|n| match (lang, n.as_str()) {
                (Lang::TypeScript, "variable.builtin") => BUILTIN,
                _ => class_for_capture(n) as u8,
            })
            .collect();
        Oracle {
            lang,
            language,
            config,
            classes,
        }
    }

    /// Per-byte classes according to tree-sitter, in the lexers' format.
    pub fn classify(&self, src: &[u8]) -> Vec<u8> {
        let mut out = vec![PLAIN; src.len()];
        self.highlight_into(src, &mut out);
        self.mark_errors(src, &mut out);
        let mut i = 0;
        while i < out.len() {
            if out[i] != BUILTIN {
                i += 1;
                continue;
            }
            let start = i;
            while i < out.len() && out[i] == BUILTIN {
                i += 1;
            }
            let keyword = matches!(&src[start..i], b"this" | b"super");
            out[start..i].fill(if keyword { Class::Keyword as u8 } else { PLAIN });
        }
        // A shebang line is uncaptured; it is colored as a comment.
        if src.starts_with(b"#!") && src.get(2) != Some(&b'[') {
            let end = memchr::memchr(b'\n', src).unwrap_or(src.len());
            out[..end].fill(Class::Comment as u8);
        }
        for i in 0..src.len() {
            if (out[i] == PLAIN || out[i] == ATTRIBUTE) && is_operator_char(self.lang, src[i]) {
                out[i] = PUNCT;
            }
            // A lifetime's `'` is captured as an operator, and labels not at all; both
            // are colored as one lifetime token.
            if self.lang == Lang::Rust && src[i] == b'\'' && out[i] == PUNCT && i + 1 < src.len() {
                let mut j = i + 1;
                while j < src.len()
                    && (src[j] == b'_' || src[j].is_ascii_alphanumeric())
                    && (out[j] == PLAIN || out[j] == LIFETIME)
                {
                    j += 1;
                }
                if j > i + 1 && (j >= src.len() || src[j] != b'\'') {
                    out[i..j].fill(LIFETIME);
                }
            }
        }
        out
    }

    /// Marks bytes inside `ERROR` nodes as `UNKNOWN`.
    fn mark_errors(&self, src: &[u8], out: &mut [u8]) {
        let mut parser = Parser::new();
        parser.set_language(&self.language).unwrap();
        let Some(tree) = parser.parse(src, None) else {
            return;
        };
        if !tree.root_node().has_error() {
            return;
        }
        let mut stack = vec![tree.root_node()];
        while let Some(node) = stack.pop() {
            if node.is_error() {
                out[node.start_byte()..node.end_byte()].fill(UNKNOWN);
                continue;
            }
            if !node.has_error() {
                continue;
            }
            let mut cursor = node.walk();
            stack.extend(node.children(&mut cursor));
        }
    }

    fn highlight_into(&self, src: &[u8], out: &mut [u8]) {
        let mut hl = Highlighter::new();
        let mut stack: Vec<u8> = Vec::new();
        let events = hl
            .highlight(&self.config, src, None, None, |_| None)
            .expect("highlight");
        for ev in events {
            match ev.expect("highlight event") {
                HighlightEvent::Source { start, end } => {
                    if let Some(&c) = stack.last() {
                        out[start..end].fill(c);
                    }
                }
                HighlightEvent::HighlightStart(h) => stack.push(self.classes[h.0]),
                HighlightEvent::HighlightEnd => {
                    stack.pop();
                }
            }
        }
        if self.lang == Lang::Rust {
            self.rust_macro_args(src, out);
        }
    }

    /// Re-highlights the contents of every macro invocation's token tree. The body is
    /// tried as a parenthesized expression list (`println!(..)`, `vec![..]`), as items
    /// (`thread_local! { static .. }`), as `match scrutinee { pattern => () }`
    /// (`matches!`) and, in pattern position, as a `let` pattern; the first that parses
    /// without errors wins, and bodies that parse in no form are marked `UNKNOWN`.
    /// Nested invocations are handled by the recursion in `highlight_into`. Macros named
    /// by a path (`wgpu::vertex_attr_array!`) are not captured by the query; the name and
    /// `!` are marked here.
    fn rust_macro_args(&self, src: &[u8], out: &mut [u8]) {
        let mut parser = Parser::new();
        parser.set_language(&self.language).unwrap();
        let Some(tree) = parser.parse(src, None) else {
            return;
        };
        let mut invocations = Vec::new();
        collect_macro_invocations(tree.root_node(), &mut invocations);
        for inv in invocations {
            if let Some((s, e)) = inv.scoped_name {
                out[s..e].fill(Class::Function as u8);
            }
            let (start, end) = inv.token_tree;
            let inner = &src[start + 1..end - 1];
            if inner.is_empty() {
                continue;
            }
            // Each candidate: (prefix, pieces of `inner` in order, suffix). A piece is a
            // byte range of `inner` copied verbatim, so classes map back by offset.
            let expr = Snippet::new(b"fn _(){(", vec![(0, inner.len())], b")}");
            let items = Snippet::new(b"", vec![(0, inner.len())], b"");
            let mut candidates = vec![expr, items];
            // `matches!(scrutinee, pattern)`: split at each top-level comma in turn. A
            // trailing comma is not part of the pattern.
            let mut pat_end = inner.len();
            while pat_end > 0 && matches!(inner[pat_end - 1], b' ' | b'\n' | b'\t' | b'\r') {
                pat_end -= 1;
            }
            if pat_end > 0 && inner[pat_end - 1] == b',' {
                pat_end -= 1;
            }
            let as_match: Vec<Snippet> = top_level_commas(&inner[..pat_end])
                .into_iter()
                .map(|c| {
                    Snippet::new(
                        b"fn _(){match (",
                        vec![(0, c), (c + 1, pat_end)],
                        b" => ()}}",
                    )
                    .with_glue(b") {")
                })
                .collect();
            if src[inv.name.0..inv.name.1].ends_with(b"matches") {
                candidates.splice(0..0, as_match);
            } else {
                candidates.extend(as_match);
            }
            if inv.in_pattern {
                let as_pattern = Snippet::new(b"fn _(){let (", vec![(0, inner.len())], b") = 0;}");
                candidates.insert(0, as_pattern);
            }
            let mut chosen = None;
            for (k, cand) in candidates.iter().enumerate() {
                let bytes = cand.build(inner);
                let Some(t) = parser.parse(&bytes, None) else {
                    continue;
                };
                if std::env::var_os("DIFFVADER_LEX_DEBUG").is_some() {
                    eprintln!(
                        "candidate {k} error={}: {}\n{}",
                        t.root_node().has_error(),
                        String::from_utf8_lossy(&bytes),
                        t.root_node().to_sexp()
                    );
                }
                if !t.root_node().has_error() {
                    chosen = Some(cand.clone());
                    break;
                }
            }
            let Some(cand) = chosen else {
                out[start + 1..end - 1].fill(UNKNOWN);
                continue;
            };
            let bytes = cand.build(inner);
            let mut sub = vec![PLAIN; bytes.len()];
            self.highlight_into(&bytes, &mut sub);
            cand.copy_back(&sub, &mut out[start + 1..end - 1]);
        }
    }
}

/// A wrapper around pieces of a macro body: `prefix piece0 glue piece1 ... suffix`.
#[derive(Clone)]
struct Snippet {
    prefix: Vec<u8>,
    pieces: Vec<(usize, usize)>,
    glue: Vec<u8>,
    suffix: Vec<u8>,
}

impl Snippet {
    fn new(prefix: &[u8], pieces: Vec<(usize, usize)>, suffix: &[u8]) -> Snippet {
        Snippet {
            prefix: prefix.to_vec(),
            pieces,
            glue: Vec::new(),
            suffix: suffix.to_vec(),
        }
    }

    fn with_glue(mut self, glue: &[u8]) -> Snippet {
        self.glue = glue.to_vec();
        self
    }

    fn build(&self, inner: &[u8]) -> Vec<u8> {
        let mut v = self.prefix.clone();
        for (k, &(s, e)) in self.pieces.iter().enumerate() {
            if k > 0 {
                v.extend_from_slice(&self.glue);
            }
            v.extend_from_slice(&inner[s..e]);
        }
        v.extend_from_slice(&self.suffix);
        v
    }

    fn copy_back(&self, classes: &[u8], out: &mut [u8]) {
        let mut pos = self.prefix.len();
        for (k, &(s, e)) in self.pieces.iter().enumerate() {
            if k > 0 {
                pos += self.glue.len();
            }
            out[s..e].copy_from_slice(&classes[pos..pos + (e - s)]);
            pos += e - s;
        }
    }
}

/// Offsets of the `,`s outside brackets and strings (generics are not brackets here, so
/// callers try each).
fn top_level_commas(src: &[u8]) -> Vec<usize> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut i = 0;
    while i < src.len() {
        match src[i] {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            b',' if depth == 0 => out.push(i),
            b'"' => {
                i += 1;
                while i < src.len() && src[i] != b'"' {
                    if src[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    out
}

struct Invocation {
    /// Byte range of the macro name (a path for `a::b!`).
    name: (usize, usize),
    /// The invocation is a pattern (`sp!(loc, Pat(x)) => ..`).
    in_pattern: bool,
    token_tree: (usize, usize),
    /// Range of the name segment plus `!` when the macro is named by a path.
    scoped_name: Option<(usize, usize)>,
}

fn collect_macro_invocations(node: Node, out: &mut Vec<Invocation>) {
    let mut cursor = node.walk();
    if node.kind() == "macro_invocation" {
        let mut token_tree = None;
        let mut scoped_name = None;
        let name = node
            .child_by_field_name("macro")
            .map(|m| (m.start_byte(), m.end_byte()))
            .unwrap_or((0, 0));
        for child in node.children(&mut cursor) {
            match child.kind() {
                "token_tree" => token_tree = Some((child.start_byte(), child.end_byte())),
                "scoped_identifier" => {
                    if let Some(name) = child.child_by_field_name("name") {
                        scoped_name = Some((name.start_byte(), name.end_byte() + 1));
                    }
                }
                _ => {}
            }
        }
        if let Some(token_tree) = token_tree {
            let in_pattern = node.parent().is_some_and(|p| match p.kind() {
                "let_declaration" | "let_condition" => {
                    p.child_by_field_name("pattern").is_some_and(|c| c == node)
                }
                "match_pattern"
                | "tuple_pattern"
                | "tuple_struct_pattern"
                | "or_pattern"
                | "ref_pattern"
                | "slice_pattern"
                | "field_pattern" => true,
                _ => false,
            });
            out.push(Invocation {
                name,
                in_pattern,
                token_tree,
                scoped_name,
            });
        }
        // Nested invocations inside are found again when the snippet is parsed.
        return;
    }
    for child in node.children(&mut cursor) {
        collect_macro_invocations(child, out);
    }
}

/// One run of bytes where the two classifications disagree.
#[derive(Debug, PartialEq, Eq)]
pub struct Mismatch {
    pub start: usize,
    pub end: usize,
    pub ours: Class,
    pub oracle: Class,
}

pub fn compare(src: &[u8], ours: &[u8], oracle: &[u8]) -> Vec<Mismatch> {
    assert_eq!(ours.len(), oracle.len());
    let mut out = Vec::new();
    let mut i = 0;
    while i < ours.len() {
        if ours[i] == oracle[i] || matches!(src[i], b'\n' | b'\r') || oracle[i] == UNKNOWN {
            i += 1;
            continue;
        }
        let start = i;
        while i < ours.len() && ours[i] == ours[start] && oracle[i] == oracle[start] {
            i += 1;
        }
        out.push(Mismatch {
            start,
            end: i,
            ours: Class::from_u8(ours[start]),
            oracle: Class::from_u8(oracle[start]),
        });
    }
    out
}

fn describe(src: &[u8], m: &Mismatch) -> String {
    let line = memchr::memchr_iter(b'\n', &src[..m.start]).count() + 1;
    let line_start = memchr::memrchr(b'\n', &src[..m.start]).map_or(0, |p| p + 1);
    let line_end = memchr::memchr(b'\n', &src[m.start..]).map_or(src.len(), |p| m.start + p);
    let text = String::from_utf8_lossy(&src[m.start..m.end.min(line_end)]);
    let context = String::from_utf8_lossy(&src[line_start..line_end]);
    format!(
        "{line}:{}: ours={} oracle={} «{text}»\n    {}",
        m.start - line_start + 1,
        m.ours.name(),
        m.oracle.name(),
        context.trim_end()
    )
}

#[derive(Default)]
struct Report {
    files: usize,
    bytes: usize,
    mismatched: usize,
    unverified: usize,
    runs: usize,
    by_pair: BTreeMap<(Class, Class), usize>,
    lex_time: f64,
}

fn check_file(path: &Path, oracle: &Oracle, show: usize, report: &mut Report) {
    let src = std::fs::read(path).unwrap();
    let t = Instant::now();
    let ours = super::lex(oracle.lang, &src);
    report.lex_time += t.elapsed().as_secs_f64();
    let truth = oracle.classify(&src);
    let mismatches = compare(&src, &ours, &truth);
    report.files += 1;
    report.bytes += src.len();
    report.unverified += truth.iter().filter(|&&c| c == UNKNOWN).count();
    report.runs += mismatches.len();
    for m in &mismatches {
        report.mismatched += m.end - m.start;
        *report.by_pair.entry((m.ours, m.oracle)).or_default() += 1;
    }
    if !mismatches.is_empty() {
        println!("{}: {} mismatched runs", path.display(), mismatches.len());
        for m in mismatches.iter().take(show) {
            println!("  {}", describe(&src, m));
        }
    }
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.file_name().is_some_and(|n| n == ".git" || n == "target") {
            continue;
        }
        if p.is_dir() {
            walk(&p, out);
        } else if Lang::from_path(&p.to_string_lossy()).is_some() {
            out.push(p);
        }
    }
}

fn run_corpus(root: &Path) -> Report {
    let show: usize = std::env::var("DIFFVADER_LEX_SHOW")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(20);
    let mut files = Vec::new();
    walk(root, &mut files);
    files.sort();
    let mut report = Report::default();
    let mut oracles: BTreeMap<u8, Oracle> = BTreeMap::new();
    for f in &files {
        let lang = Lang::from_path(&f.to_string_lossy()).unwrap();
        let oracle = oracles
            .entry(lang as u8)
            .or_insert_with(|| Oracle::new(lang));
        check_file(f, oracle, show, &mut report);
    }
    let mut pairs: Vec<_> = report.by_pair.iter().collect();
    pairs.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
    println!(
        "\n{} files, {} bytes ({} unverifiable), {} mismatched bytes in {} runs ({:.4}%); lexing {:.1} MB/s",
        report.files,
        report.bytes,
        report.unverified,
        report.mismatched,
        report.runs,
        100.0 * report.mismatched as f64 / report.bytes.max(1) as f64,
        report.bytes as f64 / 1e6 / report.lex_time.max(1e-9)
    );
    for ((ours, oracle), n) in pairs {
        println!("  {n:6}  ours={} oracle={}", ours.name(), oracle.name());
    }
    report
}

#[test]
fn lex_oracle() {
    let root = std::env::var("DIFFVADER_LEX_CORPUS")
        .map(PathBuf::from)
        .unwrap_or_else(|_| Path::new(env!("CARGO_MANIFEST_DIR")).join("src"));
    let external = std::env::var("DIFFVADER_LEX_CORPUS").is_ok();
    let report = run_corpus(&root);
    assert!(report.files > 0, "no files under {}", root.display());
    if !external {
        assert_eq!(report.runs, 0, "lexer disagrees with tree-sitter");
    }
}
