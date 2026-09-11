//! `git difftool` integration without one window per file.
//!
//! git runs the configured tool once per changed file and waits for it, so a viewer that
//! blocked would show one file at a time. Instead every invocation copies its pair into a
//! per-session directory and returns immediately; the first invocation spawns one detached
//! viewer that reads the session directory and picks up files as they arrive.
//!
//! Session layout: `<tmp>/diffvader-difftool-<git pid>/` containing `manifest` (one
//! record per file, see [`Record`]), `<n>.a` / `<n>.b` copies of git's temporary files,
//! and a `done` marker once the last file has been written.

use std::io::Write;
use std::path::{Path, PathBuf};

use crate::files::{FileEntry, Source, Status};
use crate::trace;

const SEP: char = '\u{1f}';

/// What the per-file invocation should do next.
pub enum Outcome {
    /// Not running under `git difftool`: show the pair like a normal two-file diff.
    Single,
    /// The pair was recorded (and the viewer spawned if this was the first); exit now.
    Done,
}

/// Handles `diffvader --difftool LOCAL REMOTE [BASE]`.
pub fn invocation(local: &Path, remote: &Path, base: Option<&str>) -> Result<Outcome, String> {
    let _s = trace::span("difftool-invocation");
    let counter: usize = std::env::var("GIT_DIFF_PATH_COUNTER")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let total: usize = std::env::var("GIT_DIFF_PATH_TOTAL")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    if counter == 0 || total == 0 || local.is_dir() || remote.is_dir() {
        return Ok(Outcome::Single);
    }
    let dir = session_dir();
    if counter == 1 {
        sweep_stale_sessions();
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    } else if !dir.is_dir() || viewer_dead(&dir) {
        // The viewer quit (it removes its session directory on a clean exit) or died.
        // Exiting with a signal-like status makes git-difftool--helper stop instead of
        // spending ~65 ms on each remaining file; git reports "external diff died".
        let _ = std::fs::remove_dir_all(&dir);
        eprintln!("diffvader: viewer closed, stopping git difftool");
        std::process::exit(130);
    }

    let rel = base
        .filter(|b| !b.is_empty())
        .map(String::from)
        .or_else(|| std::env::var("BASE").ok().filter(|b| !b.is_empty()))
        .or_else(|| std::env::var("MERGED").ok().filter(|b| !b.is_empty()))
        .unwrap_or_else(|| {
            let p = if is_null(remote) { local } else { remote };
            p.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        });
    let a = keep_side(local, &dir, counter, 'a')?;
    let b = keep_side(remote, &dir, counter, 'b')?;
    let record = Record {
        rel,
        left: a,
        right: b,
    };
    {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("manifest"))
            .map_err(|e| format!("manifest: {e}"))?;
        writeln!(f, "{}", record.encode()).map_err(|e| format!("manifest: {e}"))?;
    }
    if counter == total {
        let _ = std::fs::File::create(dir.join("done"));
    }
    if counter == 1 {
        spawn_viewer(&dir)?;
    }
    Ok(Outcome::Done)
}

/// Written by the viewer at startup so later invocations can tell whether it is alive
/// even when it died without cleaning up (crash, kill -9).
pub fn write_viewer_pid(dir: &Path) {
    let _ = std::fs::write(dir.join("viewer.pid"), std::process::id().to_string());
}

fn viewer_dead(dir: &Path) -> bool {
    let Some(pid) = std::fs::read_to_string(dir.join("viewer.pid"))
        .ok()
        .and_then(|s| s.trim().parse::<i32>().ok())
    else {
        // Not written yet: the viewer was spawned moments ago and is still starting.
        return false;
    };
    unsafe { libc::kill(pid, 0) != 0 && *libc::__error() == libc::ESRCH }
}

/// Removes session directories older than an hour that a crashed viewer left behind.
fn sweep_stale_sessions() {
    let Ok(rd) = std::fs::read_dir(std::env::temp_dir()) else {
        return;
    };
    let cutoff = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
    for e in rd.flatten() {
        let name = e.file_name();
        if !name.to_string_lossy().starts_with("diffvader-difftool-") {
            continue;
        }
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .map(|t| t < cutoff)
            .unwrap_or(false);
        if old {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}

fn is_null(p: &Path) -> bool {
    p == Path::new("/dev/null")
}

/// Returns a path that will still be valid after this invocation returns: git deletes its
/// temporary blob files, so those are copied; working-tree files are kept by absolute path.
fn keep_side(p: &Path, dir: &Path, n: usize, tag: char) -> Result<Option<PathBuf>, String> {
    if is_null(p) || !p.exists() {
        return Ok(None);
    }
    let abs = std::path::absolute(p).map_err(|e| format!("{}: {e}", p.display()))?;
    let tmp = std::env::temp_dir();
    let is_git_temp = abs.starts_with(&tmp)
        || abs
            .components()
            .any(|c| c.as_os_str().to_string_lossy().starts_with("git-blob-"));
    if !is_git_temp {
        return Ok(Some(abs));
    }
    let dst = dir.join(format!("{n}.{tag}"));
    std::fs::copy(&abs, &dst).map_err(|e| format!("copy {}: {e}", abs.display()))?;
    Ok(Some(dst))
}

/// The nearest `git` ancestor identifies one `git difftool` run: each file gets fresh
/// shells, but they all descend from the same `git diff` process.
fn session_dir() -> PathBuf {
    let mut pid = std::process::id();
    let mut git_pid = None;
    for _ in 0..8 {
        let Some((ppid, name)) = parent_of(pid) else {
            break;
        };
        if ppid <= 1 {
            break;
        }
        if name == "git" {
            git_pid = Some(ppid);
            break;
        }
        pid = ppid;
    }
    let key = git_pid.unwrap_or_else(|| unsafe { libc::getppid() } as u32);
    std::env::temp_dir().join(format!("diffvader-difftool-{key}"))
}

fn parent_of(pid: u32) -> Option<(u32, String)> {
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    let n = unsafe {
        libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDTBSDINFO,
            0,
            &mut info as *mut _ as *mut libc::c_void,
            size,
        )
    };
    if n != size {
        return None;
    }
    let name = unsafe { std::ffi::CStr::from_ptr(info.pbi_comm.as_ptr()) }
        .to_string_lossy()
        .into_owned();
    Some((info.pbi_ppid, name))
}

/// Starts the viewer in its own session so it outlives git and the shells it spawned.
fn spawn_viewer(dir: &Path) -> Result<(), String> {
    use std::os::unix::process::CommandExt;
    let exe = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;
    std::process::Command::new(exe)
        .arg("--session")
        .arg(dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .process_group(0)
        .spawn()
        .map_err(|e| format!("spawn viewer: {e}"))?;
    Ok(())
}

/// One manifest line.
pub struct Record {
    pub rel: String,
    pub left: Option<PathBuf>,
    pub right: Option<PathBuf>,
}

impl Record {
    fn encode(&self) -> String {
        let side = |p: &Option<PathBuf>| {
            p.as_ref()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default()
        };
        format!(
            "{}{SEP}{}{SEP}{}",
            self.rel,
            side(&self.left),
            side(&self.right)
        )
    }

    fn decode(line: &str) -> Option<Record> {
        let mut it = line.split(SEP);
        let rel = it.next()?.to_string();
        let side = |s: Option<&str>| s.filter(|s| !s.is_empty()).map(PathBuf::from);
        let left = side(it.next());
        let right = side(it.next());
        Some(Record { rel, left, right })
    }

    pub fn entry(&self) -> FileEntry {
        let src = |p: &Option<PathBuf>| match p {
            Some(p) => Source::Path(p.clone()),
            None => Source::Empty,
        };
        let status = match (&self.left, &self.right) {
            (None, Some(_)) => Status::Added,
            (Some(_), None) => Status::Deleted,
            _ => Status::Modified,
        };
        FileEntry {
            rel: self.rel.clone(),
            left: src(&self.left),
            right: src(&self.right),
            status,
        }
    }
}

/// Reads the manifest; returns all records so far and whether the session is complete.
pub fn read_session(dir: &Path) -> (Vec<Record>, bool) {
    let text = std::fs::read_to_string(dir.join("manifest")).unwrap_or_default();
    let records = text.lines().filter_map(Record::decode).collect();
    (records, dir.join("done").exists())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_roundtrip() {
        let r = Record {
            rel: "src/a b.rs".into(),
            left: Some(PathBuf::from("/tmp/x/1.a")),
            right: None,
        };
        let d = Record::decode(&r.encode()).unwrap();
        assert_eq!(d.rel, "src/a b.rs");
        assert_eq!(d.left, r.left);
        assert_eq!(d.right, None);
        assert_eq!(d.entry().status, Status::Deleted);
    }
}
