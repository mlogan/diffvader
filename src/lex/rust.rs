//! Rust lexer. One linear pass; see the module docs for the output contract.
//!
//! Context that a parser would supply is approximated with one token of lookbehind
//! (`prev`), a small bracket stack and a few flags, which is enough to tell `fn name`,
//! `x.field`, `x.method(`, `path::func(`, `name!`, `Struct { field: .. }` and
//! `Variant(..)` in patterns versus calls apart the way tree-sitter-rust's highlight
//! query does (see `oracle.rs` for the exact contract).

use super::Class;

const PLAIN: u8 = Class::Plain as u8;
const COMMENT: u8 = Class::Comment as u8;
const STRING: u8 = Class::String as u8;
const ESCAPE: u8 = Class::Escape as u8;
const NUMBER: u8 = Class::Number as u8;
const KEYWORD: u8 = Class::Keyword as u8;
const TYPE: u8 = Class::Type as u8;
const FUNCTION: u8 = Class::Function as u8;
const ATTRIBUTE: u8 = Class::Attribute as u8;
const LIFETIME: u8 = Class::Lifetime as u8;
const PROPERTY: u8 = Class::Property as u8;
const PUNCT: u8 = Class::Punct as u8;

/// The previous significant token, as far as identifier classification cares.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Prev {
    Other,
    /// A token that ends a value (identifier, literal, closing bracket): `|` after it is
    /// a binary operator, not a closure.
    Value,
    /// An identifier classified as a type: `<` after it opens type arguments.
    Type,
    /// `fn`
    Fn,
    /// `.`
    Dot,
    /// `{`, `,`, `pub(..)`: an identifier followed by `:` here is a field name.
    FieldStart,
    /// `pub`: like `FieldStart`, and a `(` next is a visibility scope.
    Pub,
    /// `macro_rules! name`: the next bracket opens a token tree.
    MacroRules,
    /// `::` outside a type: `name {` after it is a struct literal.
    Path,
    /// In a type: after `as`, `->`, `::<`, or `:` in a declaration; kept through `&`,
    /// `*`, `<`, `mut`, `dyn`, `impl`, lifetimes and path segments. The next identifier
    /// is a type whatever its spelling.
    TypeCtx,
}

/// Bracket kinds on the stack.
const OTHER: u8 = b'(';
const BRACE: u8 = b'{';
/// `match x {`: arms alternate between patterns and bodies.
const MATCH_BRACE: u8 = b'm';
/// `matches!(`: a pattern follows the first comma.
const MATCH_PAREN: u8 = b'M';
/// `struct S {`: field declarations, `name: Type`.
const DECL_BRACE: u8 = b'd';
/// `enum E {`: variants, where `Name(..)` is a type.
const ENUM_BRACE: u8 = b'e';
/// `fn f(`: parameters, `name: Type`.
const PARAMS: u8 = b'p';
/// `pub(`: visibility, after which a field name may follow.
const VIS: u8 = b'v';
/// `<` after a type name (`Vec<u8>`): `,` inside stays in the type context and `>`
/// closes back to `Prev::Type`. Unbalanced ones (`MAX < x`) are discarded when an
/// enclosing bracket closes.
const GENERIC: u8 = b'<';
/// `<` opened from a type context (`for<'a>`, `&<T as Tr>`): `>` closes back to it.
const GENERIC_CTX: u8 = b'C';
/// `::<` turbofish: `>` closes back to an expression (`collect::<T>(x)`).
const TURBOFISH: u8 = b'f';
/// `(` in a type: a tuple type, `,` inside stays in the type context.
const TYPE_PAREN: u8 = b't';

const fn make_table(start: bool) -> [bool; 256] {
    let mut t = [false; 256];
    let mut b = 0usize;
    while b < 256 {
        let c = b as u8;
        t[b] = c == b'_' || c.is_ascii_alphabetic() || c >= 0x80 || (!start && c.is_ascii_digit());
        b += 1;
    }
    t
}

static IDENT_START: [bool; 256] = make_table(true);
static IDENT_CONT: [bool; 256] = make_table(false);

#[inline]
fn is_ident_start(b: u8) -> bool {
    IDENT_START[b as usize]
}

#[inline]
fn is_ident_cont(b: u8) -> bool {
    IDENT_CONT[b as usize]
}

#[inline]
fn ident_end(src: &[u8], mut i: usize) -> usize {
    while i < src.len() && is_ident_cont(src[i]) {
        i += 1;
    }
    i
}

/// Byte at `i`, or 0 past the end (0 never matters to any rule).
#[inline]
fn at(src: &[u8], i: usize) -> u8 {
    if i < src.len() {
        src[i]
    } else {
        0
    }
}

/// Index of the next byte that is not a space, tab, `\r` or `\n`.
#[inline]
fn skip_ws(src: &[u8], mut i: usize) -> usize {
    while i < src.len() && matches!(src[i], b' ' | b'\t' | b'\n' | b'\r') {
        i += 1;
    }
    i
}

/// Keywords, literals and primitive types. Contextual keywords that are common as
/// identifiers (`default`, `union`, `raw`, `gen`) are left to the identifier rules.
fn keyword(id: &[u8]) -> u8 {
    match id {
        b"as" | b"async" | b"await" | b"break" | b"const" | b"continue" | b"crate" | b"dyn"
        | b"else" | b"enum" | b"extern" | b"fn" | b"for" | b"if" | b"impl" | b"in" | b"let"
        | b"loop" | b"macro_rules" | b"match" | b"mod" | b"move" | b"mut" | b"pub" | b"ref"
        | b"return" | b"self" | b"static" | b"struct" | b"super" | b"trait" | b"type"
        | b"unsafe" | b"use" | b"where" | b"while" | b"yield" => KEYWORD,
        b"true" | b"false" => NUMBER,
        b"bool" | b"char" | b"str" | b"u8" | b"u16" | b"u32" | b"u64" | b"u128" | b"usize"
        | b"i8" | b"i16" | b"i32" | b"i64" | b"i128" | b"isize" | b"f32" | b"f64" => TYPE,
        _ => PLAIN,
    }
}

/// End of the escape sequence whose `\` is at `j`.
#[inline]
fn escape_end(src: &[u8], j: usize) -> usize {
    let e = match at(src, j + 1) {
        b'x' => j + 4,
        b'u' if at(src, j + 2) == b'{' => {
            memchr::memchr(b'}', &src[j..]).map_or(src.len(), |k| j + k + 1)
        }
        _ => j + 2,
    };
    e.min(src.len())
}

/// A `"` string: `start` is its first byte (a `b`/`c` prefix or the quote), `quote` the
/// opening quote. Returns the index after the closing quote.
fn string(src: &[u8], out: &mut [u8], start: usize, quote: usize) -> usize {
    out[start..=quote].fill(STRING);
    let mut j = quote + 1;
    loop {
        let Some(k) = memchr::memchr2(b'"', b'\\', &src[j..]) else {
            out[j..].fill(STRING);
            return src.len();
        };
        out[j..j + k].fill(STRING);
        j += k;
        if src[j] == b'"' {
            out[j] = STRING;
            return j + 1;
        }
        let e = escape_end(src, j);
        out[j..e].fill(ESCAPE);
        j = e;
    }
}

/// Raw string with `hashes` `#`s; `quote` is the opening `"`, `start` the `r` prefix.
fn raw_string(src: &[u8], out: &mut [u8], start: usize, quote: usize, hashes: usize) -> usize {
    let mut j = quote + 1;
    loop {
        let Some(k) = memchr::memchr(b'"', &src[j..]) else {
            out[start..].fill(STRING);
            return src.len();
        };
        j += k + 1;
        if src.len() - j >= hashes && src[j..j + hashes].iter().all(|&b| b == b'#') {
            let end = j + hashes;
            out[start..end].fill(STRING);
            return end;
        }
    }
}

/// Char literal or lifetime; `i` is the `'`, `start` the literal's first byte (a `b`
/// prefix or the quote itself).
fn quote(src: &[u8], out: &mut [u8], start: usize, i: usize) -> usize {
    let n1 = at(src, i + 1);
    if n1 == b'\\' {
        // Escaped char: scan to the closing quote, bounded so a stray `'\` stays local.
        let mut j = i + 3;
        while j < src.len() && j < i + 12 && src[j] != b'\'' {
            j += 1;
        }
        let end = (j + 1).min(src.len());
        out[start..end].fill(STRING);
        return end;
    }
    let ch_len = match n1 {
        0 => return i + 1,
        b if b < 0x80 => 1,
        b if b >= 0xf0 => 4,
        b if b >= 0xe0 => 3,
        _ => 2,
    };
    if at(src, i + 1 + ch_len) == b'\'' {
        let end = i + 2 + ch_len;
        out[start..end].fill(STRING);
        return end;
    }
    if is_ident_start(n1) {
        let end = ident_end(src, i + 1);
        out[i..end].fill(LIFETIME);
        return end;
    }
    i + 1
}

/// Number literal at `i`. After `.` only an integer is taken (`t.0.1` is two indexes).
fn number(src: &[u8], out: &mut [u8], i: usize, integer_only: bool) -> usize {
    let mut j = i + 1;
    if src[i] == b'0' && matches!(at(src, j), b'x' | b'o' | b'b') {
        j += 1;
        while j < src.len() && (src[j].is_ascii_hexdigit() || src[j] == b'_') {
            j += 1;
        }
    } else {
        while j < src.len() && (src[j].is_ascii_digit() || src[j] == b'_') {
            j += 1;
        }
        if integer_only {
            out[i..j].fill(NUMBER);
            return j;
        }
        // `1.5` and `1.`, but not `1..2` or `1.method()`.
        if at(src, j) == b'.' && at(src, j + 1) != b'.' && !is_ident_start(at(src, j + 1)) {
            j += 1;
            while j < src.len() && (src[j].is_ascii_digit() || src[j] == b'_') {
                j += 1;
            }
        }
        if matches!(at(src, j), b'e' | b'E') {
            let mut k = j + 1;
            if matches!(at(src, k), b'+' | b'-') {
                k += 1;
            }
            if at(src, k).is_ascii_digit() {
                j = k;
                while j < src.len() && (src[j].is_ascii_digit() || src[j] == b'_') {
                    j += 1;
                }
            }
        }
    }
    // Type suffix (`u8`, `f64`, ...).
    j = ident_end(src, j);
    out[i..j].fill(NUMBER);
    j
}

/// For a `<` at `i`: whether `::` or `{` follows the matching `>` (`Vec::<u8>::new` and
/// `S::<T> { .. }` versus `Ok::<T>(..)`). The scan is bounded; unbalanced counts as no.
fn generics_then_path(src: &[u8], i: usize) -> bool {
    let mut depth = 0i32;
    let mut j = i;
    let limit = (i + 512).min(src.len());
    while j < limit {
        match src[j] {
            b'<' => depth += 1,
            b'-' if at(src, j + 1) == b'>' => j += 1,
            b'>' => {
                depth -= 1;
                if depth == 0 {
                    let k = skip_ws(src, j + 1);
                    return (at(src, k) == b':' && at(src, k + 1) == b':') || at(src, k) == b'{';
                }
            }
            _ => {}
        }
        j += 1;
    }
    false
}

/// Just past the bracket matching the one at `i`, skipping strings and comments.
fn bracket_end(src: &[u8], i: usize) -> usize {
    let mut j = i;
    let mut depth = 0i32;
    while j < src.len() {
        match src[j] {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => {
                depth -= 1;
                if depth <= 0 {
                    return j + 1;
                }
            }
            b'"' => {
                let mut k = j + 1;
                while k < src.len() && src[k] != b'"' {
                    if src[k] == b'\\' {
                        k += 1;
                    }
                    k += 1;
                }
                j = k;
            }
            b'/' if at(src, j + 1) == b'/' => {
                j = memchr::memchr(b'\n', &src[j..]).map_or(src.len(), |k| j + k);
            }
            _ => {}
        }
        j += 1;
    }
    src.len()
}

pub fn lex(src: &[u8], out: &mut [u8]) {
    debug_assert_eq!(src.len(), out.len());
    let n = src.len();
    let mut i = 0usize;
    let mut prev = Prev::Other;
    // Innermost open bracket kinds; beyond the array only the depth is tracked.
    let mut brackets = [OTHER; 64];
    let mut depth = 0usize;
    // Bit per depth: inside a `match` block, whether the arm's pattern is still open.
    let mut pattern_bits = 0u64;
    // After `let`/`for` until `=`/`in`, `:` or `;`: a pattern, where `Variant(..)` is not
    // a call. `:` is a type ascription only at the `let`'s own depth (`let x: T`, not
    // `let S { f: pat }`).
    let mut let_pattern = false;
    let mut let_depth = 0usize;
    // Per depth, the kind the next bracket at that depth gets after `match` / `struct` /
    // `enum` / `matches!` (0 when none). Per depth because a scrutinee can nest another.
    let mut pending = [0u8; 64];
    // `fn name` seen: the next `(` opens the parameter list.
    let mut fn_pending = false;
    // Between the `|`s of a closure's parameter list, where `a: T` is not a field.
    let mut in_closure = false;
    // Attributes and `macro_rules!` bodies are token trees: lexed as ordinary tokens (over
    // an Attribute background for attributes) but without the contextual rules. The token
    // before an attribute is restored as `prev` at its end so `#[attr] field: T` works.
    let mut tt_end = 0usize;
    let mut tt_prev = Prev::Other;
    // Start of an attribute's path (`#[name`), which is never a keyword.
    let mut attr_path_at = usize::MAX;
    // `use` until `;`: `as` renames instead of casting.
    let mut in_use = false;
    // Start of a `r#name` raw identifier whose name is at `i` (usize::MAX when none).
    let mut raw_at = usize::MAX;

    macro_rules! innermost {
        () => {
            if depth > 0 {
                brackets[(depth - 1).min(brackets.len() - 1)]
            } else {
                0
            }
        };
    }
    // Whether an identifier here is in a pattern: after `let`/`for`, in a parameter list,
    // or in the pattern half of a `match` arm (looking through `(` and `{` of nested
    // tuple and struct patterns).
    macro_rules! in_pattern {
        () => {{
            let mut d = depth;
            while d > 0
                && matches!(
                    brackets[(d - 1).min(brackets.len() - 1)],
                    OTHER | BRACE | GENERIC | GENERIC_CTX | TURBOFISH
                )
            {
                d -= 1;
            }
            let kind = if d > 0 {
                brackets[(d - 1).min(brackets.len() - 1)]
            } else {
                0
            };
            let_pattern
                || in_closure
                || kind == PARAMS
                || (matches!(kind, MATCH_BRACE | MATCH_PAREN)
                    && pattern_bits >> (d - 1).min(63) & 1 == 1)
        }};
    }

    if src.starts_with(b"#!") && at(src, 2) != b'[' {
        i = memchr::memchr(b'\n', src).unwrap_or(n);
        out[..i].fill(COMMENT);
    }

    while i < n {
        let b = src[i];
        match b {
            b' ' | b'\t' | b'\n' | b'\r' => {
                i += 1;
            }
            b'/' if at(src, i + 1) == b'/' => {
                let end = memchr::memchr(b'\n', &src[i..]).map_or(n, |k| i + k);
                out[i..end].fill(COMMENT);
                i = end;
            }
            b'/' if at(src, i + 1) == b'*' => {
                let mut level = 1;
                let mut j = i + 2;
                while j < n {
                    let Some(k) = memchr::memchr2(b'*', b'/', &src[j..]) else {
                        j = n;
                        break;
                    };
                    j += k;
                    if src[j] == b'*' && at(src, j + 1) == b'/' {
                        level -= 1;
                        j += 2;
                        if level == 0 {
                            break;
                        }
                    } else if src[j] == b'/' && at(src, j + 1) == b'*' {
                        level += 1;
                        j += 2;
                    } else {
                        j += 1;
                    }
                }
                out[i..j].fill(COMMENT);
                i = j;
            }
            b'"' => {
                i = string(src, out, i, i);
                prev = Prev::Value;
            }
            b'\''
                if i < tt_end
                    && src[i + 1..].starts_with(b"static")
                    && !is_ident_cont(at(src, i + 7)) =>
            {
                // In a token tree `'static` is `'` plus the keyword.
                out[i] = PUNCT;
                out[i + 1..i + 7].fill(KEYWORD);
                i += 7;
                prev = Prev::Other;
            }
            b'\'' => {
                let end = quote(src, out, i, i);
                // A lifetime keeps a type context (`&'a foo::bar`).
                if !(out[i] == LIFETIME && prev == Prev::TypeCtx) {
                    prev = Prev::Value;
                }
                i = end;
            }
            b'0'..=b'9' => {
                i = number(src, out, i, prev == Prev::Dot);
                prev = Prev::Value;
            }
            b'#' if i >= tt_end
                && (at(src, i + 1) == b'['
                    || (at(src, i + 1) == b'!' && at(src, i + 2) == b'[')) =>
            {
                let open = if at(src, i + 1) == b'[' { i + 1 } else { i + 2 };
                let end = bracket_end(src, open);
                out[i..end].fill(ATTRIBUTE);
                tt_prev = prev;
                tt_end = end;
                attr_path_at = skip_ws(src, open + 1);
                i += 1;
            }
            _ if is_ident_start(b) => {
                let end = ident_end(src, i);
                let id = &src[i..end];
                let next = at(src, end);
                // Literal prefixes: `r"`, `r#"`, `br"`, `cr"`, `b"`, `c"`, `b'`, and raw
                // identifiers `r#name`.
                if next == b'"' || next == b'#' || next == b'\'' {
                    match id {
                        b"r" | b"br" | b"cr" if next != b'\'' => {
                            let mut hashes = 0;
                            while at(src, end + hashes) == b'#' {
                                hashes += 1;
                            }
                            if at(src, end + hashes) == b'"' {
                                i = raw_string(src, out, i, end + hashes, hashes);
                                prev = Prev::Value;
                                continue;
                            }
                            if id == b"r" && hashes == 1 && is_ident_start(at(src, end + 1)) {
                                raw_at = i;
                                i = end + 1;
                                continue;
                            }
                        }
                        b"b" | b"c" if next == b'"' => {
                            i = string(src, out, i, end);
                            prev = Prev::Value;
                            continue;
                        }
                        b"b" if next == b'\'' => {
                            i = quote(src, out, i, end);
                            prev = Prev::Value;
                            continue;
                        }
                        _ => {}
                    }
                }
                // Raw identifiers, `$meta` variables and attribute names are never keywords;
                // metavariables are not types either, whatever their spelling.
                let raw = std::mem::replace(&mut raw_at, usize::MAX);
                let metavar = i > 0 && src[i - 1] == b'$';
                let plain_name = raw != usize::MAX || i == attr_path_at || metavar;
                let mut class = if plain_name { PLAIN } else { keyword(id) };
                let mut new_prev = Prev::Value;
                let after = skip_ws(src, end);
                let nb = at(src, after);
                let path_next = nb == b':' && at(src, after + 1) == b':';
                let turbofish = path_next && at(src, after + 2) == b'<';
                let in_tt = i < tt_end;
                if in_tt && !plain_name {
                    // Token trees know a slightly different keyword set.
                    class = match id {
                        b"else" | b"in" | b"move" | b"ref" | b"dyn" | b"extern" | b"yield"
                        | b"macro_rules" => PLAIN,
                        b"default" | b"union" | b"gen" => KEYWORD,
                        _ => class,
                    };
                }
                match class {
                    KEYWORD => {
                        new_prev = Prev::Other;
                        match id {
                            b"fn" if !in_tt => new_prev = Prev::Fn,
                            b"pub" => new_prev = Prev::Pub,
                            b"match" | b"struct" | b"union" | b"enum" if !in_tt => {
                                pending[depth.min(63)] = match id[0] {
                                    b'm' => MATCH_BRACE,
                                    b'e' => ENUM_BRACE,
                                    _ => DECL_BRACE,
                                };
                                if id != b"match" {
                                    new_prev = Prev::TypeCtx;
                                }
                            }
                            b"let" if !in_tt => {
                                let_pattern = true;
                                let_depth = depth;
                            }
                            b"for" if !in_tt && !matches!(prev, Prev::TypeCtx | Prev::Type) => {
                                let_pattern = true;
                                let_depth = depth;
                            }
                            b"in" => let_pattern = false,
                            // A guard: the arm's pattern is over.
                            b"if" if matches!(innermost!(), MATCH_BRACE | MATCH_PAREN) => {
                                pattern_bits &= !(1 << (depth - 1).min(63));
                            }
                            // `use path;` or `use<'a>` precise capturing.
                            b"use" if nb != b'<' => in_use = true,
                            b"as" if !in_use => new_prev = Prev::TypeCtx,
                            b"impl" | b"trait" | b"type" => new_prev = Prev::TypeCtx,
                            b"mut" | b"const" | b"dyn" | b"for" if prev == Prev::TypeCtx => {
                                new_prev = Prev::TypeCtx
                            }
                            b"self" => new_prev = Prev::Value,
                            // `macro_rules!` is one keyword.
                            b"macro_rules" if next == b'!' => {
                                out[i..=end].fill(KEYWORD);
                                i = end + 1;
                                prev = Prev::MacroRules;
                                continue;
                            }
                            _ => {}
                        }
                    }
                    // Primitive names are types only in type positions: not `fn bool`,
                    // `F::bool(..)`, `x.bool` or `u8::MAX`.
                    TYPE if prev == Prev::Fn => {
                        fn_pending = true;
                        class = FUNCTION;
                    }
                    TYPE if nb == b'(' => class = if in_pattern!() { PLAIN } else { FUNCTION },
                    TYPE if prev == Prev::Dot => class = PROPERTY,
                    // `use std::str`, `let str = ..`
                    TYPE if path_next || in_use || (let_pattern && prev != Prev::TypeCtx) => {
                        class = PLAIN
                    }
                    TYPE => new_prev = Prev::Type,
                    PLAIN => {
                        let upper = id[0].is_ascii_uppercase();
                        class = if in_tt {
                            if upper && !metavar {
                                TYPE
                            } else {
                                PLAIN
                            }
                        } else if prev == Prev::MacroRules {
                            new_prev = Prev::MacroRules;
                            PLAIN
                        } else if prev == Prev::Fn {
                            fn_pending = true;
                            FUNCTION
                        } else if next == b'!' && at(src, end + 1) != b'=' {
                            // Macro invocation: the `!` is part of the name.
                            out[i..=end].fill(FUNCTION);
                            i = end + 1;
                            prev = Prev::Other;
                            if id == b"matches" {
                                pending[depth.min(63)] = MATCH_PAREN;
                            }
                            continue;
                        } else if prev == Prev::Dot {
                            if nb == b'(' || turbofish {
                                FUNCTION
                            } else {
                                PROPERTY
                            }
                        } else if !upper && nb == b'(' {
                            FUNCTION
                        } else if prev == Prev::Path && !upper && nb == b'{' {
                            // `path::name { .. }` struct literal
                            TYPE
                        } else if prev == Prev::TypeCtx && path_next {
                            new_prev = Prev::TypeCtx;
                            if upper {
                                TYPE
                            } else {
                                PLAIN
                            }
                        } else if prev == Prev::TypeCtx {
                            TYPE
                        } else if nb == b'('
                            || (turbofish && (!upper || !generics_then_path(src, after + 2)))
                        {
                            // `Variant(..)` is a constructor in patterns and enum
                            // declarations, a call elsewhere.
                            if upper && (in_pattern!() || innermost!() == ENUM_BRACE) {
                                TYPE
                            } else {
                                FUNCTION
                            }
                        } else if upper {
                            TYPE
                        } else if nb == b':'
                            && !path_next
                            && matches!(prev, Prev::FieldStart | Prev::Pub)
                            && !in_closure
                            && matches!(innermost!(), BRACE | DECL_BRACE)
                        {
                            PROPERTY
                        } else {
                            PLAIN
                        };
                        if class == TYPE {
                            new_prev = Prev::Type;
                        }
                    }
                    _ => {}
                }
                if class != PLAIN {
                    out[raw.min(i)..end].fill(class);
                }
                if in_tt {
                    new_prev = Prev::Other;
                }
                prev = new_prev;
                i = end;
            }
            b'(' | b'[' | b'{' => {
                if prev == Prev::MacroRules {
                    tt_end = tt_end.max(bracket_end(src, i));
                }
                let waiting = pending[depth.min(63)];
                let kind = match b {
                    b'{' | b'(' if waiting != 0 && (b == b'{') == (waiting != MATCH_PAREN) => {
                        pending[depth.min(63)] = 0;
                        waiting
                    }
                    // `enum E { V { field: T } }`
                    b'{' if innermost!() == ENUM_BRACE => DECL_BRACE,
                    b'{' => BRACE,
                    b'(' if fn_pending => PARAMS,
                    b'(' if prev == Prev::Pub => VIS,
                    b'(' if prev == Prev::TypeCtx => TYPE_PAREN,
                    _ => OTHER,
                };
                fn_pending = false;
                if depth < brackets.len() {
                    brackets[depth] = kind;
                }
                if kind == MATCH_BRACE {
                    pattern_bits |= 1 << depth.min(63);
                }
                depth += 1;
                out[i] = PUNCT;
                prev = match kind {
                    TYPE_PAREN => Prev::TypeCtx,
                    _ if b == b'{' => Prev::FieldStart,
                    _ => Prev::Other,
                };
                in_closure = false;
                in_use &= b != b'{';
                i += 1;
            }
            b')' | b']' | b'}' => {
                while matches!(innermost!(), GENERIC | GENERIC_CTX | TURBOFISH) {
                    depth -= 1;
                }
                let closed = innermost!();
                depth = depth.saturating_sub(1);
                if b == b'}' && innermost!() == MATCH_BRACE {
                    // A block-bodied arm ended; the next arm's pattern follows.
                    pattern_bits |= 1 << (depth - 1).min(63);
                }
                out[i] = PUNCT;
                i += 1;
                prev = if i == tt_end {
                    tt_prev
                } else if closed == VIS {
                    Prev::FieldStart
                } else if closed == TYPE_PAREN {
                    Prev::Type
                } else {
                    Prev::Value
                };
                in_closure = false;
            }
            b',' => {
                out[i] = PUNCT;
                if matches!(innermost!(), MATCH_BRACE | MATCH_PAREN) {
                    pattern_bits |= 1 << (depth - 1).min(63);
                }
                prev = if matches!(innermost!(), GENERIC | GENERIC_CTX | TURBOFISH | TYPE_PAREN) {
                    Prev::TypeCtx
                } else {
                    Prev::FieldStart
                };
                i += 1;
            }
            b':' if at(src, i + 1) == b':' => {
                out[i] = PUNCT;
                out[i + 1] = PUNCT;
                i += 2;
                if at(src, i) == b'<' {
                    out[i] = PUNCT;
                    if depth < brackets.len() {
                        brackets[depth] = TURBOFISH;
                    }
                    depth += 1;
                    prev = Prev::TypeCtx;
                    i += 1;
                } else if prev != Prev::TypeCtx {
                    prev = Prev::Path;
                }
            }
            b':' => {
                out[i] = PUNCT;
                // `T: Bound`, `let x: T`, `|a: T|`, fields and parameters.
                prev = if (let_pattern && depth == let_depth)
                    || in_closure
                    || prev == Prev::Type
                    || matches!(innermost!(), DECL_BRACE | PARAMS)
                {
                    Prev::TypeCtx
                } else {
                    Prev::Other
                };
                let_pattern &= depth != let_depth;
                i += 1;
            }
            b'.' => {
                if at(src, i + 1) == b'.' {
                    let end = if matches!(at(src, i + 2), b'.' | b'=') {
                        i + 3
                    } else {
                        i + 2
                    };
                    out[i..end].fill(PUNCT);
                    prev = Prev::Other;
                    i = end;
                } else {
                    out[i] = PUNCT;
                    prev = Prev::Dot;
                    i += 1;
                }
            }
            b'|' => {
                out[i] = PUNCT;
                if at(src, i + 1) == b'|' {
                    out[i + 1] = PUNCT;
                    i += 2;
                } else {
                    in_closure = !in_closure && prev != Prev::Value && prev != Prev::Type;
                    i += 1;
                }
                prev = Prev::Other;
            }
            b'=' => {
                out[i] = PUNCT;
                if at(src, i + 1) == b'>' {
                    out[i + 1] = PUNCT;
                    i += 2;
                    if depth > 0 {
                        pattern_bits &= !(1 << (depth - 1).min(63));
                    }
                } else {
                    i += 1;
                }
                let_pattern = false;
                prev = Prev::Other;
            }
            b'-' if at(src, i + 1) == b'>' => {
                out[i] = PUNCT;
                out[i + 1] = PUNCT;
                prev = Prev::TypeCtx;
                i += 2;
            }
            b'&' | b'*' | b'<' => {
                out[i] = PUNCT;
                let in_type = prev == Prev::TypeCtx || (b == b'<' && prev == Prev::Type);
                if b == b'<' && in_type {
                    if depth < brackets.len() {
                        brackets[depth] = if prev == Prev::Type {
                            GENERIC
                        } else {
                            GENERIC_CTX
                        };
                    }
                    depth += 1;
                }
                prev = if in_type { Prev::TypeCtx } else { Prev::Other };
                i += 1;
            }
            b'>' => {
                out[i] = PUNCT;
                prev = match innermost!() {
                    GENERIC => {
                        depth -= 1;
                        Prev::Type
                    }
                    GENERIC_CTX => {
                        depth -= 1;
                        Prev::TypeCtx
                    }
                    TURBOFISH => {
                        depth -= 1;
                        Prev::Value
                    }
                    _ => Prev::Other,
                };
                i += 1;
            }
            b';' => {
                out[i] = PUNCT;
                prev = Prev::Other;
                in_closure = false;
                let_pattern = false;
                in_use = false;
                // `struct Unit;` never gets its brace.
                pending[depth.min(63)] = 0;
                i += 1;
            }
            b'+' | b'-' | b'/' | b'%' | b'^' | b'!' | b'?' | b'@' | b'~' | b'$' => {
                out[i] = PUNCT;
                prev = Prev::Other;
                i += 1;
            }
            _ => {
                prev = Prev::Other;
                i += 1;
            }
        }
    }
}
