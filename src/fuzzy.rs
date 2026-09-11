//! Fuzzy path matching in the style of editors' quick-open: every query character must
//! appear in order; matches at word/path boundaries, consecutive runs and the file name
//! score higher than scattered matches deep in a directory.

const MATCH: i32 = 16;
const CONSECUTIVE: i32 = 8;
const GAP_START: i32 = 3;
const GAP_EXTEND: i32 = 1;
const BONUS_PATH_SEP: i32 = 10;
const BONUS_WORD_SEP: i32 = 8;
const BONUS_CAMEL: i32 = 6;
const BONUS_BASENAME: i32 = 4;
const BONUS_FIRST: i32 = 10;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Match {
    pub score: i32,
    /// Char indices into the candidate.
    pub positions: Vec<u32>,
}

fn is_sep(c: char) -> bool {
    matches!(c, '_' | '-' | '.' | ' ' | ':')
}

/// Returns `None` when the query is not a subsequence of the candidate (case-insensitive).
/// An empty query matches everything with score 0.
pub fn fuzzy_match(query: &str, candidate: &str) -> Option<Match> {
    let q: Vec<char> = query
        .chars()
        .filter(|c| !c.is_whitespace())
        .map(|c| c.to_ascii_lowercase())
        .collect();
    if q.is_empty() {
        return Some(Match {
            score: 0,
            positions: Vec::new(),
        });
    }
    let raw: Vec<char> = candidate.chars().collect();
    let c: Vec<char> = raw.iter().map(|c| c.to_ascii_lowercase()).collect();
    let (m, n) = (q.len(), c.len());
    if m > n {
        return None;
    }
    let base_start = raw.iter().rposition(|&ch| ch == '/').map_or(0, |i| i + 1);
    let bonus = |j: usize| -> i32 {
        let mut b = 0;
        if j == 0 {
            b += BONUS_WORD_SEP;
        } else {
            let prev = raw[j - 1];
            if prev == '/' {
                b += BONUS_PATH_SEP;
            } else if is_sep(prev) {
                b += BONUS_WORD_SEP;
            } else if prev.is_lowercase() && raw[j].is_uppercase() {
                b += BONUS_CAMEL;
            }
        }
        if j >= base_start {
            b += BONUS_BASENAME;
        }
        b
    };

    const NEG: i32 = i32::MIN / 4;
    // d[i][j]: best score with q[i] matched at c[j]; h[i][j]: best score for q[..=i] with the
    // last match at or before j, decayed by the gap since then.
    let mut d = vec![NEG; m * n];
    let mut h = vec![NEG; m * n];
    for i in 0..m {
        let mut running = NEG;
        for j in 0..n {
            let mut best = NEG;
            if q[i] == c[j] {
                if i == 0 {
                    best = MATCH + bonus(j) + BONUS_FIRST - (j.min(20) as i32);
                } else if j > 0 {
                    let consec = d[(i - 1) * n + j - 1];
                    if consec > NEG {
                        best = best.max(consec + CONSECUTIVE);
                    }
                    if j >= 2 {
                        let gapped = h[(i - 1) * n + j - 2];
                        if gapped > NEG {
                            best = best.max(gapped - GAP_START);
                        }
                    }
                    if best > NEG {
                        best += MATCH + bonus(j);
                    }
                }
            }
            d[i * n + j] = best;
            running = if running > NEG {
                running - GAP_EXTEND
            } else {
                NEG
            };
            running = running.max(best);
            h[i * n + j] = running;
        }
    }
    let last = m - 1;
    let (mut j, score) = (0..n)
        .map(|j| (j, d[last * n + j]))
        .max_by_key(|&(j, s)| (s, std::cmp::Reverse(j)))?;
    if score <= NEG {
        return None;
    }
    // Backtrack to recover positions.
    let mut positions = vec![0u32; m];
    for i in (0..m).rev() {
        positions[i] = j as u32;
        if i == 0 {
            break;
        }
        let here = d[i * n + j] - MATCH - bonus(j);
        let consec = d[(i - 1) * n + j - 1];
        if consec > NEG && consec + CONSECUTIVE == here {
            j -= 1;
        } else {
            // Find the k <= j-2 whose decayed score produced this match.
            let mut k = j - 2;
            loop {
                let cand = d[(i - 1) * n + k];
                if cand > NEG && cand - GAP_EXTEND * (j - 2 - k) as i32 - GAP_START == here {
                    break;
                }
                if k == 0 {
                    break;
                }
                k -= 1;
            }
            j = k;
        }
    }
    Some(Match { score, positions })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn score(q: &str, c: &str) -> i32 {
        fuzzy_match(q, c).map(|m| m.score).unwrap_or(i32::MIN)
    }

    #[test]
    fn subsequence_required() {
        assert!(fuzzy_match("abc", "a/b/c.rs").is_some());
        assert!(fuzzy_match("acb", "a/b/c.rs").is_none());
        assert!(fuzzy_match("", "anything").is_some());
        assert!(fuzzy_match("toolong", "short").is_none());
    }

    #[test]
    fn positions_are_correct() {
        let m = fuzzy_match("mdrs", "src/mod.rs").unwrap();
        let cand: Vec<char> = "src/mod.rs".chars().collect();
        let got: String = m.positions.iter().map(|&p| cand[p as usize]).collect();
        assert_eq!(got, "mdrs");
        assert_eq!(m.positions, vec![4, 6, 8, 9]);
    }

    #[test]
    fn ranking() {
        // Basename match beats a match spread through directories.
        assert!(score("app", "src/app.rs") > score("app", "a/p/p/other.rs"));
        // Word-boundary matches beat mid-word matches.
        assert!(score("er", "exec_runner.rs") > score("er", "lexer.rs"));
        // Consecutive beats scattered.
        assert!(score("sched", "scheduler.rs") > score("sched", "s_c_h_e_d.rs"));
        // Case-insensitive.
        assert!(fuzzy_match("MAIN", "src/main.rs").is_some());
    }
}
