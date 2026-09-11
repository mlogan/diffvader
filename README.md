# diffvader

A fast, GPU-rendered, side-by-side diff viewer for macOS with a vi-style interface.
Built to be used as a `git difftool`.

```
cargo build --release
./target/release/diffvader old.rs new.rs
./target/release/diffvader --install-git      # sets diff.tool=diffvader in ~/.gitconfig
git difftool HEAD~1
```

## Usage

```
diffvader [options] LEFT RIGHT [+ROW]

  -w, --ignore-all-space       ignore all whitespace
  -b, --ignore-space-change    ignore changes in the amount of whitespace
      --ignore-space-at-eol    ignore whitespace at end of line
      --font PATH              monospace font file (default: SF Mono, then Menlo)
      --font-size PT           font size in points (default 13)
      --tab-width N            tab stop width (default 4)
      --light                  light color theme
      --timing                 print a startup timeline and frame stats to stderr at exit
      --trace FILE             write Chrome trace event JSON (open in Perfetto)
      --screenshot FILE.bmp    render the first diff frame to a BMP file and exit
      --quit-after-first-frame exit as soon as the diff is on screen (benchmarking)
      --bench-scroll N         scroll through the diff in N offscreen frames, print stats
      --git-config             print the git config needed to use diffvader as a difftool
      --install-git            write that config with `git config --global`
```

`DIFFVADER_TIMING=1` and `DIFFVADER_TRACE=file.json` are equivalent to the flags, which is
handy when git launches the tool.

## Keys

| Keys | Action |
| --- | --- |
| `j` `k` `↓` `↑` | move cursor (counts work: `10j`) |
| `^e` `^y` | scroll one line |
| `^d` `^u` | half page |
| `^f` `^b` `Space` `PageDown/Up` | full page |
| `gg` `G` `:N` `NG` | top / bottom / row N |
| `]c` `[c` | next / previous change (`3]c` works) |
| `]C` `[C` | last / first change |
| `zt` `zz` `zb` | cursor row to top / center / bottom |
| `h` `l` `0` `$` `←` `→` | horizontal scroll |
| `/pattern` `n` `N` | search both sides (smart case) |
| `w` or `:ws exact\|eol\|change\|all` | cycle / set whitespace mode (re-diffs in the background) |
| `+` `-` `⌘=` `⌘-` `⌘0` | zoom |
| `t` | toggle light / dark theme |
| `?` | help overlay |
| `q` `ZZ` `:q` `⌘Q` | quit |

Mouse and trackpad scrolling work too; the cursor stays inside the visible rows like vim.

## What the colors mean

- Red / green backgrounds: deleted / inserted lines. Paired changed lines get word-level
  highlights in a stronger shade; lines that are mostly rewritten are shown as whole-line
  changes rather than confetti.
- Blue: the two lines differ only in whitespace. Whitespace is drawn visibly (`·` space,
  `→` tab, `↵` CR) on those rows and inside changed regions, and trailing whitespace is
  always shown on changed lines.
- In an ignore-whitespace mode, lines that differ only in whitespace are treated as equal
  but keep a faint gutter marker so the hidden difference is still discoverable.
- The right edge is a minimap-style scrollbar: ticks mark every change.

## Performance notes

Measured on an M-series Mac Studio, release build, `--timing`:

- Load, index and diff run on a background thread started before the window exists.
  A 2.2 MB / 80k-line C file pair with 110k changed lines diffs in ~27 ms; typical source
  files take 1-3 ms. This work fully overlaps AppKit startup.
- Frame cost (build draw list + upload + render, GPU complete) is ~0.7-0.9 ms median and
  ~6 ms worst case on those files, independent of file size: only visible rows are touched
  and intra-line diffs are computed lazily and cached.
- Cold start to first diff frame is ~140 ms, of which ~120 ms is AppKit: `NSApplication
  sharedApplication` (~40 ms) and the first titled `NSWindow` (~40 ms), plus dyld (~15 ms)
  and `finishLaunching` (~25 ms). A bare Objective-C program that only opens a window
  measures the same on this machine (macOS 26); diffvader adds ~15 ms on top. A
  borderless window would save ~22 ms of the NSWindow cost at the price of a custom
  title bar.

Use `--trace out.json` and open it in [Perfetto](https://ui.perfetto.dev) to see every
span, including per-frame breakdowns; `--timing` prints the startup timeline and frame
percentiles at exit.

## Development

See `IMPLEMENTATION.md` for the architecture and status. `cargo test` covers the diff
engine, line indexing and the vi key state machine.
