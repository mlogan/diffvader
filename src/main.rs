mod app;
mod comments;
mod config;
mod diff;
mod difftool;
mod explain;
mod files;
mod font;
mod fuzzy;
mod git;
mod gpu;
mod icon;
mod keys;
// Not wired into the renderer yet.
#[allow(dead_code)]
mod lex;
mod text;
mod theme;
mod trace;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};

use winit::event_loop::{ControlFlow, EventLoop};
use winit::platform::macos::{ActivationPolicy, EventLoopBuilderExtMacOS};

use crate::app::{App, Input, Loaded, Msg, Options};
use crate::diff::WhitespaceMode;
use crate::files::{FileEntry, FileSet, Source};
use crate::text::FileData;

const USAGE: &str = "\
diffvader — fast side-by-side diff viewer

usage: diffvader [options] LEFT RIGHT [+ROW]     compare two files, or two directory trees
       diffvader [options] [--git GIT-DIFF-ARGS]  show what `git diff GIT-DIFF-ARGS` would
       diffvader [options] --show [COMMIT] [ARGS]  one commit against its parent (like git show)
       diffvader --difftool LOCAL REMOTE [BASE]   what `git difftool` runs (see --install-git)

options:
  -w, --ignore-all-space       ignore all whitespace
  -b, --ignore-space-change    ignore changes in the amount of whitespace
      --ignore-space-at-eol    ignore whitespace at end of line
      --font PATH|NAME         monospace font file or family name (default: SF Mono / Menlo)
      --font-size PT           font size in points (default 13)
      --tab-width N            tab stop width (default 4)
      --light                  light color theme
      --agent NAME|CMD         AI agent for `e` (explain a change): claude, codex, gemini,
                               or a command line; `{prompt}` marks where the prompt goes
      --timing                 print startup timeline and frame stats to stderr at exit
      --trace FILE             write Chrome trace event JSON (Perfetto / chrome://tracing)
      --screenshot FILE.bmp    render the first diff frame to a BMP file and exit
      --quit-after-first-frame exit as soon as the diff is on screen (for benchmarking)
      --bench-scroll N         scroll through the diff in N frames, print stats, exit
      --config FILE            settings file (default: ~/.config/diffvader/config)
      --init-config            write a commented settings template there and exit
      --git-config             print the git config needed to use diffvader as a difftool
      --install-git            write that config to ~/.gitconfig, including a `git dv` alias
  -h, --help                   show this help
  -V, --version                show version

`--git` (or no arguments inside a repository) reads the changed-file list and contents
straight from git, so any `git diff` arguments work: `diffvader --git HEAD~3`,
`diffvader --git --cached`, `diffvader --git main.. -- src/`. Two directories (what
`git difftool --dir-diff` passes) work the same way. Files are shown one at a time; ⌘P
opens a fuzzy file picker. `e` asks an AI agent found on PATH (or `$DIFFVADER_AGENT`)
to explain the change under the cursor. Press ? inside the app for the full key list.
";

fn main() {
    trace::init();
    let opts = match parse_args() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("diffvader: {e}\n\n{USAGE}");
            std::process::exit(2);
        }
    };

    // Font parsing and Metal device creation need no window; overlap them with AppKit init.
    let font_path = opts.font.clone();
    let font_pt = opts.font_pt;
    let font_thread = std::thread::Builder::new()
        .name("font".into())
        .spawn(move || font::FontSet::load(font_path.as_deref(), font_pt * 2.0))
        .expect("spawn font thread");
    let gpu_thread = std::thread::Builder::new()
        .name("gpu".into())
        .spawn(gpu::GpuCore::init)
        .expect("spawn gpu thread");

    let (tx, rx) = mpsc::channel::<Msg>();
    let wanted = Arc::new(AtomicUsize::new(0));
    // The loader starts before the event loop exists; it gets the proxy (to wake the loop)
    // through this side channel once the loop has been created.
    let (proxy_tx, proxy_rx) = mpsc::channel::<winit::event_loop::EventLoopProxy<()>>();
    {
        let tx = tx.clone();
        let wanted = wanted.clone();
        let input = opts.input.clone();
        let titles = (opts.left_title.clone(), opts.right_title.clone());
        let mode = opts.whitespace;
        std::thread::Builder::new()
            .name("loader".into())
            .spawn(move || {
                let _s = trace::span("load-and-diff");
                let mut proxy = None;
                let mut notify = |msg: Msg| {
                    let _ = tx.send(msg);
                    if proxy.is_none() {
                        proxy = proxy_rx.recv().ok();
                    }
                    if let Some(p) = &proxy {
                        let _ = p.send_event(());
                    }
                };
                let mut blobs = None;
                let discovered = match &input {
                    Input::Pair(left, right) => {
                        files::discover(left.as_path(), right.as_path(), titles)
                    }
                    Input::Session(dir) => {
                        let (records, _) = difftool::read_session(dir);
                        if records.is_empty() {
                            Err(format!("empty difftool session {}", dir.display()))
                        } else {
                            Ok(FileSet {
                                entries: records.iter().map(|r| r.entry()).collect(),
                                multi: true,
                                root: None,
                            })
                        }
                    }
                    Input::Git(_) | Input::Show(..) => {
                        let args = match &input {
                            Input::Git(a) => a.clone(),
                            Input::Show(c, rest) => git::show_args(c, rest),
                            Input::Pair(..) | Input::Session(_) => unreachable!(),
                        };
                        git::discover(args.as_slice()).map(|(entries, root, reader)| {
                            blobs = Some(reader);
                            FileSet {
                                entries,
                                multi: true,
                                root: Some(root),
                            }
                        })
                    }
                };
                let set = match discovered {
                    Ok(set) => set,
                    Err(e) => {
                        notify(Msg::Files(Err(e)));
                        return;
                    }
                };
                let mut entries: Vec<FileEntry> = set.entries.clone();
                notify(Msg::Files(Ok(set)));
                // Load the file the UI wants first, then the rest in order. A difftool
                // session keeps growing while git runs, so poll it until it is complete.
                let session = match &input {
                    Input::Session(dir) => Some(dir.clone()),
                    _ => None,
                };
                let mut complete = session.is_none();
                let mut done = vec![false; entries.len()];
                loop {
                    let w = wanted.load(Ordering::Relaxed);
                    let next = if w < entries.len() && !done[w] {
                        Some(w)
                    } else {
                        done.iter().position(|d| !d)
                    };
                    if let Some(i) = next {
                        let result = load_pair(&entries[i], mode, &mut blobs);
                        done[i] = true;
                        notify(Msg::Loaded { file: i, result });
                        continue;
                    }
                    if complete {
                        break;
                    }
                    let dir = session.as_ref().unwrap();
                    let (records, finished) = difftool::read_session(dir);
                    if records.len() > entries.len() {
                        let new: Vec<FileEntry> =
                            records[entries.len()..].iter().map(|r| r.entry()).collect();
                        entries.extend(new.iter().cloned());
                        done.resize(entries.len(), false);
                        notify(Msg::MoreFiles(new));
                    } else if finished {
                        complete = true;
                    } else {
                        std::thread::sleep(std::time::Duration::from_millis(25));
                    }
                }
            })
            .expect("spawn loader thread");
    }

    let event_loop = {
        let _s = trace::span("event-loop-create");
        let mut builder = EventLoop::<()>::with_user_event();
        builder
            .with_activation_policy(ActivationPolicy::Regular)
            // No default menu: its Quit item calls terminate:, which exits inside an event
            // dispatch and skips all cleanup. ⌘Q is handled as a key instead.
            .with_default_menu(false)
            .with_activate_ignoring_other_apps(true);
        builder.build().expect("create event loop")
    };
    event_loop.set_control_flow(ControlFlow::Wait);
    let proxy = event_loop.create_proxy();
    let _ = proxy_tx.send(proxy.clone());

    let app_input = opts.input.clone();
    let app_trace_path = opts.trace_path.clone();
    let app_timing = opts.timing;
    let mut app = match App::new(opts, rx, tx, proxy.clone(), wanted, font_thread, gpu_thread) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("diffvader: {e}");
            std::process::exit(1);
        }
    };
    if let Some(ms) = std::env::var("DIFFVADER_EXIT_AFTER_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
    {
        let at = std::time::Instant::now() + std::time::Duration::from_millis(ms);
        app.set_deadline(at);
        let proxy = proxy.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(ms));
            let _ = proxy.send_event(());
        });
    }
    if let Input::Session(dir) = &app_input {
        difftool::write_viewer_pid(dir);
    }
    trace::register_exit_hook(
        match &app_input {
            Input::Session(dir) => Some(dir.clone()),
            _ => None,
        },
        app_trace_path,
        app_timing,
    );
    trace::mark("run-app");
    if let Err(e) = event_loop.run_app(&mut app) {
        eprintln!("diffvader: event loop error: {e}");
        std::process::exit(1);
    }
}

fn load_pair(
    entry: &FileEntry,
    mode: WhitespaceMode,
    blobs: &mut Option<git::BlobReader>,
) -> Result<Loaded, String> {
    let _s = trace::span("load-pair");
    let mut load = |src: &Source| -> Result<FileData, String> {
        match src {
            Source::Empty => Ok(FileData::empty()),
            Source::Path(p) => FileData::load(p).map_err(|e| format!("{}: {e}", p.display())),
            Source::Blob(sha) => {
                let reader = blobs.as_mut().ok_or("no git blob reader")?;
                let bytes = reader.read(sha)?;
                Ok(FileData::from_bytes(text::Bytes::Owned(bytes)))
            }
        }
    };
    let a = load(&entry.left)?;
    let b = load(&entry.right)?;
    if a.binary || b.binary {
        return Err("binary files differ".to_string());
    }
    let d = diff::diff_files(&a, &b, mode);
    Ok(Loaded {
        left: Arc::new(a),
        right: Arc::new(b),
        diff: d,
    })
}

fn parse_args() -> Result<Options, String> {
    let all: Vec<String> = std::env::args().skip(1).collect();
    // The config file supplies defaults, so it is read before the flags are parsed.
    let config_path = all
        .iter()
        .position(|a| a == "--config")
        .and_then(|i| all.get(i + 1))
        .map(PathBuf::from)
        .unwrap_or_else(config::path);
    if all.iter().any(|a| a == "--init-config") {
        match config::init(&config_path) {
            Ok(true) => println!("wrote {}", config_path.display()),
            Ok(false) => println!("{} already exists", config_path.display()),
            Err(e) => {
                eprintln!("diffvader: {e}");
                std::process::exit(1);
            }
        }
        std::process::exit(0);
    }
    let cfg = match config::load(&config_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("diffvader: ignoring config: {e}");
            config::Config::default()
        }
    };
    let mut args = all.into_iter();
    let mut paths: Vec<PathBuf> = Vec::new();
    let mut font = cfg.font;
    let mut font_pt = cfg.font_pt.unwrap_or(13.0);
    let mut tab_width = cfg.tab_width.unwrap_or(4).clamp(1, 16);
    let mut whitespace = cfg.whitespace.unwrap_or(WhitespaceMode::Exact);
    let mut light = cfg.light.unwrap_or(false);
    let mut agent = std::env::var("DIFFVADER_AGENT").ok().or(cfg.agent);
    let mut timing = std::env::var_os("DIFFVADER_TIMING").is_some();
    let mut trace_path = std::env::var("DIFFVADER_TRACE").ok();
    let mut screenshot = None;
    let mut quit_after_first_frame = false;
    let mut bench_scroll = None;
    let mut start_row = None;
    let mut git_args: Option<Vec<String>> = None;
    let mut show_args: Option<Vec<String>> = None;
    let mut session: Option<PathBuf> = None;
    let mut difftool_base: Option<String> = None;
    while let Some(a) = args.next() {
        let mut value = |name: &str| args.next().ok_or_else(|| format!("{name} needs a value"));
        match a.as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                std::process::exit(0);
            }
            "-V" | "--version" => {
                println!("diffvader {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            "--git-config" => {
                print!("{}", git_config_snippet());
                std::process::exit(0);
            }
            "--install-git" => {
                install_git();
                std::process::exit(0);
            }
            "--git" => {
                git_args = Some(args.by_ref().collect());
                break;
            }
            "--show" => {
                show_args = Some(args.by_ref().collect());
                break;
            }
            "--session" => session = Some(PathBuf::from(value("--session")?)),
            "--difftool" => {
                let local = PathBuf::from(value("--difftool")?);
                let remote = PathBuf::from(value("--difftool")?);
                let base = args.next();
                match difftool::invocation(&local, &remote, base.as_deref()) {
                    Ok(difftool::Outcome::Done) => {
                        if std::env::var_os("DIFFVADER_TIMING").is_some() {
                            eprintln!(
                                "diffvader:   difftool: exiting at {:.1} ms",
                                trace::elapsed_us() as f64 / 1000.0
                            );
                        }
                        std::process::exit(0)
                    }
                    Ok(difftool::Outcome::Single) => {}
                    Err(e) => {
                        eprintln!("diffvader: difftool: {e}");
                        std::process::exit(1);
                    }
                }
                difftool_base = base;
                paths.push(local);
                paths.push(remote);
            }
            "-w" | "--ignore-all-space" => whitespace = WhitespaceMode::IgnoreAll,
            "-b" | "--ignore-space-change" => whitespace = WhitespaceMode::IgnoreChange,
            "--ignore-space-at-eol" => whitespace = WhitespaceMode::IgnoreEol,
            "--config" => {
                value("--config")?;
            }
            "--font" => font = Some(value("--font")?),
            "--font-size" => {
                font_pt = value("--font-size")?
                    .parse()
                    .map_err(|_| "--font-size must be a number".to_string())?
            }
            "--tab-width" => {
                tab_width = value("--tab-width")?
                    .parse::<u32>()
                    .map_err(|_| "--tab-width must be an integer".to_string())?
                    .clamp(1, 16)
            }
            "--light" => light = true,
            "--agent" => agent = Some(value("--agent")?),
            "--timing" => timing = true,
            "--trace" => trace_path = Some(value("--trace")?),
            "--screenshot" => screenshot = Some(PathBuf::from(value("--screenshot")?)),
            "--quit-after-first-frame" => quit_after_first_frame = true,
            "--bench-scroll" => {
                bench_scroll = Some(
                    value("--bench-scroll")?
                        .parse::<u32>()
                        .map_err(|_| "--bench-scroll must be an integer".to_string())?,
                )
            }
            s if s.starts_with('+') && s[1..].parse::<u64>().is_ok() => {
                start_row = s[1..].parse().ok();
            }
            s if s.starts_with('-') && s.len() > 1 => return Err(format!("unknown option {s}")),
            _ => paths.push(PathBuf::from(a)),
        }
    }
    if (git_args.is_some() || show_args.is_some()) && !paths.is_empty() {
        return Err("--git / --show cannot be combined with file arguments".into());
    }
    let git_input = if let Some(dir) = session {
        Some(Input::Session(dir))
    } else if let Some(mut rest) = show_args {
        // The first non-option argument is the commit; everything else goes to git diff.
        let commit = rest
            .iter()
            .position(|a| !a.starts_with('-'))
            .map(|i| rest.remove(i))
            .unwrap_or_else(|| "HEAD".to_string());
        Some(Input::Show(commit, rest))
    } else if let Some(g) = git_args {
        Some(Input::Git(g))
    } else if paths.is_empty() {
        Some(Input::Git(Vec::new()))
    } else {
        None
    };
    let (input, left_title, right_title) = match git_input {
        Some(input) => (input, String::new(), String::new()),
        None => {
            if paths.len() != 2 {
                return Err("expected exactly two files or directories".into());
            }
            let right = paths.pop().unwrap();
            let left = paths.pop().unwrap();
            if let Some(b) = &difftool_base {
                std::env::set_var("BASE", b);
            }
            let (lt, rt) = titles(&left, &right);
            (Input::Pair(left, right), lt, rt)
        }
    };
    Ok(Options {
        input,
        left_title,
        right_title,
        font,
        font_pt: font_pt.clamp(6.0, 48.0),
        tab_width,
        whitespace,
        light,
        agent,
        timing,
        trace_path,
        screenshot,
        quit_after_first_frame,
        bench_scroll,
        start_row,
    })
}

/// Display names for the panes. `git difftool` hands us temp files and puts the real
/// path in `$BASE`/`$MERGED`, so prefer that when the argument looks like a temp file.
fn titles(left: &Path, right: &Path) -> (String, String) {
    let base = std::env::var("MERGED")
        .ok()
        .or_else(|| std::env::var("BASE").ok())
        .filter(|s| !s.is_empty());
    let is_temp = |p: &Path| {
        let s = p.to_string_lossy();
        s.starts_with("/tmp/") || s.starts_with("/var/folders/") || s.starts_with("/private/")
    };
    let name = |p: &Path, tag: &str| -> String {
        if p == Path::new("/dev/null") {
            return "/dev/null".into();
        }
        match &base {
            Some(b) if is_temp(p) => format!("{b}  ({tag})"),
            _ => p.to_string_lossy().into_owned(),
        }
    };
    (name(left, "old"), name(right, "new"))
}

fn exe_path() -> String {
    std::env::current_exe()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "diffvader".into())
}

fn git_config_snippet() -> String {
    let exe = exe_path();
    format!(
        "[alias]\n\tdv = !{exe} --git\n\tdvs = !{exe} --show\n[diff]\n\ttool = diffvader\n[difftool]\n\tprompt = false\n[difftool \"diffvader\"]\n\tcmd = {exe} --difftool \"$LOCAL\" \"$REMOTE\" \"$BASE\"\n\n# `git dv [<git diff args>]` is the fast path (no temp files, one window for all files);\n# `git dvs [<commit>]` shows one commit against its parent, like git show;\n# `git difftool` also works (one window for all files) but pays git's per-file setup cost.\n"
    )
}

fn install_git() {
    let exe = exe_path();
    let settings = [
        ("alias.dv", format!("!{exe} --git")),
        ("alias.dvs", format!("!{exe} --show")),
        ("diff.tool", "diffvader".to_string()),
        ("difftool.prompt", "false".to_string()),
        (
            "difftool.diffvader.cmd",
            format!("{exe} --difftool \"$LOCAL\" \"$REMOTE\" \"$BASE\""),
        ),
    ];
    for (k, v) in settings {
        let status = std::process::Command::new("git")
            .args(["config", "--global", k, &v])
            .status();
        match status {
            Ok(s) if s.success() => println!("set {k} = {v}"),
            Ok(s) => {
                eprintln!("git config {k} failed: {s}");
                std::process::exit(1);
            }
            Err(e) => {
                eprintln!("cannot run git: {e}");
                std::process::exit(1);
            }
        }
    }
    println!("done. `git dv [<git diff args>]` and `git dvs [<commit>]` are the fast paths; `git difftool` also works.");
}
