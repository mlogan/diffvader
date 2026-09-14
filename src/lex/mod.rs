//! Syntax highlighting lexers.
//!
//! Each lexer is a single linear pass over a file's bytes that writes one `Class` byte per
//! input byte into a preallocated buffer. There is no token stream: the renderer indexes
//! `classes[byte_offset]` while drawing a line. `Class::Plain` is 0, so bytes a lexer does
//! not classify need no write at all.

use std::path::Path;

mod rust;

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
    /// Whole attribute (`#[derive(Debug)]`).
    Attribute,
    /// Lifetimes and labels (`'a`, `'outer`).
    Lifetime,
    /// Field names (`x.field`).
    Property,
    /// `ALL_CAPS` identifiers.
    Constant,
    /// Brackets and delimiters: `()[]{}` `,;:` `::` `.`.
    Punct,
    Operator,
}

pub const CLASS_COUNT: usize = Class::Operator as usize + 1;

impl Class {
    pub fn from_u8(b: u8) -> Class {
        // Safety: the renderer only reads buffers produced by the lexers, which write
        // `Class` discriminants; a bounds check keeps garbage from becoming UB.
        if (b as usize) < CLASS_COUNT {
            unsafe { std::mem::transmute(b) }
        } else {
            Class::Plain
        }
    }

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
            Class::Operator => "operator",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    Rust,
    // TODO: C, Cpp, Go, Python, JavaScript, TypeScript, Java, Swift, Move, Shell.
}

impl Lang {
    pub fn from_path(path: &str) -> Option<Lang> {
        let ext = Path::new(path).extension()?.to_str()?;
        match ext {
            "rs" => Some(Lang::Rust),
            _ => None,
        }
    }
}

/// Classifies every byte of `src`. The result has exactly `src.len()` entries.
pub fn lex(lang: Lang, src: &[u8]) -> Vec<u8> {
    let _s = crate::trace::span_arg("lex", src.len() as u64);
    // Zeroed allocation comes from calloc, so Plain bytes cost nothing up front.
    let mut out = vec![Class::Plain as u8; src.len()];
    match lang {
        Lang::Rust => rust::lex(src, &mut out),
    }
    out
}
