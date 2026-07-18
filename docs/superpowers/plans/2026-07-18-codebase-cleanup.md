# Codebase Cleanup Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Trim 8 verified-unused dependencies across 5 crates, and split `crates/aivyx-core/src/agent.rs` (4052 lines) into a directory module (`agent/mod.rs`, `agent/types.rs`, `agent/tests.rs`) — preparing aivyx-coder's codebase for its first public GitHub push.

**Architecture:** Two independent, sequential tasks. Task 1 is a pure `Cargo.toml` edit (no `.rs` changes). Task 2 is pure code motion on one file (no logic changes, no signature changes, no behavior changes) — it follows the project's existing directory-module convention (`lsp/mod.rs`, `mcp/mod.rs`).

**Tech Stack:** Rust (edition 2024), Cargo workspace. `cargo-machete` (already installed at `/home/julian/.cargo/bin/cargo-machete` in this environment) for dependency-audit verification.

## Global Constraints

- Full authoritative spec: `docs/superpowers/specs/2026-07-18-codebase-cleanup-design.md` — read it before starting; every decision below traces back to it.
- No files outside the scope of this plan's two tasks may change — specifically, do **not** touch `crates/aivyx-tui/src/app.rs`, `crates/aivyx-sandbox/src/confirmation.rs`, or `crates/aivyx-tools/src/lsp/mod.rs` (explicitly excluded by the spec).
- `crates/aivyx-core/src/lib.rs` must not change at all — its `pub mod agent;` (line 1) and `pub use agent::{Agent, AgentConfig, AgentError, AgentEvent, EditFormat};` (line 9) must keep resolving to the exact same items after Task 2 as before.
- No dependency *version* changes — only removal of unused entries.
- No changes to `docs/**` — this is a code-only sub-project.
- Every task's verification gate (below) must pass with zero new warnings and zero test regressions before moving to the next task.

---

### Task 1: Remove 8 unused dependencies

**Files:**
- Modify: `crates/aivyx-llm/Cargo.toml`
- Modify: `crates/aivyx-sandbox/Cargo.toml`
- Modify: `crates/aivyx-tools/Cargo.toml`
- Modify: `crates/aivyx/Cargo.toml`
- Modify: `crates/aivyx-repomap/Cargo.toml`

**Interfaces:**
- None — this task has no code dependencies on Task 2, and Task 2 has none on this task. Independent.

- [ ] **Step 1: Verify cargo-machete is available**

```bash
which cargo-machete || /home/julian/.cargo/bin/cargo-machete --version
```

If neither works, install it: `cargo install cargo-machete --locked`.

- [ ] **Step 2: Confirm the current findings match what this task expects**

```bash
cd /path/to/this/worktree
cargo-machete 2>&1
```

Expected output includes exactly these 5 crates with exactly these dependencies flagged (order within each crate's list may vary):
```
aivyx-llm -- ./crates/aivyx-llm/Cargo.toml:
	tokio
	tracing
aivyx-sandbox -- ./crates/aivyx-sandbox/Cargo.toml:
	aivyx-types
	serde
	thiserror
	tokio-util
aivyx-tools -- ./crates/aivyx-tools/Cargo.toml:
	grep-matcher
aivyx -- ./crates/aivyx/Cargo.toml:
	aivyx-types
aivyx-repomap -- ./crates/aivyx-repomap/Cargo.toml:
	tracing
```

If the actual output differs (different crates, different dependency names), STOP and report — the codebase has changed since this plan was written and the removals below may no longer be correct.

- [ ] **Step 3: Edit crates/aivyx-llm/Cargo.toml**

Use the Edit tool with:

old_string:
```
[dependencies]
aivyx-types = { version = "0.1.0", path = "../aivyx-types" }
async-trait = "0.1.89"
eventsource-stream = "0.2.3"
futures = "0.3.32"
reqwest = { version = "0.13.4", default-features = false, features = ["json", "stream", "rustls"] }
serde = { version = "1.0.228", features = ["derive"] }
serde_json = "1.0.150"
thiserror = "2.0.18"
tokio = { version = "1.52.3", features = ["rt-multi-thread", "macros", "time"] }
tokio-stream = "0.1.18"
tracing = "0.1.44"
```

new_string:
```
[dependencies]
aivyx-types = { version = "0.1.0", path = "../aivyx-types" }
async-trait = "0.1.89"
eventsource-stream = "0.2.3"
futures = "0.3.32"
reqwest = { version = "0.13.4", default-features = false, features = ["json", "stream", "rustls"] }
serde = { version = "1.0.228", features = ["derive"] }
serde_json = "1.0.150"
thiserror = "2.0.18"
tokio-stream = "0.1.18"
```

(Removes the `tokio` and `tracing` lines; `aivyx-types`, `thiserror`, and everything else stays — those are genuinely used in this crate.)

- [ ] **Step 4: Edit crates/aivyx-sandbox/Cargo.toml**

Use the Edit tool with:

old_string:
```
[dependencies]
aivyx-types = { version = "0.1.0", path = "../aivyx-types" }
async-trait = "0.1.89"
landlock = { version = "0.4.5", optional = true }
libc = { version = "0.2.186", optional = true }
seccompiler = { version = "0.5.0", optional = true }
serde = { version = "1.0.228", features = ["derive"] }
serde_json = "1.0.150"
thiserror = "2.0.18"
tokio = { version = "1.52.3", features = ["process"] }
tokio-util = "0.7.18"
tracing = "0.1.44"
```

new_string:
```
[dependencies]
async-trait = "0.1.89"
landlock = { version = "0.4.5", optional = true }
libc = { version = "0.2.186", optional = true }
seccompiler = { version = "0.5.0", optional = true }
serde_json = "1.0.150"
tokio = { version = "1.52.3", features = ["process"] }
tracing = "0.1.44"
```

(Removes `aivyx-types`, `serde`, `thiserror`, `tokio-util`. Keeps `tokio` with its `["process"]` features — that entry was **not** flagged and is genuinely used — and keeps `tracing`, which was **not** flagged for this crate, only for `aivyx-llm` and `aivyx-repomap`.)

Do **not** touch this file's `[dev-dependencies]` section (its own separate `tokio = { version = "1.52.3", features = ["macros", "rt"] }` and `tempfile = "3.27.0"` entries are unrelated and not flagged) or its `[features]` section.

- [ ] **Step 5: Edit crates/aivyx-tools/Cargo.toml**

Use the Edit tool with:

old_string:
```
grep-matcher = "0.1.8"
grep-regex = "0.1.14"
```

new_string:
```
grep-regex = "0.1.14"
```

- [ ] **Step 6: Edit crates/aivyx/Cargo.toml**

Use the Edit tool with:

old_string:
```
aivyx-tui = { version = "0.1.0", path = "../aivyx-tui" }
aivyx-types = { version = "0.1.0", path = "../aivyx-types" }
anyhow = "1.0.103"
```

new_string:
```
aivyx-tui = { version = "0.1.0", path = "../aivyx-tui" }
anyhow = "1.0.103"
```

- [ ] **Step 7: Edit crates/aivyx-repomap/Cargo.toml**

Use the Edit tool with:

old_string:
```
[dependencies]
ignore = "0.4.28"
streaming-iterator = "0.1.9"
tracing = "0.1.44"
tree-sitter = "0.26.10"
tree-sitter-rust = "0.24.2"
```

new_string:
```
[dependencies]
ignore = "0.4.28"
streaming-iterator = "0.1.9"
tree-sitter = "0.26.10"
tree-sitter-rust = "0.24.2"
```

- [ ] **Step 8: Verify cargo-machete now reports nothing for these 5 crates**

```bash
cargo-machete 2>&1
```

Expected: none of `aivyx-llm`, `aivyx-sandbox`, `aivyx-tools`, `aivyx`, `aivyx-repomap` appear in the output (it's fine if any *other* crate is flagged — none ever were, but that's not this task's concern if it happens).

- [ ] **Step 9: Full workspace verification**

```bash
cargo build --workspace 2>&1 | tail -30
cargo test --workspace 2>&1 | grep -E "^test result:|FAILED|error\["
cargo clippy --workspace --all-targets 2>&1 | tail -30
```

Expected: build succeeds with no errors; every `test result:` line reads `ok` with the same pass counts as before this task (0 failures anywhere); clippy produces zero warnings (matching the pre-existing clean baseline). `Cargo.lock` will have changed as a side effect of the removed dependencies — that's expected.

- [ ] **Step 10: Commit**

```bash
git add crates/aivyx-llm/Cargo.toml crates/aivyx-sandbox/Cargo.toml crates/aivyx-tools/Cargo.toml crates/aivyx/Cargo.toml crates/aivyx-repomap/Cargo.toml Cargo.lock
git commit -m "Chore: remove 8 unused dependencies (cargo-machete audit)"
```

---

### Task 2: Split agent.rs into agent/{mod,types,tests}.rs

**Files:**
- Create: `crates/aivyx-core/src/agent/mod.rs`
- Create: `crates/aivyx-core/src/agent/types.rs`
- Create: `crates/aivyx-core/src/agent/tests.rs`
- Delete: `crates/aivyx-core/src/agent.rs`

**Interfaces:**
- Consumes: nothing from Task 1.
- Produces: the exact same public API `crates/aivyx-core/src/lib.rs` already depends on (`agent::{Agent, AgentConfig, AgentError, AgentEvent, EditFormat}`) — `lib.rs` itself is not modified.

This task is pure code motion — every step below either copies text verbatim or makes a small, precisely-specified adjustment needed only because the code now spans multiple files (module declarations, and visibility fixes for two structs that move into a child module). No logic changes, no signature changes anywhere.

- [ ] **Step 1: Re-verify agent.rs's structure hasn't drifted**

```bash
grep -n "^pub struct\|^struct\|^pub enum\|^enum\|^impl \|^#\[cfg(test)\]\|^mod tests" crates/aivyx-core/src/agent.rs
wc -l crates/aivyx-core/src/agent.rs
```

Expected output (this plan was written against this exact structure):
```
21:pub enum AgentEvent {
117:pub enum EditFormat {
177:struct VerificationConfig {
188:pub struct AgentConfig {
197:impl Default for AgentConfig {
208:pub enum AgentError {
226:struct AgentsFileConfig {
231:pub struct Agent {
315:impl Agent {
1501:#[cfg(test)]
1502:mod tests {
```
and `4052 crates/aivyx-core/src/agent.rs`.

If this doesn't match exactly, STOP and report — every subsequent step in this task uses exact line numbers and exact text derived from this structure.

- [ ] **Step 2: Capture the pre-split test baseline for aivyx-core**

```bash
cargo test -p aivyx-core 2>&1 | grep -E "^test |^test result:" > /tmp/aivyx-core-tests-before.txt
cat /tmp/aivyx-core-tests-before.txt | tail -5
```

Keep this file — Step 11 compares against it.

- [ ] **Step 3: Create the agent/ directory and extract tests.rs verbatim**

```bash
mkdir -p crates/aivyx-core/src/agent
sed -n '1503,4051p' crates/aivyx-core/src/agent.rs > crates/aivyx-core/src/agent/tests.rs
```

This extracts the *body* of `mod tests { ... }` (line 1502's opening brace and line 4052's closing brace are deliberately excluded — the `mod tests;` declaration itself will live in `agent/mod.rs`, not in this file, per Rust's module-file convention).

Verify:
```bash
wc -l crates/aivyx-core/src/agent/tests.rs
head -3 crates/aivyx-core/src/agent/tests.rs
tail -3 crates/aivyx-core/src/agent/tests.rs
```
Expected: 2549 lines. First line: `    use super::*;`. Last line: `    }` (the closing brace of the last test function, one level of indentation — the original file's `mod tests { ... }` indentation is preserved verbatim, which is fine; Rust doesn't require top-level module content to be unindented, and `rustfmt` would normalize it if run, which this task does not require).

- [ ] **Step 4: Create agent/mod.rs from the non-test portion**

```bash
head -n 1499 crates/aivyx-core/src/agent.rs > crates/aivyx-core/src/agent/mod.rs
wc -l crates/aivyx-core/src/agent/mod.rs
```

Expected: 1499 lines. This captures everything except the tests module (lines 1500's blank separator and 1501-4052 are excluded). At this point `agent/mod.rs` is an exact copy of the first 1499 lines of the original file — it still contains the 6 type/struct/enum definitions that need to move to `types.rs` (removed in Step 6) and still has 2 import lines that need adjusting (Step 5).

- [ ] **Step 5: Fix agent/mod.rs's import block**

Two edits, using the Edit tool on `crates/aivyx-core/src/agent/mod.rs`:

**Edit 5a** — drop `LlmError` (now only needed by `types.rs`, not by anything in `mod.rs`'s own `impl Agent` block — verified by grep before this plan was written):

old_string:
```
use aivyx_llm::{ChatRequest, FinishReason, LlmBackend, LlmError, StreamEvent, ToolChoice};
```

new_string:
```
use aivyx_llm::{ChatRequest, FinishReason, LlmBackend, StreamEvent, ToolChoice};
```

**Edit 5b** — remove the now-unused `thiserror::Error` import (only needed for `AgentError`'s `#[derive(Debug, Error)]`, which moves to `types.rs`; verified by grep that `Error`/`thiserror` has no other use in `mod.rs`'s own content):

old_string:
```
use futures::StreamExt;
use thiserror::Error;
use tokio::sync::mpsc::UnboundedSender;
```

new_string:
```
use futures::StreamExt;
use tokio::sync::mpsc::UnboundedSender;
```

- [ ] **Step 6: Remove the 6 type definitions from agent/mod.rs, add module declarations and re-exports**

Three edits, using the Edit tool on `crates/aivyx-core/src/agent/mod.rs`. Each removes one contiguous chunk that will now live in `types.rs`, using surrounding stable text as anchors so the match is unambiguous.

**Edit 6a** — remove `AgentEvent`, and in the same edit add the new module declarations and re-exports right after the last `use crate::` line:

old_string:
```
use crate::session::{self, SessionState, Task};

#[derive(Debug, Clone)]
pub enum AgentEvent {
    TextDelta(String),
    ToolCallDetected(ToolCall),
    ToolResult(ToolResult),
    TurnComplete,
    Error(String),
    /// The per-round-trip iteration cap (`max_tool_iterations`) was hit
    /// while the model was still actively issuing tool calls — not a
    /// natural no-more-tool-calls stop. Deliberately distinct from both
    /// `TurnComplete` and `Error`: nothing was lost (every dispatched tool
    /// call already has a matching result in history, and the session
    /// persists as usual), so this is a pause the user can resume from by
    /// sending another message, not a failure. See ROADMAP.md Phase 12
    /// Part A — `AgentError::MaxIterationsExceeded` is reserved for a
    /// future, coarser autonomous-session budget (Phase 11c) instead.
    TurnPaused(String),
    /// The backend's reported prompt-token count for the most recent
    /// request, against the configured context window — drives the TUI's
    /// live budget indicator.
    ContextUsage {
        used: u32,
        limit: u32,
    },
    /// The task list changed during this turn (the model called
    /// `set_tasks`) — carries the full new list for the TUI's task panel.
    TasksUpdated(Vec<Task>),
    /// One block of council-mode output (a stage banner, a member's answer
    /// or ranking, the chairman's synthesis, or a failure note) — the whole
    /// deliberation streams through these; see `council::convene`.
    CouncilNote(String),
    /// One block of architect-mode output (the planning-in-progress note,
    /// the produced plan, or a failure note) — mirrors `CouncilNote`
    /// exactly, but for the single-seat architect/editor pairing feature.
    /// See `crate::architect::plan`.
    ArchitectNote(String),
    /// One event produced by a `delegate_task` sub-agent's own turn loop,
    /// forwarded verbatim from its private `AgentEvent` channel so it can
    /// render in the transcript distinguished from the parent's own
    /// activity — see `crate::delegate::DelegateTaskTool`. `Box`ed since
    /// `AgentEvent` itself isn't `Copy` and this variant would otherwise
    /// make every `AgentEvent` at least as large as its own biggest
    /// variant recursively.
    SubAgentActivity(Box<AgentEvent>),
}

/// Caps unbounded growth of a single turn's accumulated assistant text from
```

new_string:
```
use crate::session::{self, SessionState, Task};

mod types;
#[cfg(test)]
mod tests;

pub use types::{AgentConfig, AgentError, AgentEvent, EditFormat};
use types::{AgentsFileConfig, VerificationConfig};

/// Caps unbounded growth of a single turn's accumulated assistant text from
```

**Edit 6b** — remove `EditFormat`:

old_string:
```
Do not claim to have made any changes — you cannot make any in this mode.";

/// How edit content travels to and from the model. See ROADMAP.md Phase 2
/// for the A/B evidence behind the configured default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditFormat {
    /// Edits are edit_file/write_file tool calls with JSON arguments.
    Native,
    /// Edits are SEARCH/REPLACE blocks in the assistant's plain text,
    /// parsed by `edit_blocks` and applied through the same tools (and
    /// therefore the same permission gate) as synthesized calls; the edit
    /// tools themselves are withheld from the model's tool list.
    Prompted,
}

/// The tools hidden from the model (but kept registered — the synthesized
```

new_string:
```
Do not claim to have made any changes — you cannot make any in this mode.";

/// The tools hidden from the model (but kept registered — the synthesized
```

**Edit 6c** — remove `VerificationConfig`, `AgentConfig` + its `Default` impl, `AgentError`, and `AgentsFileConfig` (these four are contiguous in the file, with no constants interspersed between them, so they're removed in one edit):

old_string:
```
Leaving tasks incomplete means you will be prompted to continue working toward the goal.";

/// Configures the enforced verification loop (ROADMAP.md Phase 12 Part B):
/// after file edits, before a turn is allowed to end, the named
/// `allowed_commands` entry is auto-run via `run_command`.
#[derive(Debug, Clone)]
struct VerificationConfig {
    command_name: String,
    /// Clamped to a minimum of 1 by `Agent::set_verification` — a 0 here
    /// would report "still failing" without ever actually attempting a
    /// verification run.
    max_retries: u32,
}

/// Scalar knobs for an `Agent`, grouped so `Agent::new`'s arity stays sane
/// as configuration accumulates (it has grown every phase so far).
#[derive(Debug, Clone, Copy)]
pub struct AgentConfig {
    /// Max LLM round-trips per user turn; clamped to a minimum of 1.
    pub max_tool_iterations: u32,
    /// The model's context window, in tokens; clamped to a minimum of 1.
    pub context_tokens: u32,
    /// How edit content travels (see `EditFormat`).
    pub edit_format: EditFormat,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            max_tool_iterations: 25,
            context_tokens: 8192,
            edit_format: EditFormat::Native,
        }
    }
}

#[derive(Debug, Error)]
pub enum AgentError {
    #[error("llm backend error: {0}")]
    Llm(#[from] LlmError),
    /// Reserved for a future, coarser autonomous-session budget (Phase
    /// 11c) — the per-round-trip `max_tool_iterations` cap no longer
    /// constructs this (see `AgentEvent::TurnPaused`, ROADMAP.md Phase 12
    /// Part A): hitting that cap mid-work is a resumable pause, not a
    /// failure. Kept as a real hard ceiling for whatever unattended-budget
    /// check Phase 11c adds, so an unbounded autonomous loop still has
    /// something able to say no.
    #[error("maximum tool iterations ({0}) exceeded for this turn")]
    MaxIterationsExceeded(u32),
    #[error("response exceeded the maximum allowed size and was aborted")]
    ResponseTooLarge,
}

/// Where to find the user-global `AGENTS.md` (if resolvable) and the
/// per-file token budget both the global and project files share.
struct AgentsFileConfig {
    global_path: Option<PathBuf>,
    budget_tokens: u32,
}

pub struct Agent {
```

new_string:
```
Leaving tasks incomplete means you will be prompted to continue working toward the goal.";

pub struct Agent {
```

After these three edits, `agent/mod.rs` should be 1499 − 46 (Edit 6a's removed AgentEvent+blank, net of the 8 added mod/use lines) − 13 (Edit 6b) − 58 (Edit 6c) lines — don't hand-verify this arithmetic; Step 8's `wc -l` is the real check.

- [ ] **Step 7: Create agent/types.rs**

Use the Write tool to create `crates/aivyx-core/src/agent/types.rs` with exactly this content:

```rust
use std::path::PathBuf;

use aivyx_llm::LlmError;
use aivyx_types::{ToolCall, ToolResult};
use thiserror::Error;

use crate::session::Task;

#[derive(Debug, Clone)]
pub enum AgentEvent {
    TextDelta(String),
    ToolCallDetected(ToolCall),
    ToolResult(ToolResult),
    TurnComplete,
    Error(String),
    /// The per-round-trip iteration cap (`max_tool_iterations`) was hit
    /// while the model was still actively issuing tool calls — not a
    /// natural no-more-tool-calls stop. Deliberately distinct from both
    /// `TurnComplete` and `Error`: nothing was lost (every dispatched tool
    /// call already has a matching result in history, and the session
    /// persists as usual), so this is a pause the user can resume from by
    /// sending another message, not a failure. See ROADMAP.md Phase 12
    /// Part A — `AgentError::MaxIterationsExceeded` is reserved for a
    /// future, coarser autonomous-session budget (Phase 11c) instead.
    TurnPaused(String),
    /// The backend's reported prompt-token count for the most recent
    /// request, against the configured context window — drives the TUI's
    /// live budget indicator.
    ContextUsage {
        used: u32,
        limit: u32,
    },
    /// The task list changed during this turn (the model called
    /// `set_tasks`) — carries the full new list for the TUI's task panel.
    TasksUpdated(Vec<Task>),
    /// One block of council-mode output (a stage banner, a member's answer
    /// or ranking, the chairman's synthesis, or a failure note) — the whole
    /// deliberation streams through these; see `council::convene`.
    CouncilNote(String),
    /// One block of architect-mode output (the planning-in-progress note,
    /// the produced plan, or a failure note) — mirrors `CouncilNote`
    /// exactly, but for the single-seat architect/editor pairing feature.
    /// See `crate::architect::plan`.
    ArchitectNote(String),
    /// One event produced by a `delegate_task` sub-agent's own turn loop,
    /// forwarded verbatim from its private `AgentEvent` channel so it can
    /// render in the transcript distinguished from the parent's own
    /// activity — see `crate::delegate::DelegateTaskTool`. `Box`ed since
    /// `AgentEvent` itself isn't `Copy` and this variant would otherwise
    /// make every `AgentEvent` at least as large as its own biggest
    /// variant recursively.
    SubAgentActivity(Box<AgentEvent>),
}

/// How edit content travels to and from the model. See ROADMAP.md Phase 2
/// for the A/B evidence behind the configured default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditFormat {
    /// Edits are edit_file/write_file tool calls with JSON arguments.
    Native,
    /// Edits are SEARCH/REPLACE blocks in the assistant's plain text,
    /// parsed by `edit_blocks` and applied through the same tools (and
    /// therefore the same permission gate) as synthesized calls; the edit
    /// tools themselves are withheld from the model's tool list.
    Prompted,
}

/// Configures the enforced verification loop (ROADMAP.md Phase 12 Part B):
/// after file edits, before a turn is allowed to end, the named
/// `allowed_commands` entry is auto-run via `run_command`.
#[derive(Debug, Clone)]
pub(crate) struct VerificationConfig {
    pub(crate) command_name: String,
    /// Clamped to a minimum of 1 by `Agent::set_verification` — a 0 here
    /// would report "still failing" without ever actually attempting a
    /// verification run.
    pub(crate) max_retries: u32,
}

/// Scalar knobs for an `Agent`, grouped so `Agent::new`'s arity stays sane
/// as configuration accumulates (it has grown every phase so far).
#[derive(Debug, Clone, Copy)]
pub struct AgentConfig {
    /// Max LLM round-trips per user turn; clamped to a minimum of 1.
    pub max_tool_iterations: u32,
    /// The model's context window, in tokens; clamped to a minimum of 1.
    pub context_tokens: u32,
    /// How edit content travels (see `EditFormat`).
    pub edit_format: EditFormat,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            max_tool_iterations: 25,
            context_tokens: 8192,
            edit_format: EditFormat::Native,
        }
    }
}

#[derive(Debug, Error)]
pub enum AgentError {
    #[error("llm backend error: {0}")]
    Llm(#[from] LlmError),
    /// Reserved for a future, coarser autonomous-session budget (Phase
    /// 11c) — the per-round-trip `max_tool_iterations` cap no longer
    /// constructs this (see `AgentEvent::TurnPaused`, ROADMAP.md Phase 12
    /// Part A): hitting that cap mid-work is a resumable pause, not a
    /// failure. Kept as a real hard ceiling for whatever unattended-budget
    /// check Phase 11c adds, so an unbounded autonomous loop still has
    /// something able to say no.
    #[error("maximum tool iterations ({0}) exceeded for this turn")]
    MaxIterationsExceeded(u32),
    #[error("response exceeded the maximum allowed size and was aborted")]
    ResponseTooLarge,
}

/// Where to find the user-global `AGENTS.md` (if resolvable) and the
/// per-file token budget both the global and project files share.
pub(crate) struct AgentsFileConfig {
    pub(crate) global_path: Option<PathBuf>,
    pub(crate) budget_tokens: u32,
}
```

**Why `pub(crate)` on `VerificationConfig`/`AgentsFileConfig` and their fields, when they were plain-private `struct` in the original file:** in the original single-file layout, `impl Agent` and these structs were in the *same* module, so private (module-local) visibility was sufficient. Now that they live in the `agent::types` child module while `impl Agent` (which constructs them via struct-literal syntax and reads their fields directly — see `agent/mod.rs`'s `set_verification`, `set_agents_file`, and the verification-retry logic) lives in the *parent* `agent` module, Rust's privacy rules require at least `pub(crate)` (or `pub(super)`) for the parent to see into the child. `pub(crate)` was chosen over `pub(super)` for consistency with the spec's actual invariant ("never referenced outside `aivyx-core`" — `pub(crate)` enforces exactly that, crate-wide, rather than the narrower and more fragile "only my direct parent module" that `pub(super)` would encode). `AgentEvent`, `EditFormat`, `AgentConfig`, and `AgentError` need no such change — they were already fully `pub` in the original file (required for `lib.rs`'s outer re-export to work), and a `pub` item in a child module remains fully visible through `pub use types::{...}` in the parent exactly as before.

- [ ] **Step 8: Delete the original agent.rs**

```bash
rm crates/aivyx-core/src/agent.rs
wc -l crates/aivyx-core/src/agent/mod.rs crates/aivyx-core/src/agent/types.rs crates/aivyx-core/src/agent/tests.rs
```

Report the three line counts — no specific expected total is given here (Step 6's arithmetic note said not to hand-verify), but `mod.rs` should be roughly 1380-1400 lines, `types.rs` roughly 130-140 lines, `tests.rs` exactly 2549 lines (from Step 3).

- [ ] **Step 9: Build**

```bash
cargo build -p aivyx-core 2>&1 | tail -60
```

This is expected to surface any missed import or visibility issue precisely (Rust's compiler errors name the exact missing item and location). Common expected-to-NOT-happen errors and what they'd mean if they did occur:
- `` cannot find type `X` in this scope `` in `mod.rs` → an import was dropped that shouldn't have been (re-check Step 5).
- `` struct `X` is private `` → the `pub(crate)` fix in Step 7 was missed or incomplete for that type/field.
- `` unused import `X` `` (warning) → an import that should have been removed in Step 5/6 wasn't, or belongs only to `types.rs` now.

Fix any such issue by re-examining the specific line the compiler names — do not guess broadly. Re-run until `cargo build -p aivyx-core` succeeds with no errors and no warnings.

- [ ] **Step 10: Full workspace build and clippy**

```bash
cargo build --workspace 2>&1 | tail -30
cargo clippy --workspace --all-targets 2>&1 | tail -30
```

Expected: both succeed cleanly, confirming the split didn't break any other crate's use of `aivyx_core::agent::*` (there shouldn't be any beyond what `lib.rs` re-exports, per the spec, but this is the real check).

- [ ] **Step 11: Compare the test suite before and after**

```bash
cargo test -p aivyx-core 2>&1 | grep -E "^test |^test result:" > /tmp/aivyx-core-tests-after.txt
diff /tmp/aivyx-core-tests-before.txt /tmp/aivyx-core-tests-after.txt
```

Expected: **no output** (identical test names, identical pass/fail counts). Any diff means the split changed behavior — this must not happen for a pure code-motion task; investigate and fix before proceeding.

- [ ] **Step 12: Full workspace test run**

```bash
cargo test --workspace 2>&1 | grep -E "^test result:|FAILED|error\["
```

Expected: every `test result:` line reads `ok`, matching Task 1's own final verification (same pass counts workspace-wide, since Task 2 touches only `aivyx-core`).

- [ ] **Step 13: Commit**

```bash
git add crates/aivyx-core/src/agent.rs crates/aivyx-core/src/agent/
git commit -m "Refactor: split agent.rs into agent/{mod,types,tests}.rs

Pure code motion, no behavior changes. Follows this project's existing
directory-module convention (lsp/mod.rs, mcp/mod.rs). agent.rs was
4052 lines: ~230 of type/error definitions, ~1185 of impl Agent
orchestration logic (kept together — one cohesive state machine), and
~2551 of its own test suite."
```

(Note: `git add crates/aivyx-core/src/agent.rs` stages the deletion — `git add` correctly handles a path that no longer exists on disk when it was previously tracked, staging it as a removal.)

---

### Final check (not a separate task — run after both tasks are committed)

```bash
git diff --stat main
```

Expected file list: the 5 `Cargo.toml` files from Task 1, `Cargo.lock`, `crates/aivyx-core/src/agent.rs` (shown as deleted), `crates/aivyx-core/src/agent/mod.rs`, `crates/aivyx-core/src/agent/types.rs`, `crates/aivyx-core/src/agent/tests.rs`. Nothing else — no `docs/**`, no other crate's `.rs` files.
