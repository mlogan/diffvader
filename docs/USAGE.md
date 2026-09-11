# diffvader usage

Full reference: options, keys, colors and performance notes. See the top-level README for
installation.

```
cargo build --release
./target/release/diffvader --install-git      # adds a `git dv` alias and sets diff.tool

git dv HEAD~3                                 # every file changed since HEAD~3, one window
git dv                                        # unstaged changes (any `git diff` arguments work)
git dvs abc123                                # one commit against its parent, like git show
diffvader --git HEAD~3                        # the same without the alias
git difftool HEAD~3                           # one window per file; slower (see below)
diffvader old.rs new.rs                       # two files
diffvader tree-a/ tree-b/                     # two directory trees (git difftool -d)
```

## Usage

```
diffvader [options] LEFT RIGHT [+ROW]         compare two files, or two directory trees
diffvader [options] [--git GIT-DIFF-ARGS]     show what `git diff GIT-DIFF-ARGS` would
diffvader [options] --show [COMMIT] [ARGS]    one commit against its parent (like git show)

  -w, --ignore-all-space       ignore all whitespace
  -b, --ignore-space-change    ignore changes in the amount of whitespace
      --ignore-space-at-eol    ignore whitespace at end of line
      --font PATH              monospace font file (default: SF Mono, then Menlo)
      --font-size PT           font size in points (default 13)
      --tab-width N            tab stop width (default 4)
      --light                  light color theme
      --agent NAME|CMD         AI agent for `e`: claude, codex, gemini, or a command line
      --timing                 print a startup timeline and frame stats to stderr at exit
      --trace FILE             write Chrome trace event JSON (open in Perfetto)
      --screenshot FILE.bmp    render the first diff frame to a BMP file and exit
      --quit-after-first-frame exit as soon as the diff is on screen (benchmarking)
      --bench-scroll N         scroll through the diff in N offscreen frames, print stats
      --git-config             print the git config needed to use diffvader as a difftool
      --install-git            write that config with `git config --global`
```

`--show COMMIT` (`git dvs`) diffs a commit against its first parent, the way `git show`
does; a root commit is compared with the empty tree, and extra arguments such as `-- path`
pass through to git. `--git` (or no arguments at all inside a repository) asks git for the changed-file list
(`git diff --raw`) and streams file contents from one `git cat-file --batch` process, so
nothing is written to disk and the git work overlaps window creation. Two directories
(what `git difftool --dir-diff` passes) work the same way. Files are loaded in the
background, the one on screen first, and shown one at a time. The header shows the
current file, its position in the set and its line counts. `⌘P` opens a VS Code style
quick-open: type to fuzzy-filter, `↑`/`↓` or `^p`/`^n` to move, `PgUp`/`PgDn`/`Home`/`End`
to move by screenfuls, `Enter` to open, `Esc` to close; with an empty query the list is in most-recently-viewed order, so `⌘P Enter` flips
back to the previous file. `]f` / `[f` step through files in order.

`DIFFVADER_TIMING=1` and `DIFFVADER_TRACE=file.json` are equivalent to the flags, which is
handy when git launches the tool.

## Keys

| Keys | Action |
| --- | --- |
| `j` `k` `]c` `[c` | next / previous change (counts work: `3j`) |
| `]C` `[C` | last / first change |
| `↓` `↑` | move cursor one line |
| `^e` `^y` | scroll one line |
| `^d` `^u` | half page |
| `^f` `^b` `Space` `PageDown/Up` | full page |
| `gg` `G` `:N` `NG` | top / bottom / row N |
| `zt` `zz` `zb` | cursor row to top / center / bottom |
| `h` `l` `0` `$` `←` `→` | horizontal scroll |
| `⌘P` `:e` | quick-open file picker |
| `]f` `[f` `:n` `:N` | next / previous file |
| `⌘↓` `⌘↑` | browse files: the list opens with the neighbor selected, release `⌘` to open it |
| `/pattern` `n` `N` | search both sides (smart case) |
| `w` or `:ws exact\|eol\|change\|all` | cycle / set whitespace mode (re-diffs in the background) |
| `+` `-` `⌘=` `⌘-` `⌘0` | zoom |
| `t` | toggle light / dark theme |
| `e` `:explain` | explain the change under the cursor with an AI agent (or the first unexplained change in view) |
| `E` `:expand` | ask for slightly more detail on the current explanation |
| `?` | key reference overlay (any key or click closes it) |
| `q` `ZZ` `:q` `⌘Q` `⌘W` | quit |

Mouse: wheel and trackpad scroll, click a row to put the cursor there, click or drag the
scrollbar on the right (its ticks mark every change). The `?` at the left edge of each
change asks for its explanation; clicking it again once the answer is in asks for more.

## AI explanations

`e` asks an AI agent CLI to explain the purpose of one change. diffvader looks for
`claude`, `codex` and `gemini` on PATH (in that order) the first time you press it, or
uses `--agent` / `$DIFFVADER_AGENT`, which take a known name or a full command line
(`{prompt}` marks where the prompt goes; without it the prompt is the last argument).
The agent runs in the repository root and may read files and run `git log` / `git show`.

The prompt carries the change as a unified diff with context, the file, what the two
sides are (the `git diff` arguments, the commit for `git dvs`, or the two paths), the
other files in the same comparison, and instructions to be concise and factual, avoid
metaphors and jargon, and explain in cause-and-effect terms. The first pass runs on the
agent's fastest model (`claude --model haiku`) and is limited to a few sentences; `E`
re-sends the previous text on the agent's default model and asks for a little more.

Answers appear in a panel above the status bar whenever the cursor is on an explained
change; the gutter icon shows `…` while the agent works, `•` when done and `!` on an
error. Agents still running when you quit are terminated.

## What the colors mean

- Red / green backgrounds: deleted / inserted lines. Paired changed lines get word-level
  highlights in a stronger shade; lines that are mostly rewritten are shown as whole-line
  changes rather than confetti.
- Blue: the two lines differ only in whitespace. Whitespace is drawn visibly (`·` space,
  `→` tab, `↵` CR) on those rows and inside changed regions, and trailing whitespace is
  always shown on changed lines.
- In an ignore-whitespace mode, lines that differ only in whitespace are treated as equal
  but keep a faint gutter marker so the hidden difference is still discoverable.
- The current change is marked with a bright bar in both gutters.
- The scrollbar on the right shows every change as a red / green / blue tick.

## Performance notes

Measured on an M-series Mac Studio, release build, `--timing`:

- Load, index and diff run on a background thread started before the window exists.
  A 2.2 MB / 80k-line C file pair with 110k changed lines diffs in ~27 ms; typical source
  files take 1-3 ms. This work fully overlaps AppKit startup. `git dv` with 341 changed
  files reaches its first frame in the same ~150 ms as a two-file diff.
- `git difftool` is inherently slower because git does work before the tool starts:
  ~150 ms per file of shell-helper work (six or seven `git config` subprocesses per file,
  one more when `-t` is not given) of which diffvader's own invocation is ~10 ms, or 250-400
  ms of temp-tree writing for 341 files in `-d` mode. diffvader still opens a single window: each per-file
  invocation hands its pair to a session and returns, and one detached viewer shows files
  as git produces them. Prefer `git dv`, which reads from git directly.
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

See `IMPLEMENTATION.md` for the architecture and status. The Dock icon is
`assets/icon.png`; regenerate it with `python3 assets/icon.py assets/icon.png` after
editing the generator. `cargo test` covers the diff
engine, line indexing and the vi key state machine.
