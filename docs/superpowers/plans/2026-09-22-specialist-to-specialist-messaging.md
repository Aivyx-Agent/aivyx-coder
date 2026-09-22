# Specialist-to-Specialist Messaging Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A specialist whose `tool_allowlist` names `spawn_specialist`/`query_specialist`/`close_specialist` can open, query, and close sessions with peer specialists directly, without the lead relaying text between two separate calls — bounded to one hop of nesting, and scoped so no actor can query/close a session it didn't itself open.

**Architecture:** `SpecialistSessionsConfig` gains `spawn_depth: u32` and `caller: SessionOwner` (`Lead` or `Specialist(session_id)`). `build_specialist_agent` conditionally registers fresh `SpawnSpecialistTool`/`QuerySpecialistTool`/`CloseSpecialistTool` instances (bound to a depth-incremented, re-identified child config) onto a specialist's own attenuated registry, only when the member's `tool_allowlist` names them and depth allows it. Every `ParkedSpecialistSession`/`PersistedSpecialistSession` records its `owner`; `query_specialist`/`close_specialist` refuse to touch a session whose owner doesn't match the caller, always restoring the session untouched on rejection.

**Tech Stack:** Rust, existing `aivyx-core`/`aivyx-team`/`aivyx-types`/`aivyx` crates.

## Global Constraints

- `decompose_task`/`verify_output`/`synthesize_results` remain fully excluded from every specialist's registry, unchanged — this feature is scoped to the three session-management tools only.
- No TUI/ACP display change — `open_sessions()`/`SpecialistSessionsUpdated`/the merged ACP Plan panel show a specialist-opened session identically to a lead-opened one.
- `MAX_SPECIALIST_SPAWN_DEPTH` is a fixed constant (`1`), not configurable.
- A rejected ownership check (query or close, live or dehydrated) MUST leave the target session exactly as found — re-insert it before returning the error. A rejected check must never have the side effect of removing or disturbing a session.
- `PersistedSpecialistSession`'s new `owner` field is `#[serde(default)]`, defaulting to `SessionOwner::Lead` — a session file written before this feature existed must still load correctly.
- File-scoped `rustfmt --edition 2024 <path>` only. NEVER run `rustfmt` on `crates/aivyx-core/src/agent/mod.rs` or `crates/aivyx-core/src/agent/tests.rs` (external `mod` declarations cause a cascade into unrelated pre-existing code, confirmed repeatedly in this project's history) — this plan doesn't touch either file, but flagging in case any task needs to.

---

### Task 1: `SessionOwner` type + `owner` field on both session types

**Files:**
- Modify: `crates/aivyx-core/src/session.rs`
- Modify: `crates/aivyx-core/src/specialist_sessions.rs`
- Modify: `crates/aivyx-core/src/lib.rs`

**Interfaces:**
- Produces: `session::SessionOwner` (`Lead` or `Specialist(String)`, derives `Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default` with `#[default] Lead`) — consumed by Task 2, Task 3, Task 4.
- Produces: `PersistedSpecialistSession.owner: SessionOwner` (`#[serde(default)]`) — consumed by Task 3.
- Produces: `ParkedSpecialistSession.owner: SessionOwner` (private field, same module) — consumed by Task 3.
- Produces: `aivyx_core::SessionOwner` (re-exported) — consumed by Task 4 (`agent_builder.rs`).

- [ ] **Step 1: Add `SessionOwner` and the `owner` field to `PersistedSpecialistSession`**

In `crates/aivyx-core/src/session.rs`, find this exact block:

```rust
/// One parked specialist session's persisted state -- just enough to
/// rebuild it: which member it is, and its own conversation history.
/// Unlike `SessionState` (the lead's own persistence format), there's no
/// `last_active`: a dehydrated session doesn't expire from inactivity,
/// since nothing is consuming resources while it sits as inert JSON --
/// only a *live* session (rebuilt via `query_specialist`) is subject to
/// the idle-timeout eviction `SpecialistSessionPool` already has.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedSpecialistSession {
    pub session_id: String,
    pub member: String,
    pub history: Vec<Message>,
}
```

Replace it with:

```rust
/// Who opened a given specialist session -- the lead itself, or another
/// specialist (identified by ITS OWN session_id in the same pool).
/// `query_specialist`/`close_specialist` refuse to touch a session whose
/// `owner` doesn't match the calling `SpecialistSessionsConfig.caller`.
/// Defaults to `Lead` (via `#[serde(default)]` on the fields that use
/// it) so a `PersistedSpecialistSession` written before this feature
/// existed still loads correctly -- every session persisted then was
/// necessarily lead-opened, since specialists couldn't open sessions
/// before now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SessionOwner {
    #[default]
    Lead,
    Specialist(String),
}

/// One parked specialist session's persisted state -- just enough to
/// rebuild it: which member it is, its own conversation history, and who
/// opened it. Unlike `SessionState` (the lead's own persistence format),
/// there's no `last_active`: a dehydrated session doesn't expire from
/// inactivity, since nothing is consuming resources while it sits as
/// inert JSON -- only a *live* session (rebuilt via `query_specialist`)
/// is subject to the idle-timeout eviction `SpecialistSessionPool`
/// already has.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedSpecialistSession {
    pub session_id: String,
    pub member: String,
    pub history: Vec<Message>,
    #[serde(default)]
    pub owner: SessionOwner,
}
```

- [ ] **Step 2: Update `session.rs`'s own `PersistedSpecialistSession` construction sites and add tests**

In `crates/aivyx-core/src/session.rs`, find this exact block (inside `round_trips_through_disk`):

```rust
            vec![PersistedSpecialistSession {
                session_id: "abc-123".to_string(),
                member: "implementer".to_string(),
                history: vec![Message::text(Role::User, "implement the thing")],
            }],
        );
```

Replace it with:

```rust
            vec![PersistedSpecialistSession {
                session_id: "abc-123".to_string(),
                member: "implementer".to_string(),
                history: vec![Message::text(Role::User, "implement the thing")],
                owner: SessionOwner::Specialist("orchestrator-session-id".to_string()),
            }],
        );
```

Then find this exact block (further down, in the same test):

```rust
        assert_eq!(loaded.specialist_sessions[0].history.len(), 1);
        assert_eq!(
            loaded.specialist_sessions[0].history[0].text_content(),
            "implement the thing"
        );
    }
```

Replace it with:

```rust
        assert_eq!(loaded.specialist_sessions[0].history.len(), 1);
        assert_eq!(
            loaded.specialist_sessions[0].history[0].text_content(),
            "implement the thing"
        );
        assert_eq!(
            loaded.specialist_sessions[0].owner,
            SessionOwner::Specialist("orchestrator-session-id".to_string())
        );
    }

    #[test]
    fn a_persisted_specialist_session_predating_owner_still_loads_as_lead_owned() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.json");
        std::fs::write(
            &path,
            serde_json::json!({
                "version": 1,
                "history": [],
                "tasks": [],
                "specialist_sessions": [
                    { "session_id": "old-one", "member": "implementer", "history": [] }
                ]
            })
            .to_string(),
        )
        .unwrap();

        let loaded = load(&path).expect("pre-existing-field-free session should still load");

        assert_eq!(loaded.specialist_sessions.len(), 1);
        assert_eq!(loaded.specialist_sessions[0].owner, SessionOwner::Lead);
    }
```

- [ ] **Step 3: Run `session.rs`'s tests**

Run: `cargo test -p aivyx-core session:: -- --nocapture`
Expected: `round_trips_through_disk` (with its new owner assertion) and the new `a_persisted_specialist_session_predating_owner_still_loads_as_lead_owned` both pass, alongside every other existing test in this module.

- [ ] **Step 4: Add `owner` to `ParkedSpecialistSession` and thread it through `snapshot_for_persistence`**

In `crates/aivyx-core/src/specialist_sessions.rs`, find this exact line (the import):

```rust
use crate::session::PersistedSpecialistSession;
```

Replace it with:

```rust
use crate::session::{PersistedSpecialistSession, SessionOwner};
```

Find this exact block:

```rust
struct ParkedSpecialistSession {
    agent: Agent,
    member: String,
    forward_task: tokio::task::JoinHandle<()>,
    accumulated: Arc<Mutex<String>>,
    /// See `run_bounded_exchange`'s barrier-sync comment: send a oneshot
    /// reply channel here and await it to be sure `forward_task` has
    /// drained every event sent by the exchange that just finished before
    /// `accumulated` is read.
    barrier_tx: BarrierSender,
    last_active: Instant,
}
```

Replace it with:

```rust
struct ParkedSpecialistSession {
    agent: Agent,
    member: String,
    forward_task: tokio::task::JoinHandle<()>,
    accumulated: Arc<Mutex<String>>,
    /// See `run_bounded_exchange`'s barrier-sync comment: send a oneshot
    /// reply channel here and await it to be sure `forward_task` has
    /// drained every event sent by the exchange that just finished before
    /// `accumulated` is read.
    barrier_tx: BarrierSender,
    last_active: Instant,
    /// Who opened this session -- the lead, or a specialist (by its own
    /// session_id). `query_specialist`/`close_specialist` check this
    /// against the calling `SpecialistSessionsConfig.caller`.
    owner: SessionOwner,
}
```

Find this exact block (inside `snapshot_for_persistence`):

```rust
        out.extend(
            state
                .sessions
                .iter()
                .map(|(id, session)| PersistedSpecialistSession {
                    session_id: id.clone(),
                    member: session.member.clone(),
                    history: session.agent.history_snapshot(),
                }),
        );
```

Replace it with:

```rust
        out.extend(
            state
                .sessions
                .iter()
                .map(|(id, session)| PersistedSpecialistSession {
                    session_id: id.clone(),
                    member: session.member.clone(),
                    history: session.agent.history_snapshot(),
                    owner: session.owner.clone(),
                }),
        );
```

- [ ] **Step 5: Add `spawn_depth`/`caller` to `SpecialistSessionsConfig` and fix every construction site**

Find this exact block:

```rust
#[derive(Clone)]
pub struct SpecialistSessionsConfig {
    pub llm: Arc<dyn LlmBackend>,
    pub enforcement: crate::specialist_enforcement::SpecialistEnforcementIngredients,
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

Replace it with:

```rust
#[derive(Clone)]
pub struct SpecialistSessionsConfig {
    pub llm: Arc<dyn LlmBackend>,
    pub enforcement: crate::specialist_enforcement::SpecialistEnforcementIngredients,
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
    /// How many specialist-initiated (not lead-initiated) spawn hops led
    /// to this config being used -- `0` for the lead's own top-level
    /// config. Only increments when a *specialist* does the spawning;
    /// being spawned by the lead doesn't itself count as a hop. See
    /// `build_specialist_agent`'s own doc comment for how this bounds
    /// nesting.
    pub spawn_depth: u32,
    /// Who this config acts on behalf of -- `Lead` for the lead's own
    /// top-level config, `Specialist(own_session_id)` for a specialist's
    /// own child config. Tags every session this config's tools open,
    /// and is checked against a target session's own `owner` before
    /// `query_specialist`/`close_specialist` touch it.
    pub caller: SessionOwner,
}
```

Now find every place in this file that constructs a `ParkedSpecialistSession` literal and add `owner: self.config.caller.clone()` (for the freshly-spawned case) or `owner: persisted.owner` (for the rehydrated-from-dehydrated case). There are exactly two such literals in this file today -- search for `ParkedSpecialistSession {` to find both (one inside `SpawnSpecialistTool::execute`, one inside `QuerySpecialistTool::execute`'s dehydration branch). For each, add the `owner` field as the last field in the literal, matching the two cases above. Do not change anything else about either literal yet -- Task 2 handles `build_specialist_agent`'s own signature change, Task 3 handles ownership *checks*.

- [ ] **Step 6: Update the test module's shared `config()` helper**

In `crates/aivyx-core/src/specialist_sessions.rs`'s `#[cfg(test)] mod tests` block, find the `config()` helper function (search for `fn config(`) and add the two new fields to its returned `SpecialistSessionsConfig` literal: `spawn_depth: 0` and `caller: SessionOwner::Lead` -- matching what the real lead's own config will have (Task 4 wires this in `agent_builder.rs`). Read the function's exact current body first to place these correctly among its existing fields.

- [ ] **Step 7: Add `SessionOwner` to `aivyx-core`'s public re-exports**

In `crates/aivyx-core/src/lib.rs`, find this exact line:

```rust
pub use session::{SessionState, Task, TaskStatus};
```

Replace it with:

```rust
pub use session::{SessionOwner, SessionState, Task, TaskStatus};
```

- [ ] **Step 8: Verify the crate compiles and its existing tests still pass**

Run: `cargo test -p aivyx-core specialist_sessions:: -- --nocapture`
Expected: every existing test in this module compiles and passes (the two `ParkedSpecialistSession` literals now have an `owner` field; no test yet asserts on ownership behavior -- that's Task 3).

- [ ] **Step 9: Format and commit**

```bash
rustfmt --edition 2024 crates/aivyx-core/src/session.rs
rustfmt --edition 2024 crates/aivyx-core/src/specialist_sessions.rs
rustfmt --edition 2024 crates/aivyx-core/src/lib.rs
git add crates/aivyx-core/src/session.rs crates/aivyx-core/src/specialist_sessions.rs crates/aivyx-core/src/lib.rs
git commit -m "feat: SessionOwner type + owner field on specialist sessions"
```

---

### Task 2: Depth-limited registry restructuring in `build_specialist_agent`

**Files:**
- Modify: `crates/aivyx-core/src/specialist_sessions.rs`

**Interfaces:**
- Consumes: `SpecialistSessionsConfig.spawn_depth`/`caller` (Task 1).
- Produces: `build_specialist_agent`'s new 4-argument signature (`member, config, cwd, own_session_id: &str`) -- consumed by Task 3 (its two call sites already exist, from Task 1's grounding; Task 3 doesn't change this signature further).

- [ ] **Step 1: Add the depth-cap constant**

In `crates/aivyx-core/src/specialist_sessions.rs`, find this exact line:

```rust
const NO_TEXT_RESPONSE: &str = "(the specialist produced no text response)";
```

Replace it with:

```rust
const NO_TEXT_RESPONSE: &str = "(the specialist produced no text response)";

/// A specialist's own registry gets `spawn_specialist`/`query_specialist`/
/// `close_specialist` (if its `tool_allowlist` names them) only when its
/// own `SpecialistSessionsConfig.spawn_depth` is below this. Bounds
/// specialist-initiated nesting to one hop: a specialist the lead spawns
/// directly (depth 0) may spawn/query/close peers; a specialist spawned
/// BY that specialist (depth 1) gets no such tools registered at all,
/// regardless of its own `tool_allowlist`. Unlike `delegate_task`'s own
/// recursion prevention (which works by never registering the tool at
/// the top level, since a sub-agent's registry there is a one-shot
/// snapshot), a specialist's own registry here is built once at spawn
/// time and reused for its whole life, so the cap has to be a depth
/// check at registration time rather than a structural absence from a
/// shared snapshot.
const MAX_SPECIALIST_SPAWN_DEPTH: u32 = 1;
```

- [ ] **Step 2: Restructure `build_specialist_agent`**

Find this exact block:

```rust
fn build_specialist_agent(
    member: &aivyx_team::TeamMember,
    config: &SpecialistSessionsConfig,
    cwd: &std::path::Path,
) -> (
    Agent,
    tokio::task::JoinHandle<()>,
    Arc<Mutex<String>>,
    BarrierSender,
) {
    let specialist_registry = compute_specialist_registry(member, &config.parent_registry);
    let (gate, confiner) =
        crate::specialist_enforcement::scoped_gate_and_confiner(&config.enforcement, member, cwd);
```

Replace it with:

```rust
fn build_specialist_agent(
    member: &aivyx_team::TeamMember,
    config: &SpecialistSessionsConfig,
    cwd: &std::path::Path,
    own_session_id: &str,
) -> (
    Agent,
    tokio::task::JoinHandle<()>,
    Arc<Mutex<String>>,
    BarrierSender,
) {
    let mut specialist_registry = compute_specialist_registry(member, &config.parent_registry);
    // `compute_specialist_registry`'s own snapshot (`config.parent_registry`)
    // structurally excludes spawn_specialist/query_specialist/
    // close_specialist/the three mission tools/delegate_to_specialist --
    // that exclusion is unchanged. This is a SEPARATE, additive
    // registration step: give the specialist fresh instances of the
    // three session tools, bound to ITS OWN child config, only if its
    // `tool_allowlist` names them and depth allows it. See
    // `MAX_SPECIALIST_SPAWN_DEPTH`'s own doc comment for why this is a
    // depth check here rather than a structural absence.
    if config.spawn_depth < MAX_SPECIALIST_SPAWN_DEPTH {
        let child_config = SpecialistSessionsConfig {
            spawn_depth: config.spawn_depth + 1,
            caller: SessionOwner::Specialist(own_session_id.to_string()),
            ..config.clone()
        };
        if member
            .tool_allowlist
            .iter()
            .any(|t| t == "spawn_specialist")
        {
            specialist_registry.register(Arc::new(SpawnSpecialistTool::new(child_config.clone())));
        }
        if member
            .tool_allowlist
            .iter()
            .any(|t| t == "query_specialist")
        {
            specialist_registry.register(Arc::new(QuerySpecialistTool::new(child_config.clone())));
        }
        if member.tool_allowlist.iter().any(|t| t == "close_specialist") {
            specialist_registry.register(Arc::new(CloseSpecialistTool::new(child_config)));
        }
    }
    let (gate, confiner) =
        crate::specialist_enforcement::scoped_gate_and_confiner(&config.enforcement, member, cwd);
```

- [ ] **Step 3: Update both existing call sites of `build_specialist_agent`**

In `SpawnSpecialistTool::execute`, find this exact block:

```rust
        let (mut agent, forward_task, accumulated, barrier_tx) =
            build_specialist_agent(member, &self.config, &ctx.cwd);
        let output = run_bounded_exchange(
            &mut agent,
            args.task,
            ctx,
            &self.config,
            &accumulated,
            &barrier_tx,
        )
        .await;

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
            barrier_tx,
            last_active: Instant::now(),
            owner: self.config.caller.clone(),
        };
```

Replace it with:

```rust
        let session_id = uuid::Uuid::new_v4().to_string();
        let (mut agent, forward_task, accumulated, barrier_tx) =
            build_specialist_agent(member, &self.config, &ctx.cwd, &session_id);
        let output = run_bounded_exchange(
            &mut agent,
            args.task,
            ctx,
            &self.config,
            &accumulated,
            &barrier_tx,
        )
        .await;

        let ToolOutput::Ok(text) = output else {
            drop(agent);
            let _ = forward_task.await;
            return Ok(output);
        };

        let session = ParkedSpecialistSession {
            agent,
            member: member.name.clone(),
            forward_task,
            accumulated,
            barrier_tx,
            last_active: Instant::now(),
            owner: self.config.caller.clone(),
        };
```

(This is the reorder from the spec's Decision 2: `session_id` is now generated *before* `build_specialist_agent` is called, so it can be threaded through as the specialist's own identity for any child config it might get.)

In `QuerySpecialistTool::execute`'s dehydration branch, find this exact line:

```rust
            let (mut agent, forward_task, accumulated, barrier_tx) =
                build_specialist_agent(member, &self.config, &ctx.cwd);
```

Replace it with:

```rust
            let (mut agent, forward_task, accumulated, barrier_tx) =
                build_specialist_agent(member, &self.config, &ctx.cwd, &args.session_id);
```

- [ ] **Step 4: Add a test proving the depth cap end-to-end**

In `crates/aivyx-core/src/specialist_sessions.rs`'s test module, add:

```rust
fn team_with_spawn_specialist_allowlisted() -> TeamConfig {
    use aivyx_team::TeamMember;
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
                name: "orchestrator".to_string(),
                role: "Orchestrator".to_string(),
                persona: "You coordinate peer specialists.".to_string(),
                tool_allowlist: vec!["spawn_specialist".to_string()],
                extra_deny_paths: vec![],
            },
            TeamMember {
                name: "worker".to_string(),
                role: "Worker".to_string(),
                persona: "You do focused work.".to_string(),
                tool_allowlist: vec!["spawn_specialist".to_string()],
                extra_deny_paths: vec![],
            },
        ],
    }
}

#[tokio::test]
async fn a_specialists_own_nesting_is_capped_at_one_hop() {
    let mock = Arc::new(MockBackend::new(vec![
        // orchestrator's own first exchange: spawns "worker" as a peer.
        vec![
            StreamEvent::ToolCallComplete(aivyx_types::ToolCall {
                id: aivyx_types::ToolCallId("c1".to_string()),
                name: "spawn_specialist".to_string(),
                arguments: serde_json::json!({
                    "member": "worker",
                    "task": "try to spawn a third specialist",
                }),
                source: aivyx_types::ToolCallSource::Native,
            }),
            StreamEvent::Done {
                finish_reason: FinishReason::Stop,
            },
        ],
        // worker's own first exchange: attempts to spawn ANOTHER peer.
        // worker was itself spawned by a specialist (depth 1), so its
        // own registry must not include spawn_specialist at all,
        // regardless of its tool_allowlist naming it -- this call must
        // fail as an unknown tool, not actually create a third session.
        vec![
            StreamEvent::ToolCallComplete(aivyx_types::ToolCall {
                id: aivyx_types::ToolCallId("c2".to_string()),
                name: "spawn_specialist".to_string(),
                arguments: serde_json::json!({
                    "member": "orchestrator",
                    "task": "this must never actually run",
                }),
                source: aivyx_types::ToolCallSource::Native,
            }),
            StreamEvent::Done {
                finish_reason: FinishReason::Stop,
            },
        ],
    ]));

    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let pool = SpecialistSessionPool::new(5, Duration::from_secs(600));
    let mut cfg = config(mock, tx, team_with_spawn_specialist_allowlisted(), pool.clone());
    cfg.max_iterations = 1;

    let tool = SpawnSpecialistTool::new(cfg);
    let ctx = exec_ctx(std::path::Path::new("."));
    let result = tool
        .execute(
            serde_json::json!({ "member": "orchestrator", "task": "spawn a worker peer" }),
            &ctx,
        )
        .await
        .unwrap();

    assert!(
        matches!(result, ToolOutput::Ok(_)),
        "expected Ok, got {result:?}"
    );
    assert_eq!(
        pool.open_sessions().len(),
        2,
        "expected exactly 2 open sessions (orchestrator + worker) -- if this is 3, worker's \
        own attempted nested spawn succeeded, violating the depth-1 cap"
    );
}
```

(Check the exact current `config(...)` test helper's parameter order/shape before finalizing this call -- it should already match the existing `config(llm, events_tx, team, pool)` shape used throughout this file's other tests, per Task 1 Step 6's own update. `MockBackend::new` takes `Vec<Vec<StreamEvent>>`, one inner `Vec` consumed per `stream_chat` call, in order -- confirmed by this file's own pre-existing `a_specialists_own_extra_deny_paths_blocks_a_write_via_spawn_specialist` test, which this test's shape directly mirrors.)

- [ ] **Step 5: Run the tests**

Run: `cargo test -p aivyx-core specialist_sessions:: -- --nocapture`
Expected: all tests in this module pass, including the new depth-cap test. If the mock response sequencing doesn't line up exactly as expected (e.g. an extra `run_turn` iteration is needed), adjust `cfg.max_iterations` or the number of queued responses -- the assertion itself (`pool.open_sessions().len() == 2`) is the fixed, correct target; the exact mock plumbing to reach it may need a small adjustment against the real `Agent::run_turn`/`last_turn_paused` behavior.

- [ ] **Step 6: Format and commit**

```bash
rustfmt --edition 2024 crates/aivyx-core/src/specialist_sessions.rs
git add crates/aivyx-core/src/specialist_sessions.rs
git commit -m "feat: depth-limited spawn_specialist/query_specialist/close_specialist for specialists"
```

---

### Task 3: Ownership enforcement in `query_specialist`/`close_specialist`

**Files:**
- Modify: `crates/aivyx-core/src/specialist_sessions.rs`

**Interfaces:**
- Consumes: `ParkedSpecialistSession.owner`, `PersistedSpecialistSession.owner`, `SpecialistSessionsConfig.caller` (Task 1).

- [ ] **Step 1: Add a shared ownership-error helper**

Find this exact block:

```rust
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
```

Replace it with:

```rust
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

/// Shared by `query_specialist`/`close_specialist`'s ownership checks --
/// a session's `owner` not matching the calling config's `caller`.
fn ownership_error(session_id: &str) -> ToolOutput {
    ToolOutput::Error(format!(
        "session_id {session_id:?} was not opened by you -- only its own opener can query or \
        close it"
    ))
}
```

- [ ] **Step 2: Enforce ownership in `QuerySpecialistTool::execute`**

Find this exact block:

```rust
        let mut session = if let Some(session) = self.config.pool.take(&args.session_id) {
            session
        } else if let Some(persisted) = self.config.pool.take_dehydrated(&args.session_id) {
            let Some(member) = self
                .config
                .team
                .members
                .iter()
                .find(|m| m.name == persisted.member)
            else {
                return Ok(ToolOutput::Error(format!(
                    "cannot resume session_id {:?}: its specialist {:?} no longer exists in \
                    the current team roster -- the session has been discarded; call \
                    spawn_specialist with a valid member instead",
                    args.session_id, persisted.member
                )));
            };
            let (mut agent, forward_task, accumulated, barrier_tx) =
                build_specialist_agent(member, &self.config, &ctx.cwd, &args.session_id);
            agent.restore_history(persisted.history);
            ParkedSpecialistSession {
                agent,
                member: member.name.clone(),
                forward_task,
                accumulated,
                barrier_tx,
                last_active: Instant::now(),
                owner: persisted.owner,
            }
        } else {
            return Ok(ToolOutput::Error(format!(
                "unknown session_id: {:?} -- it may not exist, may already be closed, or may \
                have expired from inactivity; call spawn_specialist to start a new one",
                args.session_id
            )));
        };
```

Replace it with:

```rust
        let mut session = if let Some(session) = self.config.pool.take(&args.session_id) {
            if session.owner != self.config.caller {
                self.config.pool.put_back(args.session_id.clone(), session);
                return Ok(ownership_error(&args.session_id));
            }
            session
        } else if let Some(persisted) = self.config.pool.take_dehydrated(&args.session_id) {
            if persisted.owner != self.config.caller {
                self.config.pool.seed_dehydrated(vec![persisted]);
                return Ok(ownership_error(&args.session_id));
            }
            let Some(member) = self
                .config
                .team
                .members
                .iter()
                .find(|m| m.name == persisted.member)
            else {
                return Ok(ToolOutput::Error(format!(
                    "cannot resume session_id {:?}: its specialist {:?} no longer exists in \
                    the current team roster -- the session has been discarded; call \
                    spawn_specialist with a valid member instead",
                    args.session_id, persisted.member
                )));
            };
            let (mut agent, forward_task, accumulated, barrier_tx) =
                build_specialist_agent(member, &self.config, &ctx.cwd, &args.session_id);
            agent.restore_history(persisted.history);
            ParkedSpecialistSession {
                agent,
                member: member.name.clone(),
                forward_task,
                accumulated,
                barrier_tx,
                last_active: Instant::now(),
                owner: persisted.owner,
            }
        } else {
            return Ok(ToolOutput::Error(format!(
                "unknown session_id: {:?} -- it may not exist, may already be closed, or may \
                have expired from inactivity; call spawn_specialist to start a new one",
                args.session_id
            )));
        };
```

- [ ] **Step 3: Enforce ownership in `CloseSpecialistTool::execute`**

Find this exact block:

```rust
        if let Some(session) = self.config.pool.take(&args.session_id) {
            let member = session.member.clone();
            drop(session.agent);
            let _ = session.forward_task.await;

            let _ = self
                .config
                .events_tx
                .send(AgentEvent::SpecialistSessionsUpdated(
                    self.config.pool.open_sessions(),
                ));

            return Ok(ToolOutput::Ok(format!(
                "specialist session {:?} closed ({member})",
                args.session_id
            )));
        }

        if let Some(persisted) = self.config.pool.take_dehydrated(&args.session_id) {
            return Ok(ToolOutput::Ok(format!(
                "specialist session {:?} closed ({}) -- it was dehydrated from a previous \
                run and had not been resumed",
                args.session_id, persisted.member
            )));
        }

        Ok(ToolOutput::Error(format!(
            "unknown session_id: {:?} -- it may not exist or may already be closed",
            args.session_id
        )))
```

Replace it with:

```rust
        if let Some(session) = self.config.pool.take(&args.session_id) {
            if session.owner != self.config.caller {
                self.config.pool.put_back(args.session_id.clone(), session);
                return Ok(ownership_error(&args.session_id));
            }
            let member = session.member.clone();
            drop(session.agent);
            let _ = session.forward_task.await;

            let _ = self
                .config
                .events_tx
                .send(AgentEvent::SpecialistSessionsUpdated(
                    self.config.pool.open_sessions(),
                ));

            return Ok(ToolOutput::Ok(format!(
                "specialist session {:?} closed ({member})",
                args.session_id
            )));
        }

        if let Some(persisted) = self.config.pool.take_dehydrated(&args.session_id) {
            if persisted.owner != self.config.caller {
                self.config.pool.seed_dehydrated(vec![persisted]);
                return Ok(ownership_error(&args.session_id));
            }
            return Ok(ToolOutput::Ok(format!(
                "specialist session {:?} closed ({}) -- it was dehydrated from a previous \
                run and had not been resumed",
                args.session_id, persisted.member
            )));
        }

        Ok(ToolOutput::Error(format!(
            "unknown session_id: {:?} -- it may not exist or may already be closed",
            args.session_id
        )))
```

- [ ] **Step 4: Add tests**

In `crates/aivyx-core/src/specialist_sessions.rs`'s test module, add:

```rust
#[tokio::test]
async fn query_specialist_refuses_a_live_session_it_did_not_open_and_leaves_it_intact() {
    let llm = Arc::new(MockBackend::new(vec![
        text_response("hello"),
        text_response("hi again"),
    ]));
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let pool = SpecialistSessionPool::new(3, Duration::from_secs(600));
    let lead_cfg = config(Arc::clone(&llm), tx.clone(), simple_team(), pool.clone());
    let spawn_tool = SpawnSpecialistTool::new(lead_cfg.clone());
    let ctx = exec_ctx(std::path::Path::new("."));

    let spawn_output = spawn_tool
        .execute(
            serde_json::json!({ "member": "implementer", "task": "do something" }),
            &ctx,
        )
        .await
        .unwrap();
    let ToolOutput::Ok(text) = spawn_output else {
        panic!("expected Ok, got {spawn_output:?}");
    };
    let session_id = text
        .lines()
        .next()
        .unwrap()
        .strip_prefix("session_id: ")
        .unwrap()
        .to_string();

    let mut impostor_cfg = lead_cfg.clone();
    impostor_cfg.caller = SessionOwner::Specialist("some-other-specialist".to_string());
    let query_tool = QuerySpecialistTool::new(impostor_cfg);
    let rejected = query_tool
        .execute(
            serde_json::json!({ "session_id": session_id, "message": "follow up" }),
            &ctx,
        )
        .await
        .unwrap();
    match rejected {
        ToolOutput::Error(msg) => assert!(msg.contains("was not opened by you")),
        other => panic!("expected an ownership error, got {other:?}"),
    }

    // The session must survive the rejected attempt: its real owner (the
    // lead) can still query it afterward.
    let query_tool_as_owner = QuerySpecialistTool::new(lead_cfg);
    let allowed = query_tool_as_owner
        .execute(
            serde_json::json!({ "session_id": session_id, "message": "follow up" }),
            &ctx,
        )
        .await
        .unwrap();
    assert!(
        matches!(allowed, ToolOutput::Ok(_)),
        "the session's real owner must still be able to query it after a rejected attempt \
        from someone else, expected Ok got {allowed:?}"
    );
}

#[tokio::test]
async fn close_specialist_refuses_a_dehydrated_session_it_did_not_open_and_leaves_it_intact() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let pool = SpecialistSessionPool::new(3, Duration::from_secs(600));
    pool.seed_dehydrated(vec![PersistedSpecialistSession {
        session_id: "old-session".to_string(),
        member: "implementer".to_string(),
        history: vec![],
        owner: SessionOwner::Specialist("orchestrator-session".to_string()),
    }]);
    let llm = Arc::new(MockBackend::new(vec![]));
    let mut impostor_cfg = config(llm, tx, simple_team(), pool.clone());
    impostor_cfg.caller = SessionOwner::Lead;
    let close_tool = CloseSpecialistTool::new(impostor_cfg);
    let ctx = exec_ctx(std::path::Path::new("."));

    let rejected = close_tool
        .execute(serde_json::json!({ "session_id": "old-session" }), &ctx)
        .await
        .unwrap();
    match rejected {
        ToolOutput::Error(msg) => assert!(msg.contains("was not opened by you")),
        other => panic!("expected an ownership error, got {other:?}"),
    }

    // The record must survive the rejected attempt.
    assert_eq!(
        pool.snapshot_for_persistence().len(),
        1,
        "a rejected close must not discard the dehydrated record"
    );
}
```

(Check the exact `text_response`/`config`/`simple_team`/`exec_ctx` signatures already used elsewhere in this file's test module before finalizing -- these should already match Task 1/2's own established shapes.)

- [ ] **Step 5: Run the tests**

Run: `cargo test -p aivyx-core specialist_sessions:: -- --nocapture`
Expected: all tests pass, including the two new ownership tests.

- [ ] **Step 6: Format and commit**

```bash
rustfmt --edition 2024 crates/aivyx-core/src/specialist_sessions.rs
git add crates/aivyx-core/src/specialist_sessions.rs
git commit -m "feat: query_specialist/close_specialist enforce session ownership"
```

---

### Task 4: `agent_builder.rs` wiring

**Files:**
- Modify: `crates/aivyx/src/agent_builder.rs`

**Interfaces:**
- Consumes: `aivyx_core::SessionOwner` (Task 1), `SpecialistSessionsConfig.spawn_depth`/`caller` (Task 1).

- [ ] **Step 1: Give the lead's own config `spawn_depth: 0`/`caller: SessionOwner::Lead`**

In `crates/aivyx/src/agent_builder.rs`, find this exact block:

```rust
        let specialist_sessions_config = aivyx_core::SpecialistSessionsConfig {
            llm: Arc::clone(&llm),
            enforcement: specialist_enforcement_ingredients,
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
            pool: specialist_session_pool_handle,
        };
```

Replace it with:

```rust
        let specialist_sessions_config = aivyx_core::SpecialistSessionsConfig {
            llm: Arc::clone(&llm),
            enforcement: specialist_enforcement_ingredients,
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
            pool: specialist_session_pool_handle,
            spawn_depth: 0,
            caller: aivyx_core::SessionOwner::Lead,
        };
```

- [ ] **Step 2: Make `spawn_specialist`/`query_specialist`/`close_specialist` legitimately assignable in a `tool_allowlist`**

Find this exact block:

```rust
        let team_registry_definitions = registry.definitions();
        let available_tools: Vec<&str> = team_registry_definitions
            .iter()
            .map(|d| d.name.as_str())
            .collect();
        let team = resolve_team_config(settings, &available_tools)?;
```

Replace it with:

```rust
        let team_registry_definitions = registry.definitions();
        let mut available_tools: Vec<&str> = team_registry_definitions
            .iter()
            .map(|d| d.name.as_str())
            .collect();
        // spawn_specialist/query_specialist/close_specialist aren't
        // registered on `registry` until further down (see
        // team_parent_registry's own comment above), but this feature
        // makes them legitimately assignable to a specialist's own
        // tool_allowlist (see
        // docs/superpowers/specs/2026-09-22-specialist-to-specialist-messaging-design.md)
        // -- added here explicitly so they validate-pass even though
        // they're not literally present in this snapshot.
        available_tools.extend_from_slice(&[
            "spawn_specialist",
            "query_specialist",
            "close_specialist",
        ]);
        let team = resolve_team_config(settings, &available_tools)?;
```

- [ ] **Step 3: Update `team_parent_registry`'s own doc comment**

Find this exact sentence inside the large comment block above `let mut team_parent_registry = registry.clone();` (search for `"a phase adding custom rosters will need to revisit where this snapshot is taken if specialists should ever be granted them"`):

```
    // close_specialist are registered further down, a specialist's own
    // attenuated registry can never include any of those six tools either,
    // even if a future custom roster's tool_allowlist tried to name them
    // -- a phase adding custom rosters will need to revisit where this
    // snapshot is taken if specialists should ever be granted them.
```

Replace it with:

```
    // close_specialist are registered further down, a specialist's own
    // attenuated registry can never include any of those six tools
    // either via THIS snapshot -- decompose_task/verify_output/
    // synthesize_results/delegate_to_specialist stay fully excluded this
    // way. spawn_specialist/query_specialist/close_specialist are the
    // one exception: `build_specialist_agent` (specialist_sessions.rs)
    // separately, additively registers fresh instances of those three
    // directly onto a specialist's own registry -- bound to a
    // depth-incremented child config, gated on the member's own
    // tool_allowlist and a hop-count cap -- see that function's own doc
    // comment and
    // docs/superpowers/specs/2026-09-22-specialist-to-specialist-messaging-design.md.
    // This snapshot itself is untouched by that; it's a second,
    // independent mechanism layered on top.
```

- [ ] **Step 4: Verify `aivyx` compiles**

Run: `cargo check -p aivyx`
Expected: compiles cleanly.

- [ ] **Step 5: Build/test/lint the full workspace**

Run:
```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```
Expected: all clean, zero failures, zero warnings.

- [ ] **Step 6: Commit**

```bash
git add crates/aivyx/src/agent_builder.rs
git commit -m "feat: wire specialist-to-specialist messaging into agent_builder"
```

---

## Self-Review Notes

**Spec coverage:** Decision 1 (`SpecialistSessionsConfig.spawn_depth`/`caller`) → Task 1 Step 5. Decision 2 (`SpawnSpecialistTool::execute` reordered to generate `session_id` before `build_specialist_agent`) → Task 2 Step 3. Decision 3 (`build_specialist_agent`'s new conditional registration, depth cap) → Task 2 Steps 1-2. Decision 4 (ownership enforcement, put-back-on-rejection in both tools, both live/dehydrated paths) → Task 3. Decision 5 (`PersistedSpecialistSession.owner`, `#[serde(default)]` to `Lead`) → Task 1 Steps 1-2. Decision 6 (`available_tools` gains the three literal names) → Task 4 Step 2. "What this spec does not decide" items are all genuinely untouched: `decompose_task`/`verify_output`/`synthesize_results` stay excluded (never added to `build_specialist_agent`'s new registration step), no TUI/ACP change (`open_sessions()`/`SpecialistSessionSummary` untouched by any task), `MAX_SPECIALIST_SPAWN_DEPTH` is a fixed `const`, no self-spawn restriction added beyond the pre-existing "cannot spawn the lead" check, `max_concurrent_specialist_sessions`'s semantics untouched.

**Global Constraints deviation:** none — every constraint is directly implemented by name (put-back-on-rejection in both tools' both branches, `#[serde(default)]` on `owner`, fixed depth constant, no TUI/ACP change, file-scoped `rustfmt` only, `agent/mod.rs`/`agent/tests.rs` never touched by this plan).

**Placeholder scan:** no TBD/TODO; every step shows complete, real code. Task 2 Step 5's note about possibly adjusting mock response counts is an explicit, bounded contingency (with a fixed, correct target assertion), not an open-ended placeholder — matching this project's own established plan-writing precedent for the one class of detail a plan author can't 100% pin without actually running the mocked async exchange.

**Type/interface consistency check:** `SessionOwner::{Lead, Specialist(String)}` (Task 1) is constructed identically in every later task (Task 2's depth-cap test doesn't construct it directly but exercises it via real tool calls; Task 3's tests construct `SessionOwner::Specialist("...")`/`SessionOwner::Lead` directly; Task 4's `agent_builder.rs` wiring uses `aivyx_core::SessionOwner::Lead`, matching the Task 1 Step 7 re-export). `build_specialist_agent`'s 4-argument signature (Task 2 Step 2) matches both call sites updated in Task 2 Step 3, and Task 3 doesn't change this signature further (only wraps the branch that calls it with new ownership checks before/around the existing call). `ownership_error(session_id: &str) -> ToolOutput` (Task 3 Step 1) is called identically at all four sites added in Task 3 Steps 2-3. `SpecialistSessionsConfig`'s two new fields (Task 1 Step 5) are set consistently: `spawn_depth: 0, caller: SessionOwner::Lead` for the lead's own top-level config (Task 4 Step 1, and the test helper in Task 1 Step 6), `spawn_depth: config.spawn_depth + 1, caller: SessionOwner::Specialist(own_session_id)` for a specialist's own child config (Task 2 Step 2).
