# diffvader implementation notes

Fast graphical side-by-side diff viewer for macOS, usable as a `git difftool`.

## Decisions

- **Language:** Rust. Single static binary, no runtime, links only system frameworks.
- **Windowing / GPU:** `winit` 0.30 + `wgpu` 30 (Metal backend only). Rendering is a
  single instanced-quad pipeline: every background rect and glyph is one instance
  drawn in one pass, so a frame is one buffer upload plus a handful of draw calls.
- **Text:** custom glyph atlas rasterized with `fontdue` from a system monospace font
  (SF Mono / Menlo) loaded by path. No font database scan at startup (that alone costs
  100ms+ in cosmic-text style stacks), no shaping (monospace grid).
- **Diff:** `imara-diff` (histogram algorithm) on interned lines. Whitespace modes
  normalize lines before interning. Intra-line (word-level) diffs are computed lazily
  for visible rows only, so file size never affects per-frame cost.
- **Telemetry:** `src/trace.rs` records timestamped spans/marks into an in-memory
  buffer (always on, ~tens of ns per span). `--trace FILE` writes Chrome trace event
  JSON (open in Perfetto / chrome://tracing); `--timing` prints a startup timeline
  measured from *process start* (via `proc_pidinfo`, so dyld/pre-main time is included)
  and frame time percentiles at exit.
- **Concurrency:** file loading + diffing runs on a background thread spawned before
  the window is created, so window/GPU init overlaps I/O and diffing.
- **Vi interface:** cursor row + counts + operator-pending keys (`g`, `z`, `[`, `]`)
  and a `:` command line. See README for the keymap.

## Layout

```
src/main.rs   CLI parsing, startup orchestration
src/trace.rs  telemetry (spans, marks, chrome trace output, timing summary)
src/text.rs   file loading (mmap), line index, binary detection
src/diff.rs   line diff, whitespace modes, row model, hunks, intra-line diff
src/font.rs   font loading, glyph rasterization, atlas packing
src/gpu.rs    wgpu device/surface/pipeline, per-frame quad upload + draw
src/theme.rs  color palettes
src/keys.rs   vi key state machine -> Actions
src/app.rs    winit handler: layout, scrolling, draw-list construction, status bar
```

## Progress

- [x] Repo, toolchain pin (1.96.1), Cargo skeleton
- [ ] trace.rs telemetry
- [ ] text.rs file loading
- [ ] diff.rs line diff + rows + whitespace modes + intra-line
- [ ] font.rs atlas
- [ ] gpu.rs renderer
- [ ] app.rs layout/scroll/render
- [ ] keys.rs vi keymap
- [ ] git difftool integration + README
- [ ] perf pass on large files with tracing

## Remaining / ideas

- Search (`/`, `n`, `N`)
- Folding long unchanged regions
- Directory diff (`git difftool --dir-diff`)
- Optional CoreText rasterizer for pixel-identical Terminal.app text
