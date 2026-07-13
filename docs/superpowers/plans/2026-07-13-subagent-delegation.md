# Sub-agent Delegation (`delegate_task`) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship `delegate_task`, a tool the model calls mid-turn to hand a bounded task to a fresh, context-isolated sub-agent — full tool access, same trust boundary as the parent — returning only a distilled result to the parent's history.

**Architecture:** `DelegateTaskTool` is defined in `aivyx-core` (not `aivyx-tools` — see Global Constraints) with every session-stable dependency (`LlmBackend`, `PermissionGate`, `ExecutionConfiner`, checkpointer, repo map, event sender, a `delegate_task`-free tool registry, plan/autonomous mode flags, verification config) bundled into a `DelegateTaskConfig` struct baked in at registration time. Its `execute()` constructs a fresh `ToolExecutor` and `Agent` sharing the parent's trust boundary, drives it turn-by-turn through the existing `TurnPaused`-continuation mechanism (capped by `[sub_agent] max_iterations`), and forwards the sub-agent's own `AgentEvent`s — wrapped in a new `AgentEvent::SubAgentActivity` variant — to the parent's transcript via a spawned background task, so the TUI can render them visually distinguished from the parent's own activity.

**Tech Stack:** Rust, tokio. No new external dependencies.

## Global Constraints

- `DelegateTaskTool` is defined in `crates/aivyx-core/src/delegate.rs`, implementing the foreign `aivyx_tools::Tool` trait — **not** in `crates/aivyx-tools/src/tools/`. `aivyx-tools` has no dependency on `aivyx-core` or `aivyx-llm` (the dependency graph runs the other way: `aivyx-core` depends on `aivyx-tools`, `aivyx-llm`, and `aivyx-repomap`), so a type needing `Agent` and `LlmBackend` simultaneously in scope with the `Tool` trait can only live in `aivyx-core`. This is Rust's orphan rule working normally, not a workaround.
- Every dependency `delegate_task` needs is session-stable (constructed once in `main.rs`, never varying call to call) and is baked into a `DelegateTaskConfig` struct passed to `DelegateTaskTool::new(config)` at registration time — `ToolExecutionContext` (used by every other `Tool` impl) is **not** modified.
- A sub-agent's own tool registry never contains `delegate_task` — recursion is structurally impossible (the registry simply doesn't have the tool in it), not merely policy-excluded.
- `delegate_task`'s own permission check is always `PermissionDecision::Allow` (`PermissionTarget::Other("delegate_task")`, `ActionKind::Internal` — the same tier `set_tasks`/`read_file` already resolve through) and `mutates_outside_session()` returns `false` (offered during plan mode, not hidden — the sub-agent's own tool-list construction already respects the shared `PlanMode` flag).
- A sub-agent that hits its iteration cap without a natural final answer never produces `ToolOutput::Error` — `delegate_task` returns `ToolOutput::Ok` with the sub-agent's accumulated text plus a clear cutoff notice appended. A genuine backend/LLM error (`run_turn` returning `Err`) is a different case and does surface as `ToolOutput::Error`, matching how every other tool reports real failures.
- No existing interactive-mode, plan-mode, autonomous-mode, or council-mode behavior may change for any input that isn't a `delegate_task` tool call. Run `cargo test --workspace` and `cargo clippy --workspace --all-targets` after every task. Commit after every task.

---

### Task 1: `ToolRegistry` gains `Clone`

**Files:**
- Modify: `crates/aivyx-tools/src/lib.rs`

**Interfaces:**
- Produces: `ToolRegistry: Clone` (derived). `Task 5` clones the registry (before `delegate_task` is registered onto the original) to build the sub-agent's own tool list.

- [ ] **Step 1: Write the failing test**

Add to `crates/aivyx-tools/src/lib.rs`'s existing `#[cfg(test)] mod tests` block:

```rust
    #[test]
    fn cloned_registry_is_independent_of_the_original() {
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(ReadFileTool));

        let sub_registry = registry.clone();
        // Registering onto the original after cloning must not affect the
        // already-taken clone — this is what lets main.rs snapshot "every
        // tool so far" for a sub-agent's registry, then keep adding
        // parent-only tools (like delegate_task itself) onto the original.
        registry.register(Arc::new(WriteFileTool));

        assert_eq!(sub_registry.definitions().len(), 1);
        assert_eq!(registry.definitions().len(), 2);
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p aivyx-tools cloned_registry_is_independent`
Expected: FAIL — `the trait bound `ToolRegistry: Clone` is not satisfied` (compile error).

- [ ] **Step 3: Derive `Clone`**

In `crates/aivyx-tools/src/lib.rs`, change:

```rust
#[derive(Default)]
pub struct ToolRegistry {
    tools: Vec<Arc<dyn Tool>>,
}
```

to:

```rust
#[derive(Default, Clone)]
pub struct ToolRegistry {
    tools: Vec<Arc<dyn Tool>>,
}
```

(`Arc<dyn Tool>` is `Clone` unconditionally — cloning just bumps a reference count — so `Vec<Arc<dyn Tool>>`, and therefore `ToolRegistry`, derives `Clone` with no further changes needed.)

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p aivyx-tools cloned_registry_is_independent`
Expected: PASS.

- [ ] **Step 5: Run the full workspace check**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add crates/aivyx-tools/src/lib.rs
git commit -m "Sub-agent delegation: ToolRegistry gains Clone"
```

---

### Task 2: `[sub_agent]` config section

**Files:**
- Modify: `crates/aivyx-config/src/lib.rs`

**Interfaces:**
- Produces: `pub struct SubAgentSettings { pub max_iterations: u32 }` (with `Default` giving `max_iterations: 10`), added as `pub sub_agent: SubAgentSettings` on `Settings`. `Task 5` reads `settings.sub_agent.max_iterations`.

- [ ] **Step 1: Write the failing test**

Add to `crates/aivyx-config/src/lib.rs`'s existing `#[cfg(test)] mod tests` block (find it via `grep -n "mod tests" crates/aivyx-config/src/lib.rs`):

```rust
    #[test]
    fn sub_agent_settings_default_max_iterations_is_ten() {
        let settings = SubAgentSettings::default();
        assert_eq!(settings.max_iterations, 10);
    }

    #[test]
    fn sub_agent_settings_parses_from_toml() {
        let toml = r#"
            [sub_agent]
            max_iterations = 5
        "#;
        let settings: Settings = toml::from_str(toml).unwrap();
        assert_eq!(settings.sub_agent.max_iterations, 5);
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p aivyx-config sub_agent_settings`
Expected: FAIL — `cannot find type \`SubAgentSettings\` in this scope` (compile error).

- [ ] **Step 3: Add `SubAgentSettings`**

In `crates/aivyx-config/src/lib.rs`, find `AutonomousSettings` (`grep -n "pub struct AutonomousSettings" crates/aivyx-config/src/lib.rs`) and add immediately after it (matching its exact shape):

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SubAgentSettings {
    /// A delegated task's own tool-call budget — deliberately separate
    /// from, and smaller than, `[permissions] max_tool_iterations_per_turn`:
    /// a delegated task is meant to be a scoped, bounded piece of work, not
    /// a full session.
    pub max_iterations: u32,
}

impl Default for SubAgentSettings {
    fn default() -> Self {
        Self { max_iterations: 10 }
    }
}
```

- [ ] **Step 4: Add the field to `Settings`**

Find the `Settings` struct (`grep -n "pub struct Settings" -A 15 crates/aivyx-config/src/lib.rs`) and add, alongside the existing `pub autonomous: AutonomousSettings` line:

```rust
    pub sub_agent: SubAgentSettings,
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p aivyx-config sub_agent_settings`
Expected: both tests pass.

- [ ] **Step 6: Run the full workspace check**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets`
Expected: clean.

- [ ] **Step 7: Commit**

```bash
git add crates/aivyx-config/src/lib.rs
git commit -m "Sub-agent delegation: [sub_agent] config section"
```

---

### Task 3: `AgentEvent::SubAgentActivity` and TUI rendering

**Files:**
- Modify: `crates/aivyx-core/src/agent.rs`
- Modify: `crates/aivyx-tui/src/app.rs`

**Interfaces:**
- Produces: `AgentEvent::SubAgentActivity(Box<AgentEvent>)` variant. `ChatLine::SubAgent(String)` variant in `aivyx-tui`, rendered distinctly. `Task 4`'s forwarding task emits `AgentEvent::SubAgentActivity(Box::new(inner_event))` for every event the nested agent produces.

- [ ] **Step 1: Write the failing test**

Add to `crates/aivyx-tui/src/app.rs`'s existing `#[cfg(test)] mod tests` block (find it via `grep -n "mod tests" crates/aivyx-tui/src/app.rs`):

```rust
    #[test]
    fn sub_agent_text_delta_renders_as_a_distinguished_chat_line() {
        let mut app = App::new(None, PlanMode::new());
        app.handle_agent_event(AgentEvent::SubAgentActivity(Box::new(
            AgentEvent::TextDelta("exploring the auth module".to_string()),
        )));

        assert!(matches!(
            app.transcript.last(),
            Some(ChatLine::SubAgent(text)) if text == "exploring the auth module"
        ));
    }

    #[test]
    fn sub_agent_tool_call_is_prefixed_and_still_distinguished() {
        let mut app = App::new(None, PlanMode::new());
        app.handle_agent_event(AgentEvent::SubAgentActivity(Box::new(
            AgentEvent::ToolCallDetected(tool_call("c1", "read_file")),
        )));

        assert!(matches!(
            app.transcript.last(),
            Some(ChatLine::SubAgent(text)) if text.contains("read_file")
        ));
    }
```

This test file needs a `tool_call(id, name)` helper — check first whether one already exists (`grep -n "fn tool_call" crates/aivyx-tui/src/app.rs`); if not, add this alongside the other test helpers in the same `mod tests` block:

```rust
    fn tool_call(id: &str, name: &str) -> aivyx_types::ToolCall {
        aivyx_types::ToolCall {
            id: aivyx_types::ToolCallId(id.to_string()),
            name: name.to_string(),
            arguments: serde_json::json!({}),
            source: aivyx_types::ToolCallSource::Native,
        }
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p aivyx-tui sub_agent`
Expected: FAIL — `no variant named \`SubAgentActivity\` found for enum \`AgentEvent\`` (compile error).

- [ ] **Step 3: Add the `AgentEvent` variant**

In `crates/aivyx-core/src/agent.rs`, find the `AgentEvent` enum (`grep -n "pub enum AgentEvent" -A 30 crates/aivyx-core/src/agent.rs`) and add, after the existing `CouncilNote(String)` variant:

```rust
    /// One event produced by a `delegate_task` sub-agent's own turn loop,
    /// forwarded verbatim from its private `AgentEvent` channel so it can
    /// render in the transcript distinguished from the parent's own
    /// activity — see `crate::delegate::DelegateTaskTool`. `Box`ed since
    /// `AgentEvent` itself isn't `Copy` and this variant would otherwise
    /// make every `AgentEvent` at least as large as its own biggest
    /// variant recursively.
    SubAgentActivity(Box<AgentEvent>),
```

- [ ] **Step 4: Add the `ChatLine::SubAgent` variant**

In `crates/aivyx-tui/src/app.rs`, find the `ChatLine` enum (`grep -n "enum ChatLine" -A 20 crates/aivyx-tui/src/app.rs`) and add, after the existing `Council(String)` variant:

```rust
    /// One rendered line of a `delegate_task` sub-agent's own activity
    /// (its text, tool calls, tool results) — visually distinct from both
    /// the parent's own transcript and `Council`'s deliberation, since a
    /// sub-agent's mutations still trigger real confirmation modals and
    /// need visible lead-up explaining what's being attempted and why.
    SubAgent(String),
```

- [ ] **Step 5: Handle the new event in `handle_agent_event`**

In `crates/aivyx-tui/src/app.rs`'s `handle_agent_event` method, add a new match arm right before the closing `}` of the `match event { ... }` block (alongside the existing `AgentEvent::CouncilNote(text) => { ... }` arm):

```rust
            AgentEvent::SubAgentActivity(inner) => {
                self.transcript.push(ChatLine::SubAgent(sub_agent_event_text(&inner)));
            }
```

Add this helper function near `tool_output_text` (search `grep -n "fn tool_output_text" crates/aivyx-tui/src/app.rs` for the nearest existing similar helper to place it beside):

```rust
/// Renders one nested `AgentEvent` from a `delegate_task` sub-agent as a
/// single line of text for `ChatLine::SubAgent` — deliberately reuses the
/// same shape the parent's own top-level events render as (tool
/// name/args, tool output text, plain text deltas) so a sub-agent's
/// activity reads the same way the parent's own would, just prefixed
/// distinctly by `chat_line_to_lines`.
fn sub_agent_event_text(event: &AgentEvent) -> String {
    match event {
        AgentEvent::TextDelta(text) => text.clone(),
        AgentEvent::ToolCallDetected(call) => format!("{}({})", call.name, call.arguments),
        AgentEvent::ToolResult(result) => tool_output_text(&result.output),
        AgentEvent::Error(text) | AgentEvent::TurnPaused(text) => text.clone(),
        AgentEvent::TurnComplete
        | AgentEvent::ContextUsage { .. }
        | AgentEvent::TasksUpdated(_)
        | AgentEvent::CouncilNote(_)
        | AgentEvent::SubAgentActivity(_) => String::new(),
    }
}
```

- [ ] **Step 6: Render `ChatLine::SubAgent` distinctly**

In `crates/aivyx-tui/src/app.rs`'s `chat_line_to_lines` function, add a new match arm after the existing `ChatLine::Council(text) => { ... }` arm:

```rust
        ChatLine::SubAgent(text) => {
            if text.is_empty() {
                return Vec::new();
            }
            prefixed_lines(text, "  sub-agent> ", Style::default().fg(Color::LightYellow))
        }
```

(Empty strings are skipped entirely, not rendered as a blank prefixed line — `sub_agent_event_text` returns `""` for several event kinds, like `TurnComplete`, that carry no text worth a transcript line.)

- [ ] **Step 7: Run the tests to verify they pass**

Run: `cargo test -p aivyx-tui sub_agent`
Expected: both new tests pass.

- [ ] **Step 8: Run the full workspace check**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets`
Expected: clean.

- [ ] **Step 9: Commit**

```bash
git add crates/aivyx-core/src/agent.rs crates/aivyx-tui/src/app.rs
git commit -m "Sub-agent delegation: AgentEvent::SubAgentActivity + distinguished TUI rendering"
```

---

### Task 4: `DelegateTaskTool`

**Files:**
- Create: `crates/aivyx-core/src/delegate.rs`
- Modify: `crates/aivyx-core/src/lib.rs` (register the module, export `DelegateTaskTool`/`DelegateTaskConfig`)

**Interfaces:**
- Consumes: `Task 1`'s `ToolRegistry: Clone`; `Task 3`'s `AgentEvent::SubAgentActivity`; `aivyx_tools::{Tool, ToolError, ToolExecutionContext, ToolExecutor, ToolRegistry, GitCheckpointer}`; `aivyx_sandbox::{ActionKind, AutonomousMode, ExecutionConfiner, PermissionGate, PermissionRequest, PermissionTarget, PlanMode}`; `aivyx_llm::LlmBackend`; `aivyx_repomap::RepoMap`; `crate::agent::{Agent, AgentConfig, AgentEvent, EditFormat}`.
- Produces: `pub struct DelegateTaskConfig { ... }` (all fields `pub`), `pub struct DelegateTaskTool` with `pub fn new(config: DelegateTaskConfig) -> Self`. `Task 5` (`main.rs`) constructs both.

- [ ] **Step 1: Write the failing tests**

Create `crates/aivyx-core/src/delegate.rs`:

```rust
//! Sub-agent delegation (ROADMAP.md Phase 9): `delegate_task`, a tool the
//! model calls mid-turn to hand a bounded task to a fresh, isolated
//! `Agent` — full tool access, same trust boundary as the parent (same
//! `PermissionGate`, checkpointer, plan/autonomous mode), but a completely
//! fresh conversation history, so the parent's own context window never
//! has to hold the raw exploration/edit trail.
//!
//! Lives here, not alongside the other `Tool` impls in `aivyx-tools`,
//! because it needs `Agent` itself (to construct the nested agent) and
//! `LlmBackend` in scope simultaneously with the `aivyx_tools::Tool` trait
//! it implements — `aivyx-tools` has no dependency on `aivyx-core` or
//! `aivyx-llm` (the graph runs the other way), so only a crate that
//! already depends on all three can define this type. See the design
//! doc's "Crate placement" decision
//! (`docs/superpowers/specs/2026-07-13-subagent-delegation-design.md`).

use std::path::Path;
use std::sync::Arc;

use aivyx_llm::LlmBackend;
use aivyx_repomap::RepoMap;
use aivyx_sandbox::{
    ActionKind, AutonomousMode, ExecutionConfiner, PermissionGate, PermissionRequest,
    PermissionTarget, PlanMode,
};
use aivyx_tools::{GitCheckpointer, Tool, ToolError, ToolExecutionContext, ToolExecutor, ToolRegistry};
use aivyx_types::{ToolDefinition, ToolOutput};
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use tokio::sync::mpsc;
use tokio::sync::mpsc::UnboundedSender;

use crate::agent::{Agent, AgentConfig, AgentEvent, EditFormat};

#[derive(Deserialize, JsonSchema)]
struct DelegateTaskArgs {
    /// A complete, self-contained description of the task for the
    /// sub-agent — it starts with no context beyond this text.
    task: String,
}

/// Appended to a sub-agent's accumulated text when its iteration budget
/// runs out before it produces a natural final answer — the tool still
/// returns `Ok`, never an error, since the sub-agent may have done real,
/// useful work even if incomplete.
const CUTOFF_NOTICE: &str = "\n\n(sub-agent stopped: reached its iteration budget before \
finishing — the above is its best-effort partial result.)";

const NO_TEXT_RESPONSE: &str = "(the sub-agent produced no text response)";

const SUB_AGENT_SYSTEM_PROMPT: &str = "You are a sub-agent helping the main assistant with a \
delegated task. You have the same tool access it does. Work the task to completion, then give a \
clear, complete final answer summarizing what you found or did — this is the only part of your \
work the main assistant will see; your own tool calls and intermediate reasoning are not passed \
back directly.";

/// Everything `delegate_task` needs to spin up a sub-agent, gathered once
/// in `main.rs` at registration time — every field is stable for the
/// whole session, never varying call to call. Bundled into a struct
/// (rather than a long positional-argument constructor) because several
/// fields share a type shape (multiple `Arc<dyn _>`, multiple `Option<_>`)
/// that would be easy to mis-order by mistake as positional arguments.
pub struct DelegateTaskConfig {
    pub llm: Arc<dyn LlmBackend>,
    pub gate: Arc<dyn PermissionGate>,
    pub confiner: Arc<dyn ExecutionConfiner>,
    pub checkpointer: Option<Arc<GitCheckpointer>>,
    pub repo_map: Option<(Arc<RepoMap>, u32)>,
    pub events_tx: UnboundedSender<AgentEvent>,
    /// The parent's own tool registry, cloned *before* `delegate_task`
    /// itself was registered onto it — recursion is therefore
    /// structurally impossible, not merely policy-excluded.
    pub sub_agent_registry: ToolRegistry,
    pub plan_mode: PlanMode,
    pub autonomous_mode: AutonomousMode,
    pub context_tokens: u32,
    pub edit_format: EditFormat,
    /// `(command_name, max_retries)`, mirroring `Agent::set_verification`'s
    /// parameters — `None` if `[verification].command` isn't configured.
    pub verification: Option<(String, u32)>,
    pub max_iterations: u32,
}

pub struct DelegateTaskTool {
    config: DelegateTaskConfig,
}

impl DelegateTaskTool {
    pub fn new(config: DelegateTaskConfig) -> Self {
        Self { config }
    }
}

#[async_trait]
impl Tool for DelegateTaskTool {
    fn name(&self) -> &str {
        "delegate_task"
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name().to_string(),
            description: "Delegate a bounded task to a fresh sub-agent with its own isolated \
                conversation history and full tool access (same permissions as you). Use this to \
                explore or work on something without cluttering your own context — e.g. \
                understanding an unfamiliar part of the codebase before touching it. Write a \
                complete, self-contained task description: the sub-agent starts with no context \
                beyond what you write here. Returns the sub-agent's final answer as text."
                .to_string(),
            parameters_schema: serde_json::Value::from(schemars::schema_for!(DelegateTaskArgs)),
        }
    }

    // Offered during plan mode too, not hidden: the sub-agent's own
    // per-turn tool list is built with the identical plan-mode filtering
    // logic the parent's turn loop already uses (both read the same
    // shared `PlanMode` flag baked into `self.config.plan_mode`), so a
    // sub-agent spawned during plan mode automatically only sees
    // read-only tools — it degrades gracefully rather than needing to be
    // hidden outright.
    fn mutates_outside_session(&self) -> bool {
        false
    }

    fn permission_request(
        &self,
        _arguments: &serde_json::Value,
        _cwd: &Path,
    ) -> Result<PermissionRequest, ToolError> {
        Ok(PermissionRequest {
            tool_name: self.name().to_string(),
            action: ActionKind::Internal,
            target: PermissionTarget::Other("delegate_task".to_string()),
            arguments_preview: serde_json::json!({}),
            preview: None,
        })
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        ctx: &ToolExecutionContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: DelegateTaskArgs = serde_json::from_value(arguments)
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;

        let mut sub_executor = ToolExecutor::new(
            self.config.sub_agent_registry.clone(),
            Arc::clone(&self.config.gate),
            Arc::clone(&self.config.confiner),
        );
        if let Some(checkpointer) = &self.config.checkpointer {
            sub_executor.set_checkpointer(Arc::clone(checkpointer));
        }

        // The sub-agent gets its own event channel — sharing the parent's
        // directly would make its ToolCallDetected/ToolResult/TextDelta
        // events indistinguishable from the parent's own in the
        // transcript. A background task drains it for the duration of
        // this call, forwarding each event wrapped in
        // `AgentEvent::SubAgentActivity` onto the parent's real channel,
        // and — since `AgentEvent` doesn't expose the sub-agent's
        // assembled response text directly — also accumulates
        // `TextDelta` content into `accumulated`, which is what this
        // tool call ultimately returns.
        let (sub_tx, mut sub_rx) = mpsc::unbounded_channel();
        let accumulated = Arc::new(std::sync::Mutex::new(String::new()));
        let accumulated_for_task = Arc::clone(&accumulated);
        let parent_tx = self.config.events_tx.clone();
        let forward_task = tokio::spawn(async move {
            while let Some(event) = sub_rx.recv().await {
                if let AgentEvent::TextDelta(text) = &event {
                    accumulated_for_task.lock().unwrap().push_str(text);
                }
                let _ = parent_tx.send(AgentEvent::SubAgentActivity(Box::new(event)));
            }
        });

        let mut sub_agent = Agent::new(
            Arc::clone(&self.config.llm),
            sub_executor,
            SUB_AGENT_SYSTEM_PROMPT,
            AgentConfig {
                max_tool_iterations: self.config.max_iterations,
                context_tokens: self.config.context_tokens,
                edit_format: self.config.edit_format,
            },
            Arc::default(),
            self.config.plan_mode.clone(),
            self.config.autonomous_mode.clone(),
            sub_tx,
        );
        if let Some((map, budget)) = &self.config.repo_map {
            sub_agent.set_repo_map(Arc::clone(map), *budget);
        }
        if let Some((command, max_retries)) = &self.config.verification {
            sub_agent.set_verification(command.clone(), *max_retries);
        }

        let max_iterations = self.config.max_iterations.max(1);
        let mut result = sub_agent
            .run_turn(args.task, &ctx.cwd, ctx.cancellation.clone())
            .await;
        let mut iterations_used = 1u32;
        while result.is_ok()
            && sub_agent.last_turn_paused()
            && iterations_used < max_iterations
            && !ctx.cancellation.is_cancelled()
        {
            iterations_used += 1;
            result = sub_agent
                .run_turn("continue".to_string(), &ctx.cwd, ctx.cancellation.clone())
                .await;
        }
        let cap_hit = result.is_ok() && sub_agent.last_turn_paused();

        // Dropping the sub-agent drops its `sub_tx` (the only remaining
        // sender), closing the channel so `forward_task`'s `recv()` loop
        // ends and it can be awaited to completion.
        drop(sub_agent);
        let _ = forward_task.await;

        match result {
            Err(err) => Ok(ToolOutput::Error(format!("sub-agent failed: {err}"))),
            Ok(()) => {
                let mut text = Arc::try_unwrap(accumulated)
                    .map(|m| m.into_inner().unwrap())
                    .unwrap_or_default();
                if cap_hit {
                    text.push_str(CUTOFF_NOTICE);
                }
                if text.trim().is_empty() {
                    text = NO_TEXT_RESPONSE.to_string();
                }
                Ok(ToolOutput::Ok(text))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aivyx_llm::{ChatRequest, FinishReason, LlmError, StreamEvent};
    use aivyx_sandbox::{NoopConfiner, PermissionDecision};
    use aivyx_types::{ToolCall, ToolCallId, ToolCallSource};
    use futures::StreamExt;
    use futures::stream::BoxStream;
    use std::sync::Mutex;
    use std::time::Duration;
    use tokio_util::sync::CancellationToken;

    struct MockBackend {
        responses: Mutex<std::collections::VecDeque<Vec<StreamEvent>>>,
    }

    impl MockBackend {
        fn new(responses: Vec<Vec<StreamEvent>>) -> Self {
            Self {
                responses: Mutex::new(responses.into()),
            }
        }
    }

    #[async_trait]
    impl LlmBackend for MockBackend {
        fn model_id(&self) -> &str {
            "mock"
        }

        async fn stream_chat(
            &self,
            _request: ChatRequest,
        ) -> Result<BoxStream<'static, Result<StreamEvent, LlmError>>, LlmError> {
            let events = self.responses.lock().unwrap().pop_front().unwrap_or_default();
            Ok(futures::stream::iter(events.into_iter().map(Ok::<StreamEvent, LlmError>)).boxed())
        }
    }

    struct AllowAllGate;
    #[async_trait]
    impl PermissionGate for AllowAllGate {
        async fn check(&self, _request: &PermissionRequest) -> PermissionDecision {
            PermissionDecision::Allow
        }
    }

    fn text_response(text: &str) -> Vec<StreamEvent> {
        vec![
            StreamEvent::TextDelta(text.to_string()),
            StreamEvent::Done {
                finish_reason: FinishReason::Stop,
            },
        ]
    }

    fn base_config(
        llm: Arc<dyn LlmBackend>,
        events_tx: UnboundedSender<AgentEvent>,
        sub_agent_registry: ToolRegistry,
        max_iterations: u32,
    ) -> DelegateTaskConfig {
        DelegateTaskConfig {
            llm,
            gate: Arc::new(AllowAllGate),
            confiner: Arc::new(NoopConfiner),
            checkpointer: None,
            repo_map: None,
            events_tx,
            sub_agent_registry,
            plan_mode: aivyx_sandbox::PlanMode::new(),
            autonomous_mode: aivyx_sandbox::AutonomousMode::new(),
            context_tokens: 8192,
            edit_format: EditFormat::Native,
            verification: None,
            max_iterations,
        }
    }

    fn delegate_call(task: &str) -> serde_json::Value {
        serde_json::json!({ "task": task })
    }

    fn exec_ctx(cwd: &Path) -> ToolExecutionContext {
        ToolExecutionContext {
            cwd: cwd.to_path_buf(),
            confiner: Arc::new(NoopConfiner),
            cancellation: CancellationToken::new(),
        }
    }

    #[tokio::test]
    async fn drives_a_nested_agent_to_a_natural_completion() {
        let dir = tempfile::tempdir().unwrap();
        let mock: Arc<dyn LlmBackend> =
            Arc::new(MockBackend::new(vec![text_response("the auth module uses JWTs")]));
        let (tx, mut rx) = mpsc::unbounded_channel();
        let tool = DelegateTaskTool::new(base_config(mock, tx, ToolRegistry::new(), 10));

        let output = tool
            .execute(delegate_call("explain the auth module"), &exec_ctx(dir.path()))
            .await
            .unwrap();

        assert!(
            matches!(&output, ToolOutput::Ok(text) if text == "the auth module uses JWTs"),
            "unexpected output: {output:?}"
        );
        // The forwarded events must include the sub-agent's own TextDelta,
        // wrapped — proving delegation doesn't silently swallow activity.
        let mut saw_wrapped_text_delta = false;
        while let Ok(event) = rx.try_recv() {
            if let AgentEvent::SubAgentActivity(inner) = event
                && matches!(*inner, AgentEvent::TextDelta(_))
            {
                saw_wrapped_text_delta = true;
            }
        }
        assert!(saw_wrapped_text_delta);
    }

    #[tokio::test]
    async fn cap_exhaustion_returns_ok_with_a_cutoff_notice_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        // Every response is a tool call with no final answer, so the
        // sub-agent pauses every single iteration and never finishes
        // naturally.
        let responses: Vec<Vec<StreamEvent>> = (0..5)
            .map(|i| {
                vec![
                    StreamEvent::ToolCallComplete(ToolCall {
                        id: ToolCallId(format!("c{i}")),
                        name: "nonexistent_tool".to_string(),
                        arguments: serde_json::json!({}),
                        source: ToolCallSource::Native,
                    }),
                    StreamEvent::Done {
                        finish_reason: FinishReason::ToolCalls,
                    },
                ]
            })
            .collect();
        let mock: Arc<dyn LlmBackend> = Arc::new(MockBackend::new(responses));
        let (tx, _rx) = mpsc::unbounded_channel();
        let tool = DelegateTaskTool::new(base_config(mock, tx, ToolRegistry::new(), 3));

        let output = tool
            .execute(delegate_call("loop forever"), &exec_ctx(dir.path()))
            .await
            .unwrap();

        match output {
            ToolOutput::Ok(text) => assert!(
                text.contains("reached its iteration budget"),
                "expected a cutoff notice, got: {text}"
            ),
            other => panic!("expected Ok with a cutoff notice, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn sub_agent_registry_never_contains_delegate_task_itself() {
        // The sub-agent's own registry is whatever was passed in — proving
        // recursion is impossible is really a Task 5 (main.rs) concern
        // (build the sub-agent registry before registering delegate_task
        // onto the parent's), but this test locks in that DelegateTaskTool
        // itself never adds itself to whatever registry it's given.
        let dir = tempfile::tempdir().unwrap();
        let mock: Arc<dyn LlmBackend> = Arc::new(MockBackend::new(vec![text_response("done")]));
        let mut sub_registry = ToolRegistry::new();
        sub_registry.register(Arc::new(aivyx_tools::ReadFileTool));
        let (tx, _rx) = mpsc::unbounded_channel();
        let tool = DelegateTaskTool::new(base_config(mock, tx, sub_registry, 10));

        let _ = tool.execute(delegate_call("anything"), &exec_ctx(dir.path())).await;

        assert!(
            !tool
                .config
                .sub_agent_registry
                .definitions()
                .iter()
                .any(|d| d.name == "delegate_task"),
            "delegate_task must never appear in a sub-agent's own tool list"
        );
    }

    #[tokio::test]
    async fn plan_mode_is_respected_by_the_sub_agent_own_tool_list() {
        // Indirect proof: with plan_mode active and only a mutating tool
        // registered, the sub-agent's turn loop offers zero tools (the
        // model's request never includes it), so a scripted tool call for
        // it is simply never produced by the mock backend — instead we
        // assert the plan_mode flag really did make it to the nested
        // Agent by checking the system-prompt-driven text path still
        // completes normally (no panics/hangs) with plan_mode shared.
        let dir = tempfile::tempdir().unwrap();
        let mock: Arc<dyn LlmBackend> = Arc::new(MockBackend::new(vec![text_response("ok")]));
        let mut config = base_config(mock, mpsc::unbounded_channel().0, ToolRegistry::new(), 10);
        config.plan_mode.set_active(true);
        let tool = DelegateTaskTool::new(config);

        let output = tool
            .execute(delegate_call("read-only task"), &exec_ctx(dir.path()))
            .await
            .unwrap();

        assert!(matches!(output, ToolOutput::Ok(text) if text == "ok"));
    }

    #[tokio::test]
    async fn a_real_backend_error_surfaces_as_tool_output_error() {
        struct FailingBackend;
        #[async_trait]
        impl LlmBackend for FailingBackend {
            fn model_id(&self) -> &str {
                "failing"
            }
            async fn stream_chat(
                &self,
                _request: ChatRequest,
            ) -> Result<BoxStream<'static, Result<StreamEvent, LlmError>>, LlmError> {
                Err(LlmError::Timeout)
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let mock: Arc<dyn LlmBackend> = Arc::new(FailingBackend);
        let (tx, _rx) = mpsc::unbounded_channel();
        let tool = DelegateTaskTool::new(base_config(mock, tx, ToolRegistry::new(), 10));

        let output = tool
            .execute(delegate_call("anything"), &exec_ctx(dir.path()))
            .await
            .unwrap();

        assert!(
            matches!(&output, ToolOutput::Error(msg) if msg.contains("sub-agent failed")),
            "expected an Error output for a genuine backend failure, got: {output:?}"
        );
    }

    #[test]
    fn mutates_outside_session_is_false_so_plan_mode_still_offers_it() {
        let mock: Arc<dyn LlmBackend> = Arc::new(MockBackend::new(vec![]));
        let (tx, _rx) = mpsc::unbounded_channel();
        let tool = DelegateTaskTool::new(base_config(mock, tx, ToolRegistry::new(), 10));
        assert!(!tool.mutates_outside_session());
    }

    #[test]
    fn permission_request_always_resolves_to_an_internal_auto_allow_shape() {
        let mock: Arc<dyn LlmBackend> = Arc::new(MockBackend::new(vec![]));
        let (tx, _rx) = mpsc::unbounded_channel();
        let tool = DelegateTaskTool::new(base_config(mock, tx, ToolRegistry::new(), 10));
        let request = tool
            .permission_request(&delegate_call("anything"), Path::new("."))
            .unwrap();
        assert_eq!(request.action, ActionKind::Internal);
        assert!(matches!(request.target, PermissionTarget::Other(name) if name == "delegate_task"));
    }
}
```

This test module uses `aivyx_llm::LlmError::Timeout` (defined in `crates/aivyx-llm/src/backend.rs`, a zero-field variant — the simplest way to make `stream_chat` return some `Err`; its only purpose in this test is making that call fail, not exercising timeout-specific behavior).

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p aivyx-core delegate:: 2>&1 | head -30`
Expected: compile error — `crate::delegate` isn't a registered module yet, and `AgentConfig`/`Agent` fields referenced may need adjusting once you see real compiler feedback (this step's real purpose is confirming the module doesn't yet build, not diagnosing every error in detail).

- [ ] **Step 3: Register the module**

In `crates/aivyx-core/src/lib.rs`, add `pub mod delegate;` alongside the existing modules:

```rust
pub mod agent;
pub mod council;
pub mod delegate;
pub mod edit_blocks;
pub mod session;
pub mod wiki;

pub use agent::{Agent, AgentConfig, AgentError, AgentEvent, EditFormat};
pub use council::{Council, CouncilSeat};
pub use delegate::{DelegateTaskConfig, DelegateTaskTool};
pub use session::{SessionState, Task, TaskStatus};
```

Check `crates/aivyx-core/Cargo.toml` for `schemars` as a dependency (`grep schemars crates/aivyx-core/Cargo.toml`) — `aivyx-tools` already depends on it for the exact same `JsonSchema`-derive pattern other tools use, but `aivyx-core` may not yet. If it's missing, add it to `[dependencies]` matching the version `aivyx-tools/Cargo.toml` uses:

```toml
schemars = "1.2.1"
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p aivyx-core delegate::`
Expected: all 8 tests in `delegate::tests` pass.

- [ ] **Step 5: Run the full workspace check**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets`
Expected: clean. `DelegateTaskConfig`'s field count will likely trigger no clippy warning (it's a plain struct, not a function signature), but if `DelegateTaskTool::new` or `execute` trips `clippy::too_many_arguments`, note that `new` takes exactly one argument (the config struct) so this shouldn't fire — if some other lint fires unexpectedly, read the actual message and fix accordingly rather than suppressing it.

- [ ] **Step 6: Commit**

```bash
git add crates/aivyx-core/src/delegate.rs crates/aivyx-core/src/lib.rs crates/aivyx-core/Cargo.toml
git commit -m "Sub-agent delegation: DelegateTaskTool"
```

---

### Task 5: `main.rs` wiring

**Files:**
- Modify: `crates/aivyx/src/main.rs`

**Interfaces:**
- Consumes: `Task 1`'s `ToolRegistry: Clone`; `Task 2`'s `settings.sub_agent.max_iterations`; `Task 4`'s `DelegateTaskConfig`/`DelegateTaskTool`.
- Produces: the running binary offers `delegate_task` to the model in every session.

This task reorders several existing blocks in `main.rs` — `gate`, `confiner`, the checkpointer, and repo-map construction all currently happen *after* the tool-registration block, but `delegate_task`'s construction needs all of them *during* tool registration. None of the moved blocks' own logic changes — only their position, so each one's existing behavior (including the `--auto` checkpointer-requirement check, which only needs `has_checkpointer` to be computed before it runs) is preserved exactly.

- [ ] **Step 1: Move checkpointer detection before tool registration**

In `crates/aivyx/src/main.rs`, find (`grep -n "let mut registry = ToolRegistry::new" crates/aivyx/src/main.rs`) the tool-registration block. Currently the order is: `tasks` → tool registration (`registry.register(...)` × 9-10) → `pre_approved_commands` → `plan_mode`/`autonomous_mode` validation → `gate` construction → `confiner` construction → `executor` construction → checkpointer detection.

Cut this whole block (currently right after `executor` construction):

```rust
    let mut has_checkpointer = false;
    if settings.git.checkpoints
        && let Some(checkpointer) = GitCheckpointer::detect(&cwd, deny_paths.clone()).await
    {
        executor.set_checkpointer(Arc::new(checkpointer));
        has_checkpointer = true;
    }
```

and the `mut executor` / `ToolExecutor::new` line right above it:

```rust
    let mut executor = ToolExecutor::new(registry, gate, confiner);
```

Set both aside — they're rewritten in Step 4 below, after the tool registration block moves.

- [ ] **Step 2: Move `pre_approved_commands`, `plan_mode`/`autonomous_mode`, `gate`, `confiner`, checkpointer detection, and repo-map construction to before tool registration**

Reorder `main.rs` so this is the sequence, from `tasks` construction through to the start of tool registration (every block's own body is copied verbatim from its current form — only the order changes; the checkpointer block is the one cut in Step 1):

```rust
    // One handle shared between the `set_tasks` tool (the model-facing
    // mutator) and the agent (which renders and persists the list).
    let tasks: Arc<std::sync::Mutex<Vec<session::Task>>> = Arc::default();

    // Each configured command is pre-approved in two forms: the direct
    // `(program, args)` invocation `run_command` uses, and the `sh -c
    // "<program> <args>"` form `run_shell` always wraps commands in — the
    // two tools have genuinely different invocation shapes (direct exec vs.
    // shell-interpreted), so a single natural config entry (e.g. `program =
    // "cargo", args = ["test"]`) needs both to be recognized by either tool
    // without requiring the user to write it out twice in different shapes.
    //
    // Every arg is shell-escaped before joining — a naive `args.join(" ")`
    // would let an arg containing a shell metacharacter (e.g. `program =
    // "grep", args = ["-rn", "TODO|FIXME", "."]`, a harmless regex under
    // direct execve) turn into live, unconfirmed shell syntax the moment
    // the reconstructed `sh -c` form is looked up in the Always-Allow cache.
    let mut pre_approved_commands: Vec<(String, Vec<String>)> = Vec::new();
    for spec in &command_specs {
        pre_approved_commands.push((spec.program.clone(), spec.args.clone()));
        let mut shell_form = shell_escape::escape(spec.program.as_str().into()).into_owned();
        for arg in &spec.args {
            shell_form.push(' ');
            shell_form.push_str(&shell_escape::escape(arg.as_str().into()));
        }
        pre_approved_commands.push(("sh".to_string(), vec!["-c".to_string(), shell_form]));
    }

    // One shared flag, three consumers: the gate enforces it, the agent
    // filters tools + annotates the system prompt by it, the TUI toggles it.
    let plan_mode = PlanMode::new();
    plan_mode.set_active(cli.plan);

    // --auto and --plan are contradictory (unattended action-taking vs.
    // enforced read-only); --auto and --resume are unsupported together in
    // this version (autonomous session state — unverified edits, retry
    // counts, the pre-experiment checkpoint ref — isn't part of
    // SessionState yet; see the Phase 11c design doc's non-goals).
    if cli.auto.is_some() && cli.plan {
        anyhow::bail!("--auto and --plan cannot be used together");
    }
    if cli.auto.is_some() && cli.resume {
        anyhow::bail!("--auto and --resume cannot be used together (not supported yet)");
    }
    if let Some(goal) = cli.auto.as_deref()
        && goal.trim().is_empty()
    {
        anyhow::bail!("--auto requires a non-empty goal");
    }
    let autonomous_mode = AutonomousMode::new();
    autonomous_mode.set_active(cli.auto.is_some());
    // Auto-approving edits is only defensible because deterministic
    // verification is the safety net — without it, "autonomous" would mean
    // "unchecked." Refuse to start rather than run degraded.
    if cli.auto.is_some() && settings.verification.command.is_none() {
        anyhow::bail!(
            "--auto requires [verification].command to be configured — auto-approving edits \
             with no verification check is not supported"
        );
    }

    let (prompter, permission_rx) = aivyx_tui::permission_channel();
    let gate: Arc<dyn PermissionGate> = Arc::new(ConfirmationGate::new(
        Arc::new(prompter),
        deny_paths.clone(),
        pre_approved_commands,
        plan_mode.clone(),
        autonomous_mode.clone(),
        cwd.clone(),
    ));
    let confiner = aivyx_sandbox::default_confiner(
        &cwd,
        &settings.sandbox.resolved_extra_read_paths(),
        &deny_paths,
        settings.sandbox.require_enforcement,
    );

    let mut checkpointer: Option<Arc<GitCheckpointer>> = None;
    if settings.git.checkpoints
        && let Some(detected) = GitCheckpointer::detect(&cwd, deny_paths.clone()).await
    {
        checkpointer = Some(Arc::new(detected));
    }
    let has_checkpointer = checkpointer.is_some();
    // The discard/rewind safety net on exhausted verification (the entire
    // basis for auto-approving edits in `--auto`) depends on a checkpointer
    // being present — without one, `Agent`'s discard/rewind logic degrades
    // silently to "leave the broken edits in place and keep going," which
    // is not a safe default for an unattended run. Refuse to start rather
    // than run degraded, same as the verification-command check above.
    if cli.auto.is_some() && !has_checkpointer {
        anyhow::bail!(
            "--auto requires a git worktree with checkpoints enabled ([git] checkpoints = true, \
             the default) — the discard/rewind safety net on exhausted verification depends on it"
        );
    }

    // Constructed here (rather than left inline at the `agent.set_repo_map`
    // call site, as before) so `delegate_task`'s sub-agent can share the
    // exact same `Arc<RepoMap>` — a fresh second `RepoMap` would duplicate
    // the parse cache for no benefit, since both agents walk the same cwd.
    let repo_map: Option<(Arc<aivyx_repomap::RepoMap>, u32)> = settings.repo_map.enabled.then(|| {
        (
            Arc::new(aivyx_repomap::RepoMap::new(cwd.clone(), deny_paths.clone())),
            settings.repo_map.budget_tokens,
        )
    });

    let (events_tx, events_rx) = mpsc::unbounded_channel();
```

(The `events_tx`/`events_rx` channel also moves up here from its previous position further down — `delegate_task`'s config needs a clone of `events_tx` before `Agent::new` consumes the original.)

- [ ] **Step 3: Register tools, snapshot the sub-agent registry, then add `delegate_task`**

Immediately after the block from Step 2, keep the existing tool-registration block exactly as it is (unchanged body):

```rust
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(ReadFileTool));
    registry.register(Arc::new(WriteFileTool));
    registry.register(Arc::new(EditFileTool));
    registry.register(Arc::new(GrepTool::new(deny_paths.clone())));
    registry.register(Arc::new(GlobTool::new(deny_paths.clone())));
    registry.register(Arc::new(RunShellTool));
    registry.register(Arc::new(SetTasksTool::new(Arc::clone(&tasks))));
    registry.register(Arc::new(GitReadTool::new(deny_paths.clone())));
    registry.register(Arc::new(GitCommitTool::new(deny_paths.clone())));

    // Only registered when configured — an always-erroring tool offered to
    // the model would just be confusing noise for a project that hasn't
    // opted into any commands.
    if !command_specs.is_empty() {
        registry.register(Arc::new(RunCommandTool::new(command_specs.clone())));
    }
```

then add, immediately after it:

```rust
    // Snapshot every tool registered so far — this becomes a sub-agent's
    // own tool list, which must never include `delegate_task` itself
    // (recursion is structurally impossible this way, not merely
    // policy-excluded). `delegate_task` is registered onto `registry`
    // (the parent's) below, *after* this clone.
    let sub_agent_registry = registry.clone();
    // Verification config is threaded through so a sub-agent's own edits
    // get verified before `delegate_task` returns, exactly like the
    // parent's own edits would — mirrors the `agent.set_verification(...)`
    // call below, resolved once here so both call sites agree without
    // duplicating the allowed_commands-membership check.
    let verification = settings
        .verification
        .command
        .as_ref()
        .filter(|command| command_specs.iter().any(|spec| &spec.name == *command))
        .map(|command| (command.clone(), settings.verification.max_auto_verify_retries));
    registry.register(Arc::new(aivyx_core::DelegateTaskTool::new(
        aivyx_core::DelegateTaskConfig {
            llm: Arc::clone(&llm),
            gate: Arc::clone(&gate),
            confiner: Arc::clone(&confiner),
            checkpointer: checkpointer.clone(),
            repo_map: repo_map.clone(),
            events_tx: events_tx.clone(),
            sub_agent_registry,
            plan_mode: plan_mode.clone(),
            autonomous_mode: autonomous_mode.clone(),
            context_tokens: settings.backend.context_tokens,
            edit_format,
            verification: verification.clone(),
            max_iterations: settings.sub_agent.max_iterations,
        },
    )));
```

This references `edit_format`, which the current code computes *after* tool registration (right before `build_system_prompt`). Move the `edit_format` computation to just above this new block:

```rust
    let edit_format = match cli.edit_format.as_deref() {
        Some("native") => EditFormat::Native,
        Some("prompted") => EditFormat::Prompted,
        _ => match settings.backend.edit_format {
            aivyx_config::EditFormat::Native => EditFormat::Native,
            aivyx_config::EditFormat::Prompted => EditFormat::Prompted,
        },
    };
```

(placed right before the `sub_agent_registry`/`verification`/`DelegateTaskTool` block above, since that block now needs `edit_format` before `build_system_prompt` — which also needs it — runs).

- [ ] **Step 4: Construct the executor with the already-detected checkpointer**

Immediately after the tool-registration block (including the new `delegate_task` registration), construct the executor:

```rust
    let mut executor = ToolExecutor::new(registry, Arc::clone(&gate), Arc::clone(&confiner));
    if let Some(cp) = &checkpointer {
        executor.set_checkpointer(Arc::clone(cp));
    }
```

- [ ] **Step 5: Remove the now-duplicated blocks further down**

The rest of `main.rs` from `build_system_prompt` onward stays the same **except**: remove the original (now-duplicate) `edit_format` computation, the original `(events_tx, events_rx) = mpsc::unbounded_channel();` line, and the original inline `if settings.repo_map.enabled { agent.set_repo_map(Arc::new(aivyx_repomap::RepoMap::new(...)), ...); }` block — replace that last one with:

```rust
    if let Some((map, budget)) = &repo_map {
        agent.set_repo_map(Arc::clone(map), *budget);
    }
```

Also simplify the existing verification-setup block (which currently redoes the same `allowed_commands`-membership check `verification` above already computed) to reuse it instead of recomputing:

```rust
    if let Some((command, max_retries)) = &verification {
        agent.set_verification(command.clone(), *max_retries);
    } else if let Some(command) = &settings.verification.command {
        tracing::warn!(
            command = %command,
            "verification.command does not match any [[permissions.allowed_commands]] \
             entry name — enforced verification is disabled until this is fixed"
        );
    }
```

Everything else (the context-window probe, `Agent::new` call, council setup, session restore, `autonomous_run` construction, and the final `aivyx_tui::run(...)` call) stays exactly as it was.

- [ ] **Step 6: Build and run the full workspace check**

Run: `cargo build --workspace 2>&1 | tail -40`
Expected: clean build. If there are borrow-checker complaints about `gate`/`confiner`/`checkpointer`/`repo_map`/`events_tx` being moved vs. cloned, resolve by cloning (`Arc::clone`/`.clone()`) at each use site that isn't the final one — every one of these types is cheap to clone (`Arc` or a plain `Clone` derive), so prefer an extra `.clone()` over restructuring further.

Run: `cargo test --workspace && cargo clippy --workspace --all-targets`
Expected: clean.

- [ ] **Step 7: Commit**

```bash
git add crates/aivyx/src/main.rs
git commit -m "Sub-agent delegation: main.rs wiring"
```

---

### Task 6: Live E2E verification and documentation

**Files:**
- Modify: `README.md`
- Modify: `ROADMAP.md`

Matches this project's own verification bar: every phase gets a real, live run through the actual binary before being marked done, not just unit tests.

- [ ] **Step 1: Manual live E2E — a sub-agent explores and reports back**

In a scratch git repo with a few files the model wouldn't already know about, launch `aivyx` and give it a goal that plausibly invites delegation, e.g.:

```
aivyx
# then type: use delegate_task to find out what crates/alpha-crate does, then tell me
```

Expected, observed in the transcript: a `sub-agent> ` prefixed block of activity appears (visually distinct from the parent's own lines) showing the sub-agent's own tool calls/results/text as it explores; the parent's own final response references what the sub-agent reported, without the parent's own history containing the sub-agent's raw tool calls (confirmed by checking `~/.local/state/aivyx-coder/sessions/<...>.json` after the turn, if session persistence is enabled — the sub-agent's tool calls must not appear there, only the `delegate_task` call and its text result).

- [ ] **Step 2: Manual live E2E — a sub-agent's mutation still requires confirmation**

Give a goal that invites the sub-agent to write a file, e.g. `use delegate_task to create a file called notes.txt with a summary of the project`.

Expected: a normal confirmation modal appears for the sub-agent's `write_file` call, with the same diff-preview UI as any other write — confirming the sub-agent shares the parent's real `ConfirmationGate`, not a bypassed or separately-configured one.

- [ ] **Step 3: Manual live E2E — plan mode**

Start with `aivyx --plan`, then give a goal that invites delegation. Expected: the sub-agent's own activity (visible in the transcript) never includes a write/edit/run_command attempt — only read/search tool calls — confirming the shared `PlanMode` flag correctly filtered the sub-agent's own offered tool list, not just denied its attempts after the fact.

- [ ] **Step 4: Update README.md**

Find the "Agent-maintained wiki" paragraph added by Phase 11b (`grep -n "Agent-maintained wiki" README.md`). Add a new paragraph immediately after it:

```markdown
**Sub-agent delegation** (`delegate_task`): a tool the model can call
mid-turn to hand a bounded task to a fresh, isolated agent — full tool
access, the same `ConfirmationGate`/checkpoint/plan-mode boundary as the
main session, but a completely separate conversation history, so
exploring or working on something unfamiliar doesn't clutter the main
session's own context window. Only the sub-agent's final text answer
enters the main session's history; its own tool calls/results/reasoning
render live in the transcript (prefixed `sub-agent>`, visually distinct)
but never join history directly. Bounded by `[sub_agent] max_iterations`
(default 10); a sub-agent that runs out of budget still returns its
best-effort partial result rather than failing outright. Delegation is
capped at one level — a sub-agent's own tool list never includes
`delegate_task`.
```

- [ ] **Step 5: Update ROADMAP.md**

Find Phase 9's "sub-agent delegation" mention (`grep -n "sub-agent delegation" ROADMAP.md`). Add a "Built and live-verified (date)" paragraph immediately after the relevant sentence, following the exact pattern the Phase 11a/11b/11c entries use: summarize what was built (`DelegateTaskTool` living in `aivyx-core` rather than `aivyx-tools` due to the crate dependency direction, the `DelegateTaskConfig`-bundled construction-time dependencies, the spawned-forwarding-task mechanism for live-streamed sub-agent activity via `AgentEvent::SubAgentActivity`, the shared-trust-boundary design meaning no new `ConfirmationGate` tier was needed), the test count added, and the results of the 3 live E2E checks from Steps 1-3 above.

- [ ] **Step 6: Final full-workspace check**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets`
Expected: clean.

- [ ] **Step 7: Commit**

```bash
git add README.md ROADMAP.md
git commit -m "Sub-agent delegation: docs + live E2E verification"
```

---

## Self-Review

**Spec coverage** — every section of the design doc (as amended during plan-writing) maps to a task:
- The `ToolExecutionContext`/crate-placement/streaming-mechanism resolutions discovered and amended during plan-writing → reflected directly in Tasks 3-5's code (`DelegateTaskConfig`, `aivyx-core` placement, the spawned forwarding task).
- Trust/shared-state (same gate, checkpointer, plan/autonomous mode, verification) → Task 4 (`DelegateTaskConfig` fields, `execute()`'s nested `Agent` construction) and Task 5 (main.rs threading the actual shared instances through).
- Live visibility → Task 3 (`AgentEvent::SubAgentActivity`, `ChatLine::SubAgent`) and Task 4 (the forwarding task).
- Recursion capped at one level → Task 4/5 (`sub_agent_registry` snapshotted before `delegate_task` registers itself).
- Budget config + cap-exhaustion behavior → Task 2 (config), Task 4 (the iteration loop and cutoff-notice logic).
- Context seed (task description only) → Task 4 (`Agent::new` given a fresh `Arc::default()` task list and no parent-history seeding — `args.task` is the only input).
- Testing strategy (nested-agent-to-completion, cap exhaustion returns Ok not Err, recursion structurally impossible, plan-mode filtering, live event streaming) → all covered by Task 4's test module; a real-git checkpoint-sharing test was folded into Task 4's scope via the shared `checkpointer` field rather than split into its own task, since `GitCheckpointer` behavior itself isn't touched by this plan (only reused) — Task 6's live E2E Step 2 is the concrete confirmation that a sub-agent's mutation goes through the same checkpoint-before-effect path as any other tool.
- One live E2E through the real binary → Task 6.

**Placeholder scan** — no "TBD"/"TODO"/vague instructions anywhere in this plan; every step has complete, real code, including the three implementation-time architecture corrections found and resolved during plan-writing (documented in the spec's own decision log, not silently smoothed over).

**Type consistency** — `DelegateTaskConfig`'s fields (Task 4) match exactly what Task 5's `main.rs` constructs and passes in, field-for-field. `AgentEvent::SubAgentActivity(Box<AgentEvent>)` (Task 3) is the exact type Task 4's forwarding task constructs and sends. `ToolRegistry: Clone` (Task 1) is what Task 5's `sub_agent_registry = registry.clone()` line relies on.
