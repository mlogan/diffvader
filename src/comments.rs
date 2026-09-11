//! Review comments on changes, collected into one prompt for an agent when the viewer
//! quits: saved to a temp file and, on request, copied to the clipboard.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use crate::explain::Excerpt;

pub struct Item {
    pub file: String,
    /// 1-based change number within the file, and the file's change count.
    pub index: usize,
    pub total: usize,
    /// `None` when the change no longer exists in the current diff (whitespace mode changed).
    pub excerpt: Option<Excerpt>,
    pub comment: String,
}

/// One prompt covering every comment. Each item shows the change as a unified diff so the
/// agent can locate it without the viewer's line model.
pub fn build_prompt(items: &[Item], comparison: &str, root: &std::path::Path) -> String {
    let mut p = String::new();
    p.push_str(&format!(
        "Review comments on {comparison}, in the repository at {}.\n\n",
        root.display()
    ));
    p.push_str(
        "Each item below shows one change as a unified diff followed by my comment on it. \
         Please address every comment: make the requested change, or explain why it should \
         stay as is. Read the surrounding code and use `git diff` / `git show` when a diff \
         excerpt alone is not enough. Ask if a comment is unclear.\n",
    );
    for (n, it) in items.iter().enumerate() {
        p.push_str(&format!(
            "\n## {}. {} (change {} of {})\n",
            n + 1,
            it.file,
            it.index,
            it.total
        ));
        match &it.excerpt {
            Some(e) => p.push_str(&format!(
                "```\n@@ -{},{} +{},{} @@\n{}```\n",
                e.old.0, e.old.1, e.new.0, e.new.1, e.text
            )),
            None => p.push_str("(this change is no longer part of the current diff view)\n"),
        }
        p.push_str(&format!("Comment: {}\n", it.comment.trim()));
    }
    p
}

/// Writes the prompt to a fresh file in the temp directory and returns its path.
pub fn save(text: &str) -> std::io::Result<PathBuf> {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let path = std::env::temp_dir().join(format!(
        "diffvader-comments-{stamp}-{}.md",
        std::process::id()
    ));
    std::fs::write(&path, text)?;
    Ok(path)
}

pub fn copy_to_clipboard(text: &str) -> Result<(), String> {
    let mut child = Command::new("pbcopy")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("pbcopy: {e}"))?;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(text.as_bytes())
        .map_err(|e| format!("pbcopy: {e}"))?;
    let status = child.wait().map_err(|e| format!("pbcopy: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("pbcopy exited with {status}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_lists_items() {
        let items = vec![
            Item {
                file: "a.rs".into(),
                index: 2,
                total: 3,
                excerpt: Some(Excerpt {
                    old: (10, 2),
                    new: (10, 3),
                    text: " x\n-y\n+Y\n+Z\n".into(),
                }),
                comment: "  rename Y to W ".into(),
            },
            Item {
                file: "b.rs".into(),
                index: 1,
                total: 1,
                excerpt: None,
                comment: "drop this".into(),
            },
        ];
        let p = build_prompt(&items, "`git diff HEAD~1`", std::path::Path::new("/repo"));
        assert!(p.starts_with("Review comments on `git diff HEAD~1`, in the repository at /repo."));
        assert!(p.contains("## 1. a.rs (change 2 of 3)\n```\n@@ -10,2 +10,3 @@\n x\n-y\n+Y\n+Z\n```\nComment: rename Y to W\n"));
        assert!(p.contains("## 2. b.rs (change 1 of 1)\n(this change is no longer part"));
        assert!(p.ends_with("Comment: drop this\n"));
    }

    #[test]
    fn save_writes_file() {
        let path = save("hello").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello");
        let _ = std::fs::remove_file(path);
    }
}
