//! AI explanations of single changes: find an agent CLI on PATH, build a prompt around one
//! hunk, run the agent on a worker thread and hand the text back to the UI.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::diff::{DiffResult, RowKind, NONE};
use crate::files::{FileEntry, Source, Status};
use crate::text::FileData;

/// Identifies a change across re-diffs: the (left, right) line numbers of its first row.
pub type HunkKey = (u32, u32);

pub fn hunk_key(diff: &DiffResult, idx: usize) -> HunkKey {
    let r = diff.rows[diff.hunks[idx].rows.start as usize];
    (r.left, r.right)
}

#[derive(Clone, Debug, PartialEq)]
pub enum NoteState {
    Pending,
    Done,
    Failed(String),
}

/// One explanation, or the request for one. `text` keeps the previous pass while an
/// expansion is pending so the panel has something to show.
#[derive(Clone, Debug)]
pub struct Note {
    pub text: String,
    /// 0 for the first pass; each expansion adds one.
    pub level: u32,
    pub state: NoteState,
    pub agent: String,
}

pub type Notes = HashMap<HunkKey, Note>;

// ---- agents ---------------------------------------------------------------------------

/// A command line that answers a prompt on stdout. `{prompt}` in an argument is replaced by
/// the prompt; without a placeholder the prompt is appended as the last argument.
#[derive(Clone, Debug, PartialEq)]
pub struct Agent {
    pub name: String,
    argv: Vec<String>,
    /// Extra arguments selecting the agent's fastest model, added on first passes.
    fast: Vec<String>,
    pub fast_label: &'static str,
}

struct Known {
    name: &'static str,
    argv: &'static [&'static str],
    fast: &'static [&'static str],
    fast_label: &'static str,
}

/// Probed in this order. Each runs non-interactively, may read the repository, and prints
/// only the answer on stdout.
const KNOWN: &[Known] = &[
    Known {
        name: "claude",
        argv: &[
            "claude",
            "-p",
            "{prompt}",
            "--output-format",
            "text",
            "--no-session-persistence",
            "--allowedTools",
            "Read",
            "Grep",
            "Glob",
            "Bash(git:*)",
        ],
        fast: &["--model", "haiku"],
        fast_label: "haiku",
    },
    Known {
        name: "codex",
        argv: &["codex", "exec", "--sandbox", "read-only", "{prompt}"],
        fast: &[],
        fast_label: "",
    },
    Known {
        name: "gemini",
        argv: &["gemini", "-p", "{prompt}"],
        fast: &["-m", "gemini-2.5-flash"],
        fast_label: "flash",
    },
];

impl Agent {
    fn known(k: &Known) -> Agent {
        Agent {
            name: k.name.to_string(),
            argv: k.argv.iter().map(|s| s.to_string()).collect(),
            fast: k.fast.iter().map(|s| s.to_string()).collect(),
            fast_label: k.fast_label,
        }
    }

    /// `spec` is a known agent name or a full command line (whitespace separated).
    fn custom(spec: &str) -> Result<Agent, String> {
        let argv: Vec<String> = spec.split_whitespace().map(String::from).collect();
        if argv.is_empty() {
            return Err("empty agent command".into());
        }
        Ok(Agent {
            name: Path::new(&argv[0])
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| argv[0].clone()),
            argv,
            fast: Vec::new(),
            fast_label: "",
        })
    }

    /// Human label for a pass: "claude (haiku)" for a fast first pass, else the name.
    pub fn label(&self, fast: bool) -> String {
        if fast && !self.fast_label.is_empty() {
            format!("{} ({})", self.name, self.fast_label)
        } else {
            self.name.clone()
        }
    }

    pub fn args(&self, prompt: &str, fast: bool) -> Vec<String> {
        let mut args = Vec::with_capacity(self.argv.len() + 3);
        let mut placed = false;
        for a in &self.argv[1..] {
            if a.contains("{prompt}") {
                args.push(a.replace("{prompt}", prompt));
                placed = true;
            } else {
                args.push(a.clone());
            }
        }
        if fast {
            args.extend(self.fast.iter().cloned());
        }
        if !placed {
            args.push(prompt.to_string());
        }
        args
    }

    fn program(&self) -> &str {
        &self.argv[0]
    }
}

/// Picks the agent: `preferred` (a known name or a command line) or the first known agent
/// on PATH. Some launchers (git aliases from a GUI, Finder) come with a short PATH, so the
/// usual install locations are probed too.
pub fn find_agent(preferred: Option<&str>) -> Result<Agent, String> {
    if let Some(spec) = preferred.map(str::trim).filter(|s| !s.is_empty()) {
        if let Some(k) = KNOWN.iter().find(|k| k.name == spec) {
            return if find_program(k.name).is_some() {
                Ok(Agent::known(k))
            } else {
                Err(format!("{spec} is not on PATH"))
            };
        }
        return Agent::custom(spec);
    }
    for k in KNOWN {
        if find_program(k.name).is_some() {
            return Ok(Agent::known(k));
        }
    }
    Err("no AI agent found (claude, codex, gemini); use --agent or DIFFVADER_AGENT".into())
}

fn find_program(name: &str) -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        dirs.push(home.join(".local/bin"));
        dirs.push(home.join(".claude/local"));
        dirs.push(home.join(".cargo/bin"));
    }
    dirs.push(PathBuf::from("/opt/homebrew/bin"));
    dirs.push(PathBuf::from("/usr/local/bin"));
    dirs.into_iter()
        .map(|d| d.join(name))
        .find(|p| is_executable(p))
}

fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// The repository containing `dir`, or `dir` itself outside a repository.
pub fn repo_root(dir: &Path) -> PathBuf {
    Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(dir)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| PathBuf::from(String::from_utf8_lossy(&o.stdout).trim_end()))
        .filter(|p| p.is_dir())
        .unwrap_or_else(|| dir.to_path_buf())
}

// ---- prompt -----------------------------------------------------------------------------

/// Rows of context around the hunk in the excerpt.
const CONTEXT_ROWS: usize = 6;
/// Larger hunks are excerpted as their head and tail with an omission marker between.
const MAX_HUNK_ROWS: usize = 240;
const MAX_LINE_BYTES: usize = 300;

/// A unified-diff style rendering of one hunk with context.
pub struct Excerpt {
    /// 1-based first line and line count on each side, as in a `@@` header.
    pub old: (u32, u32),
    pub new: (u32, u32),
    pub text: String,
}

pub fn excerpt(left: &FileData, right: &FileData, diff: &DiffResult, idx: usize) -> Excerpt {
    let h = &diff.hunks[idx];
    let rows = &diff.rows;
    let (start, end) = (h.rows.start as usize, h.rows.end as usize);
    let before = start.saturating_sub(CONTEXT_ROWS)..start;
    let after = end..(end + CONTEXT_ROWS).min(rows.len());
    let mut picked: Vec<Option<usize>> = before.map(Some).collect();
    if end - start > MAX_HUNK_ROWS {
        let keep = MAX_HUNK_ROWS / 2;
        picked.extend((start..start + keep).map(Some));
        picked.push(None);
        picked.extend((end - keep..end).map(Some));
    } else {
        picked.extend((start..end).map(Some));
    }
    picked.extend(after.map(Some));

    let mut text = String::new();
    let mut plus: Vec<String> = Vec::new();
    let mut old = (0u32, 0u32);
    let mut new = (0u32, 0u32);
    let mut omitted = 0usize;
    let line = |f: &FileData, i: u32| -> String {
        let b = f.line(i as usize);
        let b = b.strip_suffix(b"\n").unwrap_or(b);
        let b = b.strip_suffix(b"\r").unwrap_or(b);
        if b.len() > MAX_LINE_BYTES {
            let mut cut = MAX_LINE_BYTES;
            while cut > 0 && !b.is_char_boundary_at(cut) {
                cut -= 1;
            }
            format!("{}…", String::from_utf8_lossy(&b[..cut]))
        } else {
            String::from_utf8_lossy(b).into_owned()
        }
    };
    let flush = |text: &mut String, plus: &mut Vec<String>| {
        for l in plus.drain(..) {
            text.push('+');
            text.push_str(&l);
            text.push('\n');
        }
    };
    for p in picked {
        let Some(i) = p else {
            flush(&mut text, &mut plus);
            omitted = (end - start) - MAX_HUNK_ROWS;
            text.push_str(&format!("... {omitted} changed rows omitted ...\n"));
            continue;
        };
        let r = rows[i];
        if r.left != NONE {
            if old.1 == 0 {
                old.0 = r.left + 1;
            }
            old.1 += 1;
        }
        if r.right != NONE {
            if new.1 == 0 {
                new.0 = r.right + 1;
            }
            new.1 += 1;
        }
        match r.kind {
            RowKind::Equal => {
                flush(&mut text, &mut plus);
                text.push(' ');
                text.push_str(&line(left, r.left));
                text.push('\n');
            }
            RowKind::Delete => {
                text.push('-');
                text.push_str(&line(left, r.left));
                text.push('\n');
            }
            RowKind::Insert => plus.push(line(right, r.right)),
            RowKind::Modify => {
                text.push('-');
                text.push_str(&line(left, r.left));
                text.push('\n');
                plus.push(line(right, r.right));
            }
        }
    }
    flush(&mut text, &mut plus);
    if omitted > 0 {
        // Line counts are of the shown rows; the header would otherwise claim a range the
        // excerpt does not fully contain.
        text.insert_str(0, "(excerpt: the middle of this change is omitted)\n");
    }
    Excerpt { old, new, text }
}

trait CharBoundary {
    fn is_char_boundary_at(&self, i: usize) -> bool;
}

impl CharBoundary for [u8] {
    fn is_char_boundary_at(&self, i: usize) -> bool {
        i == 0 || i >= self.len() || (self[i] as i8) >= -0x40
    }
}

pub struct Context<'a> {
    pub entry: &'a FileEntry,
    /// What the two sides are, e.g. "`git diff HEAD~3`" or "commit abc against its parent".
    pub comparison: String,
    /// Other files changed in the same comparison (display names).
    pub others: &'a [String],
    pub root: &'a Path,
    pub excerpt: &'a Excerpt,
    /// The text of the previous pass when expanding.
    pub previous: Option<&'a str>,
    pub level: u32,
}

pub fn build_prompt(c: &Context) -> String {
    let sentences = 3 + 2 * c.level as usize;
    let status = match c.entry.status {
        Status::Added => "new file",
        Status::Deleted => "deleted file",
        Status::Modified => "modified",
    };
    let mut p = String::new();
    p.push_str(
        "Explain the purpose of the change shown below. Reply with plain text only: no \
         headings, no bullet points, no code fences, no preamble and no closing remarks.\n\n",
    );
    p.push_str("Rules:\n");
    p.push_str(&format!(
        "1. Be very concise and factual: at most {sentences} short sentences.\n"
    ));
    p.push_str(
        "2. No metaphors and no jargon. Use plain words and the names that appear in the code.\n",
    );
    p.push_str(
        "3. Explain in terms of cause and effect, in this shape: \"X may do Y. That can cause \
         Z. This change accounts for Z by doing W.\"\n",
    );
    p.push_str(
        "4. Do not narrate the diff line by line. Say why the change is made and what it \
         achieves.\n",
    );
    p.push_str(
        "5. If the purpose is not evident from the change, the surrounding code or the \
         history, say what the change does and that the purpose is not evident. Do not guess.\n",
    );
    if let Some(prev) = c.previous {
        p.push_str(&format!(
            "\nA previous explanation of this change was:\n\"{prev}\"\nThe reader wants \
             slightly more detail. Rewrite it expanded by one or two sentences: keep what was \
             correct and add the most useful missing cause-and-effect detail, such as what \
             goes wrong without the change or what relies on it. Keep the rules above.\n"
        ));
    }
    p.push_str(&format!(
        "\nYou are running in the repository root ({}). Read surrounding code, and use \
         `git log` / `git show` / `git diff`, when that helps determine the purpose.\n",
        c.root.display()
    ));
    p.push_str("\nContext:\n");
    p.push_str(&format!("- File: {} ({status})\n", c.entry.rel));
    p.push_str(&format!("- Comparison: {}\n", c.comparison));
    if !c.others.is_empty() {
        const SHOWN: usize = 40;
        let mut list = c
            .others
            .iter()
            .take(SHOWN)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        if c.others.len() > SHOWN {
            list.push_str(&format!(" ({} more)", c.others.len() - SHOWN));
        }
        p.push_str(&format!(
            "- Other files changed in this comparison: {list}\n"
        ));
    }
    let e = c.excerpt;
    p.push_str(&format!(
        "\nThe change, as a unified diff with {CONTEXT_ROWS} lines of context (old lines \
         {}-{}, new lines {}-{}):\n```\n@@ -{},{} +{},{} @@\n{}```\n",
        e.old.0,
        e.old.0 + e.old.1.saturating_sub(1),
        e.new.0,
        e.new.0 + e.new.1.saturating_sub(1),
        e.old.0,
        e.old.1,
        e.new.0,
        e.new.1,
        e.text
    ));
    p
}

/// Describes one side of a pair for the prompt.
pub fn describe_source(s: &Source) -> String {
    match s {
        Source::Empty => "absent".into(),
        Source::Path(p) => format!("file {}", p.display()),
        Source::Blob(sha) => format!("blob {}", &sha[..sha.len().min(12)]),
    }
}

// ---- running ----------------------------------------------------------------------------

const TIMEOUT: Duration = Duration::from_secs(180);

pub struct Job {
    pub file: usize,
    pub key: HunkKey,
    pub level: u32,
    pub prompt: String,
    pub root: PathBuf,
}

pub struct Outcome {
    pub file: usize,
    pub key: HunkKey,
    pub level: u32,
    pub result: Result<String, String>,
    pub elapsed: Duration,
}

/// Pids of agent processes still running, killed on exit so a closed viewer does not leave
/// agents working in the background.
pub type Children = Arc<Mutex<Vec<u32>>>;

pub fn kill_children(children: &Children) {
    let pids: Vec<u32> = children.lock().map(|c| c.clone()).unwrap_or_default();
    for pid in pids {
        unsafe {
            libc::kill(pid as libc::pid_t, libc::SIGTERM);
        }
    }
}

/// Runs the agent on its own thread; `done` is called from that thread.
pub fn spawn(
    agent: Agent,
    job: Job,
    children: Children,
    done: impl FnOnce(Outcome) + Send + 'static,
) {
    std::thread::Builder::new()
        .name("explain".into())
        .spawn(move || {
            let start = Instant::now();
            let fast = job.level == 0;
            let result = run(&agent, &job.prompt, fast, &job.root, &children);
            done(Outcome {
                file: job.file,
                key: job.key,
                level: job.level,
                result,
                elapsed: start.elapsed(),
            });
        })
        .expect("spawn explain thread");
}

fn run(
    agent: &Agent,
    prompt: &str,
    fast: bool,
    root: &Path,
    children: &Children,
) -> Result<String, String> {
    let mut child = Command::new(agent.program())
        .args(agent.args(prompt, fast))
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Agents refuse to nest inside their own interactive sessions; the viewer may
        // well have been launched from one.
        .env_remove("CLAUDECODE")
        .env_remove("CLAUDE_CODE_ENTRYPOINT")
        .spawn()
        .map_err(|e| format!("cannot run {}: {e}", agent.program()))?;
    let pid = child.id();
    if let Ok(mut c) = children.lock() {
        c.push(pid);
    }
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let out = std::thread::spawn(move || {
        let mut v = Vec::new();
        let _ = stdout.read_to_end(&mut v);
        v
    });
    let err = std::thread::spawn(move || {
        let mut v = Vec::new();
        let _ = stderr.read_to_end(&mut v);
        v
    });
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Ok(s),
            Ok(None) if start.elapsed() > TIMEOUT => {
                let _ = child.kill();
                let _ = child.wait();
                break Err(format!(
                    "{} timed out after {}s",
                    agent.name,
                    TIMEOUT.as_secs()
                ));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => break Err(format!("{}: {e}", agent.name)),
        }
    };
    if let Ok(mut c) = children.lock() {
        c.retain(|&p| p != pid);
    }
    let stdout = out.join().unwrap_or_default();
    let stderr = err.join().unwrap_or_default();
    let status = status?;
    let text = clean(&String::from_utf8_lossy(&stdout));
    if !status.success() || text.is_empty() {
        let err = String::from_utf8_lossy(&stderr);
        let last = err
            .lines()
            .rev()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .unwrap_or("");
        return Err(if last.is_empty() {
            format!("{} produced no answer ({status})", agent.name)
        } else {
            format!("{}: {last}", agent.name)
        });
    }
    Ok(text)
}

/// Trims the answer and drops decoration agents add despite instructions: code fences,
/// surrounding quotes, runs of blank lines.
fn clean(s: &str) -> String {
    let mut t = s.trim();
    if let Some(inner) = t.strip_prefix("```") {
        let inner = inner.split_once('\n').map(|(_, r)| r).unwrap_or(inner);
        t = inner.strip_suffix("```").unwrap_or(inner).trim();
    }
    if t.len() >= 2
        && t.starts_with('"')
        && t.ends_with('"')
        && t[1..t.len() - 1].find('"').is_none()
    {
        t = &t[1..t.len() - 1];
    }
    let mut out = String::with_capacity(t.len());
    let mut blank = false;
    for line in t.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            blank = true;
            continue;
        }
        if !out.is_empty() {
            out.push(if blank { '\n' } else { ' ' });
        }
        blank = false;
        out.push_str(line.trim_start());
    }
    out
}

// ---- layout helpers -------------------------------------------------------------------

/// Greedy word wrap by display cells; paragraphs (newlines) are kept, long words are cut.
pub fn wrap(text: &str, cols: usize) -> Vec<String> {
    let cols = cols.max(8);
    let width = |s: &str| unicode_width::UnicodeWidthStr::width(s);
    let mut out = Vec::new();
    for para in text.split('\n') {
        let mut line = String::new();
        let mut used = 0usize;
        for word in para.split_whitespace() {
            let mut w = width(word);
            if used > 0 && used + 1 + w > cols {
                out.push(std::mem::take(&mut line));
                used = 0;
            }
            if used > 0 {
                line.push(' ');
                used += 1;
            }
            let mut word = word;
            while w > cols {
                let mut cut = 0;
                let mut cw = 0;
                for (i, ch) in word.char_indices() {
                    let c = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(1);
                    if cw + c > cols - used {
                        break;
                    }
                    cw += c;
                    cut = i + ch.len_utf8();
                }
                if cut == 0 {
                    break;
                }
                line.push_str(&word[..cut]);
                out.push(std::mem::take(&mut line));
                used = 0;
                word = &word[cut..];
                w = width(word);
            }
            line.push_str(word);
            used += w;
        }
        out.push(line);
    }
    while out.last().is_some_and(|l| l.is_empty()) && out.len() > 1 {
        out.pop();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::{diff_files, WhitespaceMode};
    use crate::text::Bytes;

    fn fd(s: &str) -> FileData {
        FileData::from_bytes(Bytes::Owned(s.as_bytes().to_vec()))
    }

    #[test]
    fn excerpt_is_unified() {
        let a = fd("a\nb\nc\nd\ne\nf\ng\nh\ni\nj\n");
        let b = fd("a\nb\nc\nd\nE\nf\ng\nH1\nH2\ni\nj\n");
        let d = diff_files(&a, &b, WhitespaceMode::Exact);
        assert_eq!(d.hunks.len(), 2);
        let x = excerpt(&a, &b, &d, 0);
        // Six rows of trailing context reach into the second hunk, which is rendered as
        // changed lines too.
        assert_eq!(x.old, (1, 10));
        assert_eq!(x.new, (1, 11));
        assert_eq!(
            x.text,
            " a\n b\n c\n d\n-e\n+E\n f\n g\n-h\n+H1\n+H2\n i\n j\n"
        );
        let x = excerpt(&a, &b, &d, 1);
        assert_eq!(x.old, (2, 9));
        assert_eq!(x.new, (2, 10));
        assert!(x.text.ends_with("-h\n+H1\n+H2\n i\n j\n"));
    }

    #[test]
    fn excerpt_truncates_big_hunks() {
        let a = fd("");
        let b: String = (0..1000).map(|i| format!("l{i}\n")).collect();
        let b = fd(&b);
        let d = diff_files(&a, &b, WhitespaceMode::Exact);
        let x = excerpt(&a, &b, &d, 0);
        assert!(x.text.contains("... 760 changed rows omitted ..."));
        assert!(x.text.contains("+l0\n"));
        assert!(x.text.contains("+l999\n"));
        assert!(!x.text.contains("+l500\n"));
    }

    #[test]
    fn prompt_mentions_rules_and_previous() {
        let entry = FileEntry {
            rel: "src/x.rs".into(),
            left: Source::Blob("a".repeat(40)),
            right: Source::Empty,
            status: Status::Deleted,
        };
        let x = Excerpt {
            old: (1, 2),
            new: (0, 0),
            text: "-a\n-b\n".into(),
        };
        let c = Context {
            entry: &entry,
            comparison: "test".into(),
            others: &["y.rs".into()],
            root: Path::new("/repo"),
            excerpt: &x,
            previous: Some("It removes x."),
            level: 1,
        };
        let p = build_prompt(&c);
        assert!(p.contains("at most 5 short sentences"));
        assert!(p.contains("cause and effect"));
        assert!(p.contains("It removes x."));
        assert!(p.contains("@@ -1,2 +0,0 @@"));
        assert!(p.contains("Other files changed in this comparison: y.rs"));
    }

    #[test]
    fn agent_args() {
        let a = Agent::custom("mytool --ask {prompt} --flag").unwrap();
        assert_eq!(a.name, "mytool");
        assert_eq!(
            a.args("hi there", true),
            vec!["--ask", "hi there", "--flag"]
        );
        let a = Agent::custom("/opt/bin/other").unwrap();
        assert_eq!(a.name, "other");
        assert_eq!(a.args("q", false), vec!["q"]);
        let c = Agent::known(&KNOWN[0]);
        let args = c.args("q", true);
        assert_eq!(args[0], "-p");
        assert_eq!(args[1], "q");
        assert!(args.ends_with(&["--model".to_string(), "haiku".to_string()]));
        assert_eq!(c.label(true), "claude (haiku)");
        assert_eq!(c.label(false), "claude");
    }

    #[test]
    fn clean_strips_decoration() {
        assert_eq!(clean("```text\nfoo\nbar\n```"), "foo bar");
        assert_eq!(clean("\"quoted\""), "quoted");
        assert_eq!(clean("a\n\n\nb\nc\n"), "a\nb c");
    }

    #[test]
    fn wrap_words() {
        assert_eq!(
            wrap("the quick brown fox jumps", 10),
            vec!["the quick", "brown fox", "jumps"]
        );
        assert_eq!(wrap("a\nb", 10), vec!["a", "b"]);
        assert_eq!(
            wrap("abcdefghijklmnopqrstuvwxyz", 10),
            vec!["abcdefghij", "klmnopqrst", "uvwxyz"]
        );
        assert_eq!(wrap("", 10), vec![""]);
    }
}
