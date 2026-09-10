//! Lightweight in-process telemetry.
//!
//! Every interesting segment of the program is wrapped in a [`span`] guard (or stamped with
//! [`mark`]). Recording is always on and costs roughly one mutex lock + a 40-byte push per
//! event, which is cheap enough to leave in hot paths like frame construction. Nothing is
//! written anywhere unless `--trace` / `--timing` ask for it at exit.
//!
//! Timestamps are relative to *process start* (from `proc_pidinfo`), not `main`, so the
//! reported cold-start numbers include dyld and runtime init.

use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Clone, Copy, Debug)]
enum Kind {
    Span { dur_us: u32 },
    Mark,
}

#[derive(Clone, Copy, Debug)]
struct Event {
    name: &'static str,
    kind: Kind,
    ts_us: u64,
    tid: u16,
    arg: u64,
}

struct State {
    events: Vec<Event>,
    /// Frame durations (build + submit) in microseconds, for the exit summary.
    frames: Vec<u32>,
}

static STATE: Mutex<State> = Mutex::new(State {
    events: Vec::new(),
    frames: Vec::new(),
});

/// `Instant` corresponding to process start, computed once at [`init`].
static ORIGIN: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();

const MAX_EVENTS: usize = 2_000_000;

/// Must be called first thing in `main`. Anchors the clock to the kernel's process start time
/// so pre-main work is visible.
pub fn init() {
    let now = Instant::now();
    let pre_main = process_start_offset().unwrap_or(Duration::ZERO);
    let origin = now.checked_sub(pre_main).unwrap_or(now);
    let _ = ORIGIN.set(origin);
    record(Event {
        name: "pre-main",
        kind: Kind::Span {
            dur_us: pre_main.as_micros() as u32,
        },
        ts_us: 0,
        tid: tid(),
        arg: 0,
    });
}

/// Wall-clock time elapsed between the kernel starting this process and now.
fn process_start_offset() -> Option<Duration> {
    let mut info: libc::proc_taskallinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_taskallinfo>() as libc::c_int;
    let n = unsafe {
        libc::proc_pidinfo(
            std::process::id() as libc::c_int,
            libc::PROC_PIDTASKALLINFO,
            0,
            &mut info as *mut _ as *mut libc::c_void,
            size,
        )
    };
    if n != size {
        return None;
    }
    let start = Duration::new(
        info.pbsd.pbi_start_tvsec,
        (info.pbsd.pbi_start_tvusec * 1000) as u32,
    );
    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?;
    now.checked_sub(start)
}

fn now_us() -> u64 {
    let origin = ORIGIN.get().copied().unwrap_or_else(Instant::now);
    Instant::now().saturating_duration_since(origin).as_micros() as u64
}

/// Microseconds since process start; handy for ad-hoc logging.
pub fn elapsed_us() -> u64 {
    now_us()
}

fn tid() -> u16 {
    thread_local! {
        static TID: u16 = {
            static NEXT: std::sync::atomic::AtomicU16 = std::sync::atomic::AtomicU16::new(1);
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        };
    }
    TID.with(|t| *t)
}

fn record(ev: Event) {
    let mut st = STATE.lock().unwrap();
    if st.events.len() < MAX_EVENTS {
        st.events.push(ev);
    }
}

/// RAII guard that records a complete span when dropped.
pub struct Span {
    name: &'static str,
    start: Instant,
    start_us: u64,
    arg: u64,
}

#[inline]
pub fn span(name: &'static str) -> Span {
    span_arg(name, 0)
}

/// Like [`span`] with a numeric argument (row count, byte count, ...) attached for the trace.
#[inline]
pub fn span_arg(name: &'static str, arg: u64) -> Span {
    Span {
        name,
        start: Instant::now(),
        start_us: now_us(),
        arg,
    }
}

impl Drop for Span {
    fn drop(&mut self) {
        let dur = self.start.elapsed();
        record(Event {
            name: self.name,
            kind: Kind::Span {
                dur_us: dur.as_micros().min(u32::MAX as u128) as u32,
            },
            ts_us: self.start_us,
            tid: tid(),
            arg: self.arg,
        });
    }
}

/// Records an instantaneous event.
pub fn mark(name: &'static str) {
    record(Event {
        name,
        kind: Kind::Mark,
        ts_us: now_us(),
        tid: tid(),
        arg: 0,
    });
}

/// Records the duration of one rendered frame for the exit summary.
pub fn frame_done(dur: Duration) {
    let mut st = STATE.lock().unwrap();
    if st.frames.len() < MAX_EVENTS {
        st.frames.push(dur.as_micros().min(u32::MAX as u128) as u32);
    }
}

/// Writes all recorded events as Chrome trace event JSON.
pub fn write_chrome_trace(path: &str) -> std::io::Result<()> {
    use std::io::Write;
    let st = STATE.lock().unwrap();
    let mut out = std::io::BufWriter::new(std::fs::File::create(path)?);
    out.write_all(b"{\"traceEvents\":[\n")?;
    let pid = std::process::id();
    let mut first = true;
    for ev in &st.events {
        if !first {
            out.write_all(b",\n")?;
        }
        first = false;
        match ev.kind {
            Kind::Span { dur_us } => write!(
                out,
                "{{\"name\":\"{}\",\"ph\":\"X\",\"ts\":{},\"dur\":{},\"pid\":{},\"tid\":{},\"args\":{{\"arg\":{}}}}}",
                ev.name, ev.ts_us, dur_us, pid, ev.tid, ev.arg
            )?,
            Kind::Mark => write!(
                out,
                "{{\"name\":\"{}\",\"ph\":\"i\",\"s\":\"g\",\"ts\":{},\"pid\":{},\"tid\":{}}}",
                ev.name, ev.ts_us, pid, ev.tid
            )?,
        }
    }
    out.write_all(b"\n]}\n")?;
    out.flush()
}

/// Prints a human-readable startup timeline and frame statistics to stderr.
pub fn print_summary() {
    let st = STATE.lock().unwrap();
    eprintln!("--- diffvader timing (ms since process start) ---");
    // Startup timeline: every span/mark recorded before the first frame was presented.
    let first_frame = st
        .events
        .iter()
        .find(|e| e.name == "first-frame-presented")
        .map(|e| e.ts_us)
        .unwrap_or(u64::MAX);
    let mut startup: Vec<&Event> = st
        .events
        .iter()
        .filter(|e| e.ts_us <= first_frame)
        .collect();
    startup.sort_by_key(|e| e.ts_us);
    for ev in startup {
        match ev.kind {
            Kind::Span { dur_us } => eprintln!(
                "{:>9.2}  {:<32} {:>8.2} ms{}",
                ev.ts_us as f64 / 1000.0,
                ev.name,
                dur_us as f64 / 1000.0,
                if ev.arg != 0 {
                    format!("  ({})", ev.arg)
                } else {
                    String::new()
                }
            ),
            Kind::Mark => eprintln!(
                "{:>9.2}  {:<32}     mark",
                ev.ts_us as f64 / 1000.0,
                ev.name
            ),
        }
    }
    if !st.frames.is_empty() {
        let mut f = st.frames.clone();
        f.sort_unstable();
        let pct = |p: f64| f[((f.len() - 1) as f64 * p) as usize] as f64 / 1000.0;
        eprintln!(
            "frames: {}  p50 {:.2} ms  p95 {:.2} ms  p99 {:.2} ms  max {:.2} ms",
            f.len(),
            pct(0.5),
            pct(0.95),
            pct(0.99),
            pct(1.0)
        );
    }
}
