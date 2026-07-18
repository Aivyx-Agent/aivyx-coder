# Codebase Cleanup (pre-GitHub-push, sub-project 2 of 3) — Design

**Status:** Approved by user 2026-07-18.

## Context

This is the second of three independent sub-projects preparing aivyx-coder
for its first push to GitHub (see `ROADMAP.md`'s history). Build order,
chosen by the user: **Docs → Codebase → Release**. Sub-project 1 (docs
cleanup) is merged to `main`. This spec covers the codebase only.
Release-build strategy is a separate, later spec.

Fresh audit performed before this design was written (clippy re-run,
`grep` scans, a full per-file line-count survey, and a dependency audit
via `cargo-machete` with every finding manually verified against source
to rule out macro-only false positives — derive macros, attribute macros,
`.await`/async-fn usage — before trusting it):

- **Clippy**: clean, zero warnings across the workspace (`cargo clippy
  --workspace --all-targets`).
- **No `TODO`/`FIXME`/`XXX`** markers anywhere in `crates/*/src`.
- **No stray `#[allow(dead_code)]`** — the only occurrence
  (`crates/aivyx-tools/src/lsp/protocol.rs:17`) is a legitimate,
  pre-existing case, not touched by this spec.
- **File sizes**: `crates/aivyx-core/src/agent.rs` is the one clear
  outlier at 4052 lines (next-largest file in the workspace is 1017
  lines). Broken down: lines 1–314 are type/error definitions (~230
  lines: `AgentEvent`, `EditFormat`, `AgentConfig`, `AgentError`,
  `AgentsFileConfig` and their impls), lines 315–1500 are `impl Agent`
  (~1185 lines, 24 methods — turn execution, compaction, persistence,
  council/architect/wiki sub-dispatch, all sharing `&mut self` state,
  genuinely cohesive), and lines 1501–4052 are `#[cfg(test)] mod tests`
  (~2551 lines). Every other large file in the workspace (`aivyx-tui/src/
  app.rs` 1017, `aivyx-sandbox/src/confirmation.rs` 995,
  `aivyx-tools/src/lsp/mod.rs` 798) was explicitly considered and
  excluded from this spec's scope — not flagged as urgent, and the user
  confirmed scope is `agent.rs` specifically.
- **Unused dependencies** (`cargo-machete`, each verified by grep for
  direct path usage AND macro-only usage patterns —
  derive/attribute-macro/`.await` — before being trusted; zero matches
  found for any of them, confirming all 8 are genuinely unused):
  - `aivyx-llm`: `tokio`, `tracing`
  - `aivyx-sandbox`: `aivyx-types`, `serde`, `thiserror`, `tokio-util`
  - `aivyx-tools`: `grep-matcher`
  - `aivyx`: `aivyx-types`
  - `aivyx-repomap`: `tracing`
- **Project convention for split modules**: the workspace already uses
  directory-style modules (`mod.rs` + siblings) in two places —
  `crates/aivyx-tools/src/lsp/{mod.rs, protocol.rs, transport.rs}` and
  `crates/aivyx-tools/src/mcp/{mod.rs, ...}`. The `agent.rs` split
  follows this existing pattern rather than inventing a new one.
- `crates/aivyx-core/src/lib.rs:1` declares `pub mod agent;` and
  `lib.rs:9` re-exports `pub use agent::{Agent, AgentConfig, AgentError,
  AgentEvent, EditFormat};` — no other crate in the workspace imports
  `aivyx_core::agent::*` directly (confirmed via grep); everything
  external goes through these two lines, so the split must preserve
  both exactly, unchanged, for zero external-API impact.

## Decisions (confirmed with user via one-at-a-time questions before this doc was written)

1. **Scope**: both dependency trimming and the `agent.rs` split (not
   just one or the other).
2. **`agent.rs` split depth**: extract tests and type definitions into
   sibling files; leave the `impl Agent` orchestration logic together in
   one file rather than further splitting it — it's one cohesive state
   machine, and further splitting was explicitly assessed as
   readability-motivated-only churn with real coupling risk, not
   pursued.
3. **No other files** in scope — `app.rs`, `confirmation.rs`,
   `lsp/mod.rs` are explicitly excluded, matching this project's
   established practice of not proposing unrelated refactoring beyond
   what serves the stated goal.

## Changes

### 1. Remove 8 unused dependencies

Exact removals, one line each, from each crate's `[dependencies]` table
(none affect `[dev-dependencies]`, `[features]`, or any other section):

- `crates/aivyx-llm/Cargo.toml`: remove the `tokio = { ... }` line and
  the `tracing = "0.1.44"` line.
- `crates/aivyx-sandbox/Cargo.toml`: remove the `aivyx-types = { ... }`
  line, the `serde = { ... }` line, the `thiserror = "2.0.18"` line, and
  the `tokio-util = "0.7.18"` line. Do **not** touch the `[dev-dependencies]`
  section's own separate `tokio = { ... }` entry (used for tests, not
  flagged) or the main `[dependencies]` section's `tokio = { features =
  ["process"] }` entry (not flagged — genuinely used).
- `crates/aivyx-tools/Cargo.toml`: remove the `grep-matcher = "0.1.8"`
  line.
- `crates/aivyx/Cargo.toml`: remove the `aivyx-types = { ... }` line.
- `crates/aivyx-repomap/Cargo.toml`: remove the `tracing = "0.1.44"`
  line.

No `use` statements or code references these removed dependencies (that
was the whole basis for flagging them) — this is a pure `Cargo.toml`
edit, no `.rs` file changes for this part.

### 2. Split `agent.rs` into a directory module

Convert `crates/aivyx-core/src/agent.rs` into
`crates/aivyx-core/src/agent/` containing three files:

- **`agent/mod.rs`** — the `Agent` struct definition and its entire
  `impl Agent` block (currently lines 231–1500 of `agent.rs`), unchanged
  line-for-line except for adding, at the top: `mod types;` and
  `#[cfg(test)] mod tests;`, plus two `use` declarations pulling the
  moved types back into scope so every reference inside the unchanged
  `impl Agent` body still resolves without editing the body itself —
  `pub use types::{AgentEvent, EditFormat, AgentConfig, AgentError};`
  (these 4 are the exact set `lib.rs:9` already re-exports as
  `agent::{Agent, AgentConfig, AgentError, AgentEvent, EditFormat}` —
  this `pub use` is what keeps that line resolving unchanged) and
  `use types::{VerificationConfig, AgentsFileConfig};` (these 2 are
  private, `impl Agent`-internal-only, never referenced outside
  `aivyx-core` today, so a plain non-`pub` `use` is correct and
  sufficient).
- **`agent/types.rs`** — `AgentEvent`, `EditFormat`, `VerificationConfig`,
  `AgentConfig`, `impl Default for AgentConfig`, `AgentError`,
  `AgentsFileConfig` (currently lines 1–230 of `agent.rs`), moved
  verbatim with only `use` statements adjusted as needed for the new
  file boundary.
- **`agent/tests.rs`** — the entire `#[cfg(test)] mod tests { ... }`
  block (currently lines 1501–4052 of `agent.rs`), moved verbatim.
  `agent/mod.rs` declares it with `#[cfg(test)] mod tests;`.

**External API must not change.** `crates/aivyx-core/src/lib.rs:1`'s
`pub mod agent;` and `lib.rs:9`'s `pub use agent::{Agent, AgentConfig,
AgentError, AgentEvent, EditFormat};` stay exactly as they are — the
split is invisible from outside `aivyx-core`. Anything in `types.rs` that
`mod.rs`'s code references must be `pub use`d or otherwise made visible
from `agent/mod.rs` so those two `lib.rs` lines keep resolving to the
same items they do today.

This is pure code motion — no logic changes, no behavior changes, no
signature changes to any public or private item.

## Out of scope for this spec

- Any further splitting of the `impl Agent` orchestration logic itself
  (Decision 2).
- Any other large file (`app.rs`, `confirmation.rs`, `lsp/mod.rs`) —
  not touched.
- Any dependency *version* bumps — this spec only removes unused
  entries, doesn't touch versions of kept dependencies.
- Any change to `Cargo.lock` beyond what `cargo build` naturally
  regenerates as a side effect of the `Cargo.toml` edits.
- Release/CI/packaging work (sub-project 3).

## Testing / verification

- **Dependency removal**: after editing all 5 `Cargo.toml` files, run
  `cargo-machete` again (must report no findings for these 5 crates —
  it's fine if it still finds nothing elsewhere, since nothing else was
  ever flagged), then `cargo build --workspace`, `cargo test --workspace`,
  and `cargo clippy --workspace --all-targets` — all three must succeed
  cleanly (build succeeds, all tests that passed before still pass,
  zero new clippy warnings). `Cargo.lock` will change as a side effect;
  that's expected and correct.
- **`agent.rs` split**: before the split, capture the exact output of
  `cargo test -p aivyx-core` (test names and count — 190 tests per the
  last full workspace run, though the exact number for `aivyx-core`
  alone should be captured freshly rather than assumed). After the
  split, `cargo test -p aivyx-core` must produce the **identical** set
  of passing tests — same names, same count, zero failures, zero
  newly-ignored tests. `cargo build --workspace` and `cargo clippy
  --workspace --all-targets` must also stay clean, confirming the split
  didn't break anything outside `aivyx-core` either (e.g. via the
  `lib.rs` re-exports).
- **Final check**: `git diff --stat main` (or the branch's merge-base)
  should show only the 5 `Cargo.toml` files, `Cargo.lock`, and the
  `agent.rs` → `agent/{mod,types,tests}.rs` change (as a delete +
  3 creates, or however git represents it) — no other files touched,
  no `docs/**` changes (this is a code-only sub-project, unlike
  sub-project 1).
