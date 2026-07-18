# Release Build Strategy (pre-GitHub-push, sub-project 3 of 3) — Design

**Status:** Approved by user 2026-07-18.

## Context

This is the third and final sub-project preparing aivyx-coder for its first
push to GitHub. Build order, chosen by the user: **Docs → Codebase →
Release**. Sub-projects 1 (docs cleanup) and 2 (codebase cleanup) are both
merged to `main`. This spec covers how end users download and run
aivyx-coder on their own bare-metal hardware.

State confirmed before this design was written:

- No `.github/` directory exists — zero CI, zero release automation of any
  kind today.
- No packaging of any kind exists (no `dist/`, no install script, no distro
  package).
- `Cargo.toml`'s `[workspace.package]` declares `version = "0.1.0"`,
  `edition = "2024"`, `license = "MIT OR Apache-2.0"` — never released
  under any version.
- The binary crate is `crates/aivyx` (`crates/aivyx/Cargo.toml:2`,
  `name = "aivyx"`) — the produced executable is named `aivyx`.
- **Platform constraint driving this whole spec**: the project's actual
  security boundary (`crates/aivyx-sandbox`) is Linux-specific Landlock
  (filesystem scoping) + seccomp-bpf (syscall denylist), the default
  `sandbox-backend` feature. `sandbox.require_enforcement` defaults to
  `true` (README.md's "Security model" section), meaning a build without
  real Landlock available (any non-Linux platform) will refuse to run
  `run_command`/`run_shell` entirely unless the user explicitly opts into
  unconfined execution. Building for macOS/Windows would mean shipping a
  security-focused tool without its own security boundary by default — a
  real design question, not a build-target checkbox. `NoopConfiner` (the
  fallback) is a genuinely tested code path (referenced across the test
  suite), so it's not broken, just deliberately not this project's release
  target for now.
- The handful of `#[cfg(unix)]` blocks in the codebase
  (`crates/aivyx-core/src/session.rs:99`,
  `crates/aivyx-llm/src/openai_compat.rs:105`,
  `crates/aivyx-config/src/lib.rs:628`) are all graceful no-ops elsewhere —
  file-permission hardening (`chmod 0600` on session/config files) that's
  silently skipped on non-Unix, not a compile blocker. Not directly
  relevant to this spec (Linux-only scope), noted for completeness.
- This machine (CachyOS/Arch-based) packages the musl Rust target directly
  via pacman (`rust-musl`, confirmed via `pacman -Ss musl` returning both
  `extra/rust-musl` and `cachyos-extra-znver4/rust-musl`, version
  `1:1.97.0-1`, matching the system's own `rustc`/`cargo` version) — no
  `rustup`, no Docker, no `cross`/`cargo-zigbuild`/`zig` needed for a local
  musl build on this machine.
- `README.md`'s "Building and running" section (line 14-18) currently only
  documents `cargo run -p aivyx` — no release-build instructions exist yet.

## Decisions (confirmed with user via one-at-a-time questions before this doc was written)

1. **Platform scope**: Linux only, for now. macOS/Windows explicitly out of
   scope until there's real demand or a plan for platform-appropriate
   confinement — not silently deferred, a deliberate choice tied directly
   to the security-boundary constraint above.
2. **Target triple**: `x86_64-unknown-linux-musl` only (not `-gnu`, not
   `aarch64`). Static musl binary avoids glibc-version coupling across
   different end-user distros; Landlock/seccomp are raw kernel syscalls,
   not glibc-specific, so musl doesn't conflict with the sandbox.
3. **Build mechanism**: both a local build script (usable immediately,
   today, independent of the GitHub push) and a GitHub Actions release
   workflow (written and committed now, but inert until the repo actually
   exists on GitHub and a matching tag is pushed there).
4. **Artifact format**: a stripped release binary, packaged into a
   `.tar.gz` alongside `README.md` and both license files, with a
   `.sha256` checksum file alongside the tarball. Not a raw unpackaged
   binary, not an AUR `PKGBUILD` (that's a distribution-channel decision
   for later, out of scope here — this spec produces the artifact an AUR
   package would eventually wrap).
5. **Versioning**: keep the existing `0.1.0`, tag it `v0.1.0` — genuinely
   the first release, nothing to bump from.
6. **No `CHANGELOG.md`**: `docs/HISTORY.md` already carries the full
   phase-by-phase narrative; GitHub's own auto-generated release notes
   (from commit messages since the last tag) cover "what changed between
   versions" well enough once there's a second release to diff against.

## Changes

### 1. `Cargo.toml` — release profile tuning

Add a `[profile.release]` section to the workspace root `Cargo.toml`:

```toml
[profile.release]
strip = true
lto = "thin"
codegen-units = 1
```

`strip = true` removes debug symbols (smaller download — this is also what
"stripped binary" in Decision 4 refers to; no separate manual `strip`
invocation is needed in either the local script or CI once this is set,
`cargo build --release` does it automatically). `lto = "thin"` and
`codegen-units = 1` trade longer release-build time for a smaller, faster
binary — acceptable since a release build happens once per tag, not per
commit.

### 2. `scripts/build-release.sh` (new)

A local build script that:
- Checks whether the `x86_64-unknown-linux-musl` target is installed
  (`rustc --print target-list` or equivalent check appropriate for this
  distro-packaged, non-rustup toolchain — the implementation plan resolves
  the exact check); if not, prints a clear instruction
  (`pacman -S rust-musl` on this machine) and exits non-zero rather than
  attempting an install itself (installing system packages is not this
  script's job).
- Runs `cargo build --release --target x86_64-unknown-linux-musl -p aivyx`.
- Packages the resulting binary
  (`target/x86_64-unknown-linux-musl/release/aivyx`) together with
  `README.md`, `LICENSE-MIT`, and `LICENSE-APACHE` into
  `dist/aivyx-coder-v${VERSION}-x86_64-linux-musl.tar.gz`, where
  `${VERSION}` is read from `Cargo.toml`'s `[workspace.package]` `version`
  field (not hardcoded — so this script keeps working correctly once the
  version bumps past `0.1.0`).
- Computes a SHA-256 checksum of the tarball, writing it to
  `dist/aivyx-coder-v${VERSION}-x86_64-linux-musl.tar.gz.sha256` in the
  standard `sha256sum`-compatible format (so `sha256sum -c` verifies it
  directly).
- Prints the final artifact paths on success.

### 3. `.gitignore` — add `/dist`

The script's output directory must not be committed.

### 4. `.github/workflows/release.yml` (new)

Triggers on push of any tag matching `v*`. Runs on `ubuntu-latest`:
installs the `x86_64-unknown-linux-musl` Rust target (via whatever
mechanism is standard for GitHub's runners — the implementation plan
resolves the exact steps, likely `rustup target add` since GitHub's
`ubuntu-latest` runners ship `rustup`-based Rust, unlike this local
distro-packaged machine), builds and packages identically to
`scripts/build-release.sh` (same tarball naming, same checksum), then
creates a GitHub Release for the pushed tag with the tarball and checksum
file attached as release assets, using the tag name as the release title
and GitHub's default auto-generated release notes (commits since the
previous tag) as the body.

This workflow is inert (cannot run) until the repository is pushed to
GitHub and has at least one prior state to diff against for a tag push to
trigger — writing and committing it now is preparatory, not something this
spec claims to have verified running end-to-end.

### 5. `README.md` — document the release build

Add a short section (or extend "Building and running") covering: how to
produce a release build locally (`scripts/build-release.sh`), and a note
that tagged releases are also available via GitHub Releases once the
project's GitHub Actions workflow has run (phrased so it doesn't overclaim
availability before the repo is actually pushed — the implementation plan
resolves exact wording).

## Out of scope for this spec

- macOS/Windows builds of any kind (Decision 1).
- `aarch64` or any non-`x86_64` target (Decision 2).
- `-gnu` (glibc) builds (Decision 2).
- Any distro packaging (AUR `PKGBUILD`, `.deb`, `.rpm`, Flatpak, etc.) —
  this spec produces the tarball artifact such packaging would eventually
  wrap, not the packaging itself.
- `CHANGELOG.md` (Decision 6).
- Actually pushing the repository to GitHub, or actually cutting the
  `v0.1.0` tag/release — both remain the user's call, per this whole
  chapter's standing deferral of the GitHub push itself. This spec's
  deliverables are ready for that moment, not a trigger to cause it.
- Any change to `crates/aivyx-sandbox`'s confinement logic itself (no new
  platform-confinement backend, no change to `NoopConfiner` or
  `require_enforcement` defaults).

## Testing / verification

- **Local script**: run `scripts/build-release.sh` on this machine (after
  confirming/installing the `rust-musl` pacman package if not already
  present) and confirm it produces both
  `dist/aivyx-coder-v0.1.0-x86_64-linux-musl.tar.gz` and its `.sha256`
  file; verify the checksum with `sha256sum -c`; extract the tarball into
  a scratch directory and confirm it contains exactly the binary,
  `README.md`, `LICENSE-MIT`, and `LICENSE-APACHE`; run the extracted
  binary directly (`./aivyx --help` or equivalent) to confirm it's a real,
  working, statically-linked executable (`file` on it should show `statically
  linked`, and `ldd` should report "not a dynamic executable" or
  equivalent, confirming the musl static link actually worked and this
  isn't secretly a dynamically-linked binary that would fail on another
  machine).
- **CI workflow**: cannot be executed end-to-end as part of this
  sub-project (the repository isn't on GitHub yet) — verification here is
  limited to the workflow YAML being well-formed (a linter or GitHub's own
  `actionlint`-style validation, if available, or careful manual review)
  and structurally mirroring the local script's own steps closely enough
  that a human reviewer can confirm they'd produce the same artifact.
  Actually confirming the workflow runs correctly is explicitly deferred
  to whenever the user pushes the repo and cuts the first real tag — not
  a gap in this sub-project, a real limit of what's checkable before that
  happens.
- **`Cargo.toml` profile change**: `cargo build --workspace` (debug, the
  normal dev profile, unaffected by `[profile.release]`) and
  `cargo test --workspace` must both stay clean and unaffected — this
  change only affects `cargo build --release`, and the debug/test profile
  used by this project's other verification gates must show zero
  regression from adding the new section.
