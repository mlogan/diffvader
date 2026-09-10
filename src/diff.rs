//! Line diff, side-by-side row model, whitespace handling and intra-line diffs.

use std::borrow::Cow;
use std::ops::Range;

use imara_diff::{Algorithm, Diff};
use imara_diff::{InternedInput, Interner, Token};

use crate::text::FileData;
use crate::trace;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WhitespaceMode {
    /// Every byte counts.
    Exact,
    /// Ignore whitespace at end of line (`git diff --ignore-space-at-eol`).
    IgnoreEol,
    /// Ignore changes in the amount of whitespace (`git diff -b`).
    IgnoreChange,
    /// Ignore all whitespace (`git diff -w`).
    IgnoreAll,
}

impl WhitespaceMode {
    pub fn next(self) -> WhitespaceMode {
        match self {
            WhitespaceMode::Exact => WhitespaceMode::IgnoreEol,
            WhitespaceMode::IgnoreEol => WhitespaceMode::IgnoreChange,
            WhitespaceMode::IgnoreChange => WhitespaceMode::IgnoreAll,
            WhitespaceMode::IgnoreAll => WhitespaceMode::Exact,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            WhitespaceMode::Exact => "exact",
            WhitespaceMode::IgnoreEol => "ignore-eol",
            WhitespaceMode::IgnoreChange => "ignore-space-change",
            WhitespaceMode::IgnoreAll => "ignore-all-space",
        }
    }
}

#[inline]
pub fn is_ws(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\r' | 0x0b | 0x0c)
}

/// Canonical form of a line under a whitespace mode. Lines with equal normal forms are
/// treated as equal by the line diff.
pub fn normalize(line: &[u8], mode: WhitespaceMode) -> Cow<'_, [u8]> {
    match mode {
        WhitespaceMode::Exact => Cow::Borrowed(line),
        WhitespaceMode::IgnoreEol => Cow::Borrowed(trim_end(line)),
        WhitespaceMode::IgnoreChange => {
            let line = trim_end(line);
            if !line.windows(2).any(|w| is_ws(w[0]) && is_ws(w[1])) && !line.contains(&b'\t') {
                return Cow::Borrowed(line);
            }
            let mut out = Vec::with_capacity(line.len());
            let mut in_ws = false;
            for &b in line {
                if is_ws(b) {
                    if !in_ws {
                        out.push(b' ');
                    }
                    in_ws = true;
                } else {
                    out.push(b);
                    in_ws = false;
                }
            }
            Cow::Owned(out)
        }
        WhitespaceMode::IgnoreAll => {
            if !line.iter().any(|&b| is_ws(b)) {
                return Cow::Borrowed(line);
            }
            Cow::Owned(line.iter().copied().filter(|&b| !is_ws(b)).collect())
        }
    }
}

fn trim_end(line: &[u8]) -> &[u8] {
    let mut end = line.len();
    while end > 0 && is_ws(line[end - 1]) {
        end -= 1;
    }
    &line[..end]
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowKind {
    Equal,
    Delete,
    Insert,
    /// A deleted line paired with an inserted line; gets intra-line highlighting.
    Modify,
}

pub const NONE: u32 = u32::MAX;

#[derive(Clone, Copy, Debug)]
pub struct Row {
    /// Line index into the left file, or [`NONE`] for a filler row.
    pub left: u32,
    pub right: u32,
    pub kind: RowKind,
    /// The two lines differ only in whitespace. For `Modify` rows (exact mode) this selects
    /// a distinct color; for `Equal` rows (ignore modes) it flags a hidden difference.
    pub ws_only: bool,
}

#[derive(Clone, Debug)]
pub struct Hunk {
    pub rows: Range<u32>,
}

pub struct DiffResult {
    pub mode: WhitespaceMode,
    pub rows: Vec<Row>,
    pub hunks: Vec<Hunk>,
    pub removed: u32,
    pub added: u32,
}

impl DiffResult {
    /// Index of the hunk containing `row`, or of the previous hunk if `row` is between hunks.
    /// Returns `None` if `row` precedes every hunk.
    pub fn hunk_at_or_before(&self, row: u32) -> Option<usize> {
        match self.hunks.binary_search_by(|h| h.rows.start.cmp(&row)) {
            Ok(i) => Some(i),
            Err(0) => None,
            Err(i) => Some(i - 1),
        }
    }
}

pub fn diff_files(a: &FileData, b: &FileData, mode: WhitespaceMode) -> DiffResult {
    let _s = trace::span_arg("diff-lines", (a.line_count() + b.line_count()) as u64);
    let input = {
        let _s = trace::span("diff-intern");
        let mut interner = Interner::new(a.line_count() + b.line_count());
        let before: Vec<Token> = a
            .lines()
            .map(|l| interner.intern(normalize(l, mode)))
            .collect();
        let after: Vec<Token> = b
            .lines()
            .map(|l| interner.intern(normalize(l, mode)))
            .collect();
        InternedInput {
            before,
            after,
            interner,
        }
    };
    let diff = {
        let _s = trace::span("diff-histogram");
        let mut diff = Diff::compute(Algorithm::Histogram, &input);
        diff.postprocess_lines(&input);
        diff
    };

    let _s = trace::span("diff-rows");
    let mut rows = Vec::with_capacity(input.before.len().max(input.after.len()) + 64);
    let mut hunks = Vec::new();
    let (mut la, mut lb) = (0u32, 0u32);
    let mut removed = 0;
    let mut added = 0;
    let exact = mode == WhitespaceMode::Exact;
    let push_equal = |rows: &mut Vec<Row>, la: &mut u32, lb: &mut u32, until_a: u32| {
        while *la < until_a {
            let ws_only = !exact && a.line(*la as usize) != b.line(*lb as usize);
            rows.push(Row {
                left: *la,
                right: *lb,
                kind: RowKind::Equal,
                ws_only,
            });
            *la += 1;
            *lb += 1;
        }
    };
    for h in diff.hunks() {
        push_equal(&mut rows, &mut la, &mut lb, h.before.start);
        let start = rows.len() as u32;
        let n = h.before.len() as u32;
        let m = h.after.len() as u32;
        removed += n;
        added += m;
        let paired = n.min(m);
        for i in 0..paired {
            let (l, r) = (h.before.start + i, h.after.start + i);
            let ws_only = normalize(a.line(l as usize), WhitespaceMode::IgnoreAll)
                == normalize(b.line(r as usize), WhitespaceMode::IgnoreAll);
            rows.push(Row {
                left: l,
                right: r,
                kind: RowKind::Modify,
                ws_only,
            });
        }
        for l in h.before.start + paired..h.before.end {
            rows.push(Row {
                left: l,
                right: NONE,
                kind: RowKind::Delete,
                ws_only: false,
            });
        }
        for r in h.after.start + paired..h.after.end {
            rows.push(Row {
                left: NONE,
                right: r,
                kind: RowKind::Insert,
                ws_only: false,
            });
        }
        la = h.before.end;
        lb = h.after.end;
        hunks.push(Hunk {
            rows: start..rows.len() as u32,
        });
    }
    push_equal(&mut rows, &mut la, &mut lb, input.before.len() as u32);
    debug_assert_eq!(lb as usize, input.after.len());
    DiffResult {
        mode,
        rows,
        hunks,
        removed,
        added,
    }
}

/// Byte ranges (per side) that differ within a `Modify` row.
#[derive(Clone, Debug, Default)]
pub struct IntraDiff {
    pub left: Vec<Range<u32>>,
    pub right: Vec<Range<u32>>,
}

/// Lines with more tokens than this get whole-line highlighting instead of a token diff.
const MAX_INTRA_TOKENS: usize = 4096;

fn tokenize(line: &[u8], mode: WhitespaceMode, out: &mut Vec<Range<u32>>) {
    let line = if mode == WhitespaceMode::Exact {
        line
    } else {
        trim_end(line)
    };
    let mut i = 0;
    while i < line.len() {
        let b = line[i];
        let start = i;
        let class = |c: u8| {
            if is_ws(c) {
                0
            } else if c.is_ascii_alphanumeric() || c == b'_' || c >= 0x80 {
                1
            } else {
                2
            }
        };
        let k = class(b);
        i += 1;
        if k != 2 {
            while i < line.len() && class(line[i]) == k {
                i += 1;
            }
        }
        if k == 0 && mode == WhitespaceMode::IgnoreAll {
            continue;
        }
        out.push(start as u32..i as u32);
    }
}

/// Word-level diff of two lines. Returns `None` when the lines are too different for
/// highlighting to be useful (or too long to diff cheaply); callers then color whole lines.
pub fn intra_diff(a: &[u8], b: &[u8], mode: WhitespaceMode) -> Option<IntraDiff> {
    let mut ta = Vec::new();
    let mut tb = Vec::new();
    tokenize(a, mode, &mut ta);
    tokenize(b, mode, &mut tb);
    if ta.len() > MAX_INTRA_TOKENS || tb.len() > MAX_INTRA_TOKENS {
        return None;
    }
    fn key<'a>(line: &'a [u8], r: &Range<u32>, mode: WhitespaceMode) -> &'a [u8] {
        let s = &line[r.start as usize..r.end as usize];
        if mode == WhitespaceMode::IgnoreChange && is_ws(s[0]) {
            b" "
        } else {
            s
        }
    }
    let mut interner = Interner::new(ta.len() + tb.len());
    let before: Vec<Token> = ta
        .iter()
        .map(|r| interner.intern(key(a, r, mode)))
        .collect();
    let after: Vec<Token> = tb
        .iter()
        .map(|r| interner.intern(key(b, r, mode)))
        .collect();
    let input = InternedInput {
        before,
        after,
        interner,
    };
    let mut diff = Diff::compute(Algorithm::Myers, &input);
    diff.postprocess_no_heuristic(&input);

    let mut out = IntraDiff::default();
    let mut changed_a = 0usize;
    let mut changed_b = 0usize;
    for h in diff.hunks() {
        changed_a += h.before.len();
        changed_b += h.after.len();
        push_range(&mut out.left, &ta, h.before);
        push_range(&mut out.right, &tb, h.after);
    }
    // Mostly-rewritten lines read better as whole-line changes than as confetti.
    let noisy = |changed: usize, total: usize| total > 0 && changed * 4 > total * 3;
    if noisy(changed_a, ta.len()) && noisy(changed_b, tb.len()) {
        return None;
    }
    Some(out)
}

fn push_range(out: &mut Vec<Range<u32>>, tokens: &[Range<u32>], r: Range<u32>) {
    if r.is_empty() {
        return;
    }
    let start = tokens[r.start as usize].start;
    let end = tokens[r.end as usize - 1].end;
    match out.last_mut() {
        Some(last) if last.end == start => last.end = end,
        _ => out.push(start..end),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::Bytes;

    fn fd(s: &str) -> FileData {
        FileData::from_bytes(Bytes::Owned(s.as_bytes().to_vec()))
    }

    fn kinds(d: &DiffResult) -> Vec<RowKind> {
        d.rows.iter().map(|r| r.kind).collect()
    }

    #[test]
    fn normalize_modes() {
        assert_eq!(&*normalize(b"a  b \t", WhitespaceMode::Exact), b"a  b \t");
        assert_eq!(&*normalize(b"a  b \t", WhitespaceMode::IgnoreEol), b"a  b");
        assert_eq!(
            &*normalize(b"a  b \t", WhitespaceMode::IgnoreChange),
            b"a b"
        );
        assert_eq!(
            &*normalize(b"\ta  b \t", WhitespaceMode::IgnoreChange),
            b" a b"
        );
        assert_eq!(&*normalize(b"a  b \t", WhitespaceMode::IgnoreAll), b"ab");
    }

    #[test]
    fn basic_rows() {
        let a = fd("a\nb\nc\nd\n");
        let b = fd("a\nB\nc\ne\nf\n");
        let d = diff_files(&a, &b, WhitespaceMode::Exact);
        use RowKind::*;
        assert_eq!(kinds(&d), vec![Equal, Modify, Equal, Modify, Insert]);
        assert_eq!(d.hunks.len(), 2);
        assert_eq!(d.hunks[0].rows, 1..2);
        assert_eq!(d.hunks[1].rows, 3..5);
        assert_eq!(d.rows[4].left, NONE);
        assert_eq!(d.rows[4].right, 4);
        assert_eq!((d.removed, d.added), (2, 3));
    }

    #[test]
    fn whitespace_only_change() {
        let a = fd("x = 1\ny\n");
        let b = fd("x  =  1\ny\n");
        let d = diff_files(&a, &b, WhitespaceMode::Exact);
        assert_eq!(d.rows[0].kind, RowKind::Modify);
        assert!(d.rows[0].ws_only);

        let d = diff_files(&a, &b, WhitespaceMode::IgnoreChange);
        assert_eq!(d.rows[0].kind, RowKind::Equal);
        assert!(d.rows[0].ws_only);
        assert!(!d.rows[1].ws_only);
        assert!(d.hunks.is_empty());
    }

    #[test]
    fn empty_files() {
        let a = fd("");
        let b = fd("x\n");
        let d = diff_files(&a, &b, WhitespaceMode::Exact);
        assert_eq!(kinds(&d), vec![RowKind::Insert]);
        let d = diff_files(&a, &a, WhitespaceMode::Exact);
        assert!(d.rows.is_empty());
    }

    #[test]
    fn intra() {
        let d = intra_diff(
            b"let foo = bar(1);",
            b"let foo = baz(1, 2);",
            WhitespaceMode::Exact,
        )
        .unwrap();
        assert_eq!(d.left, vec![10..13]);
        assert_eq!(d.right, vec![10..13, 15..18]);

        assert!(intra_diff(
            b"completely",
            b"different words here",
            WhitespaceMode::Exact
        )
        .is_none());

        let d = intra_diff(b"a b", b"a  b", WhitespaceMode::Exact).unwrap();
        assert_eq!(d.left, vec![1..2]);
        assert_eq!(d.right, vec![1..3]);
        let d = intra_diff(b"a b", b"a  b", WhitespaceMode::IgnoreChange).unwrap();
        assert!(d.left.is_empty() && d.right.is_empty());
    }

    #[test]
    fn hunk_lookup() {
        let a = fd("a\nb\nc\nd\ne\n");
        let b = fd("a\nB\nc\nd\nE\n");
        let d = diff_files(&a, &b, WhitespaceMode::Exact);
        assert_eq!(d.hunk_at_or_before(0), None);
        assert_eq!(d.hunk_at_or_before(1), Some(0));
        assert_eq!(d.hunk_at_or_before(3), Some(0));
        assert_eq!(d.hunk_at_or_before(4), Some(1));
    }
}
