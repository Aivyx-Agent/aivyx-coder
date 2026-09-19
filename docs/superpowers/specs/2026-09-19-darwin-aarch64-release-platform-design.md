# darwin-aarch64 Release Platform Design

## Context

`docs/superpowers/specs/2026-09-19-acp-registry-listing-design.md`'s
Decision 4 scoped release-platform expansion as known, deferred follow-on
work: today's release pipeline
(`.github/workflows/release.yml`) builds exactly one target,
`x86_64-unknown-linux-musl`. The ACP registry accepts Linux-only
submissions (it only limits which editor-host OSes get one-click
install), so platform expansion isn't blocking a registry PR — but real
reach means adding `darwin-aarch64` (the dominant Zed/JetBrains dev
machine) at some point. This spec covers that expansion on its own,
explicitly *not* the registry PR itself — confirmed with the project
owner as this round's scope (registry submission stays separate, later,
undecided-when work).

## Grounding

Read/tested directly against the real repo, not assumed:

- `.github/workflows/release.yml` — one job, `runs-on: ubuntu-latest`,
  installs the `x86_64-unknown-linux-musl` Rust target + `musl-tools`,
  builds with `CC_x86_64_unknown_linux_musl: musl-gcc`, packages a
  tarball + sha256, creates a GitHub Release via
  `softprops/action-gh-release@v2` with `generate_release_notes: true`.
- `aivyx-coder/CLAUDE.md` and `README.md` both document that the real
  sandbox (Linux Landlock + seccomp-bpf, via the `aivyx-confine` crate)
  is on by default and that non-Linux platforms should build with
  `--no-default-features` on `aivyx-sandbox`, falling back to
  `NoopConfiner`.
- **This documented workaround does not actually work as described —
  verified empirically, not assumed.** `crates/aivyx-confine/Cargo.toml`
  (a separate, pinned `git` dependency repo) declares `landlock` and
  `seccompiler` as plain optional deps gated behind a `sandbox-backend`
  feature (`default = ["sandbox-backend"]`) — no `target_os` gating at
  all, so both are genuinely Linux-only crates pulled in unconditionally
  unless that feature is off. `crates/aivyx-sandbox/Cargo.toml` mirrors
  this (`default = ["sandbox-backend"]`, forwarding to
  `aivyx-confine/sandbox-backend`). But every workspace crate that
  depends on `aivyx-sandbox` — `aivyx`, `aivyx-acp`, `aivyx-core`,
  `aivyx-mcp-server`, `aivyx-tools`, `aivyx-tui` (full list via
  `cargo tree --workspace -e normal -i aivyx-sandbox`) — declares that
  dependency with implicit `default-features = true` (no
  `default-features = false` anywhere). Directly tested via
  `cargo tree -i landlock` under `-p aivyx --no-default-features`,
  `-p aivyx -p aivyx-sandbox --no-default-features`, and
  `--workspace --no-default-features` — `landlock` remains in the
  resolved dependency graph in every case. Cargo's `--no-default-features`
  CLI flag only suppresses default features on the package(s) it's
  invoked against directly; it does not propagate through dependency
  edges that don't themselves opt out via `default-features = false` in
  their own `Cargo.toml`. Confirmed the reverse holds too:
  `cargo tree -p aivyx-sandbox --no-default-features -i landlock` (built
  in isolation, no dependents) correctly shows no match — the mechanism
  works at the single-crate level, it just isn't wired through the rest
  of the workspace.
- `landlock`/`seccompiler` are Linux-specific (Landlock LSM,
  seccomp-bpf) and are expected to fail to compile on macOS outright,
  not merely no-op at runtime — consistent with why `aivyx-confine`
  gates them behind an opt-out feature at all.
- `crates/aivyx-config/src/lib.rs:544-556` —
  `SandboxSettings::require_enforcement` defaults to `true` on every
  platform today (`RulesetStatus` fail-closed semantics per
  `aivyx-coder/CLAUDE.md`'s "Sandbox internals" section). A build with
  `sandbox-backend` disabled (necessarily true for any macOS build)
  would hit this fail-closed path on its very first confined command,
  refusing to run anything, with `require_enforcement`'s own doc comment
  explaining the fail-closed choice was made for a platform that *can*
  enforce but isn't configured to — not for a platform that structurally
  cannot.
- `crates/aivyx/Cargo.toml`'s `[features]` block already has
  `provider-mistral-rs-metal`/`provider-mistral-rs-accelerate` —
  macOS-specific local-inference acceleration features exist and are
  opt-in (not default) already, unrelated to this spec's sandbox
  question but confirming this codebase already has some
  platform-conditional precedent to follow.
- GitHub-hosted `macos-14`/`macos-latest` runners are Apple Silicon
  (`aarch64-apple-darwin`) natively — no cross-compilation toolchain
  (e.g. `osxcross`) is needed; a native runner in the release matrix is
  sufficient.

## Decisions

**1. Platform scope: `darwin-aarch64` only, this round.** Not
`darwin-x86_64`, not `linux-aarch64`. Confirmed with the project owner —
covers the dominant Zed/JetBrains dev machine; other platforms are
separate, later, undecided-when scope if ever pursued.

**2. Fix the `aivyx-sandbox` feature-forwarding gap as the real first
task, not a documentation footnote.** Every crate in the dependency
chain (`aivyx`, `aivyx-acp`, `aivyx-core`, `aivyx-mcp-server`,
`aivyx-tools`, `aivyx-tui`) gets its `aivyx-sandbox` dependency changed
to `default-features = false`, plus its own `sandbox-backend` feature
(`default = ["sandbox-backend"]`) that forwards to
`aivyx-sandbox/sandbox-backend`. This is additive for Linux (defaults
stay on, behavior unchanged) and makes `--no-default-features
--workspace` (or equivalently, `--no-default-features` passed to every
crate in the chain via matching `-p` flags) genuinely disable Landlock
for the first time — fixing the README's already-published but
currently-inaccurate guidance for any future non-Linux contributor, not
just this release pipeline.

**3. `SandboxSettings::require_enforcement`'s default becomes
`cfg(target_os = "linux")`-gated: `true` on Linux (unchanged), `false`
everywhere else.** A platform that cannot compile `sandbox-backend` at
all should never default to a setting whose entire job is refusing to
run unconfined — that's not "safe by default," it's "broken by
default with no clear cause." No existing user's behavior changes
(Linux stays `true`); no existing macOS user is affected either, since
no macOS binary has ever been distributed.

**4. `release.yml` becomes a two-stage matrix workflow**, not N
independent single-target jobs each calling the release action: a
`build` job matrixed over
`[{target: x86_64-unknown-linux-musl, runner: ubuntu-latest}, {target: aarch64-apple-darwin, runner: macos-14}]`,
each producing and uploading a packaged artifact
(`actions/upload-artifact`) rather than creating a release directly;
then a single `release` job, gated on both matrix legs via `needs:`,
downloads every artifact and calls `softprops/action-gh-release@v2`
exactly once with the full file set. This avoids any race between
matrix legs independently trying to create or attach to the same tag's
release. The macOS leg's build step passes `--no-default-features`
(working, per Decision 2) instead of the Linux leg's musl-target/CC
setup; packaging (tarball + sha256, bundled `README.md`/license
files/`docs/logos`) stays identical in shape between legs, just with a
different `STAGE_NAME` suffix (`darwin-aarch64` vs. the existing
`x86_64-linux-musl`).

**5. The macOS release build does not enable any `provider-mistral-rs*`
feature.** Matches the existing Linux release build's behavior exactly
(no `--features` flag at all today) — the shipped binary stays a thin
HTTP client against Ollama/llama-server on every platform, same as
today. Enabling embedded/accelerated local inference for a release
binary is a separate, unscoped product decision this spec doesn't make.

## What this spec does not decide

- `darwin-x86_64`, `linux-aarch64`, or Windows support — out of scope,
  not evaluated here beyond Decision 1's platform-scope note.
- The actual ACP registry PR (`agent.json`/`icon.svg`, submission
  process) — separate, later, undecided-when scope per the parent
  spec's own Decision 4 and this round's explicit sequencing choice.
- Whether to eventually enable `provider-mistral-rs-metal` for a macOS
  release build — noted as a real, distinct future option in Decision 5,
  not decided here.
- Any change to `aivyx-confine`'s own repo/Cargo.toml — this spec's
  fixes are entirely within `aivyx-coder`'s own crates; `aivyx-confine`
  already correctly gates `landlock`/`seccompiler` behind its own
  `sandbox-backend` feature, so no change is needed there.
