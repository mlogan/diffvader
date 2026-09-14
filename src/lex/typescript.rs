//! TypeScript lexer. One linear pass; see the module docs for the output contract.
//!
//! Context that a parser would supply is approximated with the previous significant token
//! (`prev`), a bracket-kind stack and a few flags. That is enough to tell regex literals
//! from division, resume template literals after `${..}`, follow type positions, tell
//! object keys and class or interface members from expressions, and find function names at
//! declarations, calls and function-valued bindings the way tree-sitter-typescript's
//! highlight queries do (see `oracle.rs` for the exact contract).

use super::Class;

const PLAIN: u8 = Class::Plain as u8;
const COMMENT: u8 = Class::Comment as u8;
const STRING: u8 = Class::String as u8;
const NUMBER: u8 = Class::Number as u8;
const KEYWORD: u8 = Class::Keyword as u8;
const TYPE: u8 = Class::Type as u8;
const FUNCTION: u8 = Class::Function as u8;
const PROPERTY: u8 = Class::Property as u8;
const CONSTANT: u8 = Class::Constant as u8;
const PUNCT: u8 = Class::Punct as u8;

/// The previous significant token, as far as classification cares.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Prev {
    /// Statement start: `;`, a block's braces, `=>`, `else`. `{` here opens a block.
    Stmt,
    /// An operator or keyword that expects an expression. `{` opens an object, `/` a regex.
    Op,
    /// The end of a value: identifier, literal, `]`, an object's `}`. `/` divides.
    Value,
    /// `)`: `{` opens a block, `:` starts a return type.
    CloseParen,
    /// `.` or `?.`
    Dot,
    /// `function`: the name follows.
    Fn,
    /// `class`, `interface`, `type`, `enum`, `namespace`: the declared name follows.
    DeclKw,
    /// A declared name or function name: `<` opens type parameters.
    TypeName,
    /// `let`, `const`, `var`, or `,` between declarators: a binding follows.
    Binding,
    /// `{` or `,` in an object, enum or parameter list: a key or name follows.
    FieldStart,
    /// A member modifier (`static`, `readonly`, `get`, ...): the member name follows.
    Member,
    /// A type follows.
    TypeCtx,
    /// A type just ended: `<`, `[`, `|`, `&`, `.` continue it.
    Type,
    /// `ns.` inside a type.
    TypeQual,
    /// `)` of a function type's parameters: `=>` continues the type.
    TypeParenClose,
    /// `typeof` inside a type: an expression name follows.
    TypeofInType,
}

// Bracket kinds.
const PAREN: u8 = b'(';
const BRACKET: u8 = b'[';
const BLOCK: u8 = b'{';
/// Object literal or destructuring pattern.
const OBJECT: u8 = b'o';
const CLASS_BODY: u8 = b'c';
/// Interface body; closes to a statement.
const IFACE_BODY: u8 = b'i';
/// Object type literal; closes to a type.
const TYPE_BRACE: u8 = b't';
const ENUM_BODY: u8 = b'e';
/// `${` in a template literal; `}` resumes the template.
const TEMPLATE_SUB: u8 = b'$';
/// `${` in a template literal type.
const TEMPLATE_TYPE_SUB: u8 = b'%';
/// Type arguments inside a type (`Array<T>`); closes to a type.
const GENERIC: u8 = b'<';
/// Type parameters of a function type (`: <T>(x: T) => T`); closes to a type position.
const GENERIC_FN: u8 = b'f';
/// Type parameters or a type assertion (`f<T>(`, `<T>(x) =>`); closes to an operator.
const GENERIC_PARAMS: u8 = b'p';
/// Type arguments of a call (`f<T>(x)`); closes to a value.
const GENERIC_CALL: u8 = b'g';
/// `(` in a type: function type parameters or a grouped type.
const TYPE_PAREN: u8 = b'T';
/// `[` in a type: tuple, array or indexed access.
const TYPE_BRACKET: u8 = b'B';
/// `[` at a member position: index signature, mapped type key or computed name.
const INDEX_SIG: u8 = b'I';

const fn make_table(start: bool) -> [bool; 256] {
    let mut t = [false; 256];
    let mut b = 0usize;
    while b < 256 {
        let c = b as u8;
        t[b] = c == b'_'
            || c == b'$'
            || c.is_ascii_alphabetic()
            || c >= 0x80
            || (!start && c.is_ascii_digit());
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

/// Index of the next byte that is not whitespace.
#[inline]
fn skip_ws(src: &[u8], mut i: usize) -> usize {
    while i < src.len() && matches!(src[i], b' ' | b'\t' | b'\n' | b'\r') {
        i += 1;
    }
    i
}

/// Index of the next byte that is neither whitespace nor inside a comment.
fn skip_trivia(src: &[u8], mut i: usize) -> usize {
    loop {
        i = skip_ws(src, i);
        if at(src, i) == b'/' && at(src, i + 1) == b'/' {
            i = memchr::memchr(b'\n', &src[i..]).map_or(src.len(), |k| i + k);
        } else if at(src, i) == b'/' && at(src, i + 1) == b'*' {
            i = block_comment_end(src, i);
        } else {
            return i;
        }
    }
}

fn block_comment_end(src: &[u8], i: usize) -> usize {
    let mut j = i + 2;
    while let Some(k) = memchr::memchr(b'*', &src[j..]) {
        j += k + 1;
        if at(src, j) == b'/' {
            return j + 1;
        }
    }
    src.len()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Word {
    /// Not a keyword.
    Ident,
    /// Reserved word, a keyword wherever it is not a property or member name.
    Strict,
    /// Keyword only in some positions.
    Contextual,
    /// `true`, `false`, `null`, `undefined`.
    Literal,
    /// `this`, `super`.
    This,
    /// Predefined type name (`string`, `number`, ...), a type only in type positions.
    Predefined,
}

fn word(id: &[u8]) -> Word {
    if id.len() < 2 || id.len() > 10 || !id[0].is_ascii_lowercase() {
        return Word::Ident;
    }
    match id {
        b"break" | b"case" | b"catch" | b"class" | b"const" | b"continue" | b"debugger"
        | b"default" | b"delete" | b"do" | b"else" | b"enum" | b"export" | b"extends"
        | b"finally" | b"for" | b"function" | b"if" | b"import" | b"in" | b"instanceof"
        | b"new" | b"return" | b"switch" | b"throw" | b"try" | b"typeof" | b"var" | b"void"
        | b"while" | b"with" | b"yield" | b"await" => Word::Strict,
        b"as" | b"async" | b"from" | b"get" | b"of" | b"set" | b"static" | b"target" | b"let"
        | b"type" | b"readonly" | b"declare" | b"abstract" | b"namespace" | b"keyof"
        | b"satisfies" | b"override" | b"implements" | b"private" | b"protected" | b"public"
        | b"interface" => Word::Contextual,
        b"true" | b"false" | b"null" | b"undefined" => Word::Literal,
        b"this" | b"super" => Word::This,
        b"any" | b"number" | b"boolean" | b"string" | b"symbol" | b"unknown" | b"never"
        | b"object" | b"bigint" => Word::Predefined,
        _ => Word::Ident,
    }
}

/// Class of an expression identifier by naming convention: `^[A-Z_][A-Z\d_]+$` is a
/// constant, any other capitalized name a type or constructor, else `fallback`.
fn by_convention(id: &[u8], fallback: u8) -> u8 {
    if id.len() > 1
        && (id[0].is_ascii_uppercase() || id[0] == b'_')
        && id[1..]
            .iter()
            .all(|&b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
    {
        return CONSTANT;
    }
    if id[0].is_ascii_uppercase() {
        return TYPE;
    }
    fallback
}

/// Just past the bracket matching the one at `i` (any of `([{`), skipping strings and
/// comments; `None` when unbalanced within `limit` bytes.
fn bracket_end(src: &[u8], i: usize, limit: usize) -> Option<usize> {
    let end = (i + limit).min(src.len());
    let mut depth = 0i32;
    let mut j = i;
    while j < end {
        match src[j] {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(j + 1);
                }
            }
            q @ (b'"' | b'\'' | b'`') => {
                j += 1;
                while j < end && src[j] != q {
                    if src[j] == b'\\' {
                        j += 1;
                    }
                    j += 1;
                }
            }
            b'/' if at(src, j + 1) == b'/' => {
                j = memchr::memchr(b'\n', &src[j..]).map_or(end, |k| j + k);
            }
            b'/' if at(src, j + 1) == b'*' => j = block_comment_end(src, j) - 1,
            _ => {}
        }
        j += 1;
    }
    None
}

/// For a `<` at `i`: just past the matching `>` if the span looks like type arguments
/// (no operators that cannot appear in a type), within a bounded scan.
fn generic_end(src: &[u8], i: usize) -> Option<usize> {
    let end = (i + 512).min(src.len());
    let mut angle = 0i32;
    let mut j = i;
    while j < end {
        match src[j] {
            b'<' => angle += 1,
            b'>' => {
                angle -= 1;
                if angle == 0 {
                    return Some(j + 1);
                }
            }
            b'=' if at(src, j + 1) == b'>' => j += 1,
            b'(' | b'[' | b'{' => j = bracket_end(src, j, end - j)? - 1,
            b'=' if at(src, j + 1) == b'=' => return None,
            b';' | b')' | b']' | b'}' | b'+' | b'*' | b'/' | b'%' | b'^' | b'!' => return None,
            b'&' if at(src, j + 1) == b'&' => return None,
            b'|' if at(src, j + 1) == b'|' => return None,
            b'"' | b'\'' | b'`' => j = bracket_end_quote(src, j, end)?,
            _ => {}
        }
        j += 1;
    }
    None
}

/// Index of the closing quote of the string starting at `i`.
fn bracket_end_quote(src: &[u8], i: usize, end: usize) -> Option<usize> {
    let q = src[i];
    let mut j = i + 1;
    while j < end {
        if src[j] == b'\\' {
            j += 2;
            continue;
        }
        if src[j] == q {
            return Some(j);
        }
        j += 1;
    }
    None
}

/// Skips a type annotation starting at `i` (just after `:`) up to the `=` (or, with
/// `arrow_ends`, the `=>`) that ends it at nesting depth 0. Returns the index of that `=`.
fn skip_annotation(src: &[u8], i: usize, arrow_ends: bool) -> Option<usize> {
    let end = (i + 512).min(src.len());
    let mut j = i;
    let mut angle = 0i32;
    while j < end {
        match src[j] {
            b'<' => angle += 1,
            b'>' => angle -= 1,
            b'=' if at(src, j + 1) == b'>' => {
                if angle == 0 && arrow_ends {
                    return Some(j);
                }
                j += 1;
            }
            b'=' if angle == 0 => return Some(j),
            b'(' | b'[' | b'{' => j = bracket_end(src, j, end - j)? - 1,
            b';' | b')' | b']' | b'}' | b',' if angle == 0 => return None,
            b'"' | b'\'' | b'`' => j = bracket_end_quote(src, j, end)?,
            _ => {}
        }
        j += 1;
    }
    None
}

/// Whether the expression starting at `i` is a function: `function`, an arrow function
/// (`(..) =>`, `x =>`, `async ..`, `<T>(..) =>`).
fn is_function_value(src: &[u8], i: usize) -> bool {
    let mut j = skip_trivia(src, i);
    let b = at(src, j);
    if is_ident_start(b) {
        let e = ident_end(src, j);
        let id = &src[j..e];
        if id == b"function" {
            return true;
        }
        let k = skip_trivia(src, e);
        if id == b"async" && at(src, k) != b'=' && at(src, k) != b'.' {
            return is_ident_start(at(src, k)) || matches!(at(src, k), b'(' | b'<');
        }
        return at(src, k) == b'=' && at(src, k + 1) == b'>';
    }
    if b == b'<' {
        let Some(e) = generic_end(src, j) else {
            return false;
        };
        j = skip_trivia(src, e);
    }
    if at(src, j) != b'(' {
        return false;
    }
    let Some(e) = bracket_end(src, j, 4096) else {
        return false;
    };
    let mut k = skip_trivia(src, e);
    if at(src, k) == b':' {
        match skip_annotation(src, k + 1, true) {
            Some(eq) => k = eq,
            None => return false,
        }
    }
    at(src, k) == b'=' && at(src, k + 1) == b'>'
}

/// For a binding name ending at `i`: whether it is bound to a function value, looking past
/// an optional type annotation.
fn binds_function(src: &[u8], i: usize) -> bool {
    let mut k = skip_trivia(src, i);
    if at(src, k) == b':' {
        match skip_annotation(src, k + 1, false) {
            Some(eq) => k = eq,
            None => return false,
        }
    }
    at(src, k) == b'=' && !matches!(at(src, k + 1), b'=' | b'>') && is_function_value(src, k + 1)
}

/// For `import`/`export` followed by `i`: whether a clause of names follows, as opposed
/// to a declaration (`export const ..`, `export type X = ..`).
fn import_clause(src: &[u8], i: usize, export: bool) -> bool {
    if !is_ident_start(at(src, i)) {
        // `export = x` is an assignment.
        return at(src, i) != b'=';
    }
    let e = ident_end(src, i);
    match &src[i..e] {
        b"const" | b"let" | b"var" | b"function" | b"class" | b"interface" | b"enum"
        | b"default" | b"async" | b"declare" | b"abstract" | b"namespace" | b"module" => false,
        b"type" if export => {
            let name = skip_trivia(src, e);
            if !is_ident_start(at(src, name)) {
                return true;
            }
            let k = skip_trivia(src, ident_end(src, name));
            !matches!(at(src, k), b'=' | b'<')
        }
        _ => true,
    }
}

/// For a function name followed by `i`: whether a body follows the signature (a
/// declaration, not an overload or ambient signature).
fn has_body(src: &[u8], i: usize) -> bool {
    let mut j = skip_trivia(src, i);
    if at(src, j) == b'<' {
        match generic_end(src, j) {
            Some(e) => j = skip_trivia(src, e),
            None => return false,
        }
    }
    if at(src, j) != b'(' {
        return false;
    }
    let Some(e) = bracket_end(src, j, 8192) else {
        return false;
    };
    j = skip_trivia(src, e);
    if at(src, j) != b':' {
        return at(src, j) == b'{';
    }
    // Return type: a `{` right after a type operator opens an object type; any other `{`
    // is the body. A name that does not continue the type starts the next declaration.
    let end = (j + 2048).min(src.len());
    let mut last = b':';
    j += 1;
    while j < end {
        j = skip_trivia(src, j);
        let c = at(src, j);
        // `=` marks a function type's `=>`.
        let continues = matches!(
            last,
            b':' | b'|' | b'&' | b'<' | b',' | b'=' | b'(' | b'[' | b'?' | b'.'
        );
        match c {
            b'{' | b'(' | b'[' if c != b'{' || continues => match bracket_end(src, j, end - j) {
                Some(e) => {
                    j = e;
                    last = b')';
                    continue;
                }
                None => return false,
            },
            b'{' => return true,
            b';' | b'}' | 0 => return false,
            b'=' if at(src, j + 1) == b'>' => {
                j += 2;
                last = b'=';
                continue;
            }
            b'"' | b'\'' | b'`' => match bracket_end_quote(src, j, end) {
                Some(e) => {
                    j = e + 1;
                    last = b'"';
                    continue;
                }
                None => return false,
            },
            _ if is_ident_start(c) => {
                let e = ident_end(src, j);
                let op = matches!(
                    &src[j..e],
                    b"extends"
                        | b"is"
                        | b"keyof"
                        | b"typeof"
                        | b"infer"
                        | b"readonly"
                        | b"unique"
                        | b"asserts"
                        | b"new"
                );
                if !continues && !op {
                    return false;
                }
                j = e;
                last = if op { b':' } else { b'a' };
                continue;
            }
            _ => last = c,
        }
        j += 1;
    }
    false
}

/// A string literal quoted by `q` at `i`; unterminated strings end at the line.
fn string(src: &[u8], out: &mut [u8], i: usize, q: u8) -> usize {
    let mut j = i + 1;
    loop {
        let Some(k) = memchr::memchr3(q, b'\\', b'\n', &src[j..]) else {
            out[i..].fill(STRING);
            return src.len();
        };
        j += k;
        match src[j] {
            b'\\' if at(src, j + 1) == b'\r' && at(src, j + 2) == b'\n' => j += 3,
            b'\\' => j += 2,
            b'\n' => {
                out[i..j].fill(STRING);
                return j;
            }
            _ => {
                out[i..=j].fill(STRING);
                return j + 1;
            }
        }
        if j >= src.len() {
            out[i..].fill(STRING);
            return src.len();
        }
    }
}

/// Template literal text from `i` (just after the opening backtick or a substitution's
/// `}`). Returns the index after the closing backtick, or after `${` with `true`. Template
/// literal types (`in_type`) are not strings: nothing is written for them.
fn template(src: &[u8], out: &mut [u8], i: usize, in_type: bool) -> (usize, bool) {
    let (text, delim) = if in_type {
        (PLAIN, PLAIN)
    } else {
        (STRING, PUNCT)
    };
    let mut j = i;
    loop {
        let Some(k) = memchr::memchr3(b'`', b'\\', b'$', &src[j..]) else {
            out[i..].fill(text);
            return (src.len(), false);
        };
        j += k;
        match src[j] {
            b'\\' => j += 2,
            b'$' if at(src, j + 1) == b'{' => {
                out[i..j].fill(text);
                out[j] = delim;
                out[j + 1] = delim;
                return (j + 2, true);
            }
            b'$' => j += 1,
            _ => {
                out[i..=j].fill(text);
                return (j + 1, false);
            }
        }
        if j >= src.len() {
            out[i..].fill(text);
            return (src.len(), false);
        }
    }
}

/// A regex literal at the `/` at `i`, or `None` if the line ends first.
fn regex(src: &[u8], out: &mut [u8], i: usize) -> Option<usize> {
    let mut j = i + 1;
    let mut class = false;
    while j < src.len() {
        match src[j] {
            b'\\' => j += 1,
            b'\n' => return None,
            b'[' => class = true,
            b']' => class = false,
            b'/' if !class => {
                let end = ident_end(src, j + 1);
                out[i..end].fill(STRING);
                out[i] = PUNCT;
                out[j] = PUNCT;
                return Some(end);
            }
            _ => {}
        }
        j += 1;
    }
    None
}

fn number(src: &[u8], out: &mut [u8], i: usize) -> usize {
    let mut j = i;
    if src[i] == b'0' && matches!(at(src, i + 1), b'x' | b'X' | b'o' | b'O' | b'b' | b'B') {
        j = i + 2;
        while j < src.len() && (src[j].is_ascii_hexdigit() || src[j] == b'_') {
            j += 1;
        }
    } else {
        while j < src.len() && (src[j].is_ascii_digit() || src[j] == b'_') {
            j += 1;
        }
        if at(src, j) == b'.' && at(src, j + 1) != b'.' {
            j += 1;
            while j < src.len() && (src[j].is_ascii_digit() || src[j] == b'_') {
                j += 1;
            }
        }
        if matches!(at(src, j), b'e' | b'E')
            && (at(src, j + 1).is_ascii_digit()
                || (matches!(at(src, j + 1), b'+' | b'-') && at(src, j + 2).is_ascii_digit()))
        {
            j += 2;
            while j < src.len() && (src[j].is_ascii_digit() || src[j] == b'_') {
                j += 1;
            }
        }
    }
    if at(src, j) == b'n' {
        j += 1;
    }
    out[i..j].fill(NUMBER);
    j
}

pub fn lex(src: &[u8], out: &mut [u8]) {
    debug_assert_eq!(src.len(), out.len());
    let n = src.len();
    let mut i = 0usize;
    let mut prev = Prev::Stmt;
    // Innermost open bracket kinds, indexed by depth modulo 64: nesting deeper than that
    // aliases outer levels, which can only misclassify, and keeps every index in bounds.
    let mut brackets = [BLOCK; 64];
    let mut depth = 0usize;
    // Per depth: open `?` count, and a bit per open `?` that belongs to a conditional type.
    let mut tern = [0u8; 64];
    let mut tern_type = [0u32; 64];
    // Per depth: the kind the next `{` at that depth gets after `class`/`interface`/`enum`.
    let mut pending = [0u8; 64];
    // Depth of the innermost `let`/`const`/`var`, and whether its binding (before `=`)
    // is still open, where `:` starts a type.
    let mut decl_depth = usize::MAX;
    let mut decl_open = false;
    // After `type Name` until `=`: the right-hand side is a type.
    let mut type_alias = false;
    // Between `interface`/`class` and the body: `extends` / `implements` start types.
    let mut iface_header = false;
    // After `case`/`default` until `:`.
    let mut in_case = false;
    // `?:` / `!:`: the next `:` starts a type.
    let mut optional = false;
    // The next `<` opens call type arguments (`f<T>(`).
    let mut generic_call = false;
    // After `new` until its arguments: the callee is a constructor, not a call.
    let mut in_new = false;
    // In an `import`/`export` clause: `as` renames and names are never keywords.
    let mut in_import = false;
    // After `await` until its operand's arguments: tree-sitter reads `await f<T>(x)` as
    // comparisons, so the callee is not a call.
    let mut after_await = false;

    macro_rules! innermost {
        () => {
            if depth > 0 {
                brackets[(depth - 1) & 63]
            } else {
                BLOCK
            }
        };
    }
    macro_rules! push {
        ($kind:expr) => {{
            brackets[depth & 63] = $kind;
            depth += 1;
            tern[depth & 63] = 0;
            tern_type[depth & 63] = 0;
            pending[depth & 63] = 0;
        }};
    }

    if src.starts_with(b"#!") {
        i = memchr::memchr(b'\n', src).unwrap_or(n);
        out[..i].fill(COMMENT);
    }

    while i < n {
        let b = src[i];
        match b {
            b' ' | b'\t' | b'\n' | b'\r' => i = skip_ws(src, i + 1),
            b'/' if at(src, i + 1) == b'/' => {
                let end = memchr::memchr(b'\n', &src[i..]).map_or(n, |k| i + k);
                out[i..end].fill(COMMENT);
                i = end;
            }
            b'/' if at(src, i + 1) == b'*' => {
                let end = block_comment_end(src, i);
                out[i..end].fill(COMMENT);
                i = end;
            }
            b'/' if !matches!(prev, Prev::Value | Prev::CloseParen | Prev::Type) => {
                match regex(src, out, i) {
                    Some(end) => {
                        i = end;
                        prev = Prev::Value;
                    }
                    None => {
                        out[i] = PUNCT;
                        i += 1;
                        prev = Prev::Op;
                    }
                }
            }
            b'"' | b'\'' => {
                i = string(src, out, i, b);
                prev = if prev == Prev::TypeCtx {
                    Prev::Type
                } else {
                    Prev::Value
                };
            }
            b'`' => {
                let in_type = prev == Prev::TypeCtx;
                let (end, sub) = template(src, out, i + 1, in_type);
                if !in_type {
                    out[i] = STRING;
                }
                i = end;
                if sub {
                    push!(if in_type {
                        TEMPLATE_TYPE_SUB
                    } else {
                        TEMPLATE_SUB
                    });
                    prev = if in_type { Prev::TypeCtx } else { Prev::Op };
                } else {
                    prev = if in_type { Prev::Type } else { Prev::Value };
                }
            }
            b'0'..=b'9' => {
                i = number(src, out, i);
                prev = if prev == Prev::TypeCtx {
                    Prev::Type
                } else {
                    Prev::Value
                };
            }
            b'.' if at(src, i + 1).is_ascii_digit() && prev != Prev::Value => {
                i = number(src, out, i);
                prev = Prev::Value;
            }
            b'#' if is_ident_start(at(src, i + 1)) => {
                // Private names are never highlighted; a member name in a class body.
                i = ident_end(src, i + 1);
                prev = Prev::Value;
            }
            _ if is_ident_start(b) => {
                let end = ident_end(src, i);
                let id = &src[i..end];
                let w = word(id);
                let after = skip_trivia(src, end);
                let nb = at(src, after);
                let nb2 = at(src, after + 1);
                let inner = innermost!();
                // Whether a name here is a member name: object key, class member,
                // interface or type literal member, enum member.
                let member_pos = match inner {
                    OBJECT => matches!(prev, Prev::FieldStart | Prev::Member),
                    CLASS_BODY | IFACE_BODY | TYPE_BRACE => !matches!(
                        prev,
                        Prev::Op | Prev::Dot | Prev::TypeCtx | Prev::TypeQual | Prev::TypeofInType
                    ),
                    ENUM_BODY => !matches!(prev, Prev::Op | Prev::Dot),
                    _ => false,
                };
                let name_follows = matches!(nb, b':' | b'(' | b'<' | b',' | b'}' | b'=' | b';')
                    || (nb == b'?' && matches!(nb2, b':' | b'(' | b'.'))
                    || (nb == b'!' && nb2 == b':');
                let mut class = PLAIN;
                let mut new_prev = Prev::Value;
                // `x is T` / `this is T`: `is` is consumed here and a type follows.
                let is_pred = matches!(prev, Prev::TypeCtx | Prev::TypeQual)
                    && id != b"is"
                    && src[after..].starts_with(b"is")
                    && !is_ident_cont(at(src, after + 2));
                if prev == Prev::Dot {
                    // Member access. `new.target` is a keyword.
                    if nb == b'<' && nb2 != b'<' && nb2 != b'=' {
                        if let Some(e) = generic_end(src, after) {
                            if matches!(at(src, skip_trivia(src, e)), b'(' | b'`') {
                                generic_call = true;
                            }
                        }
                    }
                    class = if in_new || (after_await && generic_call) {
                        // `new ns.Ctor(..)` is not a call.
                        PROPERTY
                    } else if nb == b'('
                        || nb == b'`'
                        || generic_call
                        || (nb == b'?' && nb2 == b'.' && at(src, after + 2) == b'(')
                    {
                        FUNCTION
                    } else if nb == b'='
                        && nb2 != b'='
                        && nb2 != b'>'
                        && is_function_value(src, after + 1)
                    {
                        FUNCTION
                    } else {
                        PROPERTY
                    };
                } else if in_import && !matches!(id, b"as" | b"type" | b"from" | b"typeof") {
                    class = by_convention(id, PLAIN);
                } else if in_import {
                    class = KEYWORD;
                    new_prev = Prev::Op;
                    if id == b"from" {
                        in_import = false;
                    }
                } else if prev == Prev::TypeCtx
                    && matches!(inner, TYPE_PAREN | TYPE_BRACKET)
                    && (nb == b':' || (nb == b'?' && nb2 == b':'))
                    && w != Word::This
                {
                    // Function type parameter or tuple label, even when spelled as a keyword.
                    class = by_convention(id, PLAIN);
                } else if member_pos
                    && (matches!(w, Word::Ident | Word::Predefined) || name_follows)
                {
                    class = match inner {
                        OBJECT => {
                            if nb == b'(' || nb == b'<' {
                                FUNCTION
                            } else if nb == b':' {
                                if is_function_value(src, after + 1) {
                                    FUNCTION
                                } else {
                                    PROPERTY
                                }
                            } else {
                                by_convention(id, PLAIN)
                            }
                        }
                        CLASS_BODY => {
                            let sig = if nb == b'?' { after + 1 } else { after };
                            if matches!(at(src, sig), b'(' | b'<') && has_body(src, sig) {
                                FUNCTION
                            } else {
                                PROPERTY
                            }
                        }
                        // Construct signature.
                        _ if id == b"new" && matches!(nb, b'(' | b'<') => KEYWORD,
                        _ => PROPERTY,
                    };
                    if nb == b'<' {
                        new_prev = Prev::TypeName;
                    }
                } else {
                    match w {
                        Word::Literal => {
                            class = NUMBER;
                            if prev == Prev::TypeCtx {
                                new_prev = Prev::Type;
                            }
                        }
                        Word::This => {
                            class = KEYWORD;
                            if prev == Prev::TypeCtx {
                                new_prev = Prev::Type;
                            }
                        }
                        Word::Strict => {
                            class = KEYWORD;
                            new_prev = Prev::Op;
                            match id {
                                b"import" | b"export" if nb != b'(' && nb != b'.' => {
                                    in_import = import_clause(src, after, id == b"export");
                                }
                                b"new" if prev != Prev::TypeCtx => in_new = true,
                                b"await" => after_await = true,
                                b"function" => new_prev = Prev::Fn,
                                b"class" => {
                                    pending[depth & 63] = CLASS_BODY;
                                    iface_header = true;
                                    new_prev = Prev::DeclKw;
                                }
                                b"enum" => {
                                    pending[depth & 63] = ENUM_BODY;
                                    new_prev = Prev::DeclKw;
                                }
                                b"const" | b"var" => {
                                    if nb == b'e' && src[after..].starts_with(b"enum") {
                                        new_prev = Prev::Op;
                                    } else if prev == Prev::TypeCtx || prev == Prev::Type {
                                        // `as const`, `<const T>`
                                        new_prev = if matches!(inner, GENERIC_PARAMS | GENERIC_FN) {
                                            Prev::TypeCtx
                                        } else {
                                            Prev::Value
                                        };
                                    } else {
                                        decl_depth = depth;
                                        decl_open = true;
                                        new_prev = Prev::Binding;
                                    }
                                }
                                b"else" | b"try" | b"catch" | b"finally" | b"do" => {
                                    new_prev = Prev::Stmt
                                }
                                b"case" => in_case = true,
                                b"default" => {
                                    if nb == b':' {
                                        in_case = true;
                                    }
                                }
                                b"extends" => {
                                    if iface_header && pending[depth & 63] == IFACE_BODY
                                        || matches!(prev, Prev::Type | Prev::TypeCtx)
                                        || matches!(inner, GENERIC | GENERIC_PARAMS | GENERIC_FN)
                                    {
                                        new_prev = Prev::TypeCtx;
                                    }
                                }
                                b"typeof" if prev == Prev::TypeCtx => new_prev = Prev::TypeofInType,
                                b"new" | b"void" if prev == Prev::TypeCtx => {
                                    new_prev = if id == b"void" {
                                        Prev::Type
                                    } else {
                                        Prev::TypeCtx
                                    }
                                }
                                b"in" if inner == INDEX_SIG => new_prev = Prev::TypeCtx,
                                _ => {}
                            }
                        }
                        Word::Contextual => {
                            let terminator = matches!(
                                nb,
                                b':' | b'=' | b',' | b')' | b';' | b'.' | b']' | b'}' | b'?'
                            );
                            let starts_name = is_ident_start(nb)
                                || matches!(nb, b'[' | b'#' | b'"' | b'\'' | b'*' | b'{');
                            let kw = !terminator
                                && match id {
                                    b"as" => matches!(
                                        prev,
                                        Prev::Value
                                            | Prev::CloseParen
                                            | Prev::Type
                                            | Prev::Op
                                            | Prev::Stmt
                                    ),
                                    b"satisfies" => {
                                        matches!(prev, Prev::Value | Prev::CloseParen | Prev::Type)
                                    }
                                    b"async" => starts_name || matches!(nb, b'(' | b'<'),
                                    b"from" => matches!(nb, b'"' | b'\''),
                                    b"of" => prev == Prev::Value && inner == PAREN,
                                    b"target" => false,
                                    b"keyof" => prev == Prev::TypeCtx,
                                    b"readonly" if prev == Prev::TypeCtx => true,
                                    b"implements" => iface_header,
                                    b"type" => {
                                        is_ident_start(nb)
                                            && !matches!(prev, Prev::Binding | Prev::Value)
                                            && matches!(
                                                at(src, skip_trivia(src, ident_end(src, after))),
                                                b'=' | b'<'
                                            )
                                    }
                                    _ => starts_name,
                                };
                            if kw {
                                class = KEYWORD;
                                new_prev = match id {
                                    b"as" | b"satisfies" | b"keyof" | b"implements" => {
                                        Prev::TypeCtx
                                    }
                                    b"let" => {
                                        decl_depth = depth;
                                        decl_open = true;
                                        Prev::Binding
                                    }
                                    b"type" if is_ident_start(nb) => {
                                        type_alias = true;
                                        Prev::DeclKw
                                    }
                                    b"interface" => {
                                        pending[depth & 63] = IFACE_BODY;
                                        iface_header = true;
                                        Prev::DeclKw
                                    }
                                    b"namespace" => Prev::DeclKw,
                                    b"readonly" if prev == Prev::TypeCtx => Prev::TypeCtx,
                                    b"async" if member_pos => Prev::Member,
                                    b"async" | b"from" => Prev::Op,
                                    _ => Prev::Member,
                                };
                            } else {
                                class = by_convention(id, PLAIN);
                            }
                        }
                        Word::Ident | Word::Predefined => {}
                    }
                    if class == PLAIN
                        && matches!(w, Word::Ident | Word::Predefined | Word::Contextual)
                        && !(w == Word::Contextual && new_prev != Prev::Value)
                    {
                        let call = nb == b'('
                            || nb == b'`'
                            || (nb == b'?' && nb2 == b'.' && at(src, after + 2) == b'(')
                            || {
                                if nb == b'<'
                                    && nb2 != b'<'
                                    && nb2 != b'='
                                    && !matches!(
                                        prev,
                                        Prev::Fn | Prev::DeclKw | Prev::TypeCtx | Prev::TypeQual
                                    )
                                {
                                    if let Some(e) = generic_end(src, after) {
                                        // `new Set<T>` needs no arguments.
                                        generic_call = in_new
                                            || matches!(at(src, skip_trivia(src, e)), b'(' | b'`');
                                    }
                                }
                                generic_call
                            };
                        class = match prev {
                            Prev::TypeCtx if nb == b'.' => {
                                new_prev = Prev::TypeQual;
                                by_convention(id, PLAIN)
                            }
                            Prev::TypeCtx | Prev::TypeQual => {
                                if is_pred {
                                    new_prev = Prev::Value;
                                    PLAIN
                                } else if matches!(id, b"infer" | b"unique" | b"asserts" | b"is")
                                    && is_ident_start(nb)
                                {
                                    new_prev = Prev::TypeCtx;
                                    PLAIN
                                } else {
                                    new_prev = Prev::Type;
                                    TYPE
                                }
                            }
                            Prev::TypeofInType => {
                                new_prev = Prev::Type;
                                by_convention(id, PLAIN)
                            }
                            Prev::Fn => {
                                new_prev = Prev::TypeName;
                                // Generators and bodiless signatures are not captured.
                                let generator = src[..i].trim_ascii_end().ends_with(b"*");
                                if !generator && has_body(src, after) {
                                    by_convention(id, FUNCTION)
                                } else {
                                    by_convention(id, PLAIN)
                                }
                            }
                            Prev::DeclKw => {
                                new_prev = Prev::TypeName;
                                if pending[depth & 63] == ENUM_BODY {
                                    by_convention(id, PLAIN)
                                } else if pending[depth & 63] == 0 && !type_alias {
                                    // namespace
                                    by_convention(id, PLAIN)
                                } else {
                                    TYPE
                                }
                            }
                            _ if inner == INDEX_SIG && nb == b':' => PLAIN,
                            _ if inner == INDEX_SIG
                                && nb == b'i'
                                && src[after..].starts_with(b"in ") =>
                            {
                                TYPE
                            }
                            _ if call && after_await && generic_call => by_convention(id, PLAIN),
                            _ if call || id == b"require" => by_convention(id, FUNCTION),
                            Prev::Binding => by_convention(
                                id,
                                if binds_function(src, end) {
                                    FUNCTION
                                } else {
                                    PLAIN
                                },
                            ),
                            _ if nb == b'='
                                && !matches!(nb2, b'=' | b'>')
                                && is_function_value(src, after + 1) =>
                            {
                                by_convention(id, FUNCTION)
                            }
                            _ => by_convention(id, PLAIN),
                        };
                    }
                }
                if class != PLAIN {
                    out[i..end].fill(class);
                }
                if nb != b'.' && nb != b'<' {
                    in_new &= id == b"new";
                    after_await &= id == b"await";
                }
                prev = new_prev;
                i = end;
                if is_pred {
                    prev = Prev::TypeCtx;
                    i = after + 2;
                }
            }
            b'(' => {
                in_new = false;
                after_await = false;
                in_import = false;
                let kind = if prev == Prev::TypeCtx {
                    TYPE_PAREN
                } else {
                    PAREN
                };
                push!(kind);
                out[i] = PUNCT;
                generic_call = false;
                prev = if kind == TYPE_PAREN {
                    Prev::TypeCtx
                } else {
                    Prev::Op
                };
                i += 1;
            }
            b'[' => {
                let inner = innermost!();
                let kind = if matches!(prev, Prev::TypeCtx | Prev::Type) {
                    TYPE_BRACKET
                } else if matches!(inner, CLASS_BODY | IFACE_BODY | TYPE_BRACE)
                    && !matches!(prev, Prev::Op | Prev::Dot)
                {
                    INDEX_SIG
                } else {
                    BRACKET
                };
                push!(kind);
                out[i] = PUNCT;
                prev = if kind == TYPE_BRACKET {
                    Prev::TypeCtx
                } else {
                    Prev::Op
                };
                i += 1;
            }
            b'{' => {
                let waiting = pending[depth & 63];
                let kind = if waiting != 0 && !matches!(prev, Prev::TypeCtx) {
                    pending[depth & 63] = 0;
                    iface_header = false;
                    waiting
                } else if prev == Prev::TypeCtx
                    && !(innermost!() == TYPE_PAREN
                        && matches!(src[..i].trim_ascii_end().last(), Some(b'(' | b',')))
                {
                    TYPE_BRACE
                } else if matches!(prev, Prev::Op | Prev::FieldStart | Prev::Binding)
                    || (prev == Prev::Member && innermost!() != CLASS_BODY)
                {
                    OBJECT
                } else {
                    BLOCK
                };
                push!(kind);
                out[i] = PUNCT;
                prev = match kind {
                    OBJECT | ENUM_BODY => Prev::FieldStart,
                    _ => Prev::Stmt,
                };
                i += 1;
            }
            b')' | b']' | b'}' => {
                // Unclosed generics (a misread `<`) end with their enclosing bracket.
                while matches!(
                    innermost!(),
                    GENERIC | GENERIC_PARAMS | GENERIC_CALL | GENERIC_FN
                ) && depth > 0
                {
                    depth -= 1;
                }
                let closed = innermost!();
                depth = depth.saturating_sub(1);
                if matches!(closed, TEMPLATE_SUB | TEMPLATE_TYPE_SUB) && b == b'}' {
                    let in_type = closed == TEMPLATE_TYPE_SUB;
                    out[i] = PUNCT;
                    let (end, sub) = template(src, out, i + 1, in_type);
                    i = end;
                    if sub {
                        push!(closed);
                        prev = if in_type { Prev::TypeCtx } else { Prev::Op };
                    } else {
                        prev = if in_type { Prev::Type } else { Prev::Value };
                    }
                    continue;
                }
                out[i] = PUNCT;
                i += 1;
                prev = match closed {
                    TYPE_PAREN => Prev::TypeParenClose,
                    TYPE_BRACKET | TYPE_BRACE => Prev::Type,
                    PAREN => Prev::CloseParen,
                    BLOCK | CLASS_BODY | IFACE_BODY | ENUM_BODY => Prev::Stmt,
                    _ => Prev::Value,
                };
                if decl_depth != usize::MAX && depth < decl_depth {
                    decl_depth = usize::MAX;
                    decl_open = false;
                }
            }
            b'<' => {
                let inner_type = matches!(prev, Prev::TypeCtx | Prev::Type);
                if at(src, i + 1) == b'<' || (at(src, i + 1) == b'=' && !inner_type) {
                    let len = if at(src, i + 2) == b'=' { 3 } else { 2 };
                    out[i..i + len].fill(PUNCT);
                    i += len;
                    prev = Prev::Op;
                    continue;
                }
                out[i] = PUNCT;
                i += 1;
                if generic_call {
                    generic_call = false;
                    push!(GENERIC_CALL);
                    prev = Prev::TypeCtx;
                } else if inner_type {
                    push!(if prev == Prev::Type {
                        GENERIC
                    } else {
                        GENERIC_FN
                    });
                    prev = Prev::TypeCtx;
                } else if matches!(prev, Prev::TypeName | Prev::Op | Prev::Stmt | Prev::Binding) {
                    push!(GENERIC_PARAMS);
                    prev = Prev::TypeCtx;
                } else {
                    prev = Prev::Op;
                }
            }
            b'>' => {
                let inner = innermost!();
                if matches!(inner, GENERIC | GENERIC_PARAMS | GENERIC_CALL | GENERIC_FN) {
                    out[i] = PUNCT;
                    i += 1;
                    depth -= 1;
                    prev = match inner {
                        GENERIC => Prev::Type,
                        GENERIC_FN => Prev::TypeCtx,
                        GENERIC_CALL => Prev::Value,
                        _ => Prev::Op,
                    };
                    continue;
                }
                let mut j = i + 1;
                while at(src, j) == b'>' {
                    j += 1;
                }
                if at(src, j) == b'=' {
                    j += 1;
                }
                out[i..j].fill(PUNCT);
                i = j;
                prev = Prev::Op;
            }
            b'=' => {
                if at(src, i + 1) == b'>' {
                    out[i] = PUNCT;
                    out[i + 1] = PUNCT;
                    i += 2;
                    prev = if prev == Prev::TypeParenClose {
                        Prev::TypeCtx
                    } else {
                        Prev::Stmt
                    };
                    continue;
                }
                let mut j = i + 1;
                while at(src, j) == b'=' {
                    j += 1;
                }
                out[i..j].fill(PUNCT);
                let inner = innermost!();
                // `type A = T;` and type parameter defaults `<T = U>`.
                prev = if j > i + 1 {
                    Prev::Op
                } else if matches!(inner, GENERIC | GENERIC_PARAMS | GENERIC_FN) {
                    Prev::TypeCtx
                } else if std::mem::take(&mut type_alias) {
                    Prev::TypeCtx
                } else {
                    Prev::Op
                };
                if depth == decl_depth {
                    decl_open = false;
                }
                i = j;
            }
            b':' => {
                out[i] = PUNCT;
                i += 1;
                let d = depth & 63;
                let inner = innermost!();
                if std::mem::take(&mut optional) {
                    prev = Prev::TypeCtx;
                } else if tern[d] > 0 {
                    tern[d] -= 1;
                    let ty = tern_type[d] & 1 == 1;
                    tern_type[d] >>= 1;
                    prev = if ty { Prev::TypeCtx } else { Prev::Op };
                } else if std::mem::take(&mut in_case) {
                    prev = Prev::Stmt;
                } else if prev == Prev::CloseParen {
                    prev = Prev::TypeCtx;
                } else if inner == OBJECT || inner == ENUM_BODY {
                    prev = Prev::Op;
                } else if matches!(
                    inner,
                    PAREN
                        | TYPE_PAREN
                        | TYPE_BRACKET
                        | INDEX_SIG
                        | CLASS_BODY
                        | IFACE_BODY
                        | TYPE_BRACE
                        | GENERIC
                        | GENERIC_PARAMS
                        | GENERIC_CALL
                        | GENERIC_FN
                ) || prev == Prev::CloseParen
                    || (decl_open && depth == decl_depth)
                {
                    prev = Prev::TypeCtx;
                } else {
                    prev = Prev::Stmt;
                }
            }
            b'?' => {
                let n1 = at(src, i + 1);
                if n1 == b'.' && !at(src, i + 2).is_ascii_digit() {
                    out[i] = PUNCT;
                    out[i + 1] = PUNCT;
                    i += 2;
                    prev = Prev::Dot;
                } else if n1 == b'?' {
                    let len = if at(src, i + 2) == b'=' { 3 } else { 2 };
                    out[i..i + len].fill(PUNCT);
                    i += len;
                    prev = Prev::Op;
                } else {
                    out[i] = PUNCT;
                    i += 1;
                    let next = at(src, skip_trivia(src, i));
                    if next == b':' {
                        optional = true;
                    } else if prev == Prev::Value
                        && (matches!(next, b',' | b')' | b';' | b'=')
                            || (next == b'('
                                && matches!(innermost!(), CLASS_BODY | IFACE_BODY | TYPE_BRACE)))
                    {
                        // Optional parameter or member.
                    } else {
                        let d = depth & 63;
                        let ty = matches!(prev, Prev::Type | Prev::TypeCtx);
                        tern[d] = tern[d].saturating_add(1);
                        tern_type[d] = (tern_type[d] << 1) | ty as u32;
                        prev = if ty { Prev::TypeCtx } else { Prev::Op };
                    }
                }
            }
            b'!' => {
                if at(src, i + 1) == b':' {
                    optional = true;
                    out[i] = PUNCT;
                    i += 1;
                } else {
                    let mut j = i + 1;
                    while at(src, j) == b'=' {
                        j += 1;
                    }
                    out[i..j].fill(PUNCT);
                    // Postfix non-null assertion keeps the value.
                    if !(j == i + 1 && matches!(prev, Prev::Value | Prev::CloseParen)) {
                        prev = Prev::Op;
                    }
                    i = j;
                }
            }
            b',' => {
                out[i] = PUNCT;
                i += 1;
                prev = match innermost!() {
                    OBJECT | ENUM_BODY => Prev::FieldStart,
                    GENERIC | GENERIC_PARAMS | GENERIC_CALL | GENERIC_FN | TYPE_BRACKET
                    | TYPE_PAREN => Prev::TypeCtx,
                    IFACE_BODY | TYPE_BRACE => Prev::Stmt,
                    _ if decl_depth == depth => {
                        decl_open = true;
                        Prev::Binding
                    }
                    _ => Prev::Op,
                };
            }
            b';' => {
                out[i] = PUNCT;
                i += 1;
                prev = Prev::Stmt;
                if depth == decl_depth || decl_depth == usize::MAX || depth <= decl_depth {
                    decl_depth = usize::MAX;
                    decl_open = false;
                }
                type_alias = false;
                in_case = false;
                optional = false;
                in_new = false;
                in_import = false;
                tern[depth & 63] = 0;
            }
            b'.' => {
                if at(src, i + 1) == b'.' && at(src, i + 2) == b'.' {
                    out[i..i + 3].fill(PUNCT);
                    i += 3;
                    // Spread or rest keeps a field or binding position.
                    if !matches!(prev, Prev::FieldStart | Prev::Binding | Prev::TypeCtx) {
                        prev = Prev::Op;
                    }
                } else {
                    out[i] = PUNCT;
                    i += 1;
                    prev = if prev == Prev::TypeQual {
                        Prev::TypeCtx
                    } else {
                        Prev::Dot
                    };
                }
            }
            b'|' | b'&' => {
                let mut j = i + 1;
                while matches!(at(src, j), b'|' | b'&' | b'=') {
                    j += 1;
                }
                out[i..j].fill(PUNCT);
                prev = if j == i + 1 && matches!(prev, Prev::Type | Prev::TypeCtx) {
                    Prev::TypeCtx
                } else {
                    Prev::Op
                };
                i = j;
            }
            b'+' | b'-' => {
                let mut j = i + 1;
                if at(src, j) == b {
                    // `++` / `--` keep whether a value precedes.
                    out[i..j + 1].fill(PUNCT);
                    i = j + 1;
                    continue;
                }
                if at(src, j) == b'=' {
                    j += 1;
                }
                out[i..j].fill(PUNCT);
                i = j;
                // `-readonly` / `+?` in mapped types keep the member position.
                if !matches!(prev, Prev::Stmt | Prev::TypeCtx) || innermost!() != TYPE_BRACE {
                    prev = Prev::Op;
                }
            }
            b'*' => {
                let mut j = i + 1;
                while matches!(at(src, j), b'*' | b'=') {
                    j += 1;
                }
                out[i..j].fill(PUNCT);
                i = j;
                // Generator `function*` and `*method()` keep the name position.
                if !matches!(
                    prev,
                    Prev::Fn | Prev::Member | Prev::FieldStart | Prev::Stmt
                ) {
                    prev = Prev::Op;
                } else if prev == Prev::Stmt {
                    prev = Prev::Member;
                }
            }
            b'@' => {
                out[i] = PUNCT;
                i += 1;
                prev = Prev::Op;
            }
            b'/' | b'%' | b'^' | b'~' => {
                let mut j = i + 1;
                if at(src, j) == b'=' {
                    j += 1;
                }
                out[i..j].fill(PUNCT);
                i = j;
                prev = Prev::Op;
            }
            _ => {
                i += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::lex::{lex, Class, Lang};

    /// Non-plain, non-punctuation runs as `text:class`.
    fn spans(src: &str) -> Vec<String> {
        let classes = lex(Lang::TypeScript, src.as_bytes());
        let mut out = Vec::new();
        let mut i = 0;
        while i < classes.len() {
            let c = classes[i];
            let start = i;
            while i < classes.len() && classes[i] == c && src.as_bytes()[i] != b' ' {
                i += 1;
            }
            if i == start {
                i += 1;
                continue;
            }
            let class = Class::from_u8(c);
            if !matches!(class, Class::Plain | Class::Punct) {
                out.push(format!("{}:{}", &src[start..i], class.name()));
            }
        }
        out
    }

    #[test]
    fn contextual_identifiers() {
        assert_eq!(
            spans("const f = (x: Opt<string>): number => x.y.z(/re/g, a / b);"),
            [
                "const:keyword",
                "f:function",
                "Opt:type",
                "string:type",
                "number:type",
                "y:property",
                "z:function",
                "/re/g:string",
            ]
        );
    }
}
