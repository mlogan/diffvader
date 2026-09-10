mod app;
mod diff;
mod font;
mod gpu;
mod keys;
mod text;
mod theme;
mod trace;

use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc};

use winit::event_loop::{ControlFlow, EventLoop};
use winit::platform::macos::{ActivationPolicy, EventLoopBuilderExtMacOS};

use crate::app::{App, Loaded, Msg, Options};
use crate::diff::WhitespaceMode;
use crate::text::FileData;

const USAGE: &str = "\
diffvader — fast side-by-side diff viewer

usage: diffvader [options] LEFT RIGHT

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
      --git-config             print the git config needed to use diffvader as a difftool
      --install-git            write that config to ~/.gitconfig (git config --global)
  -h, --help                   show this help
  -V, --version                show version

keys: press ? inside the app.
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
    // The loader starts before the event loop exists; it gets the proxy (to wake the loop)
    // through this side channel once the loop has been created.
    let (proxy_tx, proxy_rx) = mpsc::channel();
    {
        let tx = tx.clone();
        let left = opts.left.clone();
        let right = opts.right.clone();
        let mode = opts.whitespace;
        std::thread::Builder::new()
            .name("loader".into())
            .spawn(move || {
                let _s = trace::span("load-and-diff");
                let result = (|| {
                    let a = load(&left)?;
                    let b = load(&right)?;
                    if a.binary || b.binary {
                        return Err("binary files differ".to_string());
                    }
                    let d = diff::diff_files(&a, &b, mode);
                    Ok::<_, String>(Loaded {
                        left: Arc::new(a),
                        right: Arc::new(b),
                        diff: d,
                    })
                })();
                let _ = tx.send(Msg::Loaded(result));
                drop(_s);
                if let Ok(proxy) = proxy_rx.recv() {
                    let proxy: winit::event_loop::EventLoopProxy<()> = proxy;
                    let _ = proxy.send_event(());
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

    let mut app = match App::new(opts, rx, tx, proxy, font_thread, gpu_thread) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("diffvader: {e}");
            std::process::exit(1);
        }
    };
    trace::mark("run-app");
    if let Err(e) = event_loop.run_app(&mut app) {
        eprintln!("diffvader: event loop error: {e}");
        std::process::exit(1);
    }
}

fn load(path: &Path) -> Result<FileData, String> {
    if path == Path::new("/dev/null") || !path.exists() {
        return Ok(FileData::empty());
    }
    FileData::load(path).map_err(|e| format!("{}: {e}", path.display()))
}

fn parse_args() -> Result<Options, String> {
    let mut args = std::env::args().skip(1);
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
            s if s.starts_with('-') && s.len() > 1 => return Err(format!("unknown option {s}")),
            _ => paths.push(PathBuf::from(a)),
        }
    }
    if paths.len() != 2 {
        return Err("expected exactly two files".into());
    }
    let right = paths.pop().unwrap();
    let left = paths.pop().unwrap();
    let (left_title, right_title) = titles(&left, &right);
    Ok(Options {
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
        "[diff]\n\ttool = diffvader\n[difftool]\n\tprompt = false\n[difftool \"diffvader\"]\n\tcmd = {exe} \"$LOCAL\" \"$REMOTE\"\n"
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
    println!("done. run `git difftool` (or `git difftool HEAD~1`) to use it.");
}
