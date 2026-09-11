//! The set of file pairs in a session: either the two files given on the command line or,
//! for `git difftool --dir-diff` style invocations, every file under two directory trees.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::trace;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Added,
    Deleted,
    Modified,
}

impl Status {
    pub fn letter(self) -> char {
        match self {
            Status::Added => 'A',
            Status::Deleted => 'D',
            Status::Modified => 'M',
        }
    }
}

#[derive(Clone, Debug)]
pub struct FileEntry {
    /// Path shown to the user: relative to the trees in directory mode, else as given.
    pub rel: String,
    pub left: Option<PathBuf>,
    pub right: Option<PathBuf>,
    pub status: Status,
}

pub struct FileSet {
    pub entries: Vec<FileEntry>,
    /// True when the arguments were directories; affects how titles are shown.
    pub dir_mode: bool,
}

/// Builds the file set. Two directories are walked and paired by relative path; anything
/// else is a single pair (missing files, including `/dev/null`, count as empty).
pub fn discover(left: &Path, right: &Path, titles: (String, String)) -> Result<FileSet, String> {
    let _s = trace::span("discover-files");
    if left.is_dir() && right.is_dir() {
        let l = walk(left)?;
        let r = walk(right)?;
        let mut entries = Vec::with_capacity(l.len().max(r.len()));
        let mut keys: Vec<&String> = l.keys().chain(r.keys()).collect();
        keys.sort();
        keys.dedup();
        for rel in keys {
            let (lp, rp) = (l.get(rel), r.get(rel));
            let status = match (lp, rp) {
                (Some(a), Some(b)) => {
                    if same_contents(a, b) {
                        continue;
                    }
                    Status::Modified
                }
                (Some(_), None) => Status::Deleted,
                (None, Some(_)) => Status::Added,
                (None, None) => unreachable!(),
            };
            entries.push(FileEntry {
                rel: rel.clone(),
                left: lp.cloned(),
                right: rp.cloned(),
                status,
            });
        }
        if entries.is_empty() {
            return Err("no differing files under the two directories".into());
        }
        return Ok(FileSet {
            entries,
            dir_mode: true,
        });
    }
    let exists = |p: &Path| p != Path::new("/dev/null") && p.exists();
    let (le, re) = (exists(left), exists(right));
    let status = match (le, re) {
        (true, false) => Status::Deleted,
        (false, true) => Status::Added,
        _ => Status::Modified,
    };
    // In single-file mode the "rel" name is the right-hand title (the newer side), which is
    // what git's $MERGED refers to.
    let rel = if re || !le { titles.1 } else { titles.0 };
    Ok(FileSet {
        entries: vec![FileEntry {
            rel,
            left: le.then(|| left.to_path_buf()),
            right: re.then(|| right.to_path_buf()),
            status,
        }],
        dir_mode: false,
    })
}

/// Regular files under `root` keyed by relative path with `/` separators. Symlinks are
/// followed (git's dir-diff links the working-tree side).
fn walk(root: &Path) -> Result<BTreeMap<String, PathBuf>, String> {
    let mut out = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let rd = std::fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        for entry in rd {
            let entry = entry.map_err(|e| format!("{}: {e}", dir.display()))?;
            let path = entry.path();
            if path.file_name().is_some_and(|n| n == ".git") {
                continue;
            }
            let Ok(meta) = std::fs::metadata(&path) else {
                continue;
            };
            if meta.is_dir() {
                stack.push(path);
            } else if meta.is_file() {
                let rel = path
                    .strip_prefix(root)
                    .unwrap_or(&path)
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy())
                    .collect::<Vec<_>>()
                    .join("/");
                out.insert(rel, path);
            }
        }
    }
    Ok(out)
}

fn same_contents(a: &Path, b: &Path) -> bool {
    let (Ok(ma), Ok(mb)) = (std::fs::metadata(a), std::fs::metadata(b)) else {
        return false;
    };
    if ma.len() != mb.len() {
        return false;
    }
    match (std::fs::read(a), std::fs::read(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dir_discovery() {
        let tmp = std::env::temp_dir().join(format!("diffvader-files-{}", std::process::id()));
        let (l, r) = (tmp.join("left"), tmp.join("right"));
        std::fs::create_dir_all(l.join("sub")).unwrap();
        std::fs::create_dir_all(r.join("sub")).unwrap();
        std::fs::write(l.join("same.txt"), "x").unwrap();
        std::fs::write(r.join("same.txt"), "x").unwrap();
        std::fs::write(l.join("sub/mod.rs"), "a").unwrap();
        std::fs::write(r.join("sub/mod.rs"), "b").unwrap();
        std::fs::write(l.join("gone.rs"), "a").unwrap();
        std::fs::write(r.join("new.rs"), "b").unwrap();
        let set = discover(&l, &r, ("l".into(), "r".into())).unwrap();
        let names: Vec<(String, Status)> = set
            .entries
            .iter()
            .map(|e| (e.rel.clone(), e.status))
            .collect();
        assert_eq!(
            names,
            vec![
                ("gone.rs".to_string(), Status::Deleted),
                ("new.rs".to_string(), Status::Added),
                ("sub/mod.rs".to_string(), Status::Modified),
            ]
        );
        assert!(set.dir_mode);
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
