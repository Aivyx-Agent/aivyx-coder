# Nonagon Team — Phase 4 (Specialist Sessions) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give the lead three new tools — `spawn_specialist`, `query_specialist`, `close_specialist` — for a resumable, multi-exchange conversation with a team specialist, alongside (not replacing) the existing one-shot `delegate_to_specialist`.

**Architecture:** A new `SpecialistSessionPool` (keyed `HashMap<String, ParkedSpecialistSession>` behind a `std::sync::Mutex`, `Arc`-shared across three new `Tool` impls in a new `crates/aivyx-core/src/specialist_sessions.rs`) parks a live specialist `Agent` between tool calls instead of dropping it after one exchange — `Agent::run_turn` is already safely callable multiple times on the same instance (confirmed by reading `delegate_to_specialist.rs`'s existing "continue" loop), so no new capability is needed in `Agent` itself. `spawn_specialist` builds the specialist and runs its first exchange; `query_specialist` looks the session back up by id and runs another exchange on the same instance; `close_specialist` tears it down. All three registered under the existing `[team] enabled` gate in `agent_builder.rs`.

**Tech Stack:** Rust, tokio, existing `aivyx-core`/`aivyx-tools`/`aivyx-sandbox`/`aivyx-team` crates. New dependency: `uuid` (already used identically by `aivyx-acp`, `aivyx-sandbox`, `aivyx-mcp-server` — `version = "1", features = ["v4"]`), added to `aivyx-core`.

## Global Constraints

- Additive only: `delegate_to_specialist.rs` and `mission_tools.rs` are **not modified** except where explicitly noted in Task 3 (a pre-existing clone/move needs correcting now that a third consumer of `team`/`team_parent_registry` exists — this is a required, disclosed side effect, not scope creep).
- All three new tools use `ActionKind::Internal` and `mutates_outside_session() == false`, identical to `delegate_to_specialist` — the real permission gating happens inside each specialist's own nested tool calls via the shared `ConfirmationGate`, unchanged by this phase.
- No idle-watcher background task. Idle-timeout eviction is **lazy** — checked via `evict_stale()` at the top of every pool `take`/`insert_new` call, mirroring `aivyx-mcp-server/src/server.rs`'s `SessionMap::evict_stale`. This is a deliberate, disclosed refinement of the design spec's literal "background idle-watcher task, polls every 30 seconds" wording — the user-visible contract (a session idle past the configured timeout eventually disappears) is unchanged; only the internal mechanism is simpler and reuses an already-proven pattern from this exact codebase.
- The concurrent-session cap fails with a clear `ToolOutput::Error` naming the cap, **not** silent LRU eviction — deliberately diverging from `aivyx-mcp-server`'s own `evict_oldest_if_full`, since a specialist session is conversational state the lead is actively relying on, not disposable client bookkeeping.
- Default cap: `3` (matches `default_coding_roster()`'s exact non-lead specialist count). Default idle timeout: `600` seconds (matches `ReplSettings.idle_timeout_secs`'s existing precedent).
- Every pool critical section is a short, synchronous `HashMap` operation, never held across an `.await` — use `std::sync::Mutex`, not `tokio::sync::Mutex`, mirroring `repl.rs`'s own output-buffer field for the identical reason.
- File-scoped `rustfmt --edition 2024 --check <path>` / `rustfmt --edition 2024 <path>` only — **never** a package-scoped `cargo fmt -p <crate>` or `cargo fmt --check -p <crate>` command with no file argument. This exact mistake has happened twice in this initiative already and had to be reverted both times.
- `cargo build --workspace`, `cargo test --workspace`, and `cargo clippy --workspace --all-targets -- -D warnings` must all stay clean after every task.

---

### Task 1: `TeamSettings` config additions

**Files:**
- Modify: `crates/aivyx-config/src/lib.rs:464-469` (the `TeamSettings` struct)
- Test: `crates/aivyx-config/src/lib.rs` (inline `#[cfg(test)]` module, existing `team_settings_*` tests around line 2157)

**Interfaces:**
- Produces: `TeamSettings { pub enabled: bool, pub max_concurrent_specialist_sessions: usize, pub specialist_session_idle_timeout_secs: u64 }`, plus `impl Default for TeamSettings`. Task 3 reads `settings.team.max_concurrent_specialist_sessions` and `settings.team.specialist_session_idle_timeout_secs`.

- [ ] **Step 1: Write the failing tests**

Find the existing test module (search for `team_settings_defaults_to_disabled` — currently around line 2157) and add two new tests immediately after `team_settings_defaults_when_section_omitted`:

```rust
    #[test]
    fn team_settings_defaults_to_a_usable_zero_config_shape() {
        let settings = TeamSettings::default();
        assert_eq!(settings.max_concurrent_specialist_sessions, 3);
        assert_eq!(settings.specialist_session_idle_timeout_secs, 600);
    }

    #[test]
    fn team_settings_specialist_session_fields_round_trip_through_toml() {
        let toml_str = "[team]\nenabled = true\nmax_concurrent_specialist_sessions = 5\nspecialist_session_idle_timeout_secs = 120\n";
        let settings: Settings = toml::from_str(toml_str).unwrap();
        assert!(settings.team.enabled);
        assert_eq!(settings.team.max_concurrent_specialist_sessions, 5);
        assert_eq!(settings.team.specialist_session_idle_timeout_secs, 120);
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p aivyx-config team_settings_defaults_to_a_usable_zero_config_shape`
Expected: FAIL — `no field \`max_concurrent_specialist_sessions\` on type \`TeamSettings\`` (compile error).

- [ ] **Step 3: Add the two new fields and switch to a manual `Default` impl**

Replace the current `TeamSettings` definition (`crates/aivyx-config/src/lib.rs:464-469`):

```rust
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct TeamSettings {
    #[serde(default)]
    pub enabled: bool,
}
```

with:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TeamSettings {
    pub enabled: bool,
    /// Hard cap on specialist sessions (`spawn_specialist`) that may be
    /// open at once. A `spawn_specialist` call beyond this cap errors
    /// clearly rather than evicting an existing session -- see
    /// `docs/superpowers/specs/2026-09-20-nonagon-team-specialist-sessions-design.md`.
    /// Default matches `default_coding_roster()`'s exact non-lead
    /// specialist count (implementer/reviewer/tester).
    pub max_concurrent_specialist_sessions: usize,
    /// Auto-close a specialist session with no `query_specialist`
    /// activity for this long, in seconds -- a safety net against a
    /// model that spawns sessions and forgets to close them, matching
    /// `ReplSettings.idle_timeout_secs`'s own precedent and default.
    pub specialist_session_idle_timeout_secs: u64,
}

impl Default for TeamSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            max_concurrent_specialist_sessions: 3,
            specialist_session_idle_timeout_secs: 600,
        }
    }
}
```

(`#[derive(Default)]` is removed since the auto-derived default would give `0`/`0` for the two new numeric fields, not the real defaults of `3`/`600`.)

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p aivyx-config team_settings`
Expected: all `team_settings_*` tests PASS (5 total: the 3 pre-existing plus the 2 new ones).

- [ ] **Step 5: File-scoped format check and full crate test**

Run: `rustfmt --edition 2024 --check crates/aivyx-config/src/lib.rs`
Expected: no output (clean). If it reports a diff, run `rustfmt --edition 2024 crates/aivyx-config/src/lib.rs` and re-check.

Run: `cargo test -p aivyx-config`
Expected: all tests PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/aivyx-config/src/lib.rs
git commit -m "feat: add specialist-session cap/idle-timeout to TeamSettings"
```

---

### Task 2: `specialist_sessions.rs` — session pool + three tools

**Files:**
- Create: `crates/aivyx-core/src/specialist_sessions.rs`
- Modify: `crates/aivyx-core/src/lib.rs` (wire the new module)
- Modify: `crates/aivyx-core/Cargo.toml` (add `uuid` dependency)

**Interfaces:**
- Consumes: `crate::delegate_to_specialist::{compute_specialist_registry, specialist_names, specialist_roster_description, specialists}` (all already `pub`/`pub(crate)` in this crate — confirmed by reading `delegate_to_specialist.rs`); `crate::agent::{Agent, AgentConfig, AgentEvent, EditFormat}`; `aivyx_team::TeamConfig`; `aivyx_tools::{Tool, ToolError, ToolExecutionContext, ToolExecutor, ToolRegistry, GitCheckpointer}`; `aivyx_sandbox::{ActionKind, AutonomousMode, ExecutionConfiner, InjectionTaint, PermissionGate, PermissionRequest, PermissionTarget, PlanMode}`; `aivyx_llm::LlmBackend`; `aivyx_repomap::RepoMap`; `aivyx_types::{ToolDefinition, ToolOutput}`.
- Produces (consumed by Task 3): `pub struct SpecialistSessionPool` with `pub fn new(max_concurrent: usize, idle_timeout: std::time::Duration) -> Self`; `#[derive(Clone)] pub struct SpecialistSessionsConfig { pub llm: Arc<dyn LlmBackend>, pub gate: Arc<dyn PermissionGate>, pub confiner: Arc<dyn ExecutionConfiner>, pub checkpointer: Option<Arc<GitCheckpointer>>, pub repo_map: Option<(Arc<RepoMap>, u32)>, pub events_tx: UnboundedSender<AgentEvent>, pub parent_registry: ToolRegistry, pub team: TeamConfig, pub plan_mode: PlanMode, pub autonomous_mode: AutonomousMode, pub injection_taint: InjectionTaint, pub context_tokens: u32, pub edit_format: EditFormat, pub verification: Option<(String, u32)>, pub max_iterations: u32, pub broker_mode: bool, pub pool: SpecialistSessionPool }`; `pub struct SpawnSpecialistTool` / `pub struct QuerySpecialistTool` / `pub struct CloseSpecialistTool`, each `pub fn new(config: SpecialistSessionsConfig) -> Self` and implementing `Tool` with names `"spawn_specialist"` / `"query_specialist"` / `"close_specialist"`.

- [ ] **Step 1: Add the `uuid` dependency**

In `crates/aivyx-core/Cargo.toml`, in the `[dependencies]` section, add (alphabetically, after `time` and before `tokio`, matching the file's existing alphabetical ordering):

```toml
uuid = { version = "1", features = ["v4"] }
```

(Same version/feature spec already used identically by `aivyx-acp`, `aivyx-sandbox`, and `aivyx-mcp-server`'s own `Cargo.toml` files — no version drift risk.)

- [ ] **Step 2: Write the pool + config types with a failing cap test**

Create `crates/aivyx-core/src/specialist_sessions.rs`:

```rust
//! Three tools for a resumable, multi-exchange conversation with a team
//! specialist -- `spawn_specialist`, `query_specialist`,
//! `close_specialist` -- additive alongside (not a replacement for) the
//! existing one-shot `delegate_to_specialist`. See
//! `docs/superpowers/specs/2026-09-20-nonagon-team-specialist-sessions-design.md`.
//!
//! The load-bearing fact this whole module rests on: `Agent::run_turn`
//! is already safely callable multiple times on the same instance --
//! `delegate_to_specialist::execute`'s own "continue" loop already does
//! exactly that for a single exchange. Parking a specialist `Agent`
//! between tool calls (instead of dropping it after one exchange, as
//! `delegate_to_specialist` does) needs no new capability in `Agent`
//! itself.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aivyx_llm::LlmBackend;
use aivyx_repomap::RepoMap;
use aivyx_sandbox::{
    ActionKind, AutonomousMode, ExecutionConfiner, InjectionTaint, PermissionGate,
    PermissionRequest, PermissionTarget, PlanMode,
};
use aivyx_team::TeamConfig;
use aivyx_tools::{
    GitCheckpointer, Tool, ToolError, ToolExecutionContext, ToolExecutor, ToolRegistry,
};
use aivyx_types::{ToolDefinition, ToolOutput};
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use tokio::sync::mpsc;
use tokio::sync::mpsc::UnboundedSender;

use crate::agent::{Agent, AgentConfig, AgentEvent, EditFormat};
use crate::delegate_to_specialist::{
    compute_specialist_registry, specialist_names, specialist_roster_description, specialists,
};

/// Mirrors `delegate_to_specialist.rs`'s own identical constants -- same
/// wording, kept consistent across both sibling modules.
const CUTOFF_NOTICE: &str = "\n\n(sub-agent stopped: reached its iteration budget before \
finishing — the above is its best-effort partial result.)";
const INJECTION_CUTOFF_NOTICE: &str = "\n\n(sub-agent stopped: a tool result was flagged as a \
possible prompt injection — the above is its best-effort partial result.)";
const NO_TEXT_RESPONSE: &str = "(the specialist produced no text response)";

struct ParkedSpecialistSession {
    agent: Agent,
    member: String,
    forward_task: tokio::task::JoinHandle<()>,
    accumulated: Arc<Mutex<String>>,
    last_active: Instant,
}

struct SessionPoolState {
    sessions: HashMap<String, ParkedSpecialistSession>,
    max_concurrent: usize,
    idle_timeout: Duration,
}

impl SessionPoolState {
    /// Removes every session idle longer than `idle_timeout`. Called at
    /// the top of every `take`/`insert_new`/`has_room` so staleness is
    /// judged relative to "now", not to whenever the pool was last
    /// touched -- mirrors `aivyx-mcp-server/src/server.rs`'s
    /// `SessionMap::evict_stale`.
    fn evict_stale(&mut self) {
        let idle_timeout = self.idle_timeout;
        self.sessions
            .retain(|_, s| s.last_active.elapsed() < idle_timeout);
    }
}

/// Shared, keyed pool of parked specialist sessions -- `Arc`-wrapped so
/// it can be cloned into all three tool structs below. A plain
/// `std::sync::Mutex`, not an async one: every critical section here is
/// a short, synchronous `HashMap` operation, never held across an
/// `.await` -- mirrors `aivyx-tools`' own `repl.rs` output-buffer field
/// for the identical reason.
#[derive(Clone)]
pub struct SpecialistSessionPool {
    inner: Arc<Mutex<SessionPoolState>>,
}

impl SpecialistSessionPool {
    pub fn new(max_concurrent: usize, idle_timeout: Duration) -> Self {
        Self {
            inner: Arc::new(Mutex::new(SessionPoolState {
                sessions: HashMap::new(),
                max_concurrent,
                idle_timeout,
            })),
        }
    }

    fn has_room(&self) -> bool {
        let mut state = self.inner.lock().unwrap();
        state.evict_stale();
        state.sessions.len() < state.max_concurrent
    }

    pub fn max_concurrent(&self) -> usize {
        self.inner.lock().unwrap().max_concurrent
    }

    /// Removes and returns a session so its turn can run WITHOUT holding
    /// the pool's lock -- the whole point of this method existing
    /// instead of a borrow-returning accessor. `None` if the id doesn't
    /// exist or has gone stale.
    fn take(&self, id: &str) -> Option<ParkedSpecialistSession> {
        let mut state = self.inner.lock().unwrap();
        state.evict_stale();
        state.sessions.remove(id)
    }

    /// Re-inserts a session after its turn completes, refreshing
    /// `last_active` to now.
    fn put_back(&self, id: String, mut session: ParkedSpecialistSession) {
        session.last_active = Instant::now();
        self.inner.lock().unwrap().sessions.insert(id, session);
    }

    /// Inserts a brand-new session, enforcing the concurrent-session cap.
    /// `Err(max_concurrent)` if the pool is already full at the moment of
    /// insertion -- callers should prefer checking `has_room()` first to
    /// avoid running a wasted specialist turn, but this is still checked
    /// here too as the authoritative guard.
    fn insert_new(&self, id: String, session: ParkedSpecialistSession) -> Result<(), usize> {
        let mut state = self.inner.lock().unwrap();
        state.evict_stale();
        if state.sessions.len() >= state.max_concurrent {
            return Err(state.max_concurrent);
        }
        state.sessions.insert(id, session);
        Ok(())
    }
}

/// Shared by all three tools below -- the same fields
/// `DelegateToSpecialistConfig` carries (this phase's tools reuse the
/// identical specialist-construction mechanism), plus the shared pool.
#[derive(Clone)]
pub struct SpecialistSessionsConfig {
    pub llm: Arc<dyn LlmBackend>,
    pub gate: Arc<dyn PermissionGate>,
    pub confiner: Arc<dyn ExecutionConfiner>,
    pub checkpointer: Option<Arc<GitCheckpointer>>,
    pub repo_map: Option<(Arc<RepoMap>, u32)>,
    pub events_tx: UnboundedSender<AgentEvent>,
    pub parent_registry: ToolRegistry,
    pub team: TeamConfig,
    pub plan_mode: PlanMode,
    pub autonomous_mode: AutonomousMode,
    pub injection_taint: InjectionTaint,
    pub context_tokens: u32,
    pub edit_format: EditFormat,
    pub verification: Option<(String, u32)>,
    pub max_iterations: u32,
    pub broker_mode: bool,
    pub pool: SpecialistSessionPool,
}
```

- [ ] **Step 3: Run to verify it compiles (no tools/tests yet)**

Run: `cargo check -p aivyx-core`
Expected: FAILS — `specialist_sessions` is not yet declared as a module in `lib.rs`, so nothing references this file. This is expected; proceed to Step 4 before checking again.

- [ ] **Step 4: Wire the module into `lib.rs`**

In `crates/aivyx-core/src/lib.rs`, change:

```rust
pub mod mission_tools;
pub mod session;
```

to:

```rust
pub mod mission_tools;
pub mod session;
pub mod specialist_sessions;
```

And change:

```rust
pub use mission_tools::{
    DecomposeTaskTool, MissionToolsConfig, SynthesizeResultsTool, VerifyOutputTool,
};
pub use session::{SessionState, Task, TaskStatus};
```

to:

```rust
pub use mission_tools::{
    DecomposeTaskTool, MissionToolsConfig, SynthesizeResultsTool, VerifyOutputTool,
};
pub use session::{SessionState, Task, TaskStatus};
pub use specialist_sessions::{
    CloseSpecialistTool, QuerySpecialistTool, SpawnSpecialistTool, SpecialistSessionPool,
    SpecialistSessionsConfig,
};
```

- [ ] **Step 5: Run to verify it compiles**

Run: `cargo check -p aivyx-core`
Expected: PASS (the pool/config types compile; no tools reference them yet, so no "unused" warnings for the pool methods since `pub` items aren't flagged unused).

- [ ] **Step 6: Add the shared bounded-exchange helper and the three `Tool` impls**

Append to `crates/aivyx-core/src/specialist_sessions.rs` (after the `SpecialistSessionsConfig` struct):

```rust
/// Runs one bounded exchange against `agent` (a `run_turn` call, then the
/// same "continue" loop `delegate_to_specialist::execute` uses for a
/// single exchange, bounded by `config.max_iterations`), draining
/// `accumulated` into the returned `ToolOutput` and leaving it empty for
/// the next exchange (`std::mem::take`, not `Arc::try_unwrap` --
/// `accumulated` must survive for a possible future exchange on the same
/// session, unlike `delegate_to_specialist`'s one-shot teardown). Used by
/// both `spawn_specialist` (the first exchange, right after constructing
/// `agent`) and `query_specialist` (every later exchange on an
/// already-parked `agent`) -- the only difference between the two call
/// sites is whether `agent` was just constructed or was already sitting
/// in the pool.
async fn run_bounded_exchange(
    agent: &mut Agent,
    input: String,
    ctx: &ToolExecutionContext,
    config: &SpecialistSessionsConfig,
    accumulated: &Arc<Mutex<String>>,
) -> ToolOutput {
    let max_iterations = config.max_iterations.max(1);
    let mut result = agent.run_turn(input, &ctx.cwd, ctx.cancellation.clone()).await;
    let mut iterations_used = 1u32;
    let is_injection_tainted =
        || config.autonomous_mode.active() && config.injection_taint.current().is_some();
    while result.is_ok()
        && agent.last_turn_paused()
        && iterations_used < max_iterations
        && !ctx.cancellation.is_cancelled()
        && !is_injection_tainted()
    {
        iterations_used += 1;
        result = agent
            .run_turn("continue".to_string(), &ctx.cwd, ctx.cancellation.clone())
            .await;
    }
    let paused = result.is_ok() && agent.last_turn_paused();
    let cap_hit = paused && !is_injection_tainted();
    let injection_hit = paused && is_injection_tainted();

    match result {
        Err(err) => ToolOutput::Error(format!("specialist turn failed: {err}")),
        Ok(()) => {
            let mut text = std::mem::take(&mut *accumulated.lock().unwrap());
            if injection_hit {
                text.push_str(INJECTION_CUTOFF_NOTICE);
            } else if cap_hit {
                text.push_str(CUTOFF_NOTICE);
            }
            if text.trim().is_empty() {
                text = NO_TEXT_RESPONSE.to_string();
            }
            ToolOutput::Ok(text)
        }
    }
}

/// Builds a fresh specialist `Agent` for `member`, identical construction
/// to `delegate_to_specialist::execute`'s own (same attenuated registry,
/// same shared gate/confiner/checkpointer/injection-taint/plan-mode/
/// autonomous-mode, same event-forwarding task). Returns the agent, its
/// forward-task handle, and the shared accumulator -- the caller
/// (`SpawnSpecialistTool::execute`) still owns running the first exchange
/// and deciding whether to park the result.
fn build_specialist_agent(
    member: &aivyx_team::TeamMember,
    config: &SpecialistSessionsConfig,
) -> (Agent, tokio::task::JoinHandle<()>, Arc<Mutex<String>>) {
    let specialist_registry = compute_specialist_registry(member, &config.parent_registry);
    let mut sub_executor = ToolExecutor::new(
        specialist_registry,
        Arc::clone(&config.gate),
        Arc::clone(&config.confiner),
    );
    if let Some(checkpointer) = &config.checkpointer {
        sub_executor.set_checkpointer(Arc::clone(checkpointer));
    }

    let (sub_tx, mut sub_rx) = mpsc::unbounded_channel();
    let accumulated = Arc::new(Mutex::new(String::new()));
    let accumulated_for_task = Arc::clone(&accumulated);
    let parent_tx = config.events_tx.clone();
    let forward_task = tokio::spawn(async move {
        while let Some(event) = sub_rx.recv().await {
            if let AgentEvent::TextDelta(text) = &event {
                accumulated_for_task.lock().unwrap().push_str(text);
            }
            let _ = parent_tx.send(AgentEvent::SubAgentActivity(Box::new(event)));
        }
    });

    let mut agent = Agent::new(
        Arc::clone(&config.llm),
        sub_executor,
        member.persona.clone(),
        AgentConfig {
            max_tool_iterations: 1,
            context_tokens: config.context_tokens,
            edit_format: config.edit_format,
        },
        Arc::default(),
        config.plan_mode.clone(),
        config.autonomous_mode.clone(),
        sub_tx,
    );
    if let Some((map, budget)) = &config.repo_map {
        agent.set_repo_map(Arc::clone(map), *budget);
    }
    if let Some((command, max_retries)) = &config.verification {
        agent.set_verification(command.clone(), *max_retries);
    }
    agent.set_injection_taint(config.injection_taint.clone());
    agent.set_broker_mode(config.broker_mode);

    (agent, forward_task, accumulated)
}

fn internal_permission_request(tool_name: &str) -> Result<PermissionRequest, ToolError> {
    Ok(PermissionRequest {
        tool_name: tool_name.to_string(),
        action: ActionKind::Internal,
        target: PermissionTarget::Other(tool_name.to_string()),
        arguments_preview: serde_json::json!({}),
        preview: None,
        diff: None,
    })
}

#[derive(Deserialize, JsonSchema)]
struct SpawnSpecialistArgs {
    /// The name of a `TeamConfig` member to spawn a session with -- must
    /// match one of the specialists listed in this tool's own
    /// description, excluding the team's own `lead`.
    member: String,
    /// A complete, self-contained description of the first task for the
    /// specialist -- it starts with no context beyond this text and the
    /// specialist's own persona.
    task: String,
}

pub struct SpawnSpecialistTool {
    config: SpecialistSessionsConfig,
}

impl SpawnSpecialistTool {
    pub fn new(config: SpecialistSessionsConfig) -> Self {
        Self { config }
    }
}

#[async_trait]
impl Tool for SpawnSpecialistTool {
    fn name(&self) -> &str {
        "spawn_specialist"
    }

    fn mutates_outside_session(&self) -> bool {
        false
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name().to_string(),
            description: format!(
                "Start a resumable session with one team specialist -- unlike \
                delegate_to_specialist (a single exchange), the specialist stays alive so \
                you can send it follow-ups with query_specialist, then end it with \
                close_specialist when done. Returns a session_id plus the specialist's \
                response to the first task. Available specialists: {}. At most {} sessions \
                may be open at once.",
                specialist_roster_description(&self.config.team),
                self.config.pool.max_concurrent(),
            ),
            parameters_schema: serde_json::Value::from(schemars::schema_for!(SpawnSpecialistArgs)),
        }
    }

    fn permission_request(
        &self,
        _arguments: &serde_json::Value,
        _cwd: &Path,
    ) -> Result<PermissionRequest, ToolError> {
        internal_permission_request(self.name())
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        ctx: &ToolExecutionContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: SpawnSpecialistArgs = serde_json::from_value(arguments)
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;

        if args.member == self.config.team.lead {
            return Ok(ToolOutput::Error(format!(
                "cannot spawn a session with the team's own lead ({:?}) -- spawn one of the \
                other specialists instead: {}",
                args.member,
                specialist_names(&self.config.team)
            )));
        }
        let Some(member) = self
            .config
            .team
            .members
            .iter()
            .find(|m| m.name == args.member)
        else {
            return Ok(ToolOutput::Error(format!(
                "unknown team member: {:?} -- valid specialists: {}",
                args.member,
                specialist_names(&self.config.team)
            )));
        };

        if !self.config.pool.has_room() {
            return Ok(ToolOutput::Error(format!(
                "cannot open a new specialist session: {} are already open -- close one with \
                close_specialist first",
                self.config.pool.max_concurrent()
            )));
        }

        let (mut agent, forward_task, accumulated) = build_specialist_agent(member, &self.config);
        let output = run_bounded_exchange(&mut agent, args.task, ctx, &self.config, &accumulated).await;

        let ToolOutput::Ok(text) = output else {
            drop(agent);
            let _ = forward_task.await;
            return Ok(output);
        };

        let session_id = uuid::Uuid::new_v4().to_string();
        let session = ParkedSpecialistSession {
            agent,
            member: member.name.clone(),
            forward_task,
            accumulated,
            last_active: Instant::now(),
        };
        if let Err(max) = self.config.pool.insert_new(session_id.clone(), session) {
            return Ok(ToolOutput::Error(format!(
                "cannot open a new specialist session: {max} are already open -- close one \
                with close_specialist first"
            )));
        }

        Ok(ToolOutput::Ok(format!("session_id: {session_id}\n\n{text}")))
    }
}

#[derive(Deserialize, JsonSchema)]
struct QuerySpecialistArgs {
    /// The session_id returned by a prior spawn_specialist call.
    session_id: String,
    /// The follow-up message to send to the specialist -- it sees this
    /// in addition to everything from its earlier exchanges in this
    /// session.
    message: String,
}

pub struct QuerySpecialistTool {
    config: SpecialistSessionsConfig,
}

impl QuerySpecialistTool {
    pub fn new(config: SpecialistSessionsConfig) -> Self {
        Self { config }
    }
}

#[async_trait]
impl Tool for QuerySpecialistTool {
    fn name(&self) -> &str {
        "query_specialist"
    }

    fn mutates_outside_session(&self) -> bool {
        false
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name().to_string(),
            description: "Send a follow-up message to a specialist session opened with \
                spawn_specialist -- the specialist remembers everything from earlier \
                exchanges in this same session. Returns the specialist's response. Errors if \
                session_id is unknown, already closed, or has expired from inactivity."
                .to_string(),
            parameters_schema: serde_json::Value::from(schemars::schema_for!(QuerySpecialistArgs)),
        }
    }

    fn permission_request(
        &self,
        _arguments: &serde_json::Value,
        _cwd: &Path,
    ) -> Result<PermissionRequest, ToolError> {
        internal_permission_request(self.name())
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        ctx: &ToolExecutionContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: QuerySpecialistArgs = serde_json::from_value(arguments)
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;

        let Some(mut session) = self.config.pool.take(&args.session_id) else {
            return Ok(ToolOutput::Error(format!(
                "unknown session_id: {:?} -- it may not exist, may already be closed, or may \
                have expired from inactivity; call spawn_specialist to start a new one",
                args.session_id
            )));
        };

        let output = run_bounded_exchange(
            &mut session.agent,
            args.message,
            ctx,
            &self.config,
            &session.accumulated,
        )
        .await;

        self.config.pool.put_back(args.session_id, session);
        Ok(output)
    }
}

#[derive(Deserialize, JsonSchema)]
struct CloseSpecialistArgs {
    /// The session_id returned by a prior spawn_specialist call.
    session_id: String,
}

pub struct CloseSpecialistTool {
    config: SpecialistSessionsConfig,
}

impl CloseSpecialistTool {
    pub fn new(config: SpecialistSessionsConfig) -> Self {
        Self { config }
    }
}

#[async_trait]
impl Tool for CloseSpecialistTool {
    fn name(&self) -> &str {
        "close_specialist"
    }

    fn mutates_outside_session(&self) -> bool {
        false
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name().to_string(),
            description: "End a specialist session opened with spawn_specialist, freeing it \
                up so a new session can be opened within the concurrent-session limit. Errors \
                if session_id is unknown or already closed."
                .to_string(),
            parameters_schema: serde_json::Value::from(schemars::schema_for!(CloseSpecialistArgs)),
        }
    }

    fn permission_request(
        &self,
        _arguments: &serde_json::Value,
        _cwd: &Path,
    ) -> Result<PermissionRequest, ToolError> {
        internal_permission_request(self.name())
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        _ctx: &ToolExecutionContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: CloseSpecialistArgs = serde_json::from_value(arguments)
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;

        let Some(session) = self.config.pool.take(&args.session_id) else {
            return Ok(ToolOutput::Error(format!(
                "unknown session_id: {:?} -- it may not exist or may already be closed",
                args.session_id
            )));
        };
        let member = session.member.clone();
        drop(session.agent);
        let _ = session.forward_task.await;
        Ok(ToolOutput::Ok(format!(
            "specialist session {:?} closed ({member})",
            args.session_id
        )))
    }
}
```

- [ ] **Step 7: Run to verify it compiles**

Run: `cargo check -p aivyx-core`
Expected: PASS. If `aivyx_team::TeamMember` isn't already imported at crate level where needed, the `use aivyx_team::TeamConfig;` line plus fully-qualifying `aivyx_team::TeamMember` in `build_specialist_agent`'s signature (as written above) is sufficient — no separate import needed.

- [ ] **Step 8: Add the test module**

Append to `crates/aivyx-core/src/specialist_sessions.rs`:

```rust
#[cfg(test)]
mod specialist_session_tests {
    use super::*;
    use aivyx_llm::{ChatRequest, FinishReason, LlmError, StreamEvent};
    use aivyx_sandbox::{NoopConfiner, PermissionDecision};
    use aivyx_team::TeamMember;
    use futures::stream::BoxStream;
    use futures::StreamExt;
    use std::time::Duration;
    use tokio_util::sync::CancellationToken;

    struct MockBackend {
        responses: Mutex<std::collections::VecDeque<Vec<StreamEvent>>>,
        received: Mutex<Vec<ChatRequest>>,
    }

    impl MockBackend {
        fn new(responses: Vec<Vec<StreamEvent>>) -> Self {
            Self {
                responses: Mutex::new(responses.into()),
                received: Mutex::new(Vec::new()),
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
            request: ChatRequest,
        ) -> Result<BoxStream<'static, Result<StreamEvent, LlmError>>, LlmError> {
            self.received.lock().unwrap().push(request);
            let events = self
                .responses
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_default();
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

    fn exec_ctx(cwd: &std::path::Path) -> ToolExecutionContext {
        ToolExecutionContext {
            cwd: cwd.to_path_buf(),
            confiner: Arc::new(NoopConfiner),
            cancellation: CancellationToken::new(),
        }
    }

    fn simple_team() -> TeamConfig {
        TeamConfig {
            lead: "coordinator".to_string(),
            members: vec![
                TeamMember {
                    name: "coordinator".to_string(),
                    role: "Lead".to_string(),
                    persona: "You delegate.".to_string(),
                    tool_allowlist: vec![],
                    extra_deny_paths: vec![],
                },
                TeamMember {
                    name: "implementer".to_string(),
                    role: "Implementer".to_string(),
                    persona: "You are the implementer specialist. You write code.".to_string(),
                    tool_allowlist: vec![],
                    extra_deny_paths: vec![],
                },
            ],
        }
    }

    fn config(
        llm: Arc<dyn LlmBackend>,
        events_tx: UnboundedSender<AgentEvent>,
        team: TeamConfig,
        pool: SpecialistSessionPool,
    ) -> SpecialistSessionsConfig {
        SpecialistSessionsConfig {
            llm,
            gate: Arc::new(AllowAllGate),
            confiner: Arc::new(NoopConfiner),
            checkpointer: None,
            repo_map: None,
            events_tx,
            parent_registry: ToolRegistry::new(),
            team,
            plan_mode: PlanMode::new(),
            autonomous_mode: AutonomousMode::new(),
            injection_taint: InjectionTaint::new(),
            context_tokens: 8192,
            edit_format: EditFormat::Native,
            verification: None,
            max_iterations: 3,
            broker_mode: false,
            pool,
        }
    }

    #[tokio::test]
    async fn spawn_specialist_rejects_the_lead_as_a_target() {
        let llm = Arc::new(MockBackend::new(vec![]));
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let pool = SpecialistSessionPool::new(3, Duration::from_secs(600));
        let cfg = config(llm.clone(), tx, simple_team(), pool);
        let tool = SpawnSpecialistTool::new(cfg);
        let ctx = exec_ctx(std::path::Path::new("."));
        let args = serde_json::json!({ "member": "coordinator", "task": "do something" });
        let result = tool.execute(args, &ctx).await.unwrap();
        match result {
            ToolOutput::Error(msg) => assert!(msg.contains("coordinator")),
            other => panic!("expected Error, got {other:?}"),
        }
        assert!(llm.received.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn spawn_specialist_rejects_an_unknown_member() {
        let llm = Arc::new(MockBackend::new(vec![]));
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let pool = SpecialistSessionPool::new(3, Duration::from_secs(600));
        let cfg = config(llm.clone(), tx, simple_team(), pool);
        let tool = SpawnSpecialistTool::new(cfg);
        let ctx = exec_ctx(std::path::Path::new("."));
        let args = serde_json::json!({ "member": "nonexistent", "task": "do something" });
        let result = tool.execute(args, &ctx).await.unwrap();
        match result {
            ToolOutput::Error(msg) => {
                assert!(msg.contains("nonexistent"));
                assert!(msg.contains("implementer"));
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn spawn_and_query_specialist_preserves_conversation_history_across_calls() {
        let llm = Arc::new(MockBackend::new(vec![
            text_response("first: got the task"),
            text_response("second: remembered the first"),
        ]));
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let pool = SpecialistSessionPool::new(3, Duration::from_secs(600));
        let cfg = config(llm.clone(), tx, simple_team(), pool);
        let spawn_tool = SpawnSpecialistTool::new(cfg.clone());
        let query_tool = QuerySpecialistTool::new(cfg);
        let ctx = exec_ctx(std::path::Path::new("."));

        let spawn_args = serde_json::json!({ "member": "implementer", "task": "fix the bug" });
        let spawn_result = spawn_tool.execute(spawn_args, &ctx).await.unwrap();
        let ToolOutput::Ok(spawn_text) = spawn_result else {
            panic!("expected Ok from spawn_specialist");
        };
        assert!(spawn_text.contains("first: got the task"));
        let session_id = spawn_text
            .lines()
            .next()
            .unwrap()
            .strip_prefix("session_id: ")
            .unwrap()
            .to_string();

        let query_args =
            serde_json::json!({ "session_id": session_id, "message": "what did I ask first?" });
        let query_result = query_tool.execute(query_args, &ctx).await.unwrap();
        let ToolOutput::Ok(query_text) = query_result else {
            panic!("expected Ok from query_specialist");
        };
        assert!(query_text.contains("second: remembered the first"));

        // The second request sent to the backend must carry the first
        // exchange's content in its message history -- proof this is a
        // real continuation, not two disconnected one-shot calls.
        let received = llm.received.lock().unwrap();
        assert_eq!(received.len(), 2);
        let second_request_text = format!("{:?}", received[1].messages);
        assert!(
            second_request_text.contains("fix the bug")
                && second_request_text.contains("first: got the task"),
            "expected the second request to carry the first exchange's content, got: {second_request_text}"
        );
    }

    #[tokio::test]
    async fn spawn_specialist_errors_once_the_concurrent_session_cap_is_reached() {
        let llm = Arc::new(MockBackend::new(vec![
            text_response("session one"),
            text_response("session two"),
        ]));
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let pool = SpecialistSessionPool::new(1, Duration::from_secs(600));
        let cfg = config(llm.clone(), tx, simple_team(), pool);
        let tool = SpawnSpecialistTool::new(cfg);
        let ctx = exec_ctx(std::path::Path::new("."));

        let first = tool
            .execute(
                serde_json::json!({ "member": "implementer", "task": "first task" }),
                &ctx,
            )
            .await
            .unwrap();
        assert!(matches!(first, ToolOutput::Ok(_)));

        let second = tool
            .execute(
                serde_json::json!({ "member": "implementer", "task": "second task" }),
                &ctx,
            )
            .await
            .unwrap();
        match second {
            ToolOutput::Error(msg) => assert!(msg.contains('1')),
            other => panic!("expected Error, got {other:?}"),
        }
        // The cap must be checked BEFORE running a wasted turn -- only
        // one backend request should have been sent.
        assert_eq!(llm.received.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn query_specialist_rejects_an_unknown_session_id() {
        let llm = Arc::new(MockBackend::new(vec![]));
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let pool = SpecialistSessionPool::new(3, Duration::from_secs(600));
        let cfg = config(llm, tx, simple_team(), pool);
        let tool = QuerySpecialistTool::new(cfg);
        let ctx = exec_ctx(std::path::Path::new("."));
        let args = serde_json::json!({ "session_id": "nonexistent", "message": "hi" });
        let result = tool.execute(args, &ctx).await.unwrap();
        assert!(matches!(result, ToolOutput::Error(_)));
    }

    #[tokio::test]
    async fn close_specialist_then_query_specialist_reports_unknown_session() {
        let llm = Arc::new(MockBackend::new(vec![text_response("hello")]));
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let pool = SpecialistSessionPool::new(3, Duration::from_secs(600));
        let cfg = config(llm, tx, simple_team(), pool);
        let spawn_tool = SpawnSpecialistTool::new(cfg.clone());
        let close_tool = CloseSpecialistTool::new(cfg.clone());
        let query_tool = QuerySpecialistTool::new(cfg);
        let ctx = exec_ctx(std::path::Path::new("."));

        let spawn_result = spawn_tool
            .execute(
                serde_json::json!({ "member": "implementer", "task": "task" }),
                &ctx,
            )
            .await
            .unwrap();
        let ToolOutput::Ok(spawn_text) = spawn_result else {
            panic!("expected Ok");
        };
        let session_id = spawn_text
            .lines()
            .next()
            .unwrap()
            .strip_prefix("session_id: ")
            .unwrap()
            .to_string();

        let close_result = close_tool
            .execute(
                serde_json::json!({ "session_id": session_id.clone() }),
                &ctx,
            )
            .await
            .unwrap();
        assert!(matches!(close_result, ToolOutput::Ok(_)));

        let query_result = query_tool
            .execute(
                serde_json::json!({ "session_id": session_id, "message": "still there?" }),
                &ctx,
            )
            .await
            .unwrap();
        assert!(matches!(query_result, ToolOutput::Error(_)));
    }

    #[tokio::test]
    async fn close_specialist_frees_a_slot_for_a_new_spawn() {
        let llm = Arc::new(MockBackend::new(vec![
            text_response("session one"),
            text_response("session two"),
        ]));
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let pool = SpecialistSessionPool::new(1, Duration::from_secs(600));
        let cfg = config(llm, tx, simple_team(), pool);
        let spawn_tool = SpawnSpecialistTool::new(cfg.clone());
        let close_tool = CloseSpecialistTool::new(cfg);
        let ctx = exec_ctx(std::path::Path::new("."));

        let first = spawn_tool
            .execute(
                serde_json::json!({ "member": "implementer", "task": "first" }),
                &ctx,
            )
            .await
            .unwrap();
        let ToolOutput::Ok(first_text) = first else {
            panic!("expected Ok");
        };
        let first_id = first_text
            .lines()
            .next()
            .unwrap()
            .strip_prefix("session_id: ")
            .unwrap()
            .to_string();

        close_tool
            .execute(serde_json::json!({ "session_id": first_id }), &ctx)
            .await
            .unwrap();

        let second = spawn_tool
            .execute(
                serde_json::json!({ "member": "implementer", "task": "second" }),
                &ctx,
            )
            .await
            .unwrap();
        assert!(matches!(second, ToolOutput::Ok(_)));
    }

    #[tokio::test]
    async fn sessions_past_the_idle_timeout_are_evicted_on_the_next_pool_touch() {
        let llm = Arc::new(MockBackend::new(vec![text_response("hello")]));
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let pool = SpecialistSessionPool::new(3, Duration::from_millis(10));
        let cfg = config(llm, tx, simple_team(), pool);
        let spawn_tool = SpawnSpecialistTool::new(cfg.clone());
        let query_tool = QuerySpecialistTool::new(cfg);
        let ctx = exec_ctx(std::path::Path::new("."));

        let spawn_result = spawn_tool
            .execute(
                serde_json::json!({ "member": "implementer", "task": "task" }),
                &ctx,
            )
            .await
            .unwrap();
        let ToolOutput::Ok(spawn_text) = spawn_result else {
            panic!("expected Ok");
        };
        let session_id = spawn_text
            .lines()
            .next()
            .unwrap()
            .strip_prefix("session_id: ")
            .unwrap()
            .to_string();

        tokio::time::sleep(Duration::from_millis(30)).await;

        let query_result = query_tool
            .execute(
                serde_json::json!({ "session_id": session_id, "message": "still there?" }),
                &ctx,
            )
            .await
            .unwrap();
        assert!(matches!(query_result, ToolOutput::Error(_)));
    }
}
```

- [ ] **Step 9: Run the new tests**

Run: `cargo test -p aivyx-core specialist_session`
Expected: all 8 tests PASS.

- [ ] **Step 10: Full crate test + file-scoped format check**

Run: `cargo test -p aivyx-core`
Expected: all tests PASS (no regressions in `delegate_to_specialist`/`mission_tools`/other existing tests).

Run: `rustfmt --edition 2024 --check crates/aivyx-core/src/specialist_sessions.rs`
Expected: no output (clean). If it reports a diff, run `rustfmt --edition 2024 crates/aivyx-core/src/specialist_sessions.rs` and re-check.

Run: `rustfmt --edition 2024 --check crates/aivyx-core/src/lib.rs`
Expected: no output (clean).

- [ ] **Step 11: Commit**

```bash
git add crates/aivyx-core/Cargo.toml crates/aivyx-core/src/lib.rs crates/aivyx-core/src/specialist_sessions.rs Cargo.lock
git commit -m "feat: add spawn_specialist/query_specialist/close_specialist tools"
```

---

### Task 3: Wire into `agent_builder.rs` + README

**Files:**
- Modify: `crates/aivyx/src/agent_builder.rs:655-730` (the `if settings.team.enabled` block)
- Modify: `README.md` (tools table + `[team]` config comment)

**Interfaces:**
- Consumes: `aivyx_core::{SpecialistSessionPool, SpecialistSessionsConfig, SpawnSpecialistTool, QuerySpecialistTool, CloseSpecialistTool}` (Task 2); `settings.team.max_concurrent_specialist_sessions`, `settings.team.specialist_session_idle_timeout_secs` (Task 1).

- [ ] **Step 1: Replace the `if settings.team.enabled` block**

In `crates/aivyx/src/agent_builder.rs`, replace the entire block from `if settings.team.enabled {` (currently line 655) through its closing `}` (currently line 730) with:

```rust
    if settings.team.enabled {
        // Cloned *before* delegate_to_specialist/spawn_specialist are
        // themselves registered onto `registry`, for the same structural
        // reason sub_agent_registry/mcp_registry are cloned before
        // delegate_task is registered above -- a specialist's own
        // attenuated registry (computed per-call by
        // compute_specialist_registry from this snapshot) must never be
        // able to include either delegation tool, or recursive
        // delegation becomes possible. Mirrors sub_agent_registry's own
        // repl_start/repl_send/repl_stop exclusion, for the identical
        // reason: a specialist sharing the parent's single global REPL
        // session would break the isolated-history guarantee delegation
        // is supposed to provide. Unlike sub_agent_registry, this does
        // NOT exclude delegate_task itself or any MCP-bridged tools -- a
        // specialist can still see/call those, so one-level-deep
        // delegation isn't fully structural yet for specialists; that's
        // accepted, current scope, not a bug to fix here. Because this
        // snapshot is taken before decompose_task/verify_output/
        // synthesize_results/spawn_specialist/query_specialist/
        // close_specialist are registered further down, a specialist's
        // own attenuated registry can never include any of those six
        // tools either, even if a future custom roster's tool_allowlist
        // tried to name them -- a phase adding custom rosters will need
        // to revisit where this snapshot is taken if specialists should
        // ever be granted them. Cloned (not moved) at each use below
        // since both delegate_to_specialist and spawn_specialist,
        // registered later in this same block, need their own copy.
        let mut team_parent_registry = registry.clone();
        team_parent_registry.exclude(&["repl_start", "repl_send", "repl_stop"]);
        let team = aivyx_team::default_coding_roster();
        registry.register(Arc::new(aivyx_core::DelegateToSpecialistTool::new(
            aivyx_core::DelegateToSpecialistConfig {
                llm: Arc::clone(&llm),
                gate: Arc::clone(&gate),
                confiner: Arc::clone(&confiner),
                checkpointer: checkpointer.clone(),
                repo_map: repo_map.clone(),
                events_tx: events_tx.clone(),
                parent_registry: team_parent_registry.clone(),
                team: team.clone(),
                plan_mode: plan_mode.clone(),
                autonomous_mode: autonomous_mode.clone(),
                injection_taint: injection_taint.clone(),
                context_tokens: settings.backend.context_tokens,
                edit_format,
                verification: verification.clone(),
                max_iterations: settings.sub_agent.max_iterations,
                broker_mode,
            },
        )));

        // Nonagon-style mission structure (see
        // docs/superpowers/specs/2026-09-20-nonagon-team-mission-structure-design.md):
        // decompose_task/verify_output/synthesize_results, gated behind the
        // same [team] enabled flag as delegate_to_specialist above -- no new
        // config knob. All three are lightweight, state-recording tools
        // (none call an LLM or construct a sub-agent), so unlike
        // delegate_to_specialist/team_parent_registry above, there's no
        // recursion-prevention exclusion to worry about here; they're
        // registered directly onto `registry`, sharing one
        // `Arc<Mutex<MissionPlan>>` across the whole mission.
        let mission_plan = Arc::new(std::sync::Mutex::new(aivyx_types::MissionPlan {
            mission: String::new(),
            steps: vec![],
            summary: None,
        }));
        let mission_tools_config = aivyx_core::MissionToolsConfig {
            team: team.clone(),
            plan: mission_plan,
        };
        registry.register(Arc::new(aivyx_core::DecomposeTaskTool::new(
            mission_tools_config.clone(),
        )));
        registry.register(Arc::new(aivyx_core::VerifyOutputTool::new(
            mission_tools_config.clone(),
        )));
        registry.register(Arc::new(aivyx_core::SynthesizeResultsTool::new(
            mission_tools_config,
        )));

        // Nonagon-style specialist sessions (see
        // docs/superpowers/specs/2026-09-20-nonagon-team-specialist-sessions-design.md):
        // spawn_specialist/query_specialist/close_specialist, gated behind
        // the same [team] enabled flag as the tools above -- no new config
        // knob for on/off (the concurrent-session cap and idle timeout ARE
        // separately configurable, see TeamSettings). Additive alongside
        // delegate_to_specialist, not a replacement -- see that spec's
        // Decision 1. `team` and `team_parent_registry` get their final
        // move here -- nothing below this point reuses them.
        let specialist_session_pool = aivyx_core::SpecialistSessionPool::new(
            settings.team.max_concurrent_specialist_sessions,
            std::time::Duration::from_secs(settings.team.specialist_session_idle_timeout_secs),
        );
        let specialist_sessions_config = aivyx_core::SpecialistSessionsConfig {
            llm: Arc::clone(&llm),
            gate: Arc::clone(&gate),
            confiner: Arc::clone(&confiner),
            checkpointer: checkpointer.clone(),
            repo_map: repo_map.clone(),
            events_tx: events_tx.clone(),
            parent_registry: team_parent_registry,
            team,
            plan_mode: plan_mode.clone(),
            autonomous_mode: autonomous_mode.clone(),
            injection_taint: injection_taint.clone(),
            context_tokens: settings.backend.context_tokens,
            edit_format,
            verification: verification.clone(),
            max_iterations: settings.sub_agent.max_iterations,
            broker_mode,
            pool: specialist_session_pool,
        };
        registry.register(Arc::new(aivyx_core::SpawnSpecialistTool::new(
            specialist_sessions_config.clone(),
        )));
        registry.register(Arc::new(aivyx_core::QuerySpecialistTool::new(
            specialist_sessions_config.clone(),
        )));
        registry.register(Arc::new(aivyx_core::CloseSpecialistTool::new(
            specialist_sessions_config,
        )));
    }
```

- [ ] **Step 2: Build to verify**

Run: `cargo build -p aivyx`
Expected: PASS. (This is the step that would surface any missed clone/move — e.g. `team`/`team_parent_registry` used after their final move — as a compile error naming the exact line.)

- [ ] **Step 3: Update README's tools table**

In `README.md`, find the tools table row for `delegate_to_specialist` (search for that exact string) and the three mission-structure rows immediately after it (`decompose_task` / `verify_output` / `synthesize_results`, added by the prior phase). Add three new rows immediately after `synthesize_results`'s row:

```markdown
| `spawn_specialist` | start a resumable session with a team specialist (off by default, see `[team]`) | none (internal state only) |
| `query_specialist` | send a follow-up to an open specialist session | none (internal state only) |
| `close_specialist` | end an open specialist session | none (internal state only) |
```

- [ ] **Step 4: Update README's `[team]` config comment**

In `README.md`, find the `[team]` config section comment (search for "Nonagon-style team delegation and mission structure"). Replace it with:

```markdown
# Nonagon-style team delegation, mission structure, and specialist
# sessions (delegate_to_specialist, decompose_task, verify_output,
# synthesize_results, spawn_specialist, query_specialist,
# close_specialist): lets the lead delegate to a fixed specialist roster
# (implementer/reviewer/tester), narrower-scoped than delegate_task's
# sub-agent; record a mission plan / verification verdicts / a final
# synthesis against it; and open a resumable multi-exchange session with
# one specialist instead of a single delegate_to_specialist exchange. Off
# by default.
[team]
enabled = false
# max_concurrent_specialist_sessions = 3  # spawn_specialist sessions open at once
# specialist_session_idle_timeout_secs = 600  # auto-close an idle session after this long
```

- [ ] **Step 5: Full workspace verification**

Run: `cargo build --workspace`
Expected: PASS.

Run: `cargo test --workspace`
Expected: all tests PASS (aside from the pre-existing, unrelated `git_pr::tests::check_gh_authenticated_spawns_through_the_confiner` flake under full-suite parallelism if the `gh` CLI isn't installed in this environment — confirmed pre-existing/environment-only in the immediately prior phase's final review).

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: clean.

Run: `rustfmt --edition 2024 --check crates/aivyx/src/agent_builder.rs`
Expected: clean for the lines this task touched. If the file reports unrelated diffs elsewhere (this file has documented pre-existing rustfmt drift from before this initiative — see Task 3's progress notes in the prior Phase 3 plan), do **not** run a package- or whole-file reformat; only touch lines this task actually changed, and confirm via `git diff` that any reported rustfmt diff outside this task's own edits is pre-existing (not introduced by this change).

- [ ] **Step 6: Commit**

```bash
git add crates/aivyx/src/agent_builder.rs README.md
git commit -m "feat: register spawn_specialist/query_specialist/close_specialist when [team] enabled"
```

---

## Self-Review Notes

**Spec coverage:** Decision 1 (additive) → Task 3 registers new tools alongside, doesn't touch `delegate_to_specialist.rs`. Decision 2 (keyed pool, not single slot) → Task 2's `SpecialistSessionPool` uses `HashMap<String, _>`. Decision 3 (reuse `Agent::run_turn`'s resumability, no new `Agent` capability) → `run_bounded_exchange` calls `run_turn` on the same `&mut Agent` across multiple tool calls; `build_specialist_agent` mirrors `delegate_to_specialist::execute`'s construction exactly. Decision 4 (cap + idle timeout, configurable) → Task 1's `TeamSettings` fields, Task 2's `SpecialistSessionPool::new` params, Task 3's wiring from `settings.team.*`. Decision 5 (same `[team] enabled` gate) → Task 3 registers inside the existing block. "What this spec does not decide" items are all genuinely untouched (no peer-to-peer channel, no DAG, no TUI rendering, no persistence) — confirmed nothing in this plan touches `aivyx-tui` or adds session persistence.

**Global Constraints deviation, disclosed:** the design spec's lifecycle section said "a background idle-watcher task... polls every 30 seconds." This plan implements lazy eviction instead (`evict_stale()` called at the top of `take`/`insert_new`/`has_room`), matching `aivyx-mcp-server`'s own proven `SessionMap::evict_stale` pattern discovered while grounding this plan. The user-visible contract (an idle session eventually disappears) is preserved; only the internal mechanism is simpler. Documented here and in the plan's own Global Constraints section rather than silently diverging.

**Type consistency check:** `SpecialistSessionsConfig`'s field list (Task 2) matches exactly what Task 3's wiring code constructs — both list `llm, gate, confiner, checkpointer, repo_map, events_tx, parent_registry, team, plan_mode, autonomous_mode, injection_taint, context_tokens, edit_format, verification, max_iterations, broker_mode, pool` in the same order. `SpecialistSessionPool::new(max_concurrent: usize, idle_timeout: Duration)` (Task 2) matches Task 3's call site exactly (`settings.team.max_concurrent_specialist_sessions`, `Duration::from_secs(settings.team.specialist_session_idle_timeout_secs)`). Tool constructor names (`SpawnSpecialistTool::new`, `QuerySpecialistTool::new`, `CloseSpecialistTool::new`) match between Task 2's definitions and Task 3's registration calls.

**Placeholder scan:** no TBD/TODO; every code step shows complete, real code; no "similar to Task N" references (Task 3's registration block is written out in full, not referenced back to Task 2).
