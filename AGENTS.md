# Agent instructions for diffvader

## After each piece of work

When a unit of work is finished and committed, ask the user whether they want to bump
the version and publish a release. Do not bump on your own. If they say yes, follow the
release procedure below.

## Release procedure

diffvader is distributed through a personal Homebrew tap, `mlogan/diffvader`, hosted at
https://github.com/mlogan/homebrew-diffvader. The tap has no CI and no bottles: the
formula downloads the tagged source tarball from this repo and builds it with cargo on
the user's machine. A release is therefore a tag here plus a two-line edit in the tap.

1. Bump `version` in `Cargo.toml`, run `cargo build` so `Cargo.lock` picks it up, and
   commit both. The formula's test asserts that `diffvader --version` prints the tag's
   version, so the Cargo version and the tag must agree.
2. Tag and push:

   ```sh
   git tag -a vX.Y.Z -m "diffvader X.Y.Z"
   git push origin main vX.Y.Z
   ```

3. Compute the tarball checksum. GitHub serves the tarball for any tag; give it a few
   seconds after pushing before fetching it.

   ```sh
   curl -sL https://github.com/mlogan/diffvader/archive/refs/tags/vX.Y.Z.tar.gz | shasum -a 256
   ```

4. Update the tap. The local checkout lives at `$(brew --repository mlogan/diffvader)`.
   In `Formula/diffvader.rb`, set `url` to the new tarball URL and `sha256` to the
   checksum from step 3. Commit and push to the tap's `main`.
5. Verify from the tap checkout before calling it done:

   ```sh
   HOMEBREW_NO_AUTO_UPDATE=1 brew audit --strict --new diffvader
   HOMEBREW_NO_AUTO_UPDATE=1 brew upgrade --build-from-source mlogan/diffvader/diffvader
   HOMEBREW_NO_AUTO_UPDATE=1 brew test diffvader
   ```

   `brew style mlogan/diffvader` lints unlabeled README code fences as Ruby, so label
   any fences you add there with a language.

## Homebrew notes

- Users install with the fully qualified name, `brew install mlogan/diffvader/diffvader`.
  Homebrew 6 trusts a third-party formula automatically only in that form; the two-step
  `brew tap` then `brew install` route needs `brew trust` first. Document only the
  fully qualified form.
- The formula must not touch the user's git config. `diffvader --install-git` stays a
  caveat the user runs themselves.
- Homebrew builds with its own `rust` formula, not rustup, so `rust-toolchain.toml` is
  ignored there. Keep the code building on current stable Rust.
