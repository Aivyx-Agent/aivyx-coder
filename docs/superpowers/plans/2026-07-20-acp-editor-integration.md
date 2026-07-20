# ACP Editor Integration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give aivyx-coder a second frontend — an Agent Client Protocol
(ACP) server mode (`aivyx --acp`) — so Zed and, via the existing
community `formulahendry.acp-client` VS Code extension, VS Code can drive
the same `Agent` core the TUI already drives, with no editor-specific
code of our own beyond registration/docs.

**Architecture:** A new crate `crates/aivyx-acp`, structurally a sibling
to `crates/aivyx-tui` (same role: a thin frontend over `aivyx-core::Agent`,
nothing more), built on the official `agent-client-protocol` Rust crate.
`crates/aivyx/src/main.rs`'s large, TUI-agnostic construction sequence
(config → tools → gate → `Agent`) is extracted into a shared function so
both frontends call the identical setup, differing only in which
`PermissionPrompter` and event consumer they plug in.

**Tech Stack:** Rust, `agent-client-protocol` (official ACP SDK, JSON-RPC
2.0 over stdio), `tokio`, `async-trait` — all already used elsewhere in
this workspace.

## Global Constraints

- Design authority: `docs/superpowers/specs/2026-07-20-acp-editor-integration-design.md`
  (approved 2026-07-20). Every task below traces back to a line in its
  Protocol Mapping table or Architecture section.
- **Refinement discovered during planning, not in the spec**: diff
  content (`old_text`/`new_text`) can only reach ACP through the
  permission-request path (`PermissionRequest.diff`, populated by
  `write_file`/`edit_file`/`delete_file`), never through the generic
  `AgentEvent::ToolCallDetected`/`ToolResult` stream — `aivyx_types::ToolCall`
  carries no diff field (`crates/aivyx-types/src/lib.rs:70-75`), and
  `ToolResult.output` is a plain string (`ToolOutput::{Ok,Error,Denied}(String)`,
  same file:86-90). So `AgentEvent::ToolCallDetected`/`ToolResult` map to
  a plain-text `ToolCall`/`ToolCallUpdate` (Task 3); the diff-carrying
  `ToolCallUpdate` sent as part of `session/request_permission` is built
  separately, straight from `PermissionRequest`, inside `AcpPrompter`
  (Task 4) — mirroring `crates/aivyx-sandbox/src/editor_approval.rs`'s
  `build_pending_request` almost exactly, just targeting ACP's `Diff`
  type instead of the bespoke `ApprovalContent` enum.
- **Gap discovered during planning, not in the spec**: `session/cancel`
  is not handled in v1 — once a `session/prompt` is in flight, the client
  cannot cancel it. Flag this to the user as a follow-up scope item; not
  blocking for this plan.
- **v1 only forwards `ContentBlock::Text` blocks** from `PromptRequest.prompt`.
  Any other content block (image, audio, resource, resource link) in the
  same message is dropped, with a trailing note appended to the forwarded
  text (`"(non-text content in this message was not forwarded)"`) so the
  omission is visible rather than silent. Full resource/image handling is
  out of scope for this plan.
- Exact crate version: resolve via `cargo add agent-client-protocol` at
  implementation time rather than hand-pinning a version number here —
  the crate is under active development (Zed itself tracked `0.9.3` as of
  the research done for the design spec, likely newer by the time this
  plan executes). Do **not** enable its `unstable` cargo feature — every
  type this plan uses (`SessionUpdate::{AgentMessageChunk,AgentThoughtChunk,
  ToolCall,ToolCallUpdate,Plan,CurrentModeUpdate}`, `RequestPermissionRequest`,
  `SetSessionModeRequest`) is stable, confirmed by reading
  `agent-client-protocol-schema`'s `src/v1/*.rs` directly (no
  `#[cfg(feature = "unstable_...")]` gate on any of them).
- New crate `aivyx-acp` depends only on `aivyx-core`, `aivyx-sandbox`,
  `aivyx-types`, `agent-client-protocol`, `tokio`, `async-trait`, `anyhow`
  — the same footprint as `aivyx-tui` (`crates/aivyx-tui/Cargo.toml`),
  deliberately excluding `aivyx-tools`/`aivyx-config`, which stay
  `crates/aivyx`-only (where the shared builder lives).
- Run `cargo test --workspace` and `cargo clippy --workspace --all-targets`
  after every task before committing (per this repo's `CLAUDE.md`).

---

### Task 1: Extract shared agent construction out of `main.rs`

**Files:**
- Create: `crates/aivyx/src/agent_builder.rs`
- Modify: `crates/aivyx/src/main.rs:123-611` (the `main` function)

**Interfaces:**
- Produces: `pub(crate) struct BuiltAgent { pub(crate) agent: Agent, pub(crate) events_rx: mpsc::UnboundedReceiver<AgentEvent>, pub(crate) cwd: PathBuf, pub(crate) plan_mode: PlanMode, pub(crate) restored: Option<session::SessionState>, pub(crate) tasks: Arc<std::sync::Mutex<Vec<session::Task>>> }`
  and `pub(crate) async fn build_agent(cli: &Cli, settings: &Settings, prompter: Arc<dyn PermissionPrompter>) -> anyhow::Result<BuiltAgent>`
  in `crates/aivyx/src/agent_builder.rs` — consumed by both the existing
  TUI path (this task) and the new `--acp` path (Task 6).

This is a pure relocation: no new logic, no behavior change. `main.rs`
currently builds *everything* the TUI needs — settings, deny_paths, the
command allowlist, `PlanMode`/`AutonomousMode`, `ConfirmationGate`, the
sandbox confiner, the git checkpointer, the tool registry (including MCP
discovery), `Agent::new`, and every `agent.set_*` call (repo map,
AGENTS.md, editor context, council, architect, verification, session
restore) — entirely before it ever touches anything TUI-specific. The
only TUI-specific pieces are: constructing `TuiPrompter`/`permission_rx`
(`aivyx_tui::permission_channel()`, `main.rs:234`) and the final call to
`aivyx_tui::run(...)` (`main.rs:601-610`). Everything in between moves
verbatim.

- [ ] **Step 1: Confirm the exact TUI behavior to preserve — capture current test/behavior baseline**

Run: `cargo test --workspace 2>&1 | tail -20`
Expected: all tests pass (this is the baseline the refactor must not change).

- [ ] **Step 2: Create `crates/aivyx/src/agent_builder.rs` with the extracted function**

Move `crates/aivyx/src/main.rs` lines 127–593 (from `let mut settings = Settings::load()?;` — actually `settings` is now a caller-owned parameter, so start the moved block at `settings.apply_overrides(cli.base_url, cli.model);`, i.e. drop the two lines `let mut settings = Settings::load()?;` and the initial `let cli = Cli::parse();` since those become the caller's job — through the end of the `let restored = match session::session_file_path(&cwd) { ... };` block) into this new file, wrapped as:

```rust
//! Shared, frontend-agnostic construction of an `Agent` from `Settings` +
//! `Cli` — the TUI (`main.rs`) and the ACP server (`aivyx-acp`) both call
//! this, differing only in which `PermissionPrompter` and event consumer
//! they plug in downstream. See `docs/superpowers/specs/
//! 2026-07-20-acp-editor-integration-design.md`.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use aivyx_config::Settings;
// Identical to main.rs's own top-of-file import list (main.rs:5-15) —
// copy it verbatim rather than retyping, so nothing is silently dropped.
// `DelegateTaskTool`/`DelegateTaskConfig` are deliberately absent here,
// matching main.rs: the pasted body (Step 2) references them
// fully-qualified as `aivyx_core::DelegateTaskTool::new(aivyx_core::DelegateTaskConfig { .. })`
// (main.rs:439-455), so no import is needed and no edit to that call
// site is needed either.
use aivyx_core::{Agent, AgentConfig, Architect, ArchitectSeat, Council, CouncilSeat, EditFormat, session};
use aivyx_llm::{LlmBackend, OpenAiCompatBackend};
use aivyx_sandbox::{AutonomousMode, ConfirmationGate, PermissionGate, PermissionPrompter, PlanMode};
use aivyx_tools::{
    CommandSpec, DeleteFileTool, EditFileTool, FindReferencesTool, GetMcpPromptTool,
    GitBranchTool, GitCheckpointer, GitCommitTool, GitPrTool, GitPushTool, GitReadTool, GlobTool,
    GoToDefinitionTool, GrepTool, ListMcpPromptsTool, ListMcpResourcesTool, LspClient, McpClient,
    McpToolAdapter, ReadFileTool, ReadMcpResourceTool, RunCommandTool, RunShellTool, SetTasksTool,
    ToolExecutor, ToolRegistry, WebFetchTool, WebSearchTool, WriteFileTool,
};
use tokio::sync::mpsc;

use crate::{
    Cli, COUNCIL_IDLE_TIMEOUT, DEFAULT_COMMAND_TIMEOUT_SECS, base_url_looks_local,
    build_system_prompt,
};

/// Everything a frontend needs to start driving a fully-configured
/// `Agent` — the exact set of values `main.rs`'s TUI path used to build
/// inline before handing off to `aivyx_tui::run`.
pub(crate) struct BuiltAgent {
    pub(crate) agent: Agent,
    pub(crate) events_rx: mpsc::UnboundedReceiver<aivyx_core::AgentEvent>,
    pub(crate) cwd: PathBuf,
    pub(crate) plan_mode: PlanMode,
    pub(crate) restored: Option<session::SessionState>,
    pub(crate) tasks: Arc<std::sync::Mutex<Vec<session::Task>>>,
}

/// Builds `Agent` + every collaborator it needs, identically regardless
/// of which frontend is asking — only `prompter` differs between the TUI
/// (`TuiPrompter`) and ACP (`AcpPrompter`) call sites.
pub(crate) async fn build_agent(
    cli: &Cli,
    settings: &Settings,
    prompter: Arc<dyn PermissionPrompter>,
) -> anyhow::Result<BuiltAgent> {
    todo!("relocate main.rs:130-593 here — see the three edits below")
}
```

Do this relocation as a literal cut-and-paste, not a rewrite: open
`crates/aivyx/src/main.rs` at line 130 (the `tracing::info!(...)` call
right after `settings.apply_overrides(...)`) and select down through
line 593 (the closing `};` of the `let restored = match session::session_file_path(&cwd) { ... };`
block) — this is the entire TUI-agnostic body described in this task's
opening paragraph (settings → deny_paths → command allowlist →
`PlanMode`/`AutonomousMode` → `ConfirmationGate` → sandbox confiner →
git checkpointer → tool registry + MCP discovery → `Agent::new` → every
`agent.set_*` call → session restore). Cut that whole range out of
`main.rs` and paste it in place of the `todo!(...)` line above, then
make exactly these three mechanical edits to the pasted block (nothing
else should need to change):

1. Delete the two lines that constructed `settings` (`let mut settings = Settings::load()?;`
   and `settings.apply_overrides(cli.base_url, cli.model);`) — `settings`
   is now a parameter; every subsequent `settings.foo` reference in the
   pasted block keeps working unchanged since it's a field access, not a
   move, and `&Settings` supports all of them (confirmed during planning:
   the one field that looked risky, `settings.backend.edit_format`, is
   `#[derive(Copy)]` — `crates/aivyx-config/src/settings.rs:467`).
2. Delete `let (prompter, permission_rx) = aivyx_tui::permission_channel();`
   (originally `main.rs:234`) entirely — `prompter` now arrives as this
   function's own parameter, already `Arc<dyn PermissionPrompter>`, so
   the `ConfirmationGate::new(Arc::new(prompter), ...)` call right below
   it becomes `ConfirmationGate::new(Arc::clone(&prompter), ...)`.
3. Replace the pasted block's final value (the `restored` match) with:

   ```rust
   Ok(BuiltAgent { agent, events_rx, cwd, plan_mode, restored, tasks })
   ```

- [ ] **Step 3: Update `crates/aivyx/src/main.rs` to call the new function**

Replace `main.rs:127-593` with:

```rust
    let mut settings = Settings::load()?;
    settings.apply_overrides(cli.base_url.clone(), cli.model.clone());

    let (tui_prompter, permission_rx) = aivyx_tui::permission_channel();
    let built = crate::agent_builder::build_agent(&cli, &settings, Arc::new(tui_prompter)).await?;
```

(`cli.base_url`/`cli.model` are cloned here since `cli` itself is still
needed below for `cli.auto`, `cli.plan`, `cli.resume`, `cli.edit_format`
inside `build_agent`, which now takes `&cli`.)

Then update the tail (former `main.rs:595-610`) to read from `built`:

```rust
    let autonomous_run = cli.auto.map(|goal| aivyx_tui::AutonomousRun {
        goal,
        max_iterations: settings.autonomous.max_iterations,
        max_duration: Duration::from_secs(settings.autonomous.max_duration_secs),
        tasks: Arc::clone(&built.tasks),
    });
    aivyx_tui::run(
        built.agent,
        built.events_rx,
        built.cwd,
        permission_rx,
        built.restored,
        built.plan_mode,
        autonomous_run,
    )
    .await
```

Add `mod agent_builder;` near the top of `main.rs`, and make `Cli`,
`COUNCIL_IDLE_TIMEOUT`, `DEFAULT_COMMAND_TIMEOUT_SECS`, and
`base_url_looks_local` visible to the new module (`pub(crate)` on the
`Cli` struct fields already used via `cli.foo` is not required — only
the items themselves need `pub(crate)` if they weren't already
crate-visible; a plain `struct Cli` at the crate root is already visible
to a child module via `crate::Cli`, so no visibility change is needed
there — only confirm `COUNCIL_IDLE_TIMEOUT`/`DEFAULT_COMMAND_TIMEOUT_SECS`
constants aren't declared `mod`-private in a way that blocks this; if
`cargo build` reports a privacy error on either, add `pub(crate)` to
that item's declaration).

- [ ] **Step 4: Build and run the full test suite to confirm zero behavior change**

Run: `cargo build --workspace && cargo test --workspace`
Expected: builds cleanly; identical pass count to Step 1's baseline (this
is a refactor — no new tests are added in this task, since there is no
new behavior to test, only relocated behavior the existing suite already
covers).

- [ ] **Step 5: Manual smoke check**

Run: `cargo run -p aivyx` against a local Ollama/llama-server instance,
send one message, confirm the TUI behaves exactly as before (streaming
text, tool calls, permission modal). This is the one part Step 4 can't
verify mechanically — the extraction touches the construction of
`ConfirmationGate`/`Agent`, both security- and correctness-critical.

- [ ] **Step 6: Commit**

```bash
git add crates/aivyx/src/agent_builder.rs crates/aivyx/src/main.rs
git commit -m "$(cat <<'EOF'
Extract shared agent construction out of main.rs into agent_builder.rs

Pure relocation, no behavior change (verified: full test suite passes
unchanged, manual TUI smoke check). Needed so the upcoming ACP frontend
can reuse the exact same construction path as the TUI instead of
duplicating ~300 lines of security-critical wiring.

Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>
EOF
)"
```

---

### Task 2: `aivyx-acp` crate skeleton that speaks `initialize`

**Files:**
- Create: `crates/aivyx-acp/Cargo.toml`
- Create: `crates/aivyx-acp/src/lib.rs`
- Create: `crates/aivyx-acp/tests/initialize.rs`
- Modify: `Cargo.toml:9-16` (workspace members/dependencies)

**Interfaces:**
- Produces: `pub async fn run() -> agent_client_protocol::Result<()>` in
  `aivyx-acp` — a minimal ACP agent that only answers `initialize`. Later
  tasks replace its body; this task exists so the crate boots and speaks
  real JSON-RPC before any aivyx-specific logic is added, matching this
  project's stated preference for small, independently-testable steps.

- [ ] **Step 1: Add the crate to the workspace**

In `Cargo.toml`, add to `[workspace.dependencies]`:

```toml
aivyx-acp = { path = "crates/aivyx-acp" }
```

- [ ] **Step 2: Write `crates/aivyx-acp/Cargo.toml`**

```toml
[package]
name = "aivyx-acp"
version.workspace = true
edition.workspace = true
license.workspace = true

[dependencies]
agent-client-protocol = "0"
aivyx-core = { version = "0.1.0", path = "../aivyx-core" }
aivyx-sandbox = { version = "0.1.0", path = "../aivyx-sandbox" }
aivyx-types = { version = "0.1.0", path = "../aivyx-types" }
anyhow = "1.0.103"
async-trait = "0.1.89"
tokio = { version = "1.52.3", features = ["rt-multi-thread", "macros", "sync"] }

[dev-dependencies]
tokio = { version = "1.52.3", features = ["macros", "rt", "process"] }
```

Run `cargo add agent-client-protocol -p aivyx-acp` instead of hand-typing
the version if `cargo add` is available in the implementation
environment — this resolves and pins the actual current version rather
than the placeholder `"0"` above (per the Global Constraints note on not
hand-pinning a guessed version).

- [ ] **Step 3: Write the failing test — a real subprocess round-trip on `initialize`**

`crates/aivyx-acp/tests/initialize.rs`:

```rust
use agent_client_protocol::schema::v1::InitializeRequest;
use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::{AcpAgent, Agent, Client, ConnectTo};
use std::str::FromStr;

#[tokio::test]
async fn initialize_round_trips_over_the_real_binary() {
    let bin = env!("CARGO_BIN_EXE_aivyx_acp_test_stub");
    let agent = AcpAgent::from_str(bin).expect("valid command");

    let response = Client::builder()
        .name("aivyx-acp-test-client")
        .connect_with(agent, async |connection| {
            connection
                .send_request(InitializeRequest::new(ProtocolVersion::V1))
                .block_task()
                .await
        })
        .await
        .expect("initialize should succeed");

    assert_eq!(response.protocol_version, ProtocolVersion::V1);
}
```

This test drives a compiled `[[bin]]` target added in Step 4
(`aivyx_acp_test_stub`) rather than the real `aivyx` binary — the real
`--acp` wiring is Task 6's job, once `build_agent` (Task 1) and the
session logic (Task 5) both exist. Testing the bare crate skeleton
against its own tiny stub binary keeps this task's test cycle
independent of every later task.

- [ ] **Step 4: Add the stub binary and the minimal `run()` implementation**

`crates/aivyx-acp/src/lib.rs`:

```rust
//! ACP server frontend for aivyx-coder's `Agent` core. See
//! `docs/superpowers/specs/2026-07-20-acp-editor-integration-design.md`.

use agent_client_protocol::schema::v1::{AgentCapabilities, InitializeRequest, InitializeResponse};
use agent_client_protocol::{Agent, Client, ConnectTo, Dispatch, Result, Stdio};

/// Runs the ACP server loop over stdin/stdout until the connection
/// closes. Only `initialize` is handled so far — `NewSessionRequest`/
/// `PromptRequest`/`SetSessionModeRequest` are added in Task 5.
pub async fn run() -> Result<()> {
    Agent::builder()
        .name("aivyx")
        .on_receive_request(
            async move |initialize: InitializeRequest, responder, _connection| {
                responder.respond(
                    InitializeResponse::new(initialize.protocol_version)
                        .agent_capabilities(AgentCapabilities::new()),
                )
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_dispatch(
            async move |message: Dispatch, cx| {
                message.respond_with_error(
                    agent_client_protocol::util::internal_error("not yet implemented"),
                    cx,
                )
            },
            agent_client_protocol::on_receive_dispatch!(),
        )
        .connect_to(Stdio::new())
        .await
}
```

`crates/aivyx-acp/Cargo.toml`, add:

```toml
[[bin]]
name = "aivyx_acp_test_stub"
path = "tests/bin/stub.rs"
test = false
```

`crates/aivyx-acp/tests/bin/stub.rs`:

```rust
#[tokio::main]
async fn main() -> agent_client_protocol::Result<()> {
    aivyx_acp::run().await
}
```

- [ ] **Step 5: Run the test to verify it passes**

Run: `cargo test -p aivyx-acp --test initialize -- --nocapture`
Expected: PASS — `response.protocol_version == ProtocolVersion::V1`.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml crates/aivyx-acp
git commit -m "$(cat <<'EOF'
Add aivyx-acp crate skeleton speaking ACP initialize over stdio

First slice of the ACP editor-integration adapter (see
docs/superpowers/specs/2026-07-20-acp-editor-integration-design.md):
just enough to prove the crate boots and round-trips real JSON-RPC
before any aivyx-specific session/prompt logic is added.

Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>
EOF
)"
```

---

### Task 3: Pure `AgentEvent` → ACP `SessionUpdate` translation

**Files:**
- Create: `crates/aivyx-acp/src/translate.rs`
- Modify: `crates/aivyx-acp/src/lib.rs` (add `mod translate;`)

**Interfaces:**
- Consumes: `aivyx_core::AgentEvent` (twelve variants, `crates/aivyx-core/src/agent/types.rs:10-58`), `aivyx_types::{ToolCall, ToolResult, ToolOutput}` (`crates/aivyx-types/src/lib.rs:49-90`).
- Produces: `pub(crate) fn translate_event(session_id: &SessionId, event: &AgentEvent) -> Option<SessionUpdate>`
  and `pub(crate) fn terminal_stop_reason(event: &AgentEvent) -> Option<StopReason>`,
  both pure functions with no I/O — consumed by Task 5's `PromptRequest` handler.

Per the Global Constraints note, this task does **not** attempt to carry
diff content — `AgentEvent::ToolCallDetected`/`ToolResult` only ever
produce plain-text tool call updates.

- [ ] **Step 1: Write the failing tests**

`crates/aivyx-acp/src/translate.rs`:

```rust
//! Pure `AgentEvent` → ACP `SessionUpdate`/`StopReason` mapping — no I/O,
//! so these are unit-tested directly with no connection or subprocess.
//! See the Protocol Mapping table in `docs/superpowers/specs/
//! 2026-07-20-acp-editor-integration-design.md`.

use agent_client_protocol::schema::v1::{
    ContentBlock, ContentChunk, Plan, PlanEntry, PlanEntryPriority, PlanEntryStatus, SessionId,
    SessionUpdate, StopReason, TextContent, ToolCall as AcpToolCall, ToolCallContent,
    ToolCallStatus, ToolCallUpdate, ToolCallUpdateFields, ToolKind,
};
use aivyx_core::AgentEvent;
use aivyx_types::{ToolOutput, TaskStatus};

/// Best-effort classification of a tool name into ACP's `ToolKind`, for
/// client icon/UI hints only — never affects behavior. Unknown/unlisted
/// names (e.g. MCP tool names, which are server-defined and unpredictable)
/// fall through to `ToolKind::Other`.
fn tool_kind(name: &str) -> ToolKind {
    match name {
        "read_file" | "grep" | "glob" | "git_read" => ToolKind::Read,
        "write_file" | "edit_file" => ToolKind::Edit,
        "delete_file" => ToolKind::Delete,
        "run_command" | "run_shell" | "git_commit" | "git_branch" | "git_push" | "git_pr" => {
            ToolKind::Execute
        }
        "web_fetch" => ToolKind::Fetch,
        "web_search" => ToolKind::Search,
        "set_tasks" => ToolKind::Think,
        _ => ToolKind::Other,
    }
}

fn text_chunk(text: String) -> ContentChunk {
    ContentChunk {
        content: ContentBlock::Text(TextContent {
            annotations: None,
            text,
            meta: None,
        }),
        message_id: None,
        meta: None,
    }
}

pub(crate) fn translate_event(session_id: &SessionId, event: &AgentEvent) -> Option<SessionUpdate> {
    let update = match event {
        AgentEvent::TextDelta(text) => SessionUpdate::AgentMessageChunk(text_chunk(text.clone())),
        AgentEvent::ReasoningDelta(text) => {
            SessionUpdate::AgentThoughtChunk(text_chunk(text.clone()))
        }
        AgentEvent::CouncilNote(text) | AgentEvent::ArchitectNote(text) => {
            SessionUpdate::AgentMessageChunk(text_chunk(text.clone()))
        }
        AgentEvent::ToolCallDetected(call) => SessionUpdate::ToolCall(AcpToolCall {
            tool_call_id: call.id.0.clone().into(),
            title: call.name.clone(),
            kind: tool_kind(&call.name),
            status: ToolCallStatus::InProgress,
            content: Vec::new(),
            locations: Vec::new(),
            raw_input: Some(call.arguments.clone()),
            raw_output: None,
            meta: None,
        }),
        AgentEvent::ToolResult(result) => {
            let (status, text) = match &result.output {
                ToolOutput::Ok(text) => (ToolCallStatus::Completed, text.clone()),
                ToolOutput::Error(text) | ToolOutput::Denied(text) => {
                    (ToolCallStatus::Failed, text.clone())
                }
            };
            SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(
                result.call_id.0.clone(),
                ToolCallUpdateFields {
                    kind: None,
                    status: Some(status),
                    title: None,
                    content: Some(vec![ToolCallContent::Content(text.into())]),
                    locations: None,
                    raw_input: None,
                    raw_output: Some(serde_json::Value::String(text)),
                },
            ))
        }
        AgentEvent::TasksUpdated(tasks) => SessionUpdate::Plan(Plan {
            entries: tasks
                .iter()
                .map(|task| {
                    let status = match task.status {
                        TaskStatus::Pending => PlanEntryStatus::Pending,
                        TaskStatus::InProgress => PlanEntryStatus::InProgress,
                        TaskStatus::Done => PlanEntryStatus::Completed,
                    };
                    PlanEntry::new(task.text.clone(), PlanEntryPriority::Medium, status)
                })
                .collect(),
            meta: None,
        }),
        AgentEvent::SubAgentActivity(inner) => return translate_event(session_id, inner),
        // Turn-terminal and non-notification events — handled by
        // `terminal_stop_reason` instead, not surfaced as a SessionUpdate.
        AgentEvent::TurnComplete
        | AgentEvent::TurnPaused(_)
        | AgentEvent::Error(_)
        | AgentEvent::ContextUsage { .. } => return None,
    };
    let _ = session_id; // session_id threading happens at the SessionNotification wrapper in Task 5
    Some(update)
}

pub(crate) fn terminal_stop_reason(event: &AgentEvent) -> Option<StopReason> {
    match event {
        AgentEvent::TurnComplete => Some(StopReason::EndTurn),
        AgentEvent::TurnPaused(_) => Some(StopReason::MaxTurnRequests),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aivyx_types::{Task, ToolCall, ToolCallId, ToolCallSource, ToolResult};

    fn sid() -> SessionId {
        SessionId::new("sess-1")
    }

    #[test]
    fn text_delta_becomes_agent_message_chunk() {
        let update = translate_event(&sid(), &AgentEvent::TextDelta("hi".to_string())).unwrap();
        match update {
            SessionUpdate::AgentMessageChunk(chunk) => {
                assert_eq!(chunk.content, ContentBlock::Text(TextContent { annotations: None, text: "hi".to_string(), meta: None }));
            }
            other => panic!("expected AgentMessageChunk, got {other:?}"),
        }
    }

    #[test]
    fn reasoning_delta_becomes_agent_thought_chunk() {
        let update =
            translate_event(&sid(), &AgentEvent::ReasoningDelta("thinking".to_string())).unwrap();
        assert!(matches!(update, SessionUpdate::AgentThoughtChunk(_)));
    }

    #[test]
    fn council_and_architect_notes_become_plain_message_text() {
        let council = translate_event(&sid(), &AgentEvent::CouncilNote("council said x".to_string())).unwrap();
        let architect = translate_event(&sid(), &AgentEvent::ArchitectNote("plan is y".to_string())).unwrap();
        assert!(matches!(council, SessionUpdate::AgentMessageChunk(_)));
        assert!(matches!(architect, SessionUpdate::AgentMessageChunk(_)));
    }

    #[test]
    fn tool_call_detected_carries_no_diff_only_metadata() {
        let call = ToolCall {
            id: ToolCallId("call-1".to_string()),
            name: "write_file".to_string(),
            arguments: serde_json::json!({"path": "a.rs"}),
            source: ToolCallSource::Native,
        };
        let update = translate_event(&sid(), &AgentEvent::ToolCallDetected(call)).unwrap();
        let SessionUpdate::ToolCall(tool_call) = update else {
            panic!("expected ToolCall");
        };
        assert_eq!(tool_call.title, "write_file");
        assert_eq!(tool_call.kind, ToolKind::Edit);
        assert!(tool_call.content.is_empty());
    }

    #[test]
    fn tool_result_ok_maps_to_completed_status() {
        let result = ToolResult {
            call_id: ToolCallId("call-1".to_string()),
            output: ToolOutput::Ok("wrote 3 lines".to_string()),
        };
        let update = translate_event(&sid(), &AgentEvent::ToolResult(result)).unwrap();
        let SessionUpdate::ToolCallUpdate(update) = update else {
            panic!("expected ToolCallUpdate");
        };
        assert_eq!(update.fields.status, Some(ToolCallStatus::Completed));
    }

    #[test]
    fn tool_result_denied_maps_to_failed_status() {
        let result = ToolResult {
            call_id: ToolCallId("call-1".to_string()),
            output: ToolOutput::Denied("user said no".to_string()),
        };
        let update = translate_event(&sid(), &AgentEvent::ToolResult(result)).unwrap();
        let SessionUpdate::ToolCallUpdate(update) = update else {
            panic!("expected ToolCallUpdate");
        };
        assert_eq!(update.fields.status, Some(ToolCallStatus::Failed));
    }

    #[test]
    fn tasks_updated_becomes_a_plan() {
        let tasks = vec![
            Task { id: 1, text: "write tests".to_string(), status: TaskStatus::Done },
            Task { id: 2, text: "write code".to_string(), status: TaskStatus::InProgress },
        ];
        let update = translate_event(&sid(), &AgentEvent::TasksUpdated(tasks)).unwrap();
        let SessionUpdate::Plan(plan) = update else {
            panic!("expected Plan");
        };
        assert_eq!(plan.entries.len(), 2);
        assert_eq!(plan.entries[0].status, PlanEntryStatus::Completed);
        assert_eq!(plan.entries[1].status, PlanEntryStatus::InProgress);
    }

    #[test]
    fn context_usage_is_not_surfaced() {
        assert!(translate_event(&sid(), &AgentEvent::ContextUsage { used: 10, limit: 100 }).is_none());
    }

    #[test]
    fn turn_complete_and_turn_paused_are_not_session_updates_but_are_stop_reasons() {
        assert!(translate_event(&sid(), &AgentEvent::TurnComplete).is_none());
        assert!(translate_event(&sid(), &AgentEvent::TurnPaused("paused".to_string())).is_none());
        assert_eq!(terminal_stop_reason(&AgentEvent::TurnComplete), Some(StopReason::EndTurn));
        assert_eq!(
            terminal_stop_reason(&AgentEvent::TurnPaused("paused".to_string())),
            Some(StopReason::MaxTurnRequests)
        );
    }

    #[test]
    fn sub_agent_activity_unwraps_to_its_inner_event() {
        let inner = Box::new(AgentEvent::TextDelta("from sub-agent".to_string()));
        let update = translate_event(&sid(), &AgentEvent::SubAgentActivity(inner)).unwrap();
        assert!(matches!(update, SessionUpdate::AgentMessageChunk(_)));
    }
}
```

Add `mod translate;` to `crates/aivyx-acp/src/lib.rs`.

- [ ] **Step 2: Run the tests to verify they fail (module doesn't compile against real types yet)**

Run: `cargo test -p aivyx-acp --lib translate -- --nocapture`
Expected: FAIL — compile errors are expected here if any field name above
drifts from the exact crate version resolved in Task 2; fix field names
against the compiler's actual error messages (this is exactly the kind
of drift the "resolve at implementation time" note in Global Constraints
anticipates — the struct/enum *shapes* above were confirmed by reading
`agent-client-protocol-schema`'s `src/v1/{agent,client,plan,tool_call,content}.rs`
directly on 2026-07-20, but a newer patch release could rename a field).

- [ ] **Step 3: Fix compile errors and re-run until green**

Run: `cargo test -p aivyx-acp --lib translate`
Expected: PASS, all 11 tests.

- [ ] **Step 4: Commit**

```bash
git add crates/aivyx-acp/src/translate.rs crates/aivyx-acp/src/lib.rs
git commit -m "$(cat <<'EOF'
Add pure AgentEvent -> ACP SessionUpdate/StopReason translation

Covers every AgentEvent variant per the Protocol Mapping table in
docs/superpowers/specs/2026-07-20-acp-editor-integration-design.md.
No I/O, unit-tested directly.

Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>
EOF
)"
```

---

### Task 4: Permission mapping + `AcpPrompter`

**Files:**
- Create: `crates/aivyx-acp/src/prompter.rs`
- Modify: `crates/aivyx-acp/src/lib.rs` (add `mod prompter;`)

**Interfaces:**
- Consumes: `aivyx_sandbox::{PermissionPrompter, PermissionRequest, PermissionTarget, DiffContent, ActionKind, UserResponse}` (`crates/aivyx-sandbox/src/lib.rs:27-177`).
- Produces: `pub(crate) fn permission_request_to_acp(session_id: SessionId, request: &PermissionRequest) -> RequestPermissionRequest`
  (pure, unit-tested); `pub struct AcpPrompter { .. }` implementing
  `PermissionPrompter`, constructed as
  `AcpPrompter::new(session_id: SessionId, connection: ConnectionTo<Client>)`;
  and `pub struct DeferredPrompter`, `pub struct PrompterInstaller`,
  `pub fn deferred_prompter() -> (DeferredPrompter, PrompterInstaller)` —
  needed because `ConfirmationGate` (built inside `build_agent`, Task 1)
  needs a `PermissionPrompter` *before* any ACP connection exists to
  build a real `AcpPrompter` from. All five are consumed by Task 5/6.

Four fixed, well-known `PermissionOption` IDs are used so the response
side never needs to look anything up: `"allow_once"`, `"allow_always"`,
`"reject_once"`, `"reject_always"`.

- [ ] **Step 1: Write the failing tests for the pure mapping function**

`crates/aivyx-acp/src/prompter.rs`:

```rust
//! Maps `aivyx-sandbox`'s `PermissionRequest`/`UserResponse` to and from
//! ACP's `session/request_permission`. See
//! `crates/aivyx-sandbox/src/editor_approval.rs` for the sibling mapping
//! this one is modeled on (same `ActionKind` match, different target
//! shape — ACP's `Diff`/`ToolCallUpdate` instead of the bespoke
//! `ApprovalContent` enum).

use agent_client_protocol::schema::v1::{
    Diff, PermissionOption, PermissionOptionKind, RequestPermissionOutcome, RequestPermissionRequest,
    RequestPermissionResponse, SelectedPermissionOutcome, SessionId, ToolCall as AcpToolCall,
    ToolCallContent, ToolCallStatus, ToolCallUpdate, ToolCallUpdateFields, ToolKind,
};
use agent_client_protocol::{Client, ConnectionTo};
use aivyx_sandbox::{ActionKind, PermissionPrompter, PermissionRequest, PermissionTarget, UserResponse};
use async_trait::async_trait;

const ALLOW_ONCE: &str = "allow_once";
const ALLOW_ALWAYS: &str = "allow_always";
const REJECT_ONCE: &str = "reject_once";
const REJECT_ALWAYS: &str = "reject_always";

fn fixed_options() -> Vec<PermissionOption> {
    vec![
        PermissionOption { option_id: ALLOW_ONCE.into(), name: "Allow".to_string(), kind: PermissionOptionKind::AllowOnce, meta: None },
        PermissionOption { option_id: ALLOW_ALWAYS.into(), name: "Always Allow".to_string(), kind: PermissionOptionKind::AllowAlways, meta: None },
        PermissionOption { option_id: REJECT_ONCE.into(), name: "Deny".to_string(), kind: PermissionOptionKind::RejectOnce, meta: None },
        PermissionOption { option_id: REJECT_ALWAYS.into(), name: "Always Deny".to_string(), kind: PermissionOptionKind::RejectAlways, meta: None },
    ]
}

fn target_string(target: &PermissionTarget) -> String {
    match target {
        PermissionTarget::Path(path) => path.display().to_string(),
        PermissionTarget::Command { program, args } => format!("{program} {}", args.join(" ")),
        PermissionTarget::Other(description) => description.clone(),
    }
}

/// Builds the `ToolCallUpdate` describing what's pending, reusing
/// `request.diff` for `Write`/`Delete` exactly like
/// `editor_approval::build_pending_request` does — this is the *only*
/// place in `aivyx-acp` where a real diff reaches the client, per the
/// Global Constraints note in the plan.
fn pending_tool_call(request: &PermissionRequest) -> ToolCallUpdate {
    let title = target_string(&request.target);
    let content = match (request.action, &request.diff) {
        (ActionKind::Write | ActionKind::Delete, Some(diff)) => vec![ToolCallContent::Diff(Diff {
            path: match &request.target {
                PermissionTarget::Path(p) => p.clone(),
                _ => std::path::PathBuf::from(&title),
            },
            old_text: if diff.old_content.is_empty() { None } else { Some(diff.old_content.clone()) },
            new_text: diff.new_content.clone(),
            meta: None,
        })],
        _ => match &request.preview {
            Some(preview) => vec![ToolCallContent::Content(preview.clone().into())],
            None => Vec::new(),
        },
    };
    let kind = match request.action {
        ActionKind::Write => ToolKind::Edit,
        ActionKind::Delete => ToolKind::Delete,
        ActionKind::Execute => ToolKind::Execute,
        ActionKind::McpTool => ToolKind::Other,
        ActionKind::Read | ActionKind::Internal => ToolKind::Other,
    };
    ToolCallUpdate::new(
        request.tool_name.clone(),
        ToolCallUpdateFields {
            kind: Some(kind),
            status: Some(ToolCallStatus::Pending),
            title: Some(title),
            content: Some(content),
            locations: None,
            raw_input: Some(request.arguments_preview.clone()),
            raw_output: None,
        },
    )
}

pub(crate) fn permission_request_to_acp(
    session_id: SessionId,
    request: &PermissionRequest,
) -> RequestPermissionRequest {
    RequestPermissionRequest {
        session_id,
        tool_call: pending_tool_call(request),
        options: fixed_options(),
        meta: None,
    }
}

/// `RejectOnce`/`RejectAlways` both deny this one call — aivyx-coder's
/// `UserResponse` has no "always deny" cache concept
/// (`crates/aivyx-sandbox/src/lib.rs:156-160`), so both collapse to
/// `Deny`. A `Cancelled` outcome (the client cancelled the whole prompt
/// turn) also fails closed to `Deny`.
fn acp_response_to_user_response(response: RequestPermissionResponse) -> UserResponse {
    match response.outcome {
        RequestPermissionOutcome::Cancelled => UserResponse::Deny,
        RequestPermissionOutcome::Selected(SelectedPermissionOutcome { option_id, .. }) => {
            match option_id.as_ref() {
                ALLOW_ONCE => UserResponse::Allow,
                ALLOW_ALWAYS => UserResponse::AllowAlways,
                _ => UserResponse::Deny,
            }
        }
    }
}

/// The `PermissionPrompter` side of the ACP bridge — structurally
/// parallel to `aivyx-tui`'s `TuiPrompter`
/// (`crates/aivyx-tui/src/permission.rs:24-45`), swapping the
/// `oneshot`-channel-to-render-loop bridge for a real
/// `session/request_permission` round trip.
pub struct AcpPrompter {
    session_id: SessionId,
    connection: ConnectionTo<Client>,
}

impl AcpPrompter {
    pub fn new(session_id: SessionId, connection: ConnectionTo<Client>) -> Self {
        Self { session_id, connection }
    }
}

#[async_trait]
impl PermissionPrompter for AcpPrompter {
    async fn prompt(&self, request: &PermissionRequest) -> UserResponse {
        let acp_request = permission_request_to_acp(self.session_id.clone(), request);
        match self.connection.send_request(acp_request).block_task().await {
            Ok(response) => acp_response_to_user_response(response),
            // Connection gone / request failed — fail closed, matching
            // `TuiPrompter`'s "render loop is gone" fallback.
            Err(_) => UserResponse::Deny,
        }
    }
}

/// A `PermissionPrompter` that blocks until the real `AcpPrompter` is
/// installed, then delegates every call to it. Needed because
/// `ConfirmationGate` (and therefore its prompter) is constructed inside
/// `build_agent`, before `session/new` — and therefore before any
/// `ConnectionTo<Client>` — exists. `NewSessionRequest`'s handler
/// (Task 5) calls `PrompterInstaller::install` exactly once, as soon as
/// it has a real connection and session id.
pub struct DeferredPrompter {
    inner: tokio::sync::Mutex<Option<AcpPrompter>>,
    installed: tokio::sync::Mutex<Option<tokio::sync::oneshot::Receiver<AcpPrompter>>>,
}

pub struct PrompterInstaller(tokio::sync::oneshot::Sender<AcpPrompter>);

impl PrompterInstaller {
    pub fn install(self, prompter: AcpPrompter) {
        let _ = self.0.send(prompter);
    }
}

pub fn deferred_prompter() -> (DeferredPrompter, PrompterInstaller) {
    let (tx, rx) = tokio::sync::oneshot::channel();
    (
        DeferredPrompter {
            inner: tokio::sync::Mutex::new(None),
            installed: tokio::sync::Mutex::new(Some(rx)),
        },
        PrompterInstaller(tx),
    )
}

#[async_trait]
impl PermissionPrompter for DeferredPrompter {
    async fn prompt(&self, request: &PermissionRequest) -> UserResponse {
        let mut inner = self.inner.lock().await;
        if inner.is_none() {
            let mut installed = self.installed.lock().await;
            if let Some(rx) = installed.take()
                && let Ok(prompter) = rx.await
            {
                *inner = Some(prompter);
            }
        }
        match inner.as_ref() {
            Some(prompter) => prompter.prompt(request).await,
            // Should not happen in practice — `session/new` always runs
            // before the first `session/prompt` that could trigger a
            // gated tool call. Fail closed if it somehow does.
            None => UserResponse::Deny,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aivyx_sandbox::DiffContent;
    use std::path::PathBuf;

    fn sid() -> SessionId {
        SessionId::new("sess-1")
    }

    fn write_request(diff: Option<DiffContent>) -> PermissionRequest {
        PermissionRequest {
            tool_name: "write_file".to_string(),
            action: ActionKind::Write,
            target: PermissionTarget::Path(PathBuf::from("/project/a.rs")),
            arguments_preview: serde_json::json!({}),
            preview: None,
            diff,
        }
    }

    #[test]
    fn write_with_diff_produces_a_diff_tool_call_content() {
        let request = write_request(Some(DiffContent {
            old_content: "old\n".to_string(),
            new_content: "new\n".to_string(),
        }));
        let acp = permission_request_to_acp(sid(), &request);
        assert_eq!(acp.options.len(), 4);
        let ToolCallContent::Diff(diff) = &acp.tool_call.fields.content.as_ref().unwrap()[0] else {
            panic!("expected a Diff content block");
        };
        assert_eq!(diff.old_text.as_deref(), Some("old\n"));
        assert_eq!(diff.new_text, "new\n");
    }

    #[test]
    fn execute_request_carries_no_diff() {
        let request = PermissionRequest {
            tool_name: "run_command".to_string(),
            action: ActionKind::Execute,
            target: PermissionTarget::Command { program: "cargo".to_string(), args: vec!["test".to_string()] },
            arguments_preview: serde_json::json!({}),
            preview: None,
            diff: None,
        };
        let acp = permission_request_to_acp(sid(), &request);
        assert!(acp.tool_call.fields.content.as_ref().unwrap().is_empty());
        assert_eq!(acp.tool_call.fields.kind, Some(ToolKind::Execute));
    }

    #[test]
    fn allow_once_option_maps_to_allow() {
        let response = RequestPermissionResponse {
            outcome: RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(ALLOW_ONCE)),
            meta: None,
        };
        assert_eq!(acp_response_to_user_response(response), UserResponse::Allow);
    }

    #[test]
    fn allow_always_option_maps_to_allow_always() {
        let response = RequestPermissionResponse {
            outcome: RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(ALLOW_ALWAYS)),
            meta: None,
        };
        assert_eq!(acp_response_to_user_response(response), UserResponse::AllowAlways);
    }

    #[test]
    fn both_reject_options_and_cancelled_map_to_deny() {
        let reject_once = RequestPermissionResponse {
            outcome: RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(REJECT_ONCE)),
            meta: None,
        };
        let reject_always = RequestPermissionResponse {
            outcome: RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(REJECT_ALWAYS)),
            meta: None,
        };
        let cancelled = RequestPermissionResponse { outcome: RequestPermissionOutcome::Cancelled, meta: None };
        assert_eq!(acp_response_to_user_response(reject_once), UserResponse::Deny);
        assert_eq!(acp_response_to_user_response(reject_always), UserResponse::Deny);
        assert_eq!(acp_response_to_user_response(cancelled), UserResponse::Deny);
    }
}
```

Add `mod prompter;` and
`pub use prompter::{AcpPrompter, DeferredPrompter, PrompterInstaller, deferred_prompter};`
to `crates/aivyx-acp/src/lib.rs`.

- [ ] **Step 2: Run to verify failure, then fix against the real compiler output**

Run: `cargo test -p aivyx-acp --lib prompter`
Expected: initial compile errors (same caveat as Task 3 Step 2 — resolve
against whatever the actually-resolved crate version reports), then all
6 tests PASS once fixed.

- [ ] **Step 3: Commit**

```bash
git add crates/aivyx-acp/src/prompter.rs crates/aivyx-acp/src/lib.rs
git commit -m "$(cat <<'EOF'
Add AcpPrompter and PermissionRequest <-> ACP permission mapping

The only place a real diff reaches an ACP client, mirroring
aivyx-sandbox's existing editor_approval::build_pending_request.

Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>
EOF
)"
```

---

### Task 5: Session handling — `new_session`, `prompt`, `set_mode`

**Files:**
- Create: `crates/aivyx-acp/src/session.rs`
- Modify: `crates/aivyx-acp/src/lib.rs` (replace the Task 2 stub `run()` body)

**Interfaces:**
- Consumes: `Task 1`'s `crate::agent_builder::{build_agent, BuiltAgent}` — but note `aivyx-acp` cannot depend on the `aivyx` binary crate (binaries aren't linkable dependencies). **`build_agent`/`BuiltAgent` are called from `crates/aivyx`'s own `--acp` wiring in Task 6, not from inside `aivyx-acp` itself** — this task's `session.rs` takes an already-built `aivyx_core::Agent` plus its collaborators as plain parameters, staying decoupled from `aivyx-config`/`aivyx-tools` exactly as planned in Global Constraints.
- Produces: `pub struct AcpSessionConfig { pub agent: aivyx_core::Agent, pub events_rx: mpsc::UnboundedReceiver<AgentEvent>, pub cwd: PathBuf, pub plan_mode: PlanMode, pub prompter_installer: PrompterInstaller }`
  and `pub async fn run(config: AcpSessionConfig) -> agent_client_protocol::Result<()>`
  in `aivyx-acp` — this replaces Task 2's parameterless `run()`; Task 6
  calls it after building the config from `build_agent` (which itself
  needs a `DeferredPrompter`/`PrompterInstaller` pair from Task 4 before
  it can even construct `ConfirmationGate`).

One process, one session (per the design's Decision 4) — `NewSessionRequest`
is only ever handled once; a second one gets an error response. State
lives in `Arc<tokio::sync::Mutex<Option<Session>>>`, filled in by the
`NewSessionRequest` handler, read by `PromptRequest`/`SetSessionModeRequest`.

- [ ] **Step 1: Write `crates/aivyx-acp/src/session.rs`**

```rust
//! Session lifecycle: `session/new` builds the one-and-only session this
//! process will ever host (see the design's Decision 4 — parallelism is
//! achieved by the editor spawning multiple `aivyx --acp` processes, not
//! by this crate hosting multiple sessions), `session/prompt` drives one
//! turn to completion while streaming `AgentEvent`s out as ACP session
//! updates, `session/set_mode` toggles `PlanMode`.

use std::path::PathBuf;
use std::sync::Arc;

use agent_client_protocol::schema::v1::{
    AgentCapabilities, ContentBlock, InitializeRequest, InitializeResponse, NewSessionRequest,
    NewSessionResponse, PromptRequest, PromptResponse, SessionId, SessionMode, SessionModeId,
    SessionModeState, SessionNotification, SetSessionModeRequest, SetSessionModeResponse,
    StopReason,
};
use agent_client_protocol::{Agent as AcpAgentBuilder, Client, ConnectTo, Dispatch, Result, Stdio};
use aivyx_core::{Agent, AgentEvent};
use aivyx_sandbox::PlanMode;
use tokio::sync::{mpsc, Mutex};
use tokio_util::sync::CancellationToken;

use crate::prompter::{AcpPrompter, PrompterInstaller};

const MODE_CODE: &str = "code";
const MODE_PLAN: &str = "plan";

/// Everything the ACP frontend needs, built by `aivyx`'s
/// `agent_builder::build_agent` (Task 1) before this crate's `run` takes
/// over. `prompter_installer` exists because `build_agent` had to hand
/// `ConfirmationGate` a `DeferredPrompter` (Task 4) — there's no real
/// `ConnectionTo<Client>` yet at that point — and this is how the real
/// `AcpPrompter` gets installed behind it, exactly once, from inside the
/// `NewSessionRequest` handler below.
pub struct AcpSessionConfig {
    pub agent: Agent,
    pub events_rx: mpsc::UnboundedReceiver<AgentEvent>,
    pub cwd: PathBuf,
    pub plan_mode: PlanMode,
    pub prompter_installer: PrompterInstaller,
}

struct Session {
    agent: Agent,
    events_rx: mpsc::UnboundedReceiver<AgentEvent>,
    cwd: PathBuf,
    plan_mode: PlanMode,
    session_id: SessionId,
}

/// Turns `req.prompt`'s content blocks into the plain text `Agent::run_turn`
/// expects. Only `ContentBlock::Text` is forwarded — see the Global
/// Constraints note on v1's text-only scope.
fn extract_prompt_text(blocks: &[ContentBlock]) -> String {
    let mut text = String::new();
    let mut dropped_any = false;
    for block in blocks {
        match block {
            ContentBlock::Text(t) => {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(&t.text);
            }
            _ => dropped_any = true,
        }
    }
    if dropped_any {
        text.push_str("\n(non-text content in this message was not forwarded)");
    }
    text
}

pub async fn run(config: AcpSessionConfig) -> Result<()> {
    let state: Arc<Mutex<Option<Session>>> = Arc::new(Mutex::new(None));
    let init_config = config;

    let new_session_state = Arc::clone(&state);
    let prompt_state = Arc::clone(&state);
    let mode_state = Arc::clone(&state);
    // `AcpSessionConfig` is only consumable once — moved into the
    // `NewSessionRequest` handler's closure below.
    let mut init_config = Some(init_config);

    AcpAgentBuilder::builder()
        .name("aivyx")
        .on_receive_request(
            async move |req: InitializeRequest, responder, _connection| {
                responder.respond(
                    InitializeResponse::new(req.protocol_version)
                        .agent_capabilities(AgentCapabilities::new()),
                )
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |req: NewSessionRequest, responder, connection| {
                let mut guard = new_session_state.lock().await;
                if guard.is_some() {
                    return responder.respond_with_error(agent_client_protocol::util::internal_error(
                        "this aivyx --acp process already hosts a session — one session per process",
                    ));
                }
                let Some(built) = init_config.take() else {
                    return responder.respond_with_error(agent_client_protocol::util::internal_error(
                        "session already consumed",
                    ));
                };
                let session_id = SessionId::new(uuid::Uuid::new_v4().to_string());
                // Installs the real prompter behind the `DeferredPrompter`
                // `ConfirmationGate` has been holding since `build_agent`
                // ran — every gated tool call in this session (any Write/
                // Delete/Execute/McpTool action) will now reach the real
                // client via `session/request_permission`.
                built
                    .prompter_installer
                    .install(AcpPrompter::new(session_id.clone(), connection.clone()));
                *guard = Some(Session {
                    agent: built.agent,
                    events_rx: built.events_rx,
                    cwd: req.cwd.clone(),
                    plan_mode: built.plan_mode,
                    session_id: session_id.clone(),
                });
                drop(guard);
                responder.respond(NewSessionResponse {
                    session_id,
                    modes: Some(SessionModeState {
                        current_mode_id: SessionModeId::new(MODE_CODE),
                        available_modes: vec![
                            SessionMode { id: SessionModeId::new(MODE_CODE), name: "Code".to_string(), description: None, meta: None },
                            SessionMode { id: SessionModeId::new(MODE_PLAN), name: "Plan".to_string(), description: Some("Read-only: the model can read, search, and build a task list, but cannot edit files or run commands.".to_string()), meta: None },
                        ],
                        meta: None,
                    }),
                    config_options: None,
                    meta: None,
                })
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |req: SetSessionModeRequest, responder, _connection| {
                let guard = mode_state.lock().await;
                let Some(session) = guard.as_ref() else {
                    return responder.respond_with_error(agent_client_protocol::util::internal_error("no session"));
                };
                session.plan_mode.set_active(req.mode_id.as_ref() == MODE_PLAN);
                responder.respond(SetSessionModeResponse { meta: None })
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |req: PromptRequest, responder, connection| {
                let mut guard = prompt_state.lock().await;
                let Some(session) = guard.as_mut() else {
                    return responder.respond_with_error(agent_client_protocol::util::internal_error("no session"));
                };
                let text = extract_prompt_text(&req.prompt);
                let cancellation = CancellationToken::new();
                let run = session.agent.run_turn(text, &session.cwd, cancellation);
                tokio::pin!(run);
                let mut stop_reason = None;
                let result = loop {
                    tokio::select! {
                        result = &mut run => break result,
                        Some(event) = session.events_rx.recv() => {
                            if let Some(reason) = crate::translate::terminal_stop_reason(&event) {
                                stop_reason = Some(reason);
                            }
                            if let Some(update) = crate::translate::translate_event(&session.session_id, &event) {
                                let _ = connection.send_notification(SessionNotification {
                                    session_id: session.session_id.clone(),
                                    update,
                                    meta: None,
                                });
                            }
                        }
                    }
                };
                // Drain anything buffered right at completion (e.g. the
                // final TextDelta/TurnComplete pair) before responding.
                while let Ok(event) = session.events_rx.try_recv() {
                    if let Some(reason) = crate::translate::terminal_stop_reason(&event) {
                        stop_reason = Some(reason);
                    }
                    if let Some(update) = crate::translate::translate_event(&session.session_id, &event) {
                        let _ = connection.send_notification(SessionNotification {
                            session_id: session.session_id.clone(),
                            update,
                            meta: None,
                        });
                    }
                }
                if let Err(err) = result {
                    return responder.respond_with_error(agent_client_protocol::util::internal_error(err.to_string()));
                }
                let stop_reason = stop_reason.unwrap_or_else(|| {
                    if session.agent.last_turn_paused() {
                        StopReason::MaxTurnRequests
                    } else {
                        StopReason::EndTurn
                    }
                });
                responder.respond(PromptResponse { stop_reason, meta: None })
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_dispatch(
            async move |message: Dispatch, cx| {
                message.respond_with_error(agent_client_protocol::util::method_not_found(), cx)
            },
            agent_client_protocol::on_receive_dispatch!(),
        )
        .connect_to(Stdio::new())
        .await
}
```

Delete the Task 2 `run()` body from `crates/aivyx-acp/src/lib.rs` and
replace it with `pub use session::{run, AcpSessionConfig};` plus
`mod session;`. Add `uuid = { version = "1", features = ["v4"] }` and
`tokio-util = "0.7.18"` to `crates/aivyx-acp/Cargo.toml`'s
`[dependencies]` (both already used elsewhere in this workspace at these
exact versions — `crates/aivyx-sandbox/Cargo.toml`, `crates/aivyx-tui/Cargo.toml`).

- [ ] **Step 2: Update the Task 2 stub test binary to match the new `run` signature**

`crates/aivyx-acp/tests/bin/stub.rs` now needs a real `AcpSessionConfig`,
which needs a real `Agent` — this stub is superseded by Task 6's real
`--acp` binary wiring. Delete `crates/aivyx-acp/tests/initialize.rs` and
`crates/aivyx-acp/tests/bin/stub.rs`, and remove the `[[bin]]` entry from
`crates/aivyx-acp/Cargo.toml` — Task 6 adds a proper E2E test against the
real `aivyx` binary instead, which is a strictly more faithful test of
the same thing.

- [ ] **Step 3: Build to confirm the crate compiles**

Run: `cargo build -p aivyx-acp`
Expected: builds cleanly (no test to run yet for this task in isolation
— `session.rs`'s logic is exercised end-to-end in Task 6, since it
fundamentally requires a real connected client, matching the same
reasoning that ruled out unit-testing `aivyx-tui`'s render loop directly).

- [ ] **Step 4: Commit**

```bash
git add crates/aivyx-acp
git commit -m "$(cat <<'EOF'
Implement ACP session lifecycle: new_session, prompt, set_mode

One session per process (design Decision 4). NewSessionRequest installs
the real AcpPrompter behind the DeferredPrompter ConfirmationGate has
been holding since construction. PromptRequest drives Agent::run_turn
while concurrently draining AgentEvents into session/update
notifications via translate::translate_event, ending on StopReason
derived from TurnComplete/TurnPaused or a plain EndTurn default.

Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>
EOF
)"
```

---

### Task 6: Wire `--acp` into the `aivyx` binary + E2E test

**Files:**
- Modify: `crates/aivyx/src/main.rs` (add the `--acp` flag and branch)
- Modify: `crates/aivyx/Cargo.toml` (add `aivyx-acp` dependency)
- Create: `crates/aivyx/tests/acp_e2e.rs`

**Interfaces:**
- Consumes: `crate::agent_builder::build_agent` (Task 1), `aivyx_acp::{run, AcpSessionConfig, AcpPrompter}` (Tasks 2–5).

- [ ] **Step 1: Add the `--acp` flag to `Cli`**

In `crates/aivyx/src/main.rs`'s `Cli` struct (currently ending at
`edit_format` per the earlier read of `main.rs:83-118`), add:

```rust
    /// Run as an Agent Client Protocol (ACP) server over stdin/stdout,
    /// for embedding in an editor (Zed, or VS Code via the
    /// formulahendry.acp-client extension) instead of the TUI. Mutually
    /// exclusive with --plan (ACP's own session/set_mode supersedes it)
    /// and --auto (not yet supported together — see docs/superpowers/
    /// specs/2026-07-20-acp-editor-integration-design.md's Out of Scope).
    #[arg(long)]
    acp: bool,
```

- [ ] **Step 2: Branch in `main()` before the TUI-specific tail**

`--acp` still needs a `PermissionPrompter` at construction time
(`build_agent`'s third argument) before `NewSessionRequest` even exists
to build the real `AcpPrompter` — but `AcpPrompter` needs a
`ConnectionTo<Client>`, which only exists once a connection is live.
Task 4's `aivyx_acp::deferred_prompter()` returns exactly the
`(DeferredPrompter, PrompterInstaller)` pair this needs: the
`DeferredPrompter` goes into `ConfirmationGate` now, the
`PrompterInstaller` travels through to `session.rs`'s `NewSessionRequest`
handler (Task 5), which installs the real `AcpPrompter` behind it the
moment a connection exists.

Replace Task 1 Step 3's unconditional
`let (tui_prompter, permission_rx) = aivyx_tui::permission_channel(); let built = ...build_agent(&cli, &settings, Arc::new(tui_prompter)).await?;`
with:

```rust
    let (prompter, tui_permission_rx, acp_prompter_installer): (
        Arc<dyn PermissionPrompter>,
        Option<aivyx_tui::PermissionModalReceiver>,
        Option<aivyx_acp::PrompterInstaller>,
    ) = if cli.acp {
        let (deferred, installer) = aivyx_acp::deferred_prompter();
        (Arc::new(deferred), None, Some(installer))
    } else {
        let (tui_prompter, permission_rx) = aivyx_tui::permission_channel();
        (Arc::new(tui_prompter), Some(permission_rx), None)
    };
    let built = crate::agent_builder::build_agent(&cli, &settings, prompter).await?;

    if cli.acp {
        if cli.plan {
            anyhow::bail!("--acp and --plan cannot be used together");
        }
        if cli.auto.is_some() {
            anyhow::bail!("--acp and --auto cannot be used together");
        }
        return aivyx_acp::run(aivyx_acp::AcpSessionConfig {
            agent: built.agent,
            events_rx: built.events_rx,
            cwd: built.cwd,
            plan_mode: built.plan_mode,
            prompter_installer: acp_prompter_installer.expect("set above when cli.acp"),
        })
        .await
        .map_err(|e| anyhow::anyhow!(e.to_string()));
    }
```

Then update `main.rs`'s TUI tail (former Task 1 Step 3's tail) to unwrap
`tui_permission_rx` before calling `aivyx_tui::run(...)`:

```rust
    let permission_rx = tui_permission_rx.expect("TUI path always sets this");
```

- [ ] **Step 3: Add `aivyx-acp` as a dependency**

`crates/aivyx/Cargo.toml`, add: `aivyx-acp = { version = "0.1.0", path = "../aivyx-acp" }`

- [ ] **Step 4: Write the failing E2E test**

`crates/aivyx/tests/acp_e2e.rs`:

```rust
use agent_client_protocol::schema::v1::InitializeRequest;
use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::{AcpAgent, Agent, Client, ConnectTo};
use std::str::FromStr;
use tempfile::tempdir;

/// Drives the real, release-identical `aivyx --acp` binary over stdio —
/// the same integration point Zed itself would use — rather than the
/// crate's own translation-layer unit tests. Requires no LLM backend:
/// `initialize`/`session/new` never call the model.
#[tokio::test]
async fn acp_initialize_and_new_session_round_trip() {
    let bin = env!("CARGO_BIN_EXE_aivyx");
    let cwd = tempdir().unwrap();
    // A dummy backend URL is fine here — this test never sends a prompt,
    // so the LLM backend is never actually contacted.
    let command = format!(
        "{} --acp --base-url http://127.0.0.1:1/v1 --model test-model",
        bin
    );
    let agent = AcpAgent::from_str(&command)
        .expect("valid command")
        .in_directory(cwd.path());

    Client::builder()
        .name("aivyx-acp-e2e-test")
        .connect_with(agent, async |connection| {
            let init = connection
                .send_request(InitializeRequest::new(ProtocolVersion::V1))
                .block_task()
                .await
                .expect("initialize should succeed");
            assert_eq!(init.protocol_version, ProtocolVersion::V1);

            let session = connection
                .build_session_cwd()
                .expect("cwd should be valid")
                .block_task()
                .start_session()
                .await
                .expect("session/new should succeed");
            assert!(!session.id().as_ref().is_empty());
            Ok::<_, agent_client_protocol::Error>(())
        })
        .await
        .expect("connection should complete without error");
}
```

If `AcpAgent`/`in_directory`/`build_session_cwd`/`start_session`/`session.id()`
don't match these exact names against the crate version actually
resolved, adjust against the compiler's error and against
`agent-client-protocol-cookbook`'s `one_shot_prompt`/`connecting_as_client`
modules (fetched and read during planning from
`https://raw.githubusercontent.com/agentclientprotocol/rust-sdk/main/src/agent-client-protocol-cookbook/src/lib.rs`)
for the exact current session-builder API shape.

- [ ] **Step 5: Run to verify it fails, then implement until it passes**

Run: `cargo test -p aivyx --test acp_e2e -- --nocapture`
Expected: initial failures while `main.rs`'s `--acp` branch/flag from
Steps 1–3 are wired up; PASS once complete.

- [ ] **Step 6: Full workspace verification**

Run: `cargo build --workspace && cargo test --workspace && cargo clippy --workspace --all-targets`
Expected: all green, including the untouched TUI test suite (confirming
Task 1's extraction really was behavior-preserving end to end, now that
a second real caller — the ACP path — exists alongside it).

- [ ] **Step 7: Manual smoke test against real Zed**

The design spec's Testing section explicitly calls for this: automated
tests (Step 5) prove the protocol shape is correct without a live model;
they cannot prove Zed itself is happy with it, since Zed's own client
implementation may be stricter or interpret something differently than
this plan's hand-written test client. With a real Ollama or llama-server
running locally:

1. Build a release or debug binary: `cargo build -p aivyx` (or the release script).
2. Add to Zed's `settings.json`:
   ```json
   { "agent_servers": { "aivyx-dev": { "command": "/absolute/path/to/target/debug/aivyx", "args": ["--acp"] } } }
   ```
3. In Zed, open the Agent panel, select "aivyx-dev", and start a session
   in this repository (or any local project).
4. Send a prompt that should trigger at least one gated tool call (e.g.
   "add a comment to the top of README.md") and confirm: the assistant's
   text streams live, a permission prompt appears in Zed's own UI (not a
   terminal), the diff shown matches the real change, and approving it
   applies the edit.
5. Toggle Zed's mode picker to "Plan" and confirm the model can no longer
   edit files (mirrors the TUI's `Ctrl+P` behavior).

If any step fails, fix the underlying issue in the relevant task's files
before considering this plan complete — do not skip this step even
though it's manual; it is the one check nothing else in this plan can
substitute for.

- [ ] **Step 8: Commit**

```bash
git add crates/aivyx crates/aivyx-acp
git commit -m "$(cat <<'EOF'
Wire --acp into the aivyx binary; add real end-to-end ACP test

aivyx --acp now speaks the Agent Client Protocol over stdio for real,
sharing the exact same agent_builder::build_agent construction path as
the TUI. E2E test drives the compiled binary the same way Zed would;
manually verified against real Zed (streaming, permission prompts, diff
review, plan mode) before this commit.

Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>
EOF
)"
```

---

### Task 7: Zed registration + VS Code documentation

**Files:**
- Modify: `README.md` (new "Editor integration (ACP)" section)

**Interfaces:** None — documentation only, no code.

- [ ] **Step 1: Add a README section**

Add after the "Serving" section of `README.md` (matching this repo's
existing doc-organization convention of grouping by user-facing
capability):

```markdown
## Editor integration (ACP)

`aivyx --acp` runs as an [Agent Client Protocol](https://agentclientprotocol.com)
server over stdin/stdout, for embedding aivyx-coder directly in an
editor's own UI instead of the terminal. Same security model as the TUI
— every tool call still passes through `ConfirmationGate`, now surfaced
as the editor's own permission UI instead of a modal.

**Zed**: add to your `settings.json`:

```json
{
  "agent_servers": {
    "aivyx": {
      "command": "/path/to/aivyx",
      "args": ["--acp"]
    }
  }
}
```

**VS Code**: install the [ACP Client](https://marketplace.visualstudio.com/items?itemName=formulahendry.acp-client)
extension, then point it at the same `aivyx --acp` command — no
aivyx-specific VS Code extension exists or is needed.

**Not yet supported over ACP**: `--auto` (autonomous mode), mid-turn
cancellation (`session/cancel`), and non-text prompt content (images,
embedded resources) — see `docs/superpowers/specs/
2026-07-20-acp-editor-integration-design.md` for the full scope.
```

- [ ] **Step 2: Commit**

```bash
git add README.md
git commit -m "$(cat <<'EOF'
Document Zed and VS Code ACP editor integration

Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>
EOF
)"
```
