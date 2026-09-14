//! Syntax highlighting lexers.
//!
//! Each lexer is a single linear pass over a file's bytes that writes one `Class` byte per
//! input byte into a preallocated buffer. There is no token stream: the renderer indexes
//! `classes[byte_offset]` while drawing a line. `Class::Plain` is 0, so bytes a lexer does
//! not classify need no write at all.

use std::path::Path;

mod rust;
mod typescript;

#[cfg(test)]
mod oracle;

/// Highlight class of one byte. Stored as `u8`; `Plain` must stay 0.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum Class {
    Plain = 0,
    Comment,
    String,
    /// Escape sequence inside a string or char literal.
    Escape,
    Number,
    Keyword,
    /// Type names, including primitive types and constructors (`Some`, `Foo(..)`).
    Type,
    /// Function and method names at definitions and calls, and macro names.
    Function,
    /// Attribute text (`#[derive(Debug)]`); names and literals inside keep their classes.
    Attribute,
    /// Lifetimes and labels (`'a`, `'outer`).
    Lifetime,
    /// Field names (`x.field`).
    Property,
    /// `ALL_CAPS` identifiers. tree-sitter-rust's rule for these never matches (its regex
    /// has a stray quote), so the Rust lexer does not emit it.
    Constant,
    /// Brackets, delimiters and operators.
    Punct,
}

pub const CLASS_COUNT: usize = Class::Punct as usize + 1;

impl Class {
    #[cfg(test)]
    const ALL: [Class; CLASS_COUNT] = [
        Class::Plain,
        Class::Comment,
        Class::String,
        Class::Escape,
        Class::Number,
        Class::Keyword,
        Class::Type,
        Class::Function,
        Class::Attribute,
        Class::Lifetime,
        Class::Property,
        Class::Constant,
        Class::Punct,
    ];

    #[cfg(test)]
    pub fn from_u8(b: u8) -> Class {
        Self::ALL.get(b as usize).copied().unwrap_or(Class::Plain)
    }

    #[cfg(test)]
    pub fn name(self) -> &'static str {
        match self {
            Class::Plain => "plain",
            Class::Comment => "comment",
            Class::String => "string",
            Class::Escape => "escape",
            Class::Number => "number",
            Class::Keyword => "keyword",
            Class::Type => "type",
            Class::Function => "function",
            Class::Attribute => "attribute",
            Class::Lifetime => "lifetime",
            Class::Property => "property",
            Class::Constant => "constant",
            Class::Punct => "punct",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    Rust,
    TypeScript,
    // TODO: one lexer per language below, each with its tree-sitter grammar added as a
    // dev-dependency and wired into `oracle::Oracle::new`, and zero oracle mismatches on a
    // fixture in `testdata/`:
    // - C (`.c`, `.h`): tree-sitter-c. Preprocessor lines are the main lexer-level state.
    // - C++ (`.cc`, `.cpp`, `.hpp`, ...): tree-sitter-cpp. Raw strings `R"delim(..)delim"`.
    // - Go (`.go`): tree-sitter-go. Backtick raw strings, no nesting comments.
    // - Python (`.py`): tree-sitter-python. Triple-quoted and prefixed (f/r/b) strings,
    //   decorators as attributes.
    // - JavaScript (`.js`, `.mjs`, `.cjs`): tree-sitter-javascript. The TypeScript lexer
    //   minus type positions; its oracle query is JavaScript's alone.
    // - TSX and JSX (`.tsx`, `.jsx`): tree-sitter-typescript's TSX grammar. JSX elements
    //   and text on top of the TypeScript lexer.
    // - Java (`.java`): tree-sitter-java. Text blocks, annotations as attributes.
    // - C# (`.cs`): tree-sitter-c-sharp. Verbatim and interpolated strings.
    // - Swift (`.swift`): tree-sitter-swift. Nested comments, `#""#` raw strings.
    // - Shell (`.sh`, `.bash`, `.zsh`): tree-sitter-bash. Heredocs and `$(..)` nesting.
    // Also wanted here: Move (`.move`), for Sui; its grammar lives outside crates.io.
}

impl Lang {
    pub fn from_path(path: &str) -> Option<Lang> {
        let ext = Path::new(path).extension()?.to_str()?;
        match ext {
            "rs" => Some(Lang::Rust),
            "ts" | "mts" | "cts" => Some(Lang::TypeScript),
            _ => None,
        }
    }
}

/// `DIFFVADER_LEX_BENCH=file cargo test --release lex_bench -- --ignored --nocapture`
#[test]
#[ignore]
fn lex_bench() {
    let path = std::env::var("DIFFVADER_LEX_BENCH").expect("DIFFVADER_LEX_BENCH");
    let src = std::fs::read(&path).unwrap();
    let lang = Lang::from_path(&path).unwrap();
    let mut out = vec![0u8; src.len()];
    let iters = 20;
    let mut best = f64::MAX;
    for _ in 0..iters {
        out.fill(0);
        let t = std::time::Instant::now();
        match lang {
            Lang::Rust => rust::lex(&src, &mut out),
            Lang::TypeScript => typescript::lex(&src, &mut out),
        }
        best = best.min(t.elapsed().as_secs_f64());
    }
    println!(
        "{} bytes: best {:.2} ms, {:.0} MB/s, {:.2} ns/byte",
        src.len(),
        best * 1e3,
        src.len() as f64 / 1e6 / best,
        best * 1e9 / src.len() as f64
    );
}

/// Classifies every byte of `src`. The result has exactly `src.len()` entries.
pub fn lex(lang: Lang, src: &[u8]) -> Vec<u8> {
    let _s = crate::trace::span_arg("lex", src.len() as u64);
    // Zeroed allocation comes from calloc, so Plain bytes cost nothing up front.
    let mut out = vec![Class::Plain as u8; src.len()];
    match lang {
        Lang::Rust => rust::lex(src, &mut out),
        Lang::TypeScript => typescript::lex(src, &mut out),
    }
    out
}
