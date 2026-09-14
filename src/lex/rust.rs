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
    // After `let` until `=`, `:` or `;`: a pattern, where `Variant(..)` is not a call.
    let mut let_pattern = false;
    // Depth at which `match` / `struct` / `enum` await their `{` (usize::MAX when none),
    // and the bracket kind that `{` gets.
    let mut pending_depth = usize::MAX;
    let mut pending_kind = BRACE;
    // `fn name` seen: the next `(` opens the parameter list.
    let mut fn_pending = false;
    // Between the `|`s of a closure's parameter list, where `a: T` is not a field.
    let mut in_closure = false;
    // Attributes and `macro_rules!` bodies are token trees: lexed as ordinary tokens (over
    // an Attribute background for attributes) but without the contextual rules. The token
    // before an attribute is restored as `prev` at its end so `#[attr] field: T` works.
    let mut tt_end = 0usize;
    let mut tt_prev = Prev::Other;

    macro_rules! innermost {
        () => {
            if depth > 0 {
                brackets[(depth - 1).min(brackets.len() - 1)]
            } else {
                0
            }
        };
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
            b'#' if at(src, i + 1) == b'['
                || (at(src, i + 1) == b'!' && at(src, i + 2) == b'[') =>
            {
                let open = if at(src, i + 1) == b'[' { i + 1 } else { i + 2 };
                let end = bracket_end(src, open);
                out[i..end].fill(ATTRIBUTE);
                if i >= tt_end {
                    tt_prev = prev;
                }
                tt_end = tt_end.max(end);
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
                                i = ident_end(src, end + 1);
                                prev = Prev::Value;
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
                let mut class = keyword(id);
                let mut new_prev = Prev::Value;
                let after = skip_ws(src, end);
                let nb = at(src, after);
                let path_next = nb == b':' && at(src, after + 1) == b':';
                let turbofish = path_next && at(src, after + 2) == b'<';
                let in_tt = i < tt_end;
                if in_tt {
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
                                pending_depth = depth;
                                pending_kind = match id[0] {
                                    b'm' => MATCH_BRACE,
                                    b'e' => ENUM_BRACE,
                                    _ => DECL_BRACE,
                                };
                            }
                            b"let" => let_pattern = true,
                            b"as" => new_prev = Prev::TypeCtx,
                            b"mut" | b"const" | b"dyn" | b"impl" if prev == Prev::TypeCtx => {
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
                    // `u8::MAX`: a primitive name in a path is a plain identifier.
                    TYPE if path_next => class = PLAIN,
                    TYPE => new_prev = Prev::Type,
                    PLAIN => {
                        let upper = id[0].is_ascii_uppercase();
                        class = if in_tt {
                            if upper {
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
                                pending_depth = depth;
                                pending_kind = MATCH_PAREN;
                            }
                            continue;
                        } else if prev == Prev::Dot {
                            if nb == b'(' || turbofish {
                                FUNCTION
                            } else {
                                PROPERTY
                            }
                        } else if prev == Prev::TypeCtx && path_next {
                            new_prev = Prev::TypeCtx;
                            PLAIN
                        } else if prev == Prev::TypeCtx {
                            TYPE
                        } else if nb == b'(' || (!upper && turbofish) {
                            // `Variant(..)` is a constructor in patterns and enum
                            // declarations, a call elsewhere.
                            let mut d = depth;
                            while d > 0 && brackets[(d - 1).min(brackets.len() - 1)] == OTHER {
                                d -= 1;
                            }
                            let kind = if d > 0 {
                                brackets[(d - 1).min(brackets.len() - 1)]
                            } else {
                                0
                            };
                            let in_pattern = let_pattern
                                || (matches!(kind, MATCH_BRACE | MATCH_PAREN)
                                    && pattern_bits >> (d - 1).min(63) & 1 == 1);
                            if upper && (in_pattern || innermost!() == ENUM_BRACE) {
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
                    out[i..end].fill(class);
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
                let kind = match b {
                    b'{' | b'('
                        if pending_depth == depth
                            && (b == b'{') == (pending_kind != MATCH_PAREN) =>
                    {
                        pending_depth = usize::MAX;
                        pending_kind
                    }
                    // `enum E { V { field: T } }`
                    b'{' if innermost!() == ENUM_BRACE => DECL_BRACE,
                    b'{' => BRACE,
                    b'(' if fn_pending => PARAMS,
                    b'(' if prev == Prev::Pub => VIS,
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
                prev = if b == b'{' {
                    Prev::FieldStart
                } else {
                    Prev::Other
                };
                in_closure = false;
                i += 1;
            }
            b')' | b']' | b'}' => {
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
                prev = Prev::FieldStart;
                i += 1;
            }
            b':' if at(src, i + 1) == b':' => {
                out[i] = PUNCT;
                out[i + 1] = PUNCT;
                if at(src, i + 2) == b'<' {
                    prev = Prev::TypeCtx;
                } else if prev != Prev::TypeCtx {
                    prev = Prev::Other;
                }
                i += 2;
            }
            b':' => {
                out[i] = PUNCT;
                prev = if let_pattern || in_closure || matches!(innermost!(), DECL_BRACE | PARAMS) {
                    Prev::TypeCtx
                } else {
                    Prev::Other
                };
                let_pattern = false;
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
                prev = if prev == Prev::TypeCtx || (b == b'<' && prev == Prev::Type) {
                    Prev::TypeCtx
                } else {
                    Prev::Other
                };
                i += 1;
            }
            b';' => {
                out[i] = PUNCT;
                prev = Prev::Other;
                in_closure = false;
                let_pattern = false;
                i += 1;
            }
            b'+' | b'-' | b'/' | b'%' | b'^' | b'!' | b'>' | b'?' | b'@' | b'~' | b'$' => {
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
