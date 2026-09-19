# darwin-aarch64 Release Platform Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make it possible to build `aivyx-coder` on macOS at all (fixing
a real, currently-broken Cargo feature-forwarding gap), make the
sandbox's fail-closed default sensible on a platform that structurally
cannot enforce it, and add a `darwin-aarch64` leg to the release
pipeline so tagged releases ship a macOS Apple Silicon binary alongside
the existing Linux one.

**Architecture:** Three sequential pieces, each depending on the last.
(1) Fix `aivyx-sandbox`'s feature-forwarding: every workspace crate that
depends on it currently forces its `sandbox-backend` default feature on
regardless of `--no-default-features`, so nothing can actually disable
Landlock/seccomp today, despite the README documenting that workaround.
(2) Make `SandboxSettings::require_enforcement`'s default
platform-conditional (`true` on Linux, `false` elsewhere) — a build that
structurally cannot enforce anything shouldn't default to refusing to
run unconfined, since that's not "safe by default," it's "broken by
default." (3) Restructure `.github/workflows/release.yml` into a
two-stage matrix (build-per-target, then one shared release job) adding
a `macos-14`/`aarch64-apple-darwin` leg.

**Tech Stack:** Rust/Cargo feature flags, GitHub Actions YAML.

## Global Constraints

- `cargo build --workspace`, `cargo test --workspace`,
  `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo fmt --check` must stay clean on this (Linux) development
  machine throughout — Linux behavior must not change.
- This plan cannot directly test macOS compilation or a live GitHub
  Actions run from this (Linux) environment. Verification is: (a) prove
  `landlock`/`seccompiler` are fully absent from the dependency graph
  under `--no-default-features` (a Linux-runnable, decisive proxy for
  "would compile without the Linux-only crates that block macOS"), and
  (b) careful structural correctness of the new workflow YAML, matching
  the existing job's already-proven-working shape. The workflow's real
  correctness is only provable by GitHub Actions itself on the next tag
  push or manual dispatch — say so plainly in each task's report rather
  than claiming false certainty.
- No `aivyx-confine` (separate repo) changes — it already correctly
  gates `landlock`/`seccompiler` behind its own `sandbox-backend`
  feature; the gap is entirely in how `aivyx-coder`'s own crates depend
  on `aivyx-sandbox`.
- The macOS release leg must not enable any `provider-mistral-rs*`
  feature — match the existing Linux leg's behavior exactly (no
  `--features` flag).

---

## Task 1: Fix `aivyx-sandbox` feature-forwarding

**Files:**
- Modify: `/home/julian/Projects/Rust/aivyx-coder/crates/aivyx/Cargo.toml`
- Modify: `/home/julian/Projects/Rust/aivyx-coder/crates/aivyx-acp/Cargo.toml`
- Modify: `/home/julian/Projects/Rust/aivyx-coder/crates/aivyx-core/Cargo.toml`
- Modify: `/home/julian/Projects/Rust/aivyx-coder/crates/aivyx-mcp-server/Cargo.toml`
- Modify: `/home/julian/Projects/Rust/aivyx-coder/crates/aivyx-tools/Cargo.toml`
- Modify: `/home/julian/Projects/Rust/aivyx-coder/crates/aivyx-tui/Cargo.toml`

**Interfaces:**
- Produces: each of the 6 crates above gains its own `sandbox-backend`
  Cargo feature (`default = ["sandbox-backend"]`, forwarding to
  `aivyx-sandbox/sandbox-backend`), and their `aivyx-sandbox` dependency
  line gains `default-features = false`. No Rust code changes — this
  task is entirely `Cargo.toml` edits.

- [ ] **Step 1: Confirm the current broken state, in writing, before changing anything**

```bash
cd /home/julian/Projects/Rust/aivyx-coder
cargo tree --workspace --no-default-features -i landlock
```

Expected: `landlock` still appears in the tree (the bug this task
fixes). Record this output in your report as the "before" baseline —
without it, there's no way to later prove the fix actually changed
anything.

- [ ] **Step 2: Edit each Cargo.toml**

For each of the 6 files, find the existing `aivyx-sandbox` dependency
line under `[dependencies]` and change it, then add a `[features]`
block (or extend an existing one) with a `sandbox-backend` feature.

Example for `crates/aivyx/Cargo.toml` (read the file first — its exact
current line for `aivyx-sandbox` and its exact current `[features]`
block are shown in the Task Description below; match its real current
formatting, don't guess):

```toml
# Before:
aivyx-sandbox = { version = "0.1.0", path = "../aivyx-sandbox" }

# After:
aivyx-sandbox = { version = "0.1.0", path = "../aivyx-sandbox", default-features = false }
```

```toml
[features]
# Real OS-level confinement (Landlock filesystem scoping + a seccomp-bpf
# syscall denylist), forwarded from aivyx-sandbox. Opt out with
# --no-default-features on platforms/kernels where it isn't available
# (e.g. macOS) -- NoopConfiner remains the fallback. This crate must
# forward the feature explicitly: depending on aivyx-sandbox without
# default-features = false means --no-default-features never actually
# reaches aivyx-sandbox's own sandbox-backend feature, no matter how
# this crate itself is built.
default = ["sandbox-backend"]
sandbox-backend = ["aivyx-sandbox/sandbox-backend"]
# (keep any pre-existing feature entries in this crate's own
# [features] block, e.g. aivyx's provider-mistral-rs-* features --
# add to the block, don't replace it)
```

Repeat for all 6 files. Each crate's own pre-existing `[features]`
content (if any) must be preserved, not overwritten — read each file in
full before editing it.

- [ ] **Step 3: Verify the fix, decisively**

```bash
cd /home/julian/Projects/Rust/aivyx-coder
cargo tree --workspace --no-default-features -i landlock
```

Expected: **no output** (or a "did not match any packages" error,
depending on the exact cargo version's phrasing for an empty match) —
`landlock` must be completely absent from the resolved graph. This is
the single most important check in this task; if `landlock` still
appears, the fix is incomplete — find which crate in the chain is still
requesting `aivyx-sandbox`'s defaults and fix that one too (the 6 files
listed above come from `cargo tree --workspace -e normal -i
aivyx-sandbox` at plan-writing time, but re-run that query yourself to
confirm it's still the complete list before declaring done).

Repeat for `seccompiler`:

```bash
cargo tree --workspace --no-default-features -i seccompiler
```

Expected: also absent.

- [ ] **Step 4: Confirm Linux behavior is unchanged**

```bash
cargo tree --workspace -i landlock
```

Expected: `landlock` **still present** here (no flags passed — Linux's
default build must be unaffected; this is the "additive, not breaking"
check).

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

Expected: all clean, identical to before this task (this task changes
no Rust code, only Cargo.toml feature wiring — a behavior change here
would indicate a mistake).

- [ ] **Step 5: Confirm the opt-out path actually compiles**

```bash
cargo build --workspace --no-default-features
```

Expected: succeeds. This is the closest thing to proving "this would
also compile on macOS" that's checkable from Linux — it proves every
crate's `NoopConfiner`-fallback code path is real, reachable, working
Rust that compiles without `sandbox-backend`, not just an assumption.
If this fails, read the compiler errors — they will point at exactly
what still assumes `sandbox-backend` is unconditionally available
(e.g. a `cfg`-less reference to a Landlock-only type) and needs its own
fix, which is in scope for this task since it blocks the goal.

- [ ] **Step 6: Commit**

```bash
cd /home/julian/Projects/Rust/aivyx-coder
git add crates/aivyx/Cargo.toml crates/aivyx-acp/Cargo.toml crates/aivyx-core/Cargo.toml crates/aivyx-mcp-server/Cargo.toml crates/aivyx-tools/Cargo.toml crates/aivyx-tui/Cargo.toml
git commit -m "fix: forward aivyx-sandbox's sandbox-backend feature through the whole dependency chain

--no-default-features on aivyx-sandbox (as README.md already documents)
never actually disabled Landlock/seccomp, because every dependent crate
(aivyx, aivyx-acp, aivyx-core, aivyx-mcp-server, aivyx-tools, aivyx-tui)
requested aivyx-sandbox's default features implicitly, regardless of
what flags the build itself was invoked with. Each dependent crate now
declares default-features = false on aivyx-sandbox and re-forwards its
own default-on sandbox-backend feature, so --no-default-features
genuinely reaches aivyx-confine's Linux-only landlock/seccompiler deps
for the first time. Verified via cargo tree -i landlock/-i seccompiler
before and after, and cargo build --workspace --no-default-features
now succeeds (was previously impossible to prove, since the flag never
took effect). No behavior change on Linux's default build."
```

---

## Task 2: `SandboxSettings::require_enforcement` becomes platform-conditional

**Files:**
- Modify: `/home/julian/Projects/Rust/aivyx-coder/crates/aivyx-config/src/lib.rs`

**Interfaces:**
- Consumes: nothing new (pure edit to an existing `Default` impl).
- Produces: `SandboxSettings::default().require_enforcement` is now
  `cfg(target_os = "linux")`-conditional (`true` on Linux, `false`
  elsewhere) instead of unconditionally `true`.

- [ ] **Step 1: Read the current code**

Read `crates/aivyx-config/src/lib.rs` around the `SandboxSettings`
struct and its `Default` impl (previously seen at lines ~536-557 in this
plan's own research — re-read the real current lines before editing, in
case they've shifted) — note the existing doc comment on
`require_enforcement` explaining the fail-closed rationale; your edit
extends that rationale, doesn't replace it.

- [ ] **Step 2: Edit the `Default` impl**

Change:

```rust
impl Default for SandboxSettings {
    fn default() -> Self {
        Self {
            extra_read_paths: Vec::new(),
            require_enforcement: true,
        }
    }
}
```

to:

```rust
impl Default for SandboxSettings {
    fn default() -> Self {
        Self {
            extra_read_paths: Vec::new(),
            // A platform that cannot compile sandbox-backend at all
            // (anything non-Linux -- landlock/seccompiler are Linux-only,
            // see crates/aivyx-sandbox/Cargo.toml) has no real
            // enforcement to ever succeed at, so defaulting to `true`
            // there wouldn't be "safe by default" -- it would be
            // "refuses to run anything, with no clear cause, on every
            // fresh install." Linux keeps the existing fail-closed
            // default; every other platform starts unconfined by
            // default (matching its actual capability), same as an
            // explicit `--no-default-features` build on Linux already
            // behaves when a user opts into that themselves.
            require_enforcement: cfg!(target_os = "linux"),
        }
    }
}
```

- [ ] **Step 3: Update/add the existing test**

Find the existing `require_enforcement_defaults_to_true` test (seen at
plan-writing time around line 1433-1435). On Linux (this development
machine, and Linux CI), the default is still `true`, so the existing
test's assertion remains correct as-is — but its name and body should
make the platform-conditionality explicit rather than implying a
universal default. Update it to:

```rust
#[test]
fn require_enforcement_defaults_to_true_on_linux() {
    // This assertion is only meaningful on Linux -- see the Default
    // impl's own doc comment for why the default is platform-
    // conditional. On any other target this test would need the
    // opposite assertion; it isn't cfg-gated here because this
    // workspace's CI only runs on Linux today (see the parent plan's
    // darwin-aarch64 release-pipeline work, which adds a macOS *build*
    // leg but not a macOS *test* leg).
    if cfg!(target_os = "linux") {
        assert!(SandboxSettings::default().require_enforcement);
    } else {
        assert!(!SandboxSettings::default().require_enforcement);
    }
}
```

- [ ] **Step 4: Run tests**

```bash
cd /home/julian/Projects/Rust/aivyx-coder
cargo test -p aivyx-config require_enforcement
```

Expected: passes (this machine is Linux, so the `true` branch is
exercised).

- [ ] **Step 5: Run full verification**

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

Expected: all clean.

- [ ] **Step 6: Commit**

```bash
cd /home/julian/Projects/Rust/aivyx-coder
git add crates/aivyx-config/src/lib.rs
git commit -m "fix: SandboxSettings::require_enforcement defaults to false on non-Linux

A platform that cannot compile sandbox-backend at all (macOS, per
Task 1's fix making that build actually possible) has no real
enforcement to ever succeed at -- defaulting require_enforcement to
true there wouldn't be safe-by-default, it would be broken-by-default
with no clear cause on every fresh install. Linux's existing
fail-closed default is unchanged."
```

---

## Task 3: `release.yml` becomes a two-stage matrix, adding `darwin-aarch64`

**Files:**
- Modify: `/home/julian/Projects/Rust/aivyx-coder/.github/workflows/release.yml`

**Interfaces:** none (CI workflow file only).

- [ ] **Step 1: Read the current file in full**

Read `.github/workflows/release.yml` completely (it's short, ~40 lines)
before editing — match its existing step names, comment style, and
`STAGE_NAME` convention exactly.

- [ ] **Step 2: Rewrite as a two-stage workflow**

Replace the single `build` job with a `build` job matrixed over targets
(producing artifacts, not a release directly) and a new `release` job
that runs once, after both matrix legs, and does the actual GitHub
Release creation:

```yaml
name: Release

on:
  push:
    tags:
      - 'v*'

permissions:
  contents: write

jobs:
  build:
    strategy:
      matrix:
        include:
          - target: x86_64-unknown-linux-musl
            runner: ubuntu-latest
            stage_suffix: x86_64-linux-musl
          - target: aarch64-apple-darwin
            runner: macos-14
            stage_suffix: darwin-aarch64
    runs-on: ${{ matrix.runner }}
    steps:
      - name: Checkout
        uses: actions/checkout@v4

      - name: Install Rust
        uses: dtolnay/rust-toolchain@stable
        with:
          targets: ${{ matrix.target }}

      - name: Install musl cross-compiler
        if: matrix.target == 'x86_64-unknown-linux-musl'
        run: sudo apt-get update && sudo apt-get install -y musl-tools

      - name: Build release binary (Linux, musl)
        if: matrix.target == 'x86_64-unknown-linux-musl'
        env:
          CC_x86_64_unknown_linux_musl: musl-gcc
        run: cargo build --release --target ${{ matrix.target }} -p aivyx

      - name: Build release binary (macOS, no sandbox-backend)
        if: matrix.target == 'aarch64-apple-darwin'
        run: cargo build --release --target ${{ matrix.target }} -p aivyx --no-default-features

      - name: Package release artifact
        run: |
          STAGE_NAME="aivyx-coder-${GITHUB_REF_NAME}-${{ matrix.stage_suffix }}"
          mkdir -p "dist/${STAGE_NAME}"
          cp target/${{ matrix.target }}/release/aivyx-coder "dist/${STAGE_NAME}/aivyx-coder"
          cp README.md LICENSE-MIT LICENSE-APACHE "dist/${STAGE_NAME}/"
          mkdir -p "dist/${STAGE_NAME}/docs"
          cp -r docs/logos "dist/${STAGE_NAME}/docs/"
          cd dist
          tar -czf "${STAGE_NAME}.tar.gz" "${STAGE_NAME}"
          sha256sum "${STAGE_NAME}.tar.gz" > "${STAGE_NAME}.tar.gz.sha256"

      - name: Upload artifact
        uses: actions/upload-artifact@v4
        with:
          name: release-${{ matrix.stage_suffix }}
          path: dist/*.tar.gz*

  release:
    needs: build
    runs-on: ubuntu-latest
    steps:
      - name: Download all artifacts
        uses: actions/download-artifact@v4
        with:
          path: dist
          merge-multiple: true

      - name: Create GitHub Release
        uses: softprops/action-gh-release@v2
        with:
          files: |
            dist/*.tar.gz
            dist/*.tar.gz.sha256
          generate_release_notes: true
```

Notes on the two build-step split (`if: matrix.target == '...'`): the
Linux leg's `sha256sum` is a GNU coreutils tool present on
`ubuntu-latest`; confirm at implementation time whether `macos-14`
runners ship it too or need `shasum -a 256` instead (macOS's BSD
userland historically lacks `sha256sum`) — if it's missing, change the
packaging step's hash line to be conditional per-`runner`/`matrix.target`
too, matching whichever tool each runner actually has. This is exactly
the kind of thing this plan's Global Constraints section already flags
as unverifiable from this Linux machine — note your finding either way
in your report, and if you have no way to confirm it before merging,
say so explicitly rather than guessing silently.

- [ ] **Step 3: Sanity-check the YAML structurally**

No YAML linter or `act`-style local GitHub Actions runner is available
in this environment (confirmed at plan-writing time — `actionlint`,
`yamllint`, `act`, and Python's `pyyaml` are all absent). Verify by
careful manual re-read instead: confirm indentation is consistent
(2-space, matching the original file), confirm every `matrix.*`
reference matches a key actually defined in `strategy.matrix.include`,
confirm the `release` job's `needs: build` is present (without it, the
release job could start before builds finish or even if they fail), and
confirm `permissions: contents: write` still applies (needed by both
`softprops/action-gh-release` and, per its own docs,
`actions/upload-artifact`/`download-artifact` typically don't need it,
but the release-creation step does). If a YAML/JSON parser becomes
available in your environment (e.g. `pip install pyyaml` succeeds),
use it to at least confirm the file parses as valid YAML — note in your
report whether you had this available or not.

- [ ] **Step 4: Commit**

```bash
cd /home/julian/Projects/Rust/aivyx-coder
git add .github/workflows/release.yml
git commit -m "feat: add darwin-aarch64 to the release pipeline

Restructures release.yml into a build-matrix + single-release-job
shape: each target (x86_64-unknown-linux-musl on ubuntu-latest,
aarch64-apple-darwin on macos-14, natively -- no cross-compile
toolchain needed) uploads a packaged artifact, then one shared release
job downloads all of them and creates the GitHub Release exactly once,
avoiding a race between matrix legs. The macOS leg builds with
--no-default-features (now load-bearing, thanks to the prior task's
feature-forwarding fix) since Landlock/seccomp cannot compile there.
Not verifiable from this Linux development environment beyond
structural review -- real correctness is proven on the next tag push
or a manual workflow_dispatch test run."
```

---

## Final verification

- [ ] Run the complete workspace check once more, after all 3 tasks:

```bash
cd /home/julian/Projects/Rust/aivyx-coder
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
cargo tree --workspace --no-default-features -i landlock
cargo tree --workspace --no-default-features -i seccompiler
```

Expected: everything clean/passing, and both `cargo tree -i` queries
show `landlock`/`seccompiler` fully absent from the `--no-default-features`
graph.

- [ ] This plan does not decide whether to push / whether to actually
  tag a release testing the new workflow — follow
  `superpowers:finishing-a-development-branch`, and separately ask
  before triggering any real tag push (a tag push runs the real release
  workflow against GitHub's infrastructure and publishes a real GitHub
  Release — a shared-state, hard-to-fully-reverse action, not something
  to do without explicit confirmation).

## Explicitly out of scope for this plan

(Copied forward from the design spec's own "What this spec does not
decide" section.)

- `darwin-x86_64`, `linux-aarch64`, Windows — not evaluated.
- The actual ACP registry PR (`agent.json`/`icon.svg`, submission
  process) — separate, later, undecided-when scope.
- Enabling `provider-mistral-rs-metal` for the macOS release build — a
  real, distinct future option, not decided here.
- Any change to the separate `aivyx-confine` repo.
