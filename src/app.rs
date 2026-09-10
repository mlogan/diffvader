//! Application state, layout, input handling and draw-list construction.

use std::collections::HashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc};
use std::time::Instant;

use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoopProxy};
use winit::keyboard::ModifiersState;
use winit::window::{Window, WindowId};

use crate::diff::{self, DiffResult, IntraDiff, Row, RowKind, WhitespaceMode, NONE};
use crate::font::{Atlas, FontSet};
use crate::gpu::{DrawList, Gpu, GpuCore, SurfaceProblem};
use crate::keys::{Action, KeyInput, Vi};
use crate::text::FileData;
use crate::theme::{self, Theme};
use crate::trace;

pub struct Options {
    pub left: PathBuf,
    pub right: PathBuf,
    pub left_title: String,
    pub right_title: String,
    pub font: Option<PathBuf>,
    pub font_pt: f32,
    pub tab_width: u32,
    pub whitespace: WhitespaceMode,
    pub light: bool,
    pub timing: bool,
    pub trace_path: Option<String>,
    pub screenshot: Option<PathBuf>,
    pub quit_after_first_frame: bool,
}

pub struct Loaded {
    pub left: Arc<FileData>,
    pub right: Arc<FileData>,
    pub diff: DiffResult,
}

pub enum Msg {
    Loaded(Result<Loaded, String>),
    Rediff(DiffResult),
}

enum State {
    Loading,
    Ready(Loaded),
    Failed(String),
}

const SCROLLOFF: usize = 3;

pub struct App {
    opts: Options,
    theme: Theme,
    dark: bool,
    rx: mpsc::Receiver<Msg>,
    proxy: EventLoopProxy<()>,
    tx: mpsc::Sender<Msg>,
    window: Option<Arc<Window>>,
    gpu: Option<Gpu>,
    gpu_core: Option<std::thread::JoinHandle<Result<GpuCore, String>>>,
    fonts: FontSet,
    atlas: Atlas,
    scale: f64,
    font_pt: f32,
    cell_w: f32,
    line_h: f32,
    state: State,
    ws_mode: WhitespaceMode,
    /// Pixels; fractional so trackpad scrolling is smooth.
    scroll_y: f64,
    /// Cells.
    scroll_x: f64,
    cursor: usize,
    vi: Vi,
    modifiers: ModifiersState,
    intra_cache: HashMap<u32, Option<IntraDiff>>,
    search: Option<String>,
    draw: DrawList,
    help: bool,
    message: Option<String>,
    first_frame_done: bool,
    first_diff_frame_done: bool,
    /// Set by render when a debug flag asks to exit after the first frame.
    want_exit: bool,
}

/// Per-frame pixel geometry (physical pixels).
#[derive(Clone, Copy, Debug)]
struct Layout {
    w: f32,
    h: f32,
    header_h: f32,
    status_h: f32,
    text_top: f32,
    text_h: f32,
    /// Full rows that fit in the text area.
    rows_visible: usize,
    pane_w: f32,
    gutter_w: f32,
    /// Left edge of each pane and of its text column (after the gutter).
    pane_x: [f32; 2],
    text_x: [f32; 2],
    cols_visible: i64,
    scrollbar_w: f32,
}

impl App {
    pub fn new(
        opts: Options,
        rx: mpsc::Receiver<Msg>,
        tx: mpsc::Sender<Msg>,
        proxy: EventLoopProxy<()>,
        fonts: std::thread::JoinHandle<Result<FontSet, String>>,
        gpu_core: std::thread::JoinHandle<Result<GpuCore, String>>,
    ) -> Result<App, String> {
        let fonts = {
            let _s = trace::span("font-join");
            fonts
                .join()
                .map_err(|_| "font thread panicked".to_string())??
        };
        let atlas = Atlas::new(opts.font_pt * 2.0, 1.0, 1.0, 1.0);
        let dark = !opts.light;
        Ok(App {
            theme: if dark { theme::DARK } else { theme::LIGHT },
            dark,
            ws_mode: opts.whitespace,
            font_pt: opts.font_pt,
            opts,
            rx,
            tx,
            proxy,
            window: None,
            gpu: None,
            gpu_core: Some(gpu_core),
            fonts,
            atlas,
            scale: 2.0,
            cell_w: 1.0,
            line_h: 1.0,
            state: State::Loading,
            scroll_y: 0.0,
            scroll_x: 0.0,
            cursor: 0,
            vi: Vi::new(),
            modifiers: ModifiersState::empty(),
            intra_cache: HashMap::new(),
            search: None,
            draw: DrawList::default(),
            help: false,
            message: None,
            first_frame_done: false,
            first_diff_frame_done: false,
            want_exit: false,
        })
    }

    fn rebuild_font(&mut self) {
        let _s = trace::span("font-rebuild");
        let px = (self.font_pt as f64 * self.scale) as f32;
        let (cell_w, line_h, ascent) = self.fonts.metrics(px);
        // Slightly looser leading than the font's own reads better for code.
        let line_h = (line_h * 1.12).ceil();
        self.cell_w = cell_w;
        self.line_h = line_h;
        self.atlas = Atlas::new(
            px,
            cell_w,
            line_h,
            ascent + ((line_h - ascent) * 0.25).floor(),
        );
    }

    fn rows(&self) -> &[Row] {
        match &self.state {
            State::Ready(l) => &l.diff.rows,
            _ => &[],
        }
    }

    fn layout(&self) -> Layout {
        let (w, h) = self.gpu.as_ref().map(|g| g.size()).unwrap_or((1, 1));
        let (w, h) = (w as f32, h as f32);
        let s = self.scale as f32;
        let header_h = (self.line_h + 8.0 * s).round();
        let status_h = (self.line_h + 8.0 * s).round();
        let text_top = header_h;
        let text_h = (h - header_h - status_h).max(0.0);
        let digits = match &self.state {
            State::Ready(l) => {
                let n = l.left.line_count().max(l.right.line_count()).max(1);
                (n as f64).log10().floor() as usize + 1
            }
            _ => 3,
        };
        let gutter_w = ((digits + 2) as f32 * self.cell_w + 4.0 * s).round();
        let scrollbar_w = (10.0 * s).round();
        let divider_w = (2.0 * s).round();
        let pane_w = ((w - divider_w - scrollbar_w) / 2.0).floor();
        let pane_x = [0.0, pane_w + divider_w];
        let text_pad = (self.cell_w * 0.5).round();
        let text_x = [
            pane_x[0] + gutter_w + text_pad,
            pane_x[1] + gutter_w + text_pad,
        ];
        let text_w = (pane_w - gutter_w - text_pad).max(0.0);
        Layout {
            w,
            h,
            header_h,
            status_h,
            text_top,
            text_h,
            rows_visible: ((text_h / self.line_h).floor() as usize).max(1),
            pane_w,
            gutter_w,
            pane_x,
            text_x,
            cols_visible: (text_w / self.cell_w).floor() as i64,
            scrollbar_w,
        }
    }

    // ---- scrolling / cursor -------------------------------------------------------------

    fn first_row(&self) -> usize {
        (self.scroll_y / self.line_h as f64).floor() as usize
    }

    fn max_scroll(&self) -> f64 {
        let n = self.rows().len();
        n.saturating_sub(1) as f64 * self.line_h as f64
    }

    fn clamp_scroll(&mut self) {
        self.scroll_y = self.scroll_y.clamp(0.0, self.max_scroll());
        self.scroll_x = self.scroll_x.max(0.0);
    }

    fn set_first_row(&mut self, row: i64) {
        self.scroll_y = row.max(0) as f64 * self.line_h as f64;
        self.clamp_scroll();
    }

    fn clamp_cursor(&mut self) {
        let n = self.rows().len();
        self.cursor = self.cursor.min(n.saturating_sub(1));
    }

    fn scrolloff(&self, lay: &Layout) -> usize {
        SCROLLOFF.min(lay.rows_visible.saturating_sub(1) / 2)
    }

    /// Scrolls minimally so the cursor is inside the scrolloff margins; big jumps center it.
    fn ensure_cursor_visible(&mut self, lay: &Layout, old_cursor: usize) {
        let vis = lay.rows_visible;
        let so = self.scrolloff(lay);
        let first = self.first_row();
        let last = first + vis - 1;
        let far = self.cursor.abs_diff(old_cursor) > vis;
        if far && (self.cursor < first || self.cursor > last) {
            self.set_first_row(self.cursor as i64 - (vis / 2) as i64);
        } else if self.cursor < first + so {
            self.set_first_row(self.cursor as i64 - so as i64);
        } else if self.cursor + so > last {
            self.set_first_row((self.cursor + so + 1) as i64 - vis as i64);
        }
    }

    /// Moves the cursor into the visible range after the view scrolled.
    fn clamp_cursor_to_view(&mut self, lay: &Layout) {
        let n = self.rows().len();
        if n == 0 {
            return;
        }
        let vis = lay.rows_visible;
        let so = self.scrolloff(lay);
        let first = self.first_row();
        let last = (first + vis - 1).min(n - 1);
        let lo = (first + so).min(last);
        let hi = last.saturating_sub(so).max(lo);
        self.cursor = self.cursor.clamp(lo, hi);
    }

    fn go_to_row(&mut self, lay: &Layout, row: usize) {
        let old = self.cursor;
        self.cursor = row;
        self.clamp_cursor();
        self.ensure_cursor_visible(lay, old);
    }

    /// Like `go_to_row`, but centers the view whenever the target is not already visible.
    fn jump_to_row(&mut self, lay: &Layout, row: usize) {
        self.cursor = row;
        self.clamp_cursor();
        let first = self.first_row();
        let so = self.scrolloff(lay);
        let visible = self.cursor >= first + so && self.cursor + so < first + lay.rows_visible;
        if visible {
            return;
        }
        self.set_first_row(self.cursor as i64 - (lay.rows_visible / 2) as i64);
    }

    // ---- actions ------------------------------------------------------------------------

    fn apply(&mut self, action: Action, el: &ActiveEventLoop) {
        let lay = self.layout();
        let n = self.rows().len();
        match action {
            Action::MoveCursor(d) => {
                let old = self.cursor;
                self.cursor =
                    (self.cursor as i64 + d).clamp(0, n.saturating_sub(1) as i64) as usize;
                self.ensure_cursor_visible(&lay, old);
            }
            Action::ScrollLines(d) => {
                self.set_first_row(self.first_row() as i64 + d);
                self.clamp_cursor_to_view(&lay);
            }
            Action::HalfPage(d) => {
                let amt = d * (lay.rows_visible / 2).max(1) as i64;
                self.set_first_row(self.first_row() as i64 + amt);
                self.cursor =
                    (self.cursor as i64 + amt).clamp(0, n.saturating_sub(1) as i64) as usize;
                self.clamp_cursor_to_view(&lay);
            }
            Action::Page(d) => {
                let amt = d * lay.rows_visible.saturating_sub(2).max(1) as i64;
                self.set_first_row(self.first_row() as i64 + amt);
                self.cursor =
                    (self.cursor as i64 + amt).clamp(0, n.saturating_sub(1) as i64) as usize;
                self.clamp_cursor_to_view(&lay);
            }
            Action::GoTop => self.go_to_row(&lay, 0),
            Action::GoBottom => self.go_to_row(&lay, n.saturating_sub(1)),
            Action::GoRow(r) => self.go_to_row(&lay, (r.max(1) - 1) as usize),
            Action::NextHunk(k) => self.jump_hunk(&lay, k as i64),
            Action::PrevHunk(k) => self.jump_hunk(&lay, -(k as i64)),
            Action::FirstHunk => {
                if let Some(h) = self.hunks().first() {
                    let r = h.rows.start as usize;
                    self.jump_to_row(&lay, r);
                }
            }
            Action::LastHunk => {
                if let Some(h) = self.hunks().last() {
                    let r = h.rows.start as usize;
                    self.jump_to_row(&lay, r);
                }
            }
            Action::ScrollCols(d) => {
                self.scroll_x = (self.scroll_x.round() + d as f64).max(0.0);
            }
            Action::ColsHome => self.scroll_x = 0.0,
            Action::ColsEnd => {
                let widest = self.widest_visible_line(&lay);
                self.scroll_x = (widest - lay.cols_visible).max(0) as f64;
            }
            Action::CursorToTop => self.set_first_row(self.cursor as i64),
            Action::CursorToCenter => {
                self.set_first_row(self.cursor as i64 - (lay.rows_visible / 2) as i64)
            }
            Action::CursorToBottom => {
                self.set_first_row(self.cursor as i64 - lay.rows_visible as i64 + 1)
            }
            Action::CycleWhitespace => {
                let m = self.ws_mode.next();
                self.set_whitespace(m);
            }
            Action::SetWhitespace(name) => {
                let m = match name {
                    "eol" => WhitespaceMode::IgnoreEol,
                    "change" => WhitespaceMode::IgnoreChange,
                    "all" => WhitespaceMode::IgnoreAll,
                    _ => WhitespaceMode::Exact,
                };
                self.set_whitespace(m);
            }
            Action::Search(p) => {
                self.search = Some(p);
                self.search_step(&lay, 1, 1);
            }
            Action::SearchNext(k) => self.search_step(&lay, 1, k),
            Action::SearchPrev(k) => self.search_step(&lay, -1, k),
            Action::ZoomIn => self.set_font_pt(self.font_pt + 1.0),
            Action::ZoomOut => self.set_font_pt(self.font_pt - 1.0),
            Action::ZoomReset => self.set_font_pt(self.opts.font_pt),
            Action::ToggleTheme => {
                self.dark = !self.dark;
                self.theme = if self.dark { theme::DARK } else { theme::LIGHT };
            }
            Action::ToggleHelp => self.help = !self.help,
            Action::Quit => el.exit(),
            Action::Message(m) => {
                self.message = if m.is_empty() { None } else { Some(m) };
            }
        }
        self.clamp_scroll();
        self.clamp_cursor();
    }

    fn hunks(&self) -> &[diff::Hunk] {
        match &self.state {
            State::Ready(l) => &l.diff.hunks,
            _ => &[],
        }
    }

    fn jump_hunk(&mut self, lay: &Layout, delta: i64) {
        let State::Ready(l) = &self.state else { return };
        let hunks = &l.diff.hunks;
        if hunks.is_empty() {
            self.message = Some("no changes".into());
            return;
        }
        let cur = self.cursor as u32;
        let at = l.diff.hunk_at_or_before(cur);
        // Position relative to hunk starts: `at` is the hunk we are in or just after.
        let target = if delta > 0 {
            let base = match at {
                None => -1,
                Some(i) => i as i64,
            };
            base + delta
        } else {
            let base = match at {
                None => 0,
                Some(i) if hunks[i].rows.start < cur => i as i64 + 1,
                Some(i) => i as i64,
            };
            base + delta
        };
        if target < 0 || target >= hunks.len() as i64 {
            self.message = Some(if delta > 0 {
                "no next change".into()
            } else {
                "no previous change".into()
            });
            let clamped = target.clamp(0, hunks.len() as i64 - 1) as usize;
            let row = hunks[clamped].rows.start as usize;
            if row != self.cursor {
                self.jump_to_row(lay, row);
            }
            return;
        }
        let row = hunks[target as usize].rows.start as usize;
        self.jump_to_row(lay, row);
    }

    fn widest_visible_line(&self, lay: &Layout) -> i64 {
        let State::Ready(l) = &self.state else {
            return 0;
        };
        let first = self.first_row();
        let mut widest = 0;
        for row in l.diff.rows.iter().skip(first).take(lay.rows_visible + 1) {
            if row.left != NONE {
                widest = widest.max(line_cols(
                    l.left.line(row.left as usize),
                    self.opts.tab_width as i64,
                ));
            }
            if row.right != NONE {
                widest = widest.max(line_cols(
                    l.right.line(row.right as usize),
                    self.opts.tab_width as i64,
                ));
            }
        }
        widest
    }

    fn set_whitespace(&mut self, mode: WhitespaceMode) {
        if mode == self.ws_mode {
            return;
        }
        self.ws_mode = mode;
        self.message = Some(format!("whitespace: {}", mode.label()));
        let State::Ready(l) = &self.state else { return };
        let (a, b) = (l.left.clone(), l.right.clone());
        let tx = self.tx.clone();
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            let d = diff::diff_files(&a, &b, mode);
            let _ = tx.send(Msg::Rediff(d));
            let _ = proxy.send_event(());
        });
    }

    fn set_font_pt(&mut self, pt: f32) {
        let pt = pt.clamp(6.0, 48.0);
        if pt == self.font_pt {
            return;
        }
        let lay = self.layout();
        let cursor_offset = self
            .cursor
            .saturating_sub(self.first_row())
            .min(lay.rows_visible);
        self.font_pt = pt;
        self.rebuild_font();
        self.set_first_row(self.cursor as i64 - cursor_offset as i64);
    }

    fn search_step(&mut self, lay: &Layout, dir: i64, count: u64) {
        let Some(pat) = self.search.clone() else {
            self.message = Some("no previous search".into());
            return;
        };
        let State::Ready(l) = &self.state else { return };
        let _s = trace::span("search");
        let n = l.diff.rows.len();
        if n == 0 {
            return;
        }
        let smart = pat.chars().any(|c| c.is_uppercase());
        let needle_lower = pat.to_lowercase();
        let finder = memchr::memmem::Finder::new(if smart {
            pat.as_bytes()
        } else {
            needle_lower.as_bytes()
        });
        let mut lower_buf = Vec::new();
        let mut matches = |line: &[u8]| -> bool {
            if smart {
                finder.find(line).is_some()
            } else {
                lower_buf.clear();
                lower_buf.extend(line.iter().map(|b| b.to_ascii_lowercase()));
                finder.find(&lower_buf).is_some()
            }
        };
        let mut row = self.cursor;
        let mut found = None;
        'outer: for _ in 0..count {
            for _ in 0..n {
                row = (row as i64 + dir).rem_euclid(n as i64) as usize;
                let r = l.diff.rows[row];
                let hit = (r.left != NONE && matches(l.left.line(r.left as usize)))
                    || (r.right != NONE && matches(l.right.line(r.right as usize)));
                if hit {
                    found = Some(row);
                    continue 'outer;
                }
            }
            break;
        }
        match found {
            Some(r) => {
                if (dir > 0 && r <= self.cursor) || (dir < 0 && r >= self.cursor) {
                    self.message = Some("search wrapped".into());
                }
                self.go_to_row(lay, r);
            }
            None => self.message = Some(format!("pattern not found: {pat}")),
        }
    }

    // ---- messages from background threads ------------------------------------------------

    fn drain_messages(&mut self) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::Loaded(Ok(l)) => {
                    trace::mark("diff-received");
                    self.ws_mode = l.diff.mode;
                    self.state = State::Ready(l);
                    self.intra_cache.clear();
                    self.cursor = 0;
                    // Land on the first change, like vimdiff does.
                    let lay = self.layout();
                    if let Some(h) = self.hunks().first() {
                        let r = h.rows.start as usize;
                        self.jump_to_row(&lay, r);
                    }
                }
                Msg::Loaded(Err(e)) => self.state = State::Failed(e),
                Msg::Rediff(d) => {
                    if d.mode != self.ws_mode {
                        continue;
                    }
                    let cursor = self.cursor;
                    let first_row = self.first_row();
                    let State::Ready(l) = &mut self.state else {
                        continue;
                    };
                    let old = l.diff.rows.get(cursor).copied();
                    let old_first = l.diff.rows.get(first_row).copied();
                    l.diff = d;
                    self.intra_cache.clear();
                    let rows = &l.diff.rows;
                    let remap = |r: Option<Row>| -> Option<usize> {
                        let r = r?;
                        if r.left != NONE {
                            Some(row_for_line(rows, true, r.left))
                        } else if r.right != NONE {
                            Some(row_for_line(rows, false, r.right))
                        } else {
                            None
                        }
                    };
                    let new_cursor = remap(old).unwrap_or(0);
                    let new_first = remap(old_first).unwrap_or(0);
                    self.cursor = new_cursor;
                    self.set_first_row(new_first as i64);
                    self.clamp_cursor();
                    let lay = self.layout();
                    self.clamp_cursor_to_view(&lay);
                }
            }
        }
    }

    // ---- rendering ----------------------------------------------------------------------

    fn render(&mut self) {
        let Some(window) = self.window.clone() else {
            return;
        };
        if self.gpu.is_none() {
            return;
        }
        let frame_start = Instant::now();
        let _s = trace::span("frame");
        let lay = self.layout();
        for attempt in 0..2 {
            let _b = trace::span("frame-build");
            self.draw.clear();
            build_frame(self, &lay);
            self.draw.finish();
            if self.atlas.reset && attempt == 0 {
                // Glyph references emitted before the reset are stale; rebuild once.
                self.atlas.reset = false;
                continue;
            }
            self.atlas.reset = false;
            break;
        }
        let gpu = self.gpu.as_mut().unwrap();
        gpu.sync_atlas(&mut self.atlas);
        let clear = theme::to_f64(self.theme.bg);
        match gpu.render(&self.draw, clear) {
            Ok(()) => {
                if !self.first_frame_done {
                    self.first_frame_done = true;
                    trace::mark("first-frame-presented");
                }
                if !self.first_diff_frame_done
                    && matches!(self.state, State::Ready(_) | State::Failed(_))
                {
                    self.first_diff_frame_done = true;
                    trace::mark("first-diff-frame-presented");
                    if self.opts.timing {
                        eprintln!(
                            "diffvader: first diff frame presented {:.1} ms after process start",
                            trace::elapsed_us() as f64 / 1000.0
                        );
                    }
                    if let Some(path) = self.opts.screenshot.clone() {
                        let (w, h, rgba) = gpu.render_to_image(&self.draw, clear);
                        match write_bmp(&path, w, h, &rgba) {
                            Ok(()) => eprintln!("diffvader: wrote screenshot {}", path.display()),
                            Err(e) => eprintln!("diffvader: screenshot failed: {e}"),
                        }
                        self.want_exit = true;
                    }
                    if self.opts.quit_after_first_frame {
                        self.want_exit = true;
                    }
                }
            }
            Err(SurfaceProblem::Reconfigure) => {
                let size = window.inner_size();
                gpu.resize(size.width, size.height);
                window.request_redraw();
            }
            Err(SurfaceProblem::Timeout) => window.request_redraw(),
            Err(SurfaceProblem::Fatal) => eprintln!("diffvader: surface validation error"),
        }
        trace::frame_done(frame_start.elapsed());
    }

    fn intra_for(&mut self, row_idx: u32) -> Option<IntraDiff> {
        if let Some(d) = self.intra_cache.get(&row_idx) {
            return d.clone();
        }
        let State::Ready(l) = &self.state else {
            return None;
        };
        let row = l.diff.rows[row_idx as usize];
        let _s = trace::span("intra-diff");
        let d = diff::intra_diff(
            l.left.line(row.left as usize),
            l.right.line(row.right as usize),
            self.ws_mode,
        );
        if self.intra_cache.len() > 8192 {
            self.intra_cache.clear();
        }
        self.intra_cache.insert(row_idx, d.clone());
        d
    }

    fn finish(&mut self) {
        if let Some(p) = &self.opts.trace_path {
            match trace::write_chrome_trace(p) {
                Ok(()) => eprintln!("diffvader: wrote trace to {p}"),
                Err(e) => eprintln!("diffvader: failed to write trace {p}: {e}"),
            }
        }
        if self.opts.timing {
            trace::print_summary();
        }
    }
}

/// Finds the row showing `line` of the given side (or the nearest following row).
fn row_for_line(rows: &[Row], left: bool, line: u32) -> usize {
    let key = |i: usize| -> u32 {
        let mut j = i;
        while j < rows.len() {
            let v = if left { rows[j].left } else { rows[j].right };
            if v != NONE {
                return v;
            }
            j += 1;
        }
        u32::MAX
    };
    let idx = rows.partition_point(|r| {
        let i = (r as *const Row as usize - rows.as_ptr() as usize) / std::mem::size_of::<Row>();
        key(i) < line
    });
    idx.min(rows.len().saturating_sub(1))
}

fn line_cols(bytes: &[u8], tab_width: i64) -> i64 {
    let mut col = 0i64;
    let mut i = 0;
    while i < bytes.len() {
        let (c, w) = decode(&bytes[i..]);
        i += w;
        col += if c == '\t' {
            tab_width - (col % tab_width)
        } else {
            unicode_width::UnicodeWidthChar::width(c)
                .unwrap_or(1)
                .clamp(1, 2) as i64
        };
    }
    col
}

/// Decodes one UTF-8 scalar; invalid bytes become U+FFFD one byte at a time so byte offsets
/// stay meaningful for intra-line ranges.
#[inline]
fn decode(b: &[u8]) -> (char, usize) {
    let b0 = b[0];
    if b0 < 0x80 {
        return (b0 as char, 1);
    }
    let w = if b0 >> 5 == 0b110 {
        2
    } else if b0 >> 4 == 0b1110 {
        3
    } else if b0 >> 3 == 0b11110 {
        4
    } else {
        return ('\u{FFFD}', 1);
    };
    if b.len() < w {
        return ('\u{FFFD}', 1);
    }
    match std::str::from_utf8(&b[..w]) {
        Ok(s) => (s.chars().next().unwrap(), w),
        Err(_) => ('\u{FFFD}', 1),
    }
}

// ---- painting ---------------------------------------------------------------------------

struct Painter<'a> {
    draw: &'a mut DrawList,
    atlas: &'a mut Atlas,
    fonts: &'a mut FontSet,
    cell_w: f32,
    line_h: f32,
    white: [f32; 4],
}

impl Painter<'_> {
    #[inline]
    fn rect(&mut self, x: f32, y: f32, w: f32, h: f32, color: u32) {
        self.draw.rect(x, y, w, h, color, self.white);
    }

    /// Draws one glyph at cell origin (x, y); returns the cells it occupies.
    #[inline]
    fn glyph(&mut self, x: f32, y: f32, c: char, color: u32) -> i64 {
        match self.atlas.glyph(self.fonts, c) {
            Some(g) => {
                self.draw.quads.push(crate::gpu::Quad {
                    pos: [x + g.offset[0], y + g.offset[1]],
                    size: g.size,
                    uv: g.uv,
                    color,
                });
                g.cells as i64
            }
            None => unicode_width::UnicodeWidthChar::width(c)
                .unwrap_or(1)
                .clamp(1, 2) as i64,
        }
    }

    /// Draws a string at (x, y); returns its width in pixels.
    fn text(&mut self, x: f32, y: f32, s: &str, color: u32) -> f32 {
        let mut cx = x;
        for c in s.chars() {
            let cells = if c == ' ' {
                1
            } else {
                self.glyph(cx, y, c, color)
            };
            cx += cells as f32 * self.cell_w;
        }
        cx - x
    }

    fn text_width(&self, s: &str) -> f32 {
        s.chars()
            .map(|c| {
                unicode_width::UnicodeWidthChar::width(c)
                    .unwrap_or(1)
                    .clamp(1, 2) as f32
            })
            .sum::<f32>()
            * self.cell_w
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum WsShow {
    None,
    Trailing,
    Ranges,
    All,
}

struct LineStyle<'a> {
    fg: u32,
    strong: u32,
    ranges: &'a [Range<u32>],
    search: &'a [Range<u32>],
    search_color: u32,
    ws: WsShow,
    ws_color: u32,
    tab_width: i64,
    first_col: i64,
    max_cols: i64,
}

/// Draws one source line starting at `x` (the column-0 origin, already shifted by the
/// horizontal scroll) with highlight ranges, search hits and whitespace markers.
fn draw_line(p: &mut Painter, x: f32, y: f32, bytes: &[u8], st: &LineStyle) {
    let trailing_start = {
        let mut e = bytes.len();
        while e > 0 && diff::is_ws(bytes[e - 1]) {
            e -= 1;
        }
        e
    };
    let mut col = 0i64;
    let mut i = 0usize;
    let mut ri = 0usize;
    let mut si = 0usize;
    let cw = p.cell_w;
    while i < bytes.len() {
        let (c, w) = decode(&bytes[i..]);
        let vis_col = col - st.first_col;
        if vis_col > st.max_cols {
            break;
        }
        let cells = if c == '\t' {
            st.tab_width - (col % st.tab_width)
        } else if c == ' ' {
            1
        } else {
            unicode_width::UnicodeWidthChar::width(c)
                .unwrap_or(1)
                .clamp(1, 2) as i64
        };
        // Advance range cursors past ranges that ended before this byte.
        while ri < st.ranges.len() && st.ranges[ri].end as usize <= i {
            ri += 1;
        }
        while si < st.search.len() && st.search[si].end as usize <= i {
            si += 1;
        }
        let in_range = ri < st.ranges.len() && (st.ranges[ri].start as usize) <= i;
        let in_search = si < st.search.len() && (st.search[si].start as usize) <= i;
        if vis_col + cells > 0 {
            let cx = x + col as f32 * cw;
            let wpx = cells as f32 * cw;
            if in_range {
                p.rect(cx, y, wpx, p.line_h, st.strong);
            }
            if in_search {
                p.rect(cx, y, wpx, p.line_h, st.search_color);
            }
            let is_ws = c == ' ' || c == '\t' || c == '\r';
            if is_ws {
                let show = match st.ws {
                    WsShow::None => false,
                    WsShow::Trailing => i >= trailing_start,
                    WsShow::Ranges => in_range || i >= trailing_start,
                    WsShow::All => true,
                };
                if show {
                    let m = match c {
                        ' ' => '\u{00B7}',
                        '\t' => '\u{2192}',
                        _ => '\u{21B5}',
                    };
                    p.glyph(cx, y, m, st.ws_color);
                }
            } else if (c as u32) < 0x20 || c == '\u{7f}' {
                p.glyph(cx, y, '\u{2400}', st.ws_color);
            } else {
                p.glyph(cx, y, c, st.fg);
            }
        }
        col += cells;
        i += w;
    }
}

fn build_frame(app: &mut App, lay: &Layout) {
    let th = app.theme;
    let s = app.scale as f32;
    let white = app.atlas.white_uv();
    let (cell_w, line_h) = (app.cell_w, app.line_h);
    let tab_width = app.opts.tab_width as i64;
    let first_row = app.first_row();
    let frac = (app.scroll_y - first_row as f64 * line_h as f64) as f32;
    let scroll_x_cells = app.scroll_x.round() as i64;
    let scroll_x_px = app.scroll_x as f32 * cell_w;
    let full = [0, 0, lay.w as u32, lay.h as u32];

    // Pre-compute intra-line diffs for visible modify rows (needs &mut app).
    let mut intra: Vec<(usize, Option<IntraDiff>)> = Vec::new();
    if let State::Ready(_) = &app.state {
        let rows_len = app.rows().len();
        let end = (first_row + lay.rows_visible + 1).min(rows_len);
        for idx in first_row..end {
            let row = app.rows()[idx];
            if row.kind == RowKind::Modify {
                let d = app.intra_for(idx as u32);
                intra.push((idx, d));
            }
        }
    }
    let search = app.search.clone();
    let smart = search
        .as_ref()
        .map(|p| p.chars().any(|c| c.is_uppercase()))
        .unwrap_or(false);
    let search_lower = search.as_ref().map(|p| p.to_lowercase());
    let finder = search.as_ref().map(|p| {
        memchr::memmem::Finder::new(if smart {
            p.as_bytes()
        } else {
            search_lower.as_ref().unwrap().as_bytes()
        })
    });
    let mut lower_buf: Vec<u8> = Vec::new();
    let mut search_hits: Vec<Range<u32>> = Vec::new();

    let App {
        draw,
        atlas,
        fonts,
        state,
        cursor,
        help,
        message,
        vi,
        ws_mode,
        opts,
        ..
    } = app;
    let mut p = Painter {
        draw,
        atlas,
        fonts,
        cell_w,
        line_h,
        white,
    };
    let cursor = *cursor;

    // ---- chrome backgrounds ----
    p.draw.begin(full);
    p.rect(0.0, 0.0, lay.w, lay.header_h, th.status_bg);
    p.rect(0.0, lay.h - lay.status_h, lay.w, lay.status_h, th.status_bg);
    for side in 0..2 {
        p.rect(
            lay.pane_x[side],
            lay.text_top,
            lay.gutter_w,
            lay.text_h,
            th.gutter_bg,
        );
    }
    let divider_x = lay.pane_x[0] + lay.pane_w;
    p.rect(
        divider_x,
        lay.text_top,
        lay.pane_x[1] - divider_x,
        lay.text_h,
        th.divider,
    );

    match state {
        State::Loading => {
            let msg = "loading…";
            let w = p.text_width(msg);
            p.text(
                (lay.w - w) / 2.0,
                lay.text_top + (lay.text_h - line_h) / 2.0,
                msg,
                th.status_dim,
            );
        }
        State::Failed(e) => {
            let msg = format!("error: {e}");
            p.text(lay.text_x[0], lay.text_top + line_h, &msg, th.error_fg);
        }
        State::Ready(l) => {
            let rows = &l.diff.rows;
            let end = (first_row + lay.rows_visible + 1).min(rows.len());
            let ws_mode = *ws_mode;

            // Row backgrounds, hunk bars and line numbers (full-window batch).
            let digits = ((lay.gutter_w - 4.0 * s) / cell_w) as usize;
            let mut num_buf = String::new();
            for idx in first_row..end {
                let row = rows[idx];
                let y = lay.text_top + (idx - first_row) as f32 * line_h - frac;
                let (lbg, rbg, bar) = match row.kind {
                    RowKind::Equal => (0, 0, if row.ws_only { th.ws_hidden_marker } else { 0 }),
                    RowKind::Delete => (th.del_bg, th.filler_bg, th.scrollbar_del),
                    RowKind::Insert => (th.filler_bg, th.add_bg, th.scrollbar_add),
                    RowKind::Modify if row.ws_only => (th.ws_bg, th.ws_bg, th.ws_marker),
                    RowKind::Modify => (th.del_bg, th.add_bg, th.hunk_marker),
                };
                for (side, bg) in [(0usize, lbg), (1, rbg)] {
                    let x = lay.pane_x[side] + lay.gutter_w;
                    let w = lay.pane_w - lay.gutter_w;
                    if bg != 0 {
                        p.rect(x, y, w, line_h, bg);
                    }
                    if bar != 0 {
                        p.rect(lay.pane_x[side], y, (3.0 * s).round(), line_h, bar);
                    }
                    let line = if side == 0 { row.left } else { row.right };
                    if line != NONE {
                        num_buf.clear();
                        use std::fmt::Write;
                        let _ = write!(num_buf, "{}", line + 1);
                        let nx = lay.pane_x[side]
                            + 4.0 * s
                            + (digits.saturating_sub(num_buf.len() + 1)) as f32 * cell_w;
                        let color = if idx == cursor {
                            th.gutter_fg_cursor
                        } else {
                            th.gutter_fg
                        };
                        p.text(nx, y, &num_buf, color);
                    }
                }
                if idx == cursor {
                    p.rect(0.0, y, lay.w - lay.scrollbar_w, line_h, th.cursor_row);
                }
            }

            // Scrollbar with change ticks.
            let sb_x = lay.w - lay.scrollbar_w;
            let n = rows.len().max(1) as f32;
            if l.diff.hunks.len() <= 20_000 {
                for h in &l.diff.hunks {
                    let y0 = lay.text_top + lay.text_h * h.rows.start as f32 / n;
                    let y1 = lay.text_top + lay.text_h * h.rows.end as f32 / n;
                    let kind = rows[h.rows.start as usize].kind;
                    let color = match kind {
                        RowKind::Delete => th.scrollbar_del,
                        RowKind::Insert => th.scrollbar_add,
                        _ => th.hunk_marker,
                    };
                    p.rect(
                        sb_x + 2.0 * s,
                        y0,
                        lay.scrollbar_w - 4.0 * s,
                        (y1 - y0).max(1.0 * s),
                        color,
                    );
                }
            }
            let thumb_y0 = lay.text_top + lay.text_h * (first_row as f32 / n);
            let thumb_h = (lay.text_h * lay.rows_visible as f32 / n)
                .max(4.0 * s)
                .min(lay.text_h);
            p.rect(
                sb_x,
                thumb_y0.min(lay.text_top + lay.text_h - thumb_h),
                lay.scrollbar_w,
                thumb_h,
                th.scrollbar,
            );

            // Text, one scissored batch per pane.
            for side in 0..2 {
                let (file, other) = if side == 0 {
                    (&l.left, &l.right)
                } else {
                    (&l.right, &l.left)
                };
                let _ = other;
                let clip_x = (lay.pane_x[side] + lay.gutter_w) as u32;
                let clip_w = (lay.pane_w - lay.gutter_w).max(0.0) as u32;
                p.draw
                    .begin([clip_x, lay.text_top as u32, clip_w, lay.text_h as u32]);
                let x0 = lay.text_x[side] - scroll_x_px;
                let mut intra_i = 0;
                for idx in first_row..end {
                    let row = rows[idx];
                    let line = if side == 0 { row.left } else { row.right };
                    if line == NONE {
                        continue;
                    }
                    let y = lay.text_top + (idx - first_row) as f32 * line_h - frac;
                    let bytes = file.line(line as usize);
                    let (strong, ranges, ws): (u32, &[Range<u32>], WsShow) = match row.kind {
                        RowKind::Equal => (
                            0,
                            &[],
                            if row.ws_only {
                                WsShow::All
                            } else {
                                WsShow::None
                            },
                        ),
                        RowKind::Delete => (th.del_strong, &[], WsShow::Trailing),
                        RowKind::Insert => (th.add_strong, &[], WsShow::Trailing),
                        RowKind::Modify => {
                            while intra_i < intra.len() && intra[intra_i].0 < idx {
                                intra_i += 1;
                            }
                            let d = intra
                                .get(intra_i)
                                .filter(|(i, _)| *i == idx)
                                .and_then(|(_, d)| d.as_ref());
                            let strong = if row.ws_only {
                                th.ws_strong
                            } else if side == 0 {
                                th.del_strong
                            } else {
                                th.add_strong
                            };
                            match d {
                                Some(d) => (
                                    strong,
                                    if side == 0 { &d.left } else { &d.right },
                                    if row.ws_only {
                                        WsShow::All
                                    } else {
                                        WsShow::Ranges
                                    },
                                ),
                                None => (
                                    strong,
                                    &[],
                                    if row.ws_only {
                                        WsShow::All
                                    } else {
                                        WsShow::Trailing
                                    },
                                ),
                            }
                        }
                    };
                    search_hits.clear();
                    if let Some(f) = &finder {
                        let hay: &[u8] = if smart {
                            bytes
                        } else {
                            lower_buf.clear();
                            lower_buf.extend(bytes.iter().map(|b| b.to_ascii_lowercase()));
                            &lower_buf
                        };
                        let nl = f.needle().len() as u32;
                        for m in f.find_iter(hay) {
                            search_hits.push(m as u32..m as u32 + nl);
                        }
                    }
                    let ws_color = if row.kind == RowKind::Equal {
                        th.ws_hidden_marker
                    } else {
                        th.ws_marker
                    };
                    let st = LineStyle {
                        fg: th.fg,
                        strong,
                        ranges,
                        search: &search_hits,
                        search_color: th.search_bg,
                        ws,
                        ws_color,
                        tab_width,
                        first_col: scroll_x_cells,
                        max_cols: lay.cols_visible + 1,
                    };
                    draw_line(&mut p, x0, y, bytes, &st);
                }
            }
            let _ = ws_mode;
        }
    }

    // ---- header and status text ----
    p.draw.begin(full);
    let header_y = (lay.header_h - line_h) / 2.0;
    for side in 0..2 {
        let title = if side == 0 {
            &opts.left_title
        } else {
            &opts.right_title
        };
        let max_cells = ((lay.pane_w - 8.0 * s) / cell_w) as usize;
        let shown = truncate_left(title, max_cells);
        p.text(lay.pane_x[side] + 4.0 * s, header_y, &shown, th.status_fg);
    }
    let status_y = lay.h - lay.status_h + (lay.status_h - line_h) / 2.0;
    let pending = vi.pending_display();
    let mut left_text = String::new();
    let mut left_color = th.status_fg;
    if !pending.is_empty() {
        left_text = pending;
        left_color = th.status_accent;
    } else if let Some(m) = message {
        left_text = m.clone();
        left_color = if m.starts_with("error") || m.contains("not found") {
            th.error_fg
        } else {
            th.status_accent
        };
    } else if let State::Ready(l) = state {
        let hunk = l.diff.hunk_at_or_before(cursor as u32);
        let in_hunk = hunk.filter(|&i| l.diff.hunks[i].rows.contains(&(cursor as u32)));
        let pos = match in_hunk {
            Some(i) => format!("change {}/{}", i + 1, l.diff.hunks.len()),
            None => format!("{} changes", l.diff.hunks.len()),
        };
        left_text = format!("{pos}   +{} −{}", l.diff.added, l.diff.removed);
    }
    p.text(4.0 * s, status_y, &left_text, left_color);

    let ws_text = format!("ws: {}", ws_mode.label());
    let right_text = match state {
        State::Ready(l) => {
            let row = l.diff.rows.get(cursor);
            let ln = |v: u32| {
                if v == NONE {
                    "-".to_string()
                } else {
                    (v + 1).to_string()
                }
            };
            match row {
                Some(r) => format!(
                    "row {}/{}   L {}  R {}",
                    cursor + 1,
                    l.diff.rows.len(),
                    ln(r.left),
                    ln(r.right)
                ),
                None => String::new(),
            }
        }
        _ => String::new(),
    };
    let rw = p.text_width(&right_text);
    p.text(lay.w - rw - 6.0 * s, status_y, &right_text, th.status_fg);
    let ww = p.text_width(&ws_text);
    let center_x = ((lay.w - ww) / 2.0).max(4.0 * s + p.text_width(&left_text) + 3.0 * cell_w);
    if center_x + ww < lay.w - rw - 8.0 * s {
        p.text(center_x, status_y, &ws_text, th.status_dim);
    }

    // ---- help overlay ----
    if *help {
        let lines = HELP_LINES;
        let box_w = lines.iter().map(|l| p.text_width(l)).fold(0.0, f32::max) + 4.0 * cell_w;
        let box_h = (lines.len() as f32 + 1.0) * line_h;
        let bx = ((lay.w - box_w) / 2.0).max(0.0);
        let by = (lay.text_top + (lay.text_h - box_h) / 2.0).max(lay.text_top);
        p.rect(
            bx - 2.0 * s,
            by - 2.0 * s,
            box_w + 4.0 * s,
            box_h + 4.0 * s,
            th.status_accent,
        );
        p.rect(bx, by, box_w, box_h, th.status_bg);
        for (i, l) in lines.iter().enumerate() {
            let color = if l.starts_with(' ') {
                th.status_fg
            } else {
                th.status_accent
            };
            p.text(bx + 2.0 * cell_w, by + (i as f32 + 0.5) * line_h, l, color);
        }
    }
}

/// Uncompressed 32-bit BMP (top-down). Debug aid: `sips -s format png` converts it.
fn write_bmp(path: &Path, w: u32, h: u32, rgba: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut f = std::io::BufWriter::new(std::fs::File::create(path)?);
    let data_len = w * h * 4;
    let mut hdr = Vec::with_capacity(54);
    hdr.extend_from_slice(b"BM");
    hdr.extend_from_slice(&(54 + data_len).to_le_bytes());
    hdr.extend_from_slice(&0u32.to_le_bytes());
    hdr.extend_from_slice(&54u32.to_le_bytes());
    hdr.extend_from_slice(&40u32.to_le_bytes());
    hdr.extend_from_slice(&(w as i32).to_le_bytes());
    hdr.extend_from_slice(&(-(h as i32)).to_le_bytes());
    hdr.extend_from_slice(&1u16.to_le_bytes());
    hdr.extend_from_slice(&32u16.to_le_bytes());
    hdr.extend_from_slice(&0u32.to_le_bytes());
    hdr.extend_from_slice(&data_len.to_le_bytes());
    hdr.extend_from_slice(&[0u8; 16]);
    f.write_all(&hdr)?;
    let mut row = Vec::with_capacity((w * 4) as usize);
    for px in rgba.chunks_exact(4) {
        row.extend_from_slice(&[px[2], px[1], px[0], 255]);
        if row.len() == (w * 4) as usize {
            f.write_all(&row)?;
            row.clear();
        }
    }
    f.flush()
}

fn truncate_left(s: &str, max_cells: usize) -> String {
    let n = s.chars().count();
    if n <= max_cells || max_cells < 2 {
        return s.to_string();
    }
    let skip = n - (max_cells - 1);
    let mut out = String::from("…");
    out.extend(s.chars().skip(skip));
    out
}

const HELP_LINES: &[&str] = &[
    "diffvader keys",
    "  j / k / ↓ / ↑        move cursor       ^e / ^y   scroll one line",
    "  ^d / ^u              half page         ^f / ^b   full page",
    "  gg / G / :N          top / bottom / row N",
    "  ]c / [c              next / previous change     [C / ]C   first / last",
    "  zt / zz / zb         cursor to top / center / bottom",
    "  h / l / 0 / $        scroll horizontally",
    "  /pat  n  N           search (smart case)",
    "  w  or  :ws <mode>    cycle whitespace: exact, eol, change, all",
    "  + / -  (⌘= / ⌘-)     zoom      t  toggle light/dark",
    "  q  ZZ  :q            quit      ?  toggle this help",
];

// ---- winit glue ---------------------------------------------------------------------------

impl ApplicationHandler<()> for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let _m = trace::span("primary-monitor");
        let size = el
            .primary_monitor()
            .map(|m| {
                let sz = m.size().to_logical::<f64>(m.scale_factor());
                LogicalSize::new(
                    (sz.width * 0.85).min(1800.0),
                    (sz.height * 0.85).min(1200.0),
                )
            })
            .unwrap_or(LogicalSize::new(1400.0, 900.0));
        drop(_m);
        let _s = trace::span("window-create");
        let attrs = Window::default_attributes()
            .with_title(format!(
                "diffvader — {} ↔ {}",
                self.opts.left_title, self.opts.right_title
            ))
            .with_inner_size(size);
        let window = match el.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                eprintln!("diffvader: cannot create window: {e}");
                el.exit();
                return;
            }
        };
        drop(_s);
        self.scale = window.scale_factor();
        self.rebuild_font();
        let core = {
            let _s = trace::span("gpu-core-join");
            self.gpu_core
                .take()
                .expect("gpu core handle")
                .join()
                .unwrap_or_else(|_| Err("gpu thread panicked".into()))
        };
        match core.and_then(|c| Gpu::new(c, window.clone(), &self.atlas)) {
            Ok(g) => self.gpu = Some(g),
            Err(e) => {
                eprintln!("diffvader: GPU init failed: {e}");
                el.exit();
                return;
            }
        }
        trace::mark("window-ready");
        self.window = Some(window.clone());
        self.drain_messages();
        // Draw now rather than waiting for AppKit's first redraw request; the request is
        // still made so a frame lands after the window is fully on screen.
        self.render();
        if self.want_exit {
            el.exit();
        }
        window.request_redraw();
    }

    fn user_event(&mut self, _el: &ActiveEventLoop, _ev: ()) {
        self.drain_messages();
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => el.exit(),
            WindowEvent::Resized(size) => {
                if let Some(g) = &mut self.gpu {
                    g.resize(size.width, size.height);
                }
                let lay = self.layout();
                self.clamp_cursor_to_view(&lay);
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                self.scale = scale_factor;
                self.rebuild_font();
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            WindowEvent::ModifiersChanged(m) => self.modifiers = m.state(),
            WindowEvent::KeyboardInput { event, .. } => {
                if event.state != ElementState::Pressed {
                    return;
                }
                trace::mark("key");
                let input = KeyInput {
                    key: &event.logical_key,
                    ctrl: self.modifiers.control_key(),
                    cmd: self.modifiers.super_key(),
                };
                if !self.vi.in_command_line() {
                    self.message = None;
                }
                if let Some(action) = self.vi.key(input) {
                    self.apply(action, el);
                }
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                trace::mark("wheel");
                let (dx, dy) = match delta {
                    MouseScrollDelta::PixelDelta(p) => (p.x, p.y),
                    MouseScrollDelta::LineDelta(x, y) => (
                        x as f64 * 3.0 * self.cell_w as f64,
                        y as f64 * 3.0 * self.line_h as f64,
                    ),
                };
                self.scroll_y -= dy;
                self.scroll_x -= dx / self.cell_w as f64;
                self.clamp_scroll();
                let lay = self.layout();
                self.clamp_cursor_to_view(&lay);
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            WindowEvent::RedrawRequested => {
                self.render();
                if self.want_exit {
                    el.exit();
                }
            }
            _ => {}
        }
    }

    fn exiting(&mut self, _el: &ActiveEventLoop) {
        self.finish();
    }
}
