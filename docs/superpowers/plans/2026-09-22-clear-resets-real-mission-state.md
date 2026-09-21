# `/clear` Resets Real Mission/Specialist-Session State Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `/clear` resets the *real* `MissionPlan`/`SpecialistSessionPool` state (not just the TUI's displayed mirror), so a later `decompose_task`/`spawn_specialist` call after a clear never resurrects stale pre-clear content.

**Architecture:** `SpecialistSessionPool` gains a `close_all` method. `Agent` gains two new optional fields (mirroring the existing `set_repo_map`/`set_injection_taint` pattern) pointing at the *same* `MissionPlan`/pool instances `agent_builder.rs` already threads to `BuiltAgent`. `clear_conversation()` resets both, if present.

**Tech Stack:** Rust, existing `aivyx-core`/`aivyx-types` crates.

## Global Constraints

- No change to ACP — it has no `/clear` path today, confirmed out of scope.
- No `AgentEvent` change — the TUI's existing `ConversationCleared` handler already resets the display correctly; only the real backing state needs fixing.
- `clear_conversation()` stays synchronous (not `async`) — `close_all` must not require awaiting anything.
- No change to `close_specialist`'s own single-session close path, or to `decompose_task`/`verify_output`/`synthesize_results`'s own logic.
- File-scoped `rustfmt --edition 2024 --check <path>` / `rustfmt --edition 2024 <path>` only — **never** a package-scoped `cargo fmt -p <crate>` command with no file argument, per this project's own repeated, documented incident history.

---

### Task 1: `SpecialistSessionPool::close_all`

**Files:**
- Modify: `crates/aivyx-core/src/specialist_sessions.rs`

**Interfaces:**
- Produces: `SpecialistSessionPool::close_all(&self)` — consumed by Task 2's `clear_conversation()`.

- [ ] **Step 1: Add `close_all`**

In `crates/aivyx-core/src/specialist_sessions.rs`, find this exact block:

```rust
    pub fn max_concurrent(&self) -> usize {
        self.inner.lock().unwrap().max_concurrent
    }
```

Replace it with:

```rust
    pub fn max_concurrent(&self) -> usize {
        self.inner.lock().unwrap().max_concurrent
    }

    /// Drops every currently-parked session at once, closing each one's
    /// underlying `Agent`. Unlike `CloseSpecialistTool`'s own
    /// single-session close path (which explicitly `.await`s the closed
    /// session's `forward_task` before reporting success to the model),
    /// this doesn't await anything: dropping a parked session's `Agent`
    /// closes the event channel its background forwarding task reads
    /// from, so that task's next `.recv()` call returns `None` and it
    /// ends on its own -- correct without needing an `async` signature
    /// here, which matters since this is called from
    /// `Agent::clear_conversation`, a synchronous method. Used by
    /// `/clear` so a specialist session never stays queryable against a
    /// lead conversation that's just been wiped.
    pub fn close_all(&self) {
        self.inner.lock().unwrap().sessions.clear();
    }
```

- [ ] **Step 2: Add tests**

In `crates/aivyx-core/src/specialist_sessions.rs`'s test module, find the existing test that constructs a session via `spawn_specialist`/`build_specialist_agent` and closes it (search for a test using `CloseSpecialistTool` or checking `open_sessions()` after a close) to confirm the exact helper functions already available (`config(...)`, `simple_team()`, `MockBackend`, etc.), then add:

```rust
    #[tokio::test]
    async fn close_all_removes_every_open_session() {
        let mock: Arc<dyn LlmBackend> = Arc::new(MockBackend::new(vec![text_response("hello")]));
        let (events_tx, _events_rx) = tokio::sync::mpsc::unbounded_channel();
        let pool = SpecialistSessionPool::new(3, Duration::from_secs(600));
        let team = simple_team();
        let cfg = config(Arc::clone(&mock), events_tx.clone(), team.clone(), pool.clone());

        let spawn_tool = SpawnSpecialistTool::new(cfg.clone());
        for _ in 0..2 {
            spawn_tool
                .execute(
                    serde_json::json!({ "member": "implementer", "task": "do something" }),
                    &exec_ctx(std::env::temp_dir().as_path()),
                )
                .await
                .unwrap();
        }
        assert_eq!(pool.open_sessions().len(), 2, "both sessions should be open before close_all");

        pool.close_all();

        assert!(pool.open_sessions().is_empty(), "close_all must remove every open session");
    }
```

(Check the exact `SpawnSpecialistArgs`/`config`/`exec_ctx`/`text_response`/`simple_team` helper signatures already in this file's test module before finalizing this test's exact call shape — mirror whatever the file's own existing `spawn_specialist`-exercising tests already do, since this step's job is proving `close_all` genuinely empties the pool, not re-deriving how to spawn a session from scratch.)

- [ ] **Step 3: Run the tests**

Run: `cargo test -p aivyx-core close_all -- --nocapture`
Expected: the new test passes.

- [ ] **Step 4: Format and commit**

```bash
rustfmt --edition 2024 crates/aivyx-core/src/specialist_sessions.rs
git add crates/aivyx-core/src/specialist_sessions.rs
git commit -m "feat: SpecialistSessionPool gains close_all"
```

---

### Task 2: `Agent` gains mission/specialist-session handles, `clear_conversation()` resets both

**Files:**
- Modify: `crates/aivyx-core/src/agent/mod.rs`
- Modify: `crates/aivyx/src/agent_builder.rs`

**Interfaces:**
- Consumes: `SpecialistSessionPool::close_all` (Task 1).

- [ ] **Step 1: Import `MissionPlan` and `SpecialistSessionPool`**

In `crates/aivyx-core/src/agent/mod.rs`, find this exact block:

```rust
use aivyx_types::{
    ContentBlock, Message, Role, ToolCall, ToolCallId, ToolCallSource, ToolDefinition, ToolOutput,
    ToolResult,
};
```

Replace it with:

```rust
use aivyx_types::{
    ContentBlock, Message, MissionPlan, Role, ToolCall, ToolCallId, ToolCallSource, ToolDefinition,
    ToolOutput, ToolResult,
};
```

Then find this exact line:

```rust
use crate::session::{self, SessionState, Task};
```

Replace it with:

```rust
use crate::session::{self, SessionState, Task};
use crate::specialist_sessions::SpecialistSessionPool;
```

- [ ] **Step 2: Add the two new fields**

In `crates/aivyx-core/src/agent/mod.rs`, find this exact block:

```rust
    /// The task list — the same `Arc` handed to the `set_tasks` tool (which
    /// mutates it); the agent reads it to emit `TasksUpdated` events and to
    /// persist it with the session.
    tasks: Arc<Mutex<Vec<Task>>>,
```

Replace it with:

```rust
    /// The task list — the same `Arc` handed to the `set_tasks` tool (which
    /// mutates it); the agent reads it to emit `TasksUpdated` events and to
    /// persist it with the session.
    tasks: Arc<Mutex<Vec<Task>>>,
    /// The same `Arc` handed to `decompose_task`/`verify_output`/
    /// `synthesize_results` (via `MissionToolsConfig`), when `[team]
    /// enabled = true` -- `None` otherwise. `clear_conversation` resets it
    /// to a pristine `MissionPlan` so a later mission-tool call after
    /// `/clear` never resurrects stale pre-clear content. Set via
    /// `set_mission_plan_handle`, not the constructor, matching
    /// `set_repo_map`'s own optional-state convention.
    mission_plan: Option<Arc<Mutex<MissionPlan>>>,
    /// The same pool handed to `spawn_specialist`/`query_specialist`/
    /// `close_specialist` (via `SpecialistSessionsConfig`), when `[team]
    /// enabled = true` -- `None` otherwise. `clear_conversation` calls
    /// `close_all()` on it for the identical reason `mission_plan` above
    /// is reset. Set via `set_specialist_session_pool_handle`.
    specialist_session_pool: Option<SpecialistSessionPool>,
```

- [ ] **Step 3: Initialize both to `None` in the constructor**

In `crates/aivyx-core/src/agent/mod.rs`, find the `Agent::new` constructor's struct-literal body (search for `tasks,` as a bare field-init-shorthand line inside the `Self { .. }` returned by `pub fn new(`) and add `mission_plan: None,` and `specialist_session_pool: None,` immediately after it. Read the surrounding lines first to confirm the exact current field-initialization order/style before editing, since this plan can't literally quote the entire constructor body without risking a stale match — this step is deliberately investigative about the exact surrounding lines, not about what value to initialize (both new fields must be `None` at construction, matching every other optional-state field in this same constructor, e.g. `repo_map: None`).

- [ ] **Step 4: Add the two new setters**

In `crates/aivyx-core/src/agent/mod.rs`, find this exact block:

```rust
    pub fn set_injection_taint(&mut self, injection_taint: InjectionTaint) {
        self.injection_taint = injection_taint;
    }
```

Replace it with:

```rust
    pub fn set_injection_taint(&mut self, injection_taint: InjectionTaint) {
        self.injection_taint = injection_taint;
    }

    pub fn set_mission_plan_handle(&mut self, mission_plan: Arc<Mutex<MissionPlan>>) {
        self.mission_plan = Some(mission_plan);
    }

    pub fn set_specialist_session_pool_handle(&mut self, pool: SpecialistSessionPool) {
        self.specialist_session_pool = Some(pool);
    }
```

- [ ] **Step 5: Reset both in `clear_conversation`**

In `crates/aivyx-core/src/agent/mod.rs`, find this exact block:

```rust
    pub fn clear_conversation(&mut self) {
        self.history.clear();
        self.tasks.lock().unwrap().clear();
        self.emit(AgentEvent::ConversationCleared);
        self.persist();
    }
```

Replace it with:

```rust
    pub fn clear_conversation(&mut self) {
        self.history.clear();
        self.tasks.lock().unwrap().clear();
        if let Some(mission_plan) = &self.mission_plan {
            *mission_plan.lock().unwrap() = MissionPlan {
                mission: String::new(),
                steps: vec![],
                summary: None,
            };
        }
        if let Some(pool) = &self.specialist_session_pool {
            pool.close_all();
        }
        self.emit(AgentEvent::ConversationCleared);
        self.persist();
    }
```

- [ ] **Step 6: Add tests**

In `crates/aivyx-core/src/agent/mod.rs`'s (or `agent/tests.rs`'s, if the crate splits agent tests into a separate file — check which one holds the existing tests around `Agent::new`/`clear_conversation` before adding here) test module, find an existing test exercising `clear_conversation` (search for `clear_conversation` in the test module) and add these two tests nearby, adapting to whatever test-construction helper (e.g. a `test_agent()`/`build_test_agent()` function) that existing test already uses:

```rust
    #[test]
    fn clear_conversation_resets_the_real_mission_plan_when_set() {
        let mut agent = test_agent(); // use this file's own existing agent-construction test helper
        let mission_plan = Arc::new(Mutex::new(MissionPlan {
            mission: "do the thing".to_string(),
            steps: vec![MissionStep {
                id: 1,
                member: "implementer".to_string(),
                task: "part one".to_string(),
                status: StepStatus::Verified,
                notes: None,
            }],
            summary: Some("done".to_string()),
        }));
        agent.set_mission_plan_handle(Arc::clone(&mission_plan));

        agent.clear_conversation();

        let cleared = mission_plan.lock().unwrap();
        assert_eq!(cleared.mission, "");
        assert!(cleared.steps.is_empty());
        assert_eq!(cleared.summary, None);
    }

    #[test]
    fn clear_conversation_is_a_no_op_when_no_mission_plan_handle_is_set() {
        // Must not panic when [team] enabled = false (the common case) --
        // mission_plan stays None, clear_conversation must simply skip it.
        let mut agent = test_agent();
        agent.clear_conversation();
    }
```

(`MissionStep`/`StepStatus` come from `aivyx_types` — check whether they're already imported in this test module or need adding, matching whatever import style the module already uses for `aivyx_types` items.)

- [ ] **Step 7: Verify `aivyx-core` compiles and its tests pass**

Run: `cargo test -p aivyx-core clear_conversation -- --nocapture`
Expected: both new tests pass, plus every pre-existing `clear_conversation`-related test (assertions unchanged).

- [ ] **Step 8: Wire the two new setters in `agent_builder.rs`**

In `crates/aivyx/src/agent_builder.rs`, find this exact line:

```rust
    agent.set_injection_taint(injection_taint.clone());
```

Replace it with:

```rust
    agent.set_injection_taint(injection_taint.clone());
    if let Some(mission_plan_handle) = &mission_plan {
        agent.set_mission_plan_handle(Arc::clone(mission_plan_handle));
    }
    if let Some(pool) = &specialist_session_pool {
        agent.set_specialist_session_pool_handle(pool.clone());
    }
```

- [ ] **Step 9: Verify `aivyx` compiles**

Run: `cargo check -p aivyx`
Expected: compiles cleanly.

- [ ] **Step 10: Format the exact files touched, and build/test/lint the full workspace**

Run:
```bash
rustfmt --edition 2024 crates/aivyx-core/src/agent/mod.rs
rustfmt --edition 2024 crates/aivyx/src/agent_builder.rs
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets
```
Expected: all clean, zero failures, zero warnings.

- [ ] **Step 11: Commit**

```bash
git add crates/aivyx-core/src/agent/mod.rs crates/aivyx/src/agent_builder.rs
git commit -m "fix: /clear resets the real MissionPlan and closes all specialist sessions"
```

---

## Self-Review Notes

**Spec coverage:** Decision 1 (`Agent`'s two new optional fields + setters, matching `set_repo_map`'s convention) → Task 2 Steps 2-4. Decision 2 (`agent_builder.rs` wires the same already-threaded `mission_plan`/`specialist_session_pool` locals) → Task 2 Step 8. Decision 3 (`SpecialistSessionPool::close_all`, no `.await` needed) → Task 1. Decision 4 (`clear_conversation` resets both, if present, no new `AgentEvent`) → Task 2 Step 5. "What this spec does not decide" items are all genuinely untouched: no ACP `/clear` wiring, no `forward_task` awaiting added, `close_specialist`/mission-tool logic unchanged, no Always-Allow cache eviction added.

**Global Constraints deviation:** none — ACP untouched, no new `AgentEvent`, `clear_conversation` stays synchronous, `close_specialist`/mission-tool logic unchanged, only file-scoped `rustfmt` used.

**Placeholder scan:** no TBD/TODO; every step shows complete, real code. Task 1 Step 2 and Task 2 Steps 3 and 6 are deliberately investigative about *exact existing test-module conventions* (which helper functions/imports already exist) rather than *what* to test, which is fully specified — matching this project's own established plan-writing pattern for details a plan author can't fully re-verify character-for-character without re-reading the entire file inline.

**Type/interface consistency check:** `SpecialistSessionPool::close_all(&self)` (Task 1) is called with the identical signature at Task 2 Step 5's `pool.close_all()`. `Agent::set_mission_plan_handle(&mut self, mission_plan: Arc<Mutex<MissionPlan>>)`/`set_specialist_session_pool_handle(&mut self, pool: SpecialistSessionPool)` (Task 2 Step 4) are called with matching types at Task 2 Step 8 (`mission_plan: &Option<Arc<Mutex<MissionPlan>>>`/`specialist_session_pool: &Option<SpecialistSessionPool>`, both already in scope in `agent_builder.rs` from the earlier `goal_achieved()` fix — confirmed by direct grep before this plan was written).
