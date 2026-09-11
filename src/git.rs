//! Native git integration: the changed-file list from `git diff --raw` and blob contents
//! from one long-lived `git cat-file --batch` process. This replaces `git difftool -d`,
//! which has to write every changed file into temporary trees before the tool starts.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use crate::files::{FileEntry, Source, Status};
use crate::trace;

/// Runs `git diff --raw <args>` and returns the entries, the repository root and a blob
/// reader positioned in the repository. `git rev-parse` and the cat-file process start
/// concurrently with the diff so the three git startups overlap.
pub fn discover(args: &[String]) -> Result<(Vec<FileEntry>, PathBuf, BlobReader), String> {
    let _s = trace::span("git-discover");
    let rev_parse = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot run git: {e}"))?;
    let mut diff = Command::new("git")
        .args([
            "diff",
            "--raw",
            "-z",
            "--no-abbrev",
            "--no-color",
            "--no-ext-diff",
            "--no-prefix",
            "-M",
        ])
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot run git: {e}"))?;
    let reader = BlobReader::spawn()?;

    let root = {
        let out = rev_parse
            .wait_with_output()
            .map_err(|e| format!("git rev-parse: {e}"))?;
        if !out.status.success() {
            return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
        }
        PathBuf::from(String::from_utf8_lossy(&out.stdout).trim_end())
    };
    let raw = {
        let mut stdout = Vec::new();
        diff.stdout
            .take()
            .unwrap()
            .read_to_end(&mut stdout)
            .map_err(|e| format!("git diff: {e}"))?;
        let mut stderr = String::new();
        let _ = diff.stderr.take().unwrap().read_to_string(&mut stderr);
        let status = diff.wait().map_err(|e| format!("git diff: {e}"))?;
        if !status.success() {
            return Err(format!("git diff: {}", stderr.trim()));
        }
        stdout
    };
    let entries = parse_raw(&raw, &root)?;
    if entries.is_empty() {
        return Err("git diff reports no changes".into());
    }
    Ok((entries, root, reader))
}

/// Builds `git diff` arguments showing `commit` against its first parent, as `git show`
/// does. A root commit is compared with the empty tree.
pub fn show_args(commit: &str, rest: &[String]) -> Vec<String> {
    let _s = trace::span("git-parent");
    const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";
    let parent = Command::new("git")
        .args(["rev-parse", "--verify", "-q", &format!("{commit}^")])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| EMPTY_TREE.to_string());
    let mut args = vec![parent, commit.to_string()];
    args.extend(rest.iter().cloned());
    args
}

/// Parses `git diff --raw -z --no-abbrev` output: records of
/// `:<mode1> <mode2> <sha1> <sha2> <status>\0<path>\0`, with a second path for R/C.
fn parse_raw(raw: &[u8], root: &std::path::Path) -> Result<Vec<FileEntry>, String> {
    let mut entries = Vec::new();
    let mut fields = raw.split(|&b| b == 0).filter(|f| !f.is_empty());
    while let Some(header) = fields.next() {
        let header = std::str::from_utf8(header).map_err(|_| "git diff: bad header")?;
        let Some(header) = header.strip_prefix(':') else {
            return Err(format!("git diff: unexpected record {header:?}"));
        };
        let parts: Vec<&str> = header.split(' ').collect();
        if parts.len() < 5 {
            return Err(format!("git diff: short record {header:?}"));
        }
        let (mode2, sha1, sha2, status) = (parts[1], parts[2], parts[3], parts[4]);
        let kind = status.chars().next().unwrap_or('M');
        let path = |f: Option<&[u8]>| -> Result<String, String> {
            f.map(|b| String::from_utf8_lossy(b).into_owned())
                .ok_or_else(|| "git diff: missing path".to_string())
        };
        let first = path(fields.next())?;
        let rel = if kind == 'R' || kind == 'C' {
            path(fields.next())?
        } else {
            first
        };
        let is_null = |sha: &str| sha.bytes().all(|b| b == b'0');
        let left = if is_null(sha1) {
            Source::Empty
        } else {
            Source::Blob(sha1.to_string())
        };
        // A null destination sha with a live mode means "read the working tree".
        let right = if mode2 == "000000" {
            Source::Empty
        } else if is_null(sha2) {
            Source::Path(root.join(&rel))
        } else {
            Source::Blob(sha2.to_string())
        };
        let status = match kind {
            'A' => Status::Added,
            'D' => Status::Deleted,
            _ => Status::Modified,
        };
        entries.push(FileEntry {
            rel,
            left,
            right,
            status,
        });
    }
    Ok(entries)
}

/// One `git cat-file --batch` process; blobs are requested and read synchronously.
pub struct BlobReader {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl BlobReader {
    fn spawn() -> Result<BlobReader, String> {
        let mut child = Command::new("git")
            .args(["cat-file", "--batch"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("cannot run git cat-file: {e}"))?;
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::with_capacity(1 << 16, child.stdout.take().unwrap());
        Ok(BlobReader {
            child,
            stdin,
            stdout,
        })
    }

    pub fn read(&mut self, sha: &str) -> Result<Vec<u8>, String> {
        let _s = trace::span("git-cat-file");
        writeln!(self.stdin, "{sha}").map_err(|e| format!("git cat-file: {e}"))?;
        self.stdin
            .flush()
            .map_err(|e| format!("git cat-file: {e}"))?;
        let mut header = String::new();
        self.stdout
            .read_line(&mut header)
            .map_err(|e| format!("git cat-file: {e}"))?;
        // "<sha> <type> <size>\n" or "<sha> missing\n".
        let mut it = header.split_whitespace();
        let _sha = it.next();
        let kind = it.next().unwrap_or("");
        if kind == "missing" || kind.is_empty() {
            return Err(format!("git cat-file: blob {sha} missing"));
        }
        let size: usize = it
            .next()
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| format!("git cat-file: bad header {header:?}"))?;
        let mut buf = vec![0u8; size + 1];
        self.stdout
            .read_exact(&mut buf)
            .map_err(|e| format!("git cat-file: {e}"))?;
        buf.pop();
        Ok(buf)
    }
}

impl Drop for BlobReader {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_raw_records() {
        let z = "\0";
        let raw = format!(
            ":000000 100644 {z0} {a} A{z}new.rs{z}:100644 100644 {a} {z0} M{z}dirty.rs{z}:100644 000000 {a} {z0} D{z}gone.rs{z}:100644 100644 {a} {b} R090{z}old.rs{z}new_name.rs{z}",
            z0 = "0".repeat(40),
            a = "a".repeat(40),
            b = "b".repeat(40),
        );
        let entries = parse_raw(raw.as_bytes(), std::path::Path::new("/repo")).unwrap();
        assert_eq!(entries.len(), 4);
        assert_eq!(entries[0].rel, "new.rs");
        assert_eq!(entries[0].status, Status::Added);
        assert!(matches!(entries[0].left, Source::Empty));
        assert!(matches!(&entries[0].right, Source::Blob(s) if s.starts_with("aaaa")));
        assert!(
            matches!(&entries[1].right, Source::Path(p) if p == std::path::Path::new("/repo/dirty.rs"))
        );
        assert_eq!(entries[2].status, Status::Deleted);
        assert!(matches!(entries[2].right, Source::Empty));
        assert_eq!(entries[3].rel, "new_name.rs");
        assert!(matches!(&entries[3].right, Source::Blob(s) if s.starts_with("bbbb")));
    }
}
