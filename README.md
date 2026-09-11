# diffvader

A fast, GPU-rendered, side-by-side diff viewer for macOS with a vi-style interface.
Cold start to a rendered diff is ~150 ms; scrolling a 2 MB file costs under 1 ms a frame.

![diffvader icon](assets/icon.png)

## Install

```
git clone https://github.com/mlogan/diffvader.git
cd diffvader
./install.sh
```

`install.sh` builds a release binary into `~/.cargo/bin` (needs a Rust toolchain from
[rustup](https://rustup.rs)) and configures git: a `git dv` alias, a `git dvs` alias and
`diff.tool = diffvader`.

## Use

```
git dv                 # unstaged changes, every file in one window
git dv HEAD~3          # any `git diff` arguments work: --cached, main.., -- path
git dvs abc123         # one commit against its parent, like git show
diffvader a.rs b.rs    # two files, or two directory trees
```

`j`/`k` step between changes, `⌘P` fuzzy-opens another file, `⌘↓`/`⌘↑` browse files,
`w` cycles whitespace modes, `?` shows every key, `q` quits.

`git difftool` is supported too and opens a single window for all files, but git itself
spends ~65 ms per file preparing temp files and shell helpers before the viewer sees them.
`git dv` and `git dvs` read straight from git and are the fast path.

Full options, keys, colors and performance notes: [docs/USAGE.md](docs/USAGE.md).
Architecture and status: [IMPLEMENTATION.md](IMPLEMENTATION.md).

## Feedback

Pull requests are disabled on this repository. Please
[file an issue](https://github.com/mlogan/diffvader/issues) for bugs, requests or
questions instead.
