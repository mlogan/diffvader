#!/bin/sh
# Builds diffvader in release mode into ~/.cargo/bin and wires it into git
# (git dv / git dvs aliases and diff.tool). Re-run after pulling to update.
set -eu
cd "$(dirname "$0")"

if ! command -v cargo >/dev/null 2>&1; then
    echo "install.sh: cargo not found; install Rust from https://rustup.rs and retry" >&2
    exit 1
fi

echo "building diffvader (release)..."
cargo install --path . --locked --quiet

DV="${CARGO_HOME:-$HOME/.cargo}/bin/diffvader"
if [ ! -x "$DV" ]; then
    echo "install.sh: expected $DV after cargo install" >&2
    exit 1
fi

echo "configuring git..."
"$DV" --install-git
echo
echo "installed $DV"
echo "try: git dv HEAD~1"
