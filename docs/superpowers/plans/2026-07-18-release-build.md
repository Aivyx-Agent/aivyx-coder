# Release Build Strategy Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give aivyx-coder a working release-build pipeline — a local script that produces a real, downloadable Linux release artifact today, and a GitHub Actions workflow ready to automate the same thing once the repository is pushed.

**Architecture:** Three additive tasks, each producing one coherent deliverable: (1) release profile tuning + the local build script, (2) the CI workflow, (3) README documentation + a final consistency pass. No existing crate source code changes — every file this plan touches is either new or a small, additive edit to a config/doc file.

**Tech Stack:** Bash (the local script), GitHub Actions YAML (the CI workflow), Cargo's `[profile.release]` mechanism.

## Global Constraints

- Target: `x86_64-unknown-linux-musl` only. No other target triple, no macOS, no Windows.
- Binary name: `aivyx` (from `crates/aivyx`, package name `aivyx`).
- Tarball naming: `aivyx-coder-v${VERSION}-x86_64-linux-musl.tar.gz`, where `${VERSION}` is read from `Cargo.toml`, never hardcoded.
- Tarball contents: exactly the binary (renamed to `aivyx`), `README.md`, `LICENSE-MIT`, `LICENSE-APACHE` — nothing else.
- Checksum file: `sha256sum`-compatible format, verifiable with `sha256sum -c`.
- No `CHANGELOG.md`, no distro packaging (AUR/deb/rpm/etc.), no `aarch64` or `-gnu` target — all explicitly out of scope per the spec.
- **Known environment caveat, confirmed before this plan was written:** this development machine currently has a distro-packaging version skew between `rust` (`1:1.97.0-1.1`) and `rust-musl` (`1:1.97.0-1`) that breaks *all* musl builds (confirmed with a trivial hello-world unrelated to this project) with the exact error `error[E0425]: cannot find function, tuple struct or tuple variant `Some` in this scope` inside `regex-syntax` or any other dependency, preceded by a `note:` block naming mismatched `rustc` build hashes for `compiler_builtins`. This is a local system package sync issue, **not** a defect in this plan, the script, or the project. Task 1's verification steps explicitly handle both possible outcomes (skew already resolved vs. still present) — do not treat the skew-present outcome as a task failure.

---

### Task 1: Release profile, .gitignore, and the local build script

**Files:**
- Modify: `Cargo.toml` (add `[profile.release]`)
- Modify: `.gitignore` (add `/dist`)
- Create: `scripts/build-release.sh`

**Interfaces:**
- Produces: `scripts/build-release.sh`, an executable script with no arguments, run from anywhere (it resolves the repo root itself) — Task 3's README documentation references this exact path and invocation (`scripts/build-release.sh`, no arguments).

- [ ] **Step 1: Add the release profile to Cargo.toml**

Use the Edit tool on `Cargo.toml`:

old_string:
```
aivyx-config = { path = "crates/aivyx-config" }
aivyx-tui = { path = "crates/aivyx-tui" }
```

new_string:
```
aivyx-config = { path = "crates/aivyx-config" }
aivyx-tui = { path = "crates/aivyx-tui" }

[profile.release]
strip = true
lto = "thin"
codegen-units = 1
```

- [ ] **Step 2: Add /dist to .gitignore**

Use the Edit tool on `.gitignore`:

old_string:
```
/target
*.log
```

new_string:
```
/target
/dist
*.log
```

- [ ] **Step 3: Verify the workspace still builds and tests cleanly with the new profile**

```bash
cargo build --workspace 2>&1 | tail -10
cargo test --workspace 2>&1 | grep -E "^test result:|FAILED|error\["
```

Expected: build succeeds (the new `[profile.release]` section doesn't affect the default dev/test profile at all — this just confirms the `Cargo.toml` edit didn't break TOML parsing), all 381 tests pass, 0 failures — matching the workspace's established baseline.

- [ ] **Step 4: Create scripts/build-release.sh**

```bash
mkdir -p scripts
```

Use the Write tool to create `scripts/build-release.sh` with exactly this content:

```bash
#!/bin/bash
set -euo pipefail

TARGET="x86_64-unknown-linux-musl"
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${REPO_ROOT}"

VERSION=$(grep '^version = ' Cargo.toml | head -1 | sed 's/version = "\(.*\)"/\1/')
echo "Building aivyx-coder v${VERSION} for ${TARGET}..."

BUILD_LOG="$(mktemp)"
trap 'rm -f "${BUILD_LOG}"' EXIT

if ! cargo build --release --target "${TARGET}" -p aivyx 2>&1 | tee "${BUILD_LOG}"; then
  if grep -q "may not be installed" "${BUILD_LOG}"; then
    echo ""
    echo "ERROR: the ${TARGET} Rust target is not installed."
    echo "On Arch/CachyOS: pacman -S rust-musl"
    echo "On rustup-managed toolchains: rustup target add ${TARGET}"
    exit 1
  fi
  echo ""
  echo "ERROR: build failed. See output above."
  exit 1
fi

BINARY="target/${TARGET}/release/aivyx"
if [ ! -f "${BINARY}" ]; then
  echo "ERROR: expected binary not found at ${BINARY}"
  exit 1
fi

DIST_DIR="dist"
STAGE_NAME="aivyx-coder-v${VERSION}-x86_64-linux-musl"
STAGE_DIR="${DIST_DIR}/${STAGE_NAME}"
TARBALL_NAME="${STAGE_NAME}.tar.gz"

rm -rf "${STAGE_DIR}"
mkdir -p "${STAGE_DIR}"
cp "${BINARY}" "${STAGE_DIR}/aivyx"
cp README.md LICENSE-MIT LICENSE-APACHE "${STAGE_DIR}/"

tar -czf "${DIST_DIR}/${TARBALL_NAME}" -C "${DIST_DIR}" "${STAGE_NAME}"
rm -rf "${STAGE_DIR}"

(cd "${DIST_DIR}" && sha256sum "${TARBALL_NAME}" > "${TARBALL_NAME}.sha256")

echo ""
echo "Built: ${DIST_DIR}/${TARBALL_NAME}"
echo "Checksum: ${DIST_DIR}/${TARBALL_NAME}.sha256"
```

- [ ] **Step 5: Make the script executable**

```bash
chmod +x scripts/build-release.sh
```

- [ ] **Step 6: Run the script and handle whichever of the two known outcomes occurs**

```bash
./scripts/build-release.sh
```

**Outcome A — it succeeds** (the toolchain skew described in Global Constraints has resolved, e.g. via a `pacman -Syu` since this plan was written, or was never present in your environment): proceed to Step 7 for full artifact verification.

**Outcome B — it fails with the exact `E0425`/`cannot find function, tuple struct or tuple variant \`Some\`` signature** (inside `regex-syntax` or any other third-party dependency, with a `note:` mentioning mismatched `compiler_builtins` build hashes): this is the known, pre-existing environment issue, not a defect in this script or task. Do the following instead of Step 7:
1. Confirm the failure signature genuinely matches (the `Some`-not-found error, not the different "target not installed" message the script's own error handling targets) — if it's a *different* error, that's a real bug, investigate and fix it normally.
2. Run `pacman -Q rust rust-musl` and record both version strings in your report.
3. Report this task as `DONE_WITH_CONCERNS`, not `BLOCKED` — the script itself is complete and its logic is sound (verified in Step 8 below, which doesn't require a successful build), the only thing unverified is the actual successful-build artifact, due to a local system package sync issue outside this project's control.
4. Skip to Step 8.

- [ ] **Step 7: (Outcome A only) Verify the produced artifact is real and correct**

```bash
VERSION=$(grep '^version = ' Cargo.toml | head -1 | sed 's/version = "\(.*\)"/\1/')
TARBALL="dist/aivyx-coder-v${VERSION}-x86_64-linux-musl.tar.gz"
ls -la "${TARBALL}" "${TARBALL}.sha256"

# Verify checksum
(cd dist && sha256sum -c "$(basename "${TARBALL}").sha256")

# Extract into a scratch dir and inspect contents
SCRATCH=$(mktemp -d)
tar -xzf "${TARBALL}" -C "${SCRATCH}"
ls -la "${SCRATCH}/aivyx-coder-v${VERSION}-x86_64-linux-musl/"

# Confirm exactly these 4 files, nothing else
ls "${SCRATCH}/aivyx-coder-v${VERSION}-x86_64-linux-musl/" | sort

# Confirm it's a real static musl binary, not accidentally dynamically linked
file "${SCRATCH}/aivyx-coder-v${VERSION}-x86_64-linux-musl/aivyx"
ldd "${SCRATCH}/aivyx-coder-v${VERSION}-x86_64-linux-musl/aivyx" || true

# Confirm it actually runs
"${SCRATCH}/aivyx-coder-v${VERSION}-x86_64-linux-musl/aivyx" --help

rm -rf "${SCRATCH}"
```

Expected: checksum verification passes (`OK`); the extracted directory contains exactly `aivyx`, `README.md`, `LICENSE-MIT`, `LICENSE-APACHE` (4 files, nothing else); `file` reports the binary as statically linked (e.g. "statically linked" or "static-pie linked", not naming a dynamic interpreter); `ldd` reports something like "not a dynamic executable" (the `|| true` is because `ldd` on a genuinely static binary can itself exit non-zero on some systems — that's expected, not a failure); the binary actually runs and prints help/usage text rather than crashing.

- [ ] **Step 8: Verify the script's error-handling path independently of Step 6/7's outcome**

This works regardless of whether the toolchain skew is present. It needs a target triple that rustc genuinely recognizes but that isn't installed here — a *fake* target name fails a completely different way (rustc rejects it immediately with "error loading target specification", never reaching the "may not be installed" message this script's error handling greps for). `aarch64-unknown-linux-musl` is a real, known target and — unless you've separately installed the `rust-aarch64-musl` package — is not installed on this machine, making it the right target for this negative test:

```bash
pacman -Q rust-aarch64-musl 2>&1
```

If that reports the package as installed, pick any other real-but-likely-uninstalled target instead (e.g. `arm-unknown-linux-musleabi`) — the point is a target rustc's own `--print target-list` includes but whose std library isn't present here.

```bash
# Temporarily point the script at a real-but-uninstalled target to
# confirm its error-handling path triggers correctly and gives an
# actionable message
sed 's/x86_64-unknown-linux-musl/aarch64-unknown-linux-musl/' scripts/build-release.sh > /tmp/build-release-negative-test.sh
chmod +x /tmp/build-release-negative-test.sh
/tmp/build-release-negative-test.sh 2>&1 | tail -15
rm -f /tmp/build-release-negative-test.sh
```

Expected: the script fails, and its output includes the `ERROR: the aarch64-unknown-linux-musl Rust target is not installed.` message with the `pacman -S rust-musl` / `rustup target add` guidance — confirming the error-handling branch works correctly and independently of any real toolchain state. (This will pull in and compile dozens of dependency crates before failing at the actual std-linking step — that's expected, matching how `cargo build` always compiles as far as it can before reporting a target-linking failure at the very end.)

- [ ] **Step 9: Commit**

```bash
git add Cargo.toml .gitignore scripts/build-release.sh
git commit -m "Add release profile tuning and local release-build script"
```

---

### Task 2: GitHub Actions release workflow

**Files:**
- Create: `.github/workflows/release.yml`

**Interfaces:**
- Consumes: nothing from Task 1 at the file level (this is a separate, standalone workflow definition — it doesn't invoke `scripts/build-release.sh`, it duplicates the equivalent steps inline, matching GitHub Actions' own idiomatic style of self-contained workflow steps rather than shelling out to a repo script; this is a deliberate choice, not an oversight — see the Note after Step 1). It must produce artifacts with the *same naming convention and contents* as Task 1's script, since both are meant to produce equivalent releases.

This task cannot be executed end-to-end (the repository isn't on GitHub yet, so no tag push can trigger it) — verification here is limited to structural correctness (valid YAML, sound step logic reviewed against Task 1's script) rather than an actual triggered run, as specified in the spec's own Testing/verification section.

- [ ] **Step 1: Create the workflow directory and file**

```bash
mkdir -p .github/workflows
```

Use the Write tool to create `.github/workflows/release.yml` with exactly this content:

```yaml
name: Release

on:
  push:
    tags:
      - 'v*'

jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - name: Checkout
        uses: actions/checkout@v4

      - name: Install Rust with musl target
        uses: dtolnay/rust-toolchain@stable
        with:
          targets: x86_64-unknown-linux-musl

      - name: Build release binary
        run: cargo build --release --target x86_64-unknown-linux-musl -p aivyx

      - name: Package release artifact
        run: |
          STAGE_NAME="aivyx-coder-${GITHUB_REF_NAME}-x86_64-linux-musl"
          mkdir -p "dist/${STAGE_NAME}"
          cp target/x86_64-unknown-linux-musl/release/aivyx "dist/${STAGE_NAME}/aivyx"
          cp README.md LICENSE-MIT LICENSE-APACHE "dist/${STAGE_NAME}/"
          cd dist
          tar -czf "${STAGE_NAME}.tar.gz" "${STAGE_NAME}"
          sha256sum "${STAGE_NAME}.tar.gz" > "${STAGE_NAME}.tar.gz.sha256"

      - name: Create GitHub Release
        uses: softprops/action-gh-release@v2
        with:
          files: |
            dist/*.tar.gz
            dist/*.tar.gz.sha256
          generate_release_notes: true
```

Note on `${GITHUB_REF_NAME}` vs Task 1's `v${VERSION}`: when this workflow runs, `GITHUB_REF_NAME` is the literal pushed tag (e.g. `v0.1.0`, already including the `v` prefix) — so `aivyx-coder-${GITHUB_REF_NAME}-x86_64-linux-musl` produces `aivyx-coder-v0.1.0-x86_64-linux-musl`, the exact same naming Task 1's script produces by concatenating `v` + `${VERSION}` (`0.1.0`) itself. Both arrive at an identical filename through different but equivalent means (the tag *is* the release version marker in CI; `Cargo.toml`'s own field is the source of truth for the local script, which has no tag to read from).

- [ ] **Step 2: Verify the YAML is well-formed**

```bash
python3 -c "import yaml; yaml.safe_load(open('.github/workflows/release.yml')); print('valid YAML')"
```

If `python3`/`pyyaml` isn't available, use any other YAML validator available in your environment (e.g. `ruby -ryaml -e "YAML.load_file('.github/workflows/release.yml')"`, or a text editor with YAML validation) — the goal is confirming the file parses as valid YAML, not tied to one specific tool.

Expected: `valid YAML` printed, no parse errors.

- [ ] **Step 3: Manually cross-check against Task 1's script**

Read `scripts/build-release.sh` (from Task 1) and `.github/workflows/release.yml` side by side. Confirm:
- Same target triple (`x86_64-unknown-linux-musl`) in both.
- Same tarball contents: `aivyx` (renamed from the built binary), `README.md`, `LICENSE-MIT`, `LICENSE-APACHE` — nothing else, in both.
- Same naming pattern: `aivyx-coder-v<version>-x86_64-linux-musl.tar.gz` in both (accounting for the `GITHUB_REF_NAME` vs `v${VERSION}` equivalence noted in Step 1).
- Same checksum approach: `sha256sum` run from inside the `dist/` directory so the checksum file's internal path is relative (matching Task 1's script), in both.

Report any discrepancy found — if the two files describe different artifacts, that's a real defect to fix before this task is done.

- [ ] **Step 4: Commit**

```bash
git add .github/workflows/release.yml
git commit -m "Add GitHub Actions release workflow (inert until repo is pushed)"
```

---

### Task 3: README documentation and final consistency check

**Files:**
- Modify: `README.md`

**Interfaces:**
- Consumes: `scripts/build-release.sh`'s exact invocation and output paths from Task 1 (documented verbatim, not paraphrased).

This task depends on Task 1 being complete (it documents the script Task 1 created). Before starting, re-grep `README.md` for `## Building and running` to confirm the surrounding text hasn't drifted since this plan was written — the plan has already been through two prior sub-projects' edits to this same file in this chapter, so re-verify rather than trust line numbers.

- [ ] **Step 1: Add release-build documentation to README.md**

Use the Edit tool on `README.md`:

old_string:
```
## Building and running

```
cargo run -p aivyx
```

Requires a local inference server. On first run a config file is written to
```

new_string:
```
## Building and running

```
cargo run -p aivyx
```

For a release build (Linux x86_64 only, static musl binary, matching
where the real Landlock+seccomp sandbox actually works):

```
scripts/build-release.sh
```

Produces `dist/aivyx-coder-v<version>-x86_64-linux-musl.tar.gz` plus a
`.sha256` checksum alongside it. Tagged releases (`vX.Y.Z`) are also
built and published automatically via GitHub Actions once this
repository is pushed to GitHub — check the repository's Releases page
for pre-built downloads at that point.

Requires a local inference server. On first run a config file is written to
```

- [ ] **Step 2: Verify the edit landed correctly and reads coherently**

```bash
grep -n -A 20 "^## Building and running" README.md | head -25
```

Expected: the new paragraph appears between the `cargo run -p aivyx` block and the `Requires a local inference server` sentence, reads as a coherent addition (not cut off mid-sentence, no leftover duplicate text).

- [ ] **Step 3: Final whole-repo consistency check**

```bash
git status --short
git diff --stat main
```

Expected file list across all three tasks: `Cargo.toml`, `.gitignore`, `scripts/build-release.sh`, `.github/workflows/release.yml`, `README.md`. No other files. No `docs/HISTORY.md` or `ROADMAP.md` changes (this sub-project doesn't touch the docs-cleanup deliverables), no `crates/**` changes (no source code touched anywhere in this plan).

- [ ] **Step 4: Final sanity build**

```bash
cargo build --workspace 2>&1 | tail -5
cargo test --workspace 2>&1 | grep -E "^test result:|FAILED|error\["
```

Expected: build succeeds, all 381 tests pass — confirming the whole branch (all three tasks combined) leaves the normal dev workflow completely unaffected.

- [ ] **Step 5: Commit**

```bash
git add README.md
git commit -m "Document release builds in README"
```
