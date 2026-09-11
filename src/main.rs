mod app;
mod diff;
mod files;
mod font;
mod fuzzy;
mod gpu;
mod keys;
mod text;
mod theme;
mod trace;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};

use winit::event_loop::{ControlFlow, EventLoop};
use winit::platform::macos::{ActivationPolicy, EventLoopBuilderExtMacOS};

use crate::app::{App, Loaded, Msg, Options};
use crate::diff::WhitespaceMode;
use crate::files::FileEntry;
use crate::text::FileData;

const USAGE: &str = "\
diffvader — fast side-by-side diff viewer

usage: diffvader [options] LEFT RIGHT [+ROW]     compare two files, or two directory trees
       diffvader [options] [--git GIT-DIFF-ARGS]  run `git difftool -d` with diffvader

options:
  -w, --ignore-all-space       ignore all whitespace
  -b, --ignore-space-change    ignore changes in the amount of whitespace
      --ignore-space-at-eol    ignore whitespace at end of line
      --font PATH              monospace font file (default: SF Mono / Menlo)
      --font-size PT           font size in points (default 13)
      --tab-width N            tab stop width (default 4)
      --light                  light color theme
      --timing                 print startup timeline and frame stats to stderr at exit
      --trace FILE             write Chrome trace event JSON (Perfetto / chrome://tracing)
      --screenshot FILE.bmp    render the first diff frame to a BMP file and exit
      --quit-after-first-frame exit as soon as the diff is on screen (for benchmarking)
      --bench-scroll N         scroll through the diff in N frames, print stats, exit
      --git-config             print the git config needed to use diffvader as a difftool
      --install-git            write that config to ~/.gitconfig (git config --global)
  -h, --help                   show this help
  -V, --version                show version

With two directories (what `git difftool --dir-diff` passes) every differing file is
loaded and shown one at a time; ⌘P opens a fuzzy file picker. Press ? inside the app
for the full key list.
";

fn main() {
    trace::init();
    let opts = match parse_args() {
        Ok(Parsed::Run(o)) => o,
        Ok(Parsed::Git(args, own)) => run_git_difftool(args, own),
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
        let left = opts.left.clone();
        let right = opts.right.clone();
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
                let set = match files::discover(&left, &right, titles) {
                    Ok(set) => set,
                    Err(e) => {
                        notify(Msg::Files(Err(e)));
                        return;
                    }
                };
                let entries: Vec<FileEntry> = set.entries.clone();
                notify(Msg::Files(Ok(set)));
                // Load the file the UI wants first, then the rest in order.
                let n = entries.len();
                let mut done = vec![false; n];
                for _ in 0..n {
                    let w = wanted.load(Ordering::Relaxed);
                    let i = if w < n && !done[w] {
                        w
                    } else {
                        done.iter().position(|d| !d).unwrap()
                    };
                    let result = load_pair(&entries[i], mode);
                    done[i] = true;
                    notify(Msg::Loaded { file: i, result });
                }
            })
            .expect("spawn loader thread");
    }

    let event_loop = {
        let _s = trace::span("event-loop-create");
        let mut builder = EventLoop::<()>::with_user_event();
        builder
            .with_activation_policy(ActivationPolicy::Regular)
            .with_default_menu(true)
            .with_activate_ignoring_other_apps(true);
        builder.build().expect("create event loop")
    };
    event_loop.set_control_flow(ControlFlow::Wait);
    let proxy = event_loop.create_proxy();
    let _ = proxy_tx.send(proxy.clone());

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
    trace::mark("run-app");
    if let Err(e) = event_loop.run_app(&mut app) {
        eprintln!("diffvader: event loop error: {e}");
        std::process::exit(1);
    }
}

fn load_pair(entry: &FileEntry, mode: WhitespaceMode) -> Result<Loaded, String> {
    let _s = trace::span("load-pair");
    let load = |p: &Option<PathBuf>| -> Result<FileData, String> {
        match p {
            Some(p) => FileData::load(p).map_err(|e| format!("{}: {e}", p.display())),
            None => Ok(FileData::empty()),
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

enum Parsed {
    Run(Options),
    /// Delegate to `git difftool -d` with these extra arguments; the second list holds our
    /// own options to forward to the child process.
    Git(Vec<String>, Vec<String>),
}

/// Options given before `--git` reach the re-executed child through this variable, because
/// `git difftool --dir-diff --extcmd` runs the command without shell parsing.
const OPTS_ENV: &str = "DIFFVADER_OPTS";

fn parse_args() -> Result<Parsed, String> {
    let inherited: Vec<String> = std::env::var(OPTS_ENV)
        .map(|v| {
            v.split('\x1f')
                .filter(|s| !s.is_empty())
                .map(String::from)
                .collect()
        })
        .unwrap_or_default();
    let mut args = inherited.into_iter().chain(std::env::args().skip(1));
    let mut passthrough: Vec<String> = Vec::new();
    let mut paths: Vec<PathBuf> = Vec::new();
    let mut font = None;
    let mut font_pt = 13.0f32;
    let mut tab_width = 4u32;
    let mut whitespace = WhitespaceMode::Exact;
    let mut light = false;
    let mut timing = std::env::var_os("DIFFVADER_TIMING").is_some();
    let mut trace_path = std::env::var("DIFFVADER_TRACE").ok();
    let mut screenshot = None;
    let mut quit_after_first_frame = false;
    let mut bench_scroll = None;
    let mut start_row = None;
    let mut git_args: Option<Vec<String>> = None;
    while let Some(a) = args.next() {
        passthrough.push(a.clone());
        let mut value = |name: &str| {
            let v = args.next().ok_or_else(|| format!("{name} needs a value"))?;
            passthrough.push(v.clone());
            Ok::<String, String>(v)
        };
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
                passthrough.pop();
                git_args = Some(args.by_ref().collect());
                break;
            }
            "-w" | "--ignore-all-space" => whitespace = WhitespaceMode::IgnoreAll,
            "-b" | "--ignore-space-change" => whitespace = WhitespaceMode::IgnoreChange,
            "--ignore-space-at-eol" => whitespace = WhitespaceMode::IgnoreEol,
            "--font" => font = Some(PathBuf::from(value("--font")?)),
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
    if let Some(git) = git_args {
        if !paths.is_empty() {
            return Err("--git cannot be combined with file arguments".into());
        }
        return Ok(Parsed::Git(git, passthrough));
    }
    if paths.is_empty() {
        return Ok(Parsed::Git(Vec::new(), passthrough));
    }
    if paths.len() != 2 {
        return Err("expected exactly two files or directories".into());
    }
    let right = paths.pop().unwrap();
    let left = paths.pop().unwrap();
    let (left_title, right_title) = titles(&left, &right);
    Ok(Parsed::Run(Options {
        left,
        right,
        left_title,
        right_title,
        font,
        font_pt: font_pt.clamp(6.0, 48.0),
        tab_width,
        whitespace,
        light,
        timing,
        trace_path,
        screenshot,
        quit_after_first_frame,
        bench_scroll,
        start_row,
    }))
}

/// Re-executes through `git difftool --dir-diff` so git prepares both trees and calls us
/// back with two directories. Never returns.
fn run_git_difftool(args: Vec<String>, own: Vec<String>) -> ! {
    let status = std::process::Command::new("git")
        .env(OPTS_ENV, own.join("\x1f"))
        .arg("difftool")
        .arg("--dir-diff")
        .arg("--no-prompt")
        .arg(format!("--extcmd={}", exe_path()))
        .args(&args)
        .status();
    match status {
        Ok(s) => std::process::exit(s.code().unwrap_or(1)),
        Err(e) => {
            eprintln!("diffvader: cannot run git: {e}");
            std::process::exit(1);
        }
    }
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
        "[diff]\n\ttool = diffvader\n[difftool]\n\tprompt = false\n[difftool \"diffvader\"]\n\tcmd = {exe} \"$LOCAL\" \"$REMOTE\"\n\n# then: git difftool -d [<commit>...]   (or just: diffvader --git [<commit>...])\n"
    )
}

fn install_git() {
    let exe = exe_path();
    let settings = [
        ("diff.tool", "diffvader".to_string()),
        ("difftool.prompt", "false".to_string()),
        (
            "difftool.diffvader.cmd",
            format!("{exe} \"$LOCAL\" \"$REMOTE\""),
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
    println!("done. run `git difftool -d` (or `diffvader --git HEAD~1`) to use it.");
}
