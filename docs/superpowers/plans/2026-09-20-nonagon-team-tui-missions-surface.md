# Nonagon Team — Phase 6a (TUI Missions Surface) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Surface the Nonagon team's `MissionPlan` and open specialist sessions in the terminal TUI, as a passive "Mission" panel mirroring the existing Tasks panel.

**Architecture:** Two new `AgentEvent` variants (`MissionsUpdated`, `SpecialistSessionsUpdated`), emitted directly by the mission-structure and specialist-session tools themselves (not polled by `Agent`, since that state is deliberately not `Agent`-owned) — mirrors the already-proven `SubAgentActivity` direct-emission pattern, not `TasksUpdated`'s `Agent`-owned-polling pattern. The TUI's `App` gains two new fields updated by two new `handle_agent_event` match arms, rendered as one combined, always-visible-when-nonempty "Mission" panel that mirrors the Tasks panel's exact layout/windowing template.

**Tech Stack:** Rust, `tokio::sync::mpsc::UnboundedSender`, `ratatui`.

## Global Constraints

- Additive only: no `Agent::new` signature change, no new field on `Agent` itself — mission/specialist-session state stays tool-local, exactly as Phase 3/4 deliberately decided.
- `MissionToolsConfig` gains one new field, `events_tx: UnboundedSender<AgentEvent>` — every existing test call site that constructs one must be updated.
- Five emission points total: `decompose_task`/`verify_output`/`synthesize_results` each emit `MissionsUpdated` after mutating the plan; `spawn_specialist`/`close_specialist` each emit `SpecialistSessionsUpdated` after changing the open-session set. `query_specialist` emits neither (the open-session set is unchanged by it).
- The Mission panel is a single combined panel (not two), visible when `mission_plan.is_some() || !open_specialist_sessions.is_empty()` (an OR — `spawn_specialist` does not require `decompose_task` to have run first).
- No new interactivity, no new keybinding, no toggle — purely a passive, always-rendered-when-nonempty display, exactly like the Tasks panel.
- Mission/specialist-session `App` state is not persisted across `--resume` (in-memory only, same as the underlying state itself).
- File-scoped `rustfmt --edition 2024 --check <path>` / `rustfmt --edition 2024 <path>` only — **never** a package-scoped `cargo fmt -p <crate>` or `cargo fmt --check -p <crate>` command with no file argument. This exact mistake has happened multiple times already in this larger initiative and had to be reverted every time.
- `cargo build --workspace`, `cargo test --workspace`, and `cargo clippy --workspace --all-targets -- -D warnings` must all stay clean after every task.

---

### Task 1: Event plumbing — `AgentEvent` variants + tool-side emission

**Files:**
- Modify: `crates/aivyx-core/src/agent/types.rs` (2 new `AgentEvent` variants)
- Modify: `crates/aivyx-core/src/specialist_sessions.rs` (new `SpecialistSessionSummary` type, `SpecialistSessionPool::open_sessions()`, emission in `spawn_specialist`/`close_specialist`, 3 new tests)
- Modify: `crates/aivyx-core/src/mission_tools.rs` (`MissionToolsConfig` gains `events_tx`, emission in all 3 tools, all 8 existing tests updated, 3 new tests)
- Modify: `crates/aivyx-core/src/lib.rs` (re-export `SpecialistSessionSummary`)
- Modify: `crates/aivyx/src/agent_builder.rs` (wire `events_tx` into `MissionToolsConfig`'s construction — 1 line)

**Interfaces:**
- Produces (consumed by Task 2): `AgentEvent::MissionsUpdated(MissionPlan)`, `AgentEvent::SpecialistSessionsUpdated(Vec<SpecialistSessionSummary>)`; `pub struct SpecialistSessionSummary { pub session_id: String, pub member: String }` (re-exported from `aivyx_core`); `MissionToolsConfig { pub team: TeamConfig, pub plan: Arc<Mutex<MissionPlan>>, pub events_tx: UnboundedSender<AgentEvent> }`.

- [ ] **Step 1: Write the failing tests for the two new `AgentEvent`-emitting behaviors in `specialist_sessions.rs`**

In `crates/aivyx-core/src/specialist_sessions.rs`, find the `specialist_session_tests` module (search for `mod specialist_session_tests`) and add these three tests immediately after the last existing test in that module (after `sessions_past_the_idle_timeout_are_evicted_on_the_next_pool_touch`):

```rust
    #[tokio::test]
    async fn spawn_specialist_emits_specialist_sessions_updated() {
        let llm = Arc::new(MockBackend::new(vec![text_response("hello")]));
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let pool = SpecialistSessionPool::new(3, Duration::from_secs(600));
        let cfg = config(llm, tx, simple_team(), pool);
        let tool = SpawnSpecialistTool::new(cfg);
        let ctx = exec_ctx(std::path::Path::new("."));

        tool.execute(
            serde_json::json!({ "member": "implementer", "task": "task" }),
            &ctx,
        )
        .await
        .unwrap();

        let mut found = false;
        while let Ok(event) = rx.try_recv() {
            if let AgentEvent::SpecialistSessionsUpdated(sessions) = event {
                assert_eq!(sessions.len(), 1);
                assert_eq!(sessions[0].member, "implementer");
                found = true;
            }
        }
        assert!(found, "expected a SpecialistSessionsUpdated event");
    }

    #[tokio::test]
    async fn close_specialist_emits_specialist_sessions_updated_with_the_session_removed() {
        let llm = Arc::new(MockBackend::new(vec![text_response("hello")]));
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let pool = SpecialistSessionPool::new(3, Duration::from_secs(600));
        let cfg = config(llm, tx, simple_team(), pool);
        let spawn_tool = SpawnSpecialistTool::new(cfg.clone());
        let close_tool = CloseSpecialistTool::new(cfg);
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
        // Drain spawn's own event so only close's event remains below.
        while rx.try_recv().is_ok() {}

        close_tool
            .execute(serde_json::json!({ "session_id": session_id }), &ctx)
            .await
            .unwrap();

        let mut found = false;
        while let Ok(event) = rx.try_recv() {
            if let AgentEvent::SpecialistSessionsUpdated(sessions) = event {
                assert!(
                    sessions.is_empty(),
                    "expected the closed session to be gone, got: {sessions:?}"
                );
                found = true;
            }
        }
        assert!(found, "expected a SpecialistSessionsUpdated event");
    }

    #[tokio::test]
    async fn query_specialist_does_not_emit_specialist_sessions_updated() {
        let llm = Arc::new(MockBackend::new(vec![
            text_response("first"),
            text_response("second"),
        ]));
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let pool = SpecialistSessionPool::new(3, Duration::from_secs(600));
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
        while rx.try_recv().is_ok() {}

        query_tool
            .execute(
                serde_json::json!({ "session_id": session_id, "message": "follow up" }),
                &ctx,
            )
            .await
            .unwrap();

        assert!(
            rx.try_recv().is_err(),
            "query_specialist must not emit SpecialistSessionsUpdated"
        );
    }
```

- [ ] **Step 2: Run to verify it fails to compile**

Run: `cargo check -p aivyx-core`
Expected: FAIL — `AgentEvent::SpecialistSessionsUpdated` does not exist yet.

- [ ] **Step 3: Add the two new `AgentEvent` variants**

In `crates/aivyx-core/src/agent/types.rs`, change the import line:

```rust
use aivyx_types::{ToolCall, ToolResult};
```

to:

```rust
use aivyx_types::{MissionPlan, ToolCall, ToolResult};
```

Then, immediately after the `TasksUpdated(Vec<Task>),` variant (and before `ConversationCleared`), insert:

```rust
    /// The mission plan changed (`decompose_task`/`verify_output`/
    /// `synthesize_results` was called) -- carries the full new plan for
    /// the TUI's mission panel. Unlike `TasksUpdated`, emitted directly by
    /// the mission-structure tools themselves (each holds its own
    /// `events_tx`), not polled/emitted by `Agent`'s own turn loop -- see
    /// `docs/superpowers/specs/2026-09-20-nonagon-team-tui-missions-surface-design.md`.
    MissionsUpdated(MissionPlan),
    /// The set of open specialist sessions changed (`spawn_specialist` or
    /// `close_specialist` was called -- not `query_specialist`, which only
    /// exchanges messages with an already-open session, changing nothing
    /// about which sessions exist). Carries the full new list.
    SpecialistSessionsUpdated(Vec<crate::specialist_sessions::SpecialistSessionSummary>),
```

Then update `ConversationCleared`'s doc comment to mention the new panel, changing:

```rust
    /// `Agent::clear_conversation` ran (the `/clear` command) — the
    /// frontend should reset whatever display state it owns (transcript,
    /// task panel, context-usage indicator). Carries no payload: the new
    /// state is simply "empty" in every dimension.
    ConversationCleared,
```

to:

```rust
    /// `Agent::clear_conversation` ran (the `/clear` command) — the
    /// frontend should reset whatever display state it owns (transcript,
    /// task panel, mission panel, context-usage indicator). Carries no
    /// payload: the new state is simply "empty" in every dimension.
    ConversationCleared,
```

- [ ] **Step 4: Run to verify it still fails (SpecialistSessionSummary doesn't exist yet)**

Run: `cargo check -p aivyx-core`
Expected: FAIL — `crate::specialist_sessions::SpecialistSessionSummary` does not exist yet, and the new tests reference types/emission behavior not yet implemented.

- [ ] **Step 5: Add `SpecialistSessionSummary` and `SpecialistSessionPool::open_sessions()`**

In `crates/aivyx-core/src/specialist_sessions.rs`, immediately after the `ParkedSpecialistSession` struct definition (before `struct SessionPoolState`), insert:

```rust
/// A minimal snapshot of one open specialist session -- interpolated into
/// cap-exceeded error messages (`open_sessions_description`) and exposed
/// to observability consumers (the TUI's mission panel) via
/// `AgentEvent::SpecialistSessionsUpdated`. Deliberately just
/// `(session_id, member)` -- no status/last-active/exchange-count, per
/// this phase's own explicit scope decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpecialistSessionSummary {
    pub session_id: String,
    pub member: String,
}
```

Then replace the existing `open_sessions_description` method:

```rust
    /// A short `"<session_id> (<member>)"` listing of every currently-open
    /// session, comma-separated -- interpolated into `spawn_specialist`'s
    /// cap-exceeded error messages so a model that hits the cap can see
    /// which sessions it could close, mirroring
    /// `delegate_to_specialist.rs`'s own `specialist_names` convention of
    /// giving a model that guessed wrong a recovery path in the same tool
    /// result. Locks briefly, mirroring `max_concurrent()`'s own pattern.
    fn open_sessions_description(&self) -> String {
        let state = self.inner.lock().unwrap();
        state
            .sessions
            .iter()
            .map(|(id, session)| format!("{id} ({})", session.member))
            .collect::<Vec<_>>()
            .join(", ")
    }
```

with:

```rust
    /// A snapshot of every currently-open session's id and member, in no
    /// particular order -- backs both `open_sessions_description()`'s
    /// error-message listing and `AgentEvent::SpecialistSessionsUpdated`.
    pub fn open_sessions(&self) -> Vec<SpecialistSessionSummary> {
        let state = self.inner.lock().unwrap();
        state
            .sessions
            .iter()
            .map(|(id, session)| SpecialistSessionSummary {
                session_id: id.clone(),
                member: session.member.clone(),
            })
            .collect()
    }

    /// A short `"<session_id> (<member>)"` listing of every currently-open
    /// session, comma-separated -- interpolated into `spawn_specialist`'s
    /// cap-exceeded error messages so a model that hits the cap can see
    /// which sessions it could close, mirroring
    /// `delegate_to_specialist.rs`'s own `specialist_names` convention of
    /// giving a model that guessed wrong a recovery path in the same tool
    /// result.
    fn open_sessions_description(&self) -> String {
        self.open_sessions()
            .iter()
            .map(|s| format!("{} ({})", s.session_id, s.member))
            .collect::<Vec<_>>()
            .join(", ")
    }
```

- [ ] **Step 6: Wire emission into `spawn_specialist` and `close_specialist`**

In `SpawnSpecialistTool::execute`, find:

```rust
        if let Err(max) = self.config.pool.insert_new(session_id.clone(), session) {
            return Ok(ToolOutput::Error(format!(
                "cannot open a new specialist session: {max} are already open ({}) -- close \
                one with close_specialist first",
                self.config.pool.open_sessions_description()
            )));
        }

        Ok(ToolOutput::Ok(format!(
            "session_id: {session_id}\n\n{text}"
        )))
```

and replace it with:

```rust
        if let Err(max) = self.config.pool.insert_new(session_id.clone(), session) {
            return Ok(ToolOutput::Error(format!(
                "cannot open a new specialist session: {max} are already open ({}) -- close \
                one with close_specialist first",
                self.config.pool.open_sessions_description()
            )));
        }

        let _ = self.config.events_tx.send(AgentEvent::SpecialistSessionsUpdated(
            self.config.pool.open_sessions(),
        ));

        Ok(ToolOutput::Ok(format!(
            "session_id: {session_id}\n\n{text}"
        )))
```

In `CloseSpecialistTool::execute`, find:

```rust
        let member = session.member.clone();
        drop(session.agent);
        let _ = session.forward_task.await;
        Ok(ToolOutput::Ok(format!(
            "specialist session {:?} closed ({member})",
            args.session_id
        )))
```

and replace it with:

```rust
        let member = session.member.clone();
        drop(session.agent);
        let _ = session.forward_task.await;

        let _ = self.config.events_tx.send(AgentEvent::SpecialistSessionsUpdated(
            self.config.pool.open_sessions(),
        ));

        Ok(ToolOutput::Ok(format!(
            "specialist session {:?} closed ({member})",
            args.session_id
        )))
```

- [ ] **Step 7: Run to verify Steps 1-6's tests pass**

Run: `cargo test -p aivyx-core specialist_session`
Expected: all tests pass (8 existing + 3 new = 11 total).

- [ ] **Step 8: Add `events_tx` to `MissionToolsConfig` and wire emission into its three tools**

In `crates/aivyx-core/src/mission_tools.rs`, change the import block from:

```rust
use std::sync::{Arc, Mutex};

use aivyx_sandbox::{ActionKind, PermissionRequest, PermissionTarget};
use aivyx_team::TeamConfig;
use aivyx_tools::{Tool, ToolError, ToolExecutionContext};
use aivyx_types::{MissionPlan, MissionStep, StepStatus, ToolDefinition, ToolOutput};
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;

use crate::delegate_to_specialist::{specialist_names, specialists};
```

to:

```rust
use std::sync::{Arc, Mutex};

use aivyx_sandbox::{ActionKind, PermissionRequest, PermissionTarget};
use aivyx_team::TeamConfig;
use aivyx_tools::{Tool, ToolError, ToolExecutionContext};
use aivyx_types::{MissionPlan, MissionStep, StepStatus, ToolDefinition, ToolOutput};
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use tokio::sync::mpsc::UnboundedSender;

use crate::agent::AgentEvent;
use crate::delegate_to_specialist::{specialist_names, specialists};
```

Change the `MissionToolsConfig` struct from:

```rust
#[derive(Clone)]
pub struct MissionToolsConfig {
    pub team: TeamConfig,
    pub plan: Arc<Mutex<MissionPlan>>,
}
```

to:

```rust
#[derive(Clone)]
pub struct MissionToolsConfig {
    pub team: TeamConfig,
    pub plan: Arc<Mutex<MissionPlan>>,
    pub events_tx: UnboundedSender<AgentEvent>,
}
```

In `DecomposeTaskTool::execute`, find:

```rust
        let summary = summarize_plan(&args.mission, &steps);
        *self.config.plan.lock().unwrap() = MissionPlan {
            mission: args.mission,
            steps,
            summary: None,
        };
        Ok(ToolOutput::Ok(summary))
```

and replace it with:

```rust
        let summary = summarize_plan(&args.mission, &steps);
        let new_plan = MissionPlan {
            mission: args.mission,
            steps,
            summary: None,
        };
        *self.config.plan.lock().unwrap() = new_plan.clone();
        let _ = self.config.events_tx.send(AgentEvent::MissionsUpdated(new_plan));
        Ok(ToolOutput::Ok(summary))
```

In `VerifyOutputTool::execute`, find:

```rust
        step.status = match args.verdict {
            Verdict::Pass => StepStatus::Verified,
            Verdict::Fail => StepStatus::Failed,
        };
        step.notes = Some(args.notes.clone());
        Ok(ToolOutput::Ok(format!(
            "step {} ({}: {}) marked {:?}: {}",
            step.id, step.member, step.task, step.status, args.notes
        )))
```

and replace it with:

```rust
        step.status = match args.verdict {
            Verdict::Pass => StepStatus::Verified,
            Verdict::Fail => StepStatus::Failed,
        };
        step.notes = Some(args.notes.clone());
        let message = format!(
            "step {} ({}: {}) marked {:?}: {}",
            step.id, step.member, step.task, step.status, args.notes
        );
        let updated_plan = plan.clone();
        drop(plan);
        let _ = self.config.events_tx.send(AgentEvent::MissionsUpdated(updated_plan));
        Ok(ToolOutput::Ok(message))
```

In `SynthesizeResultsTool::execute`, find:

```rust
        let char_count = args.summary.chars().count();
        let line_count = args.summary.lines().count();
        self.config.plan.lock().unwrap().summary = Some(args.summary);
        Ok(ToolOutput::Ok(format!(
            "mission synthesis recorded ({char_count} chars, {line_count} line(s))"
        )))
```

and replace it with:

```rust
        let char_count = args.summary.chars().count();
        let line_count = args.summary.lines().count();
        let updated_plan = {
            let mut plan = self.config.plan.lock().unwrap();
            plan.summary = Some(args.summary);
            plan.clone()
        };
        let _ = self.config.events_tx.send(AgentEvent::MissionsUpdated(updated_plan));
        Ok(ToolOutput::Ok(format!(
            "mission synthesis recorded ({char_count} chars, {line_count} line(s))"
        )))
```

- [ ] **Step 9: Run to verify it fails to compile (test fixtures not updated yet)**

Run: `cargo check -p aivyx-core`
Expected: FAIL — `mission_tools_tests`' `config()` helper doesn't supply `events_tx`.

- [ ] **Step 10: Replace `mission_tools.rs`'s entire test module**

In `crates/aivyx-core/src/mission_tools.rs`, replace the entire `#[cfg(test)] mod mission_tools_tests { ... }` block (from `#[cfg(test)]` through the module's closing `}`) with:

```rust
#[cfg(test)]
mod mission_tools_tests {
    use super::*;
    use aivyx_team::{TeamConfig, TeamMember};
    use aivyx_types::StepStatus;

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
                    persona: "You write code.".to_string(),
                    tool_allowlist: vec![],
                    extra_deny_paths: vec![],
                },
            ],
        }
    }

    fn config(events_tx: UnboundedSender<AgentEvent>, team: TeamConfig) -> MissionToolsConfig {
        MissionToolsConfig {
            team,
            plan: Arc::new(Mutex::new(MissionPlan {
                mission: String::new(),
                steps: vec![],
                summary: None,
            })),
            events_tx,
        }
    }

    fn ctx() -> aivyx_tools::ToolExecutionContext {
        aivyx_tools::ToolExecutionContext {
            cwd: std::path::PathBuf::from("."),
            confiner: Arc::new(aivyx_sandbox::NoopConfiner),
            cancellation: tokio_util::sync::CancellationToken::new(),
        }
    }

    #[tokio::test]
    async fn decompose_task_stores_a_valid_plan_and_echoes_it_back() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let cfg = config(tx, simple_team());
        let tool = DecomposeTaskTool::new(cfg.clone());
        let args = serde_json::json!({
            "mission": "fix the bug",
            "steps": [{ "member": "implementer", "task": "write the fix" }],
        });
        let result = tool.execute(args, &ctx()).await.unwrap();
        let aivyx_types::ToolOutput::Ok(text) = result else {
            panic!("expected Ok");
        };
        assert!(text.contains("implementer"));
        let stored = cfg.plan.lock().unwrap();
        assert_eq!(stored.steps.len(), 1);
        assert_eq!(stored.steps[0].status, StepStatus::Pending);
    }

    #[tokio::test]
    async fn decompose_task_rejects_the_lead_as_a_step_member() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let cfg = config(tx, simple_team());
        let tool = DecomposeTaskTool::new(cfg.clone());
        let args = serde_json::json!({
            "mission": "fix the bug",
            "steps": [{ "member": "coordinator", "task": "do it yourself" }],
        });
        let result = tool.execute(args, &ctx()).await.unwrap();
        let aivyx_types::ToolOutput::Error(msg) = result else {
            panic!("expected Error");
        };
        assert!(msg.contains("coordinator"));
        assert!(cfg.plan.lock().unwrap().steps.is_empty());
    }

    #[tokio::test]
    async fn decompose_task_rejects_an_unknown_member() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let cfg = config(tx, simple_team());
        let tool = DecomposeTaskTool::new(cfg.clone());
        let args = serde_json::json!({
            "mission": "fix the bug",
            "steps": [{ "member": "nonexistent", "task": "do it" }],
        });
        let result = tool.execute(args, &ctx()).await.unwrap();
        let aivyx_types::ToolOutput::Error(msg) = result else {
            panic!("expected Error");
        };
        assert!(msg.contains("nonexistent"));
        assert!(
            msg.contains("implementer"),
            "should list the valid specialist(s), got: {msg:?}"
        );
    }

    #[tokio::test]
    async fn decompose_task_replaces_the_whole_plan_including_the_summary() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let cfg = config(tx, simple_team());
        let tool = DecomposeTaskTool::new(cfg.clone());

        let first_args = serde_json::json!({
            "mission": "fix the bug",
            "steps": [{ "member": "implementer", "task": "write the fix" }],
        });
        tool.execute(first_args, &ctx()).await.unwrap();
        cfg.plan.lock().unwrap().summary = Some("old synthesis result".to_string());

        let second_args = serde_json::json!({
            "mission": "ship the feature",
            "steps": [
                { "member": "implementer", "task": "write the feature" },
                { "member": "implementer", "task": "write tests" },
            ],
        });
        tool.execute(second_args, &ctx()).await.unwrap();

        let stored = cfg.plan.lock().unwrap();
        assert_eq!(stored.mission, "ship the feature");
        assert_eq!(stored.steps.len(), 2);
        assert_eq!(stored.steps[0].id, 1);
        assert_eq!(stored.steps[0].task, "write the feature");
        assert_eq!(stored.steps[1].id, 2);
        assert_eq!(stored.steps[1].task, "write tests");
        assert_eq!(stored.summary, None);
    }

    #[tokio::test]
    async fn decompose_task_emits_missions_updated() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let cfg = config(tx, simple_team());
        let tool = DecomposeTaskTool::new(cfg);
        let args = serde_json::json!({
            "mission": "fix the bug",
            "steps": [{ "member": "implementer", "task": "write the fix" }],
        });
        tool.execute(args, &ctx()).await.unwrap();

        let mut found = false;
        while let Ok(event) = rx.try_recv() {
            if let AgentEvent::MissionsUpdated(plan) = event {
                assert_eq!(plan.mission, "fix the bug");
                assert_eq!(plan.steps.len(), 1);
                found = true;
            }
        }
        assert!(found, "expected a MissionsUpdated event");
    }

    #[tokio::test]
    async fn verify_output_updates_the_named_steps_status() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let cfg = config(tx, simple_team());
        cfg.plan
            .lock()
            .unwrap()
            .steps
            .push(aivyx_types::MissionStep {
                id: 1,
                member: "implementer".to_string(),
                task: "write the fix".to_string(),
                status: StepStatus::Pending,
                notes: None,
            });
        let tool = VerifyOutputTool::new(cfg.clone());
        let args = serde_json::json!({ "step_id": 1, "verdict": "pass", "notes": "looks good" });
        let result = tool.execute(args, &ctx()).await.unwrap();
        assert!(matches!(result, aivyx_types::ToolOutput::Ok(_)));
        let stored = cfg.plan.lock().unwrap();
        assert_eq!(stored.steps[0].status, StepStatus::Verified);
        assert_eq!(stored.steps[0].notes, Some("looks good".to_string()));
    }

    #[tokio::test]
    async fn verify_output_records_a_failed_verdict() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let cfg = config(tx, simple_team());
        cfg.plan
            .lock()
            .unwrap()
            .steps
            .push(aivyx_types::MissionStep {
                id: 1,
                member: "implementer".to_string(),
                task: "write the fix".to_string(),
                status: StepStatus::Pending,
                notes: None,
            });
        let tool = VerifyOutputTool::new(cfg.clone());
        let args =
            serde_json::json!({ "step_id": 1, "verdict": "fail", "notes": "does not compile" });
        let result = tool.execute(args, &ctx()).await.unwrap();
        assert!(matches!(result, aivyx_types::ToolOutput::Ok(_)));
        let stored = cfg.plan.lock().unwrap();
        assert_eq!(stored.steps[0].status, StepStatus::Failed);
        assert_eq!(stored.steps[0].notes, Some("does not compile".to_string()));
    }

    #[tokio::test]
    async fn verify_output_rejects_an_unknown_step_id() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let cfg = config(tx, simple_team());
        let tool = VerifyOutputTool::new(cfg.clone());
        let args = serde_json::json!({ "step_id": 99, "verdict": "pass", "notes": "n/a" });
        let result = tool.execute(args, &ctx()).await.unwrap();
        assert!(matches!(result, aivyx_types::ToolOutput::Error(_)));
    }

    #[tokio::test]
    async fn verify_output_emits_missions_updated() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let cfg = config(tx, simple_team());
        cfg.plan
            .lock()
            .unwrap()
            .steps
            .push(aivyx_types::MissionStep {
                id: 1,
                member: "implementer".to_string(),
                task: "write the fix".to_string(),
                status: StepStatus::Pending,
                notes: None,
            });
        let tool = VerifyOutputTool::new(cfg);
        let args = serde_json::json!({ "step_id": 1, "verdict": "pass", "notes": "looks good" });
        tool.execute(args, &ctx()).await.unwrap();

        let mut found = false;
        while let Ok(event) = rx.try_recv() {
            if let AgentEvent::MissionsUpdated(plan) = event {
                assert_eq!(plan.steps[0].status, StepStatus::Verified);
                found = true;
            }
        }
        assert!(found, "expected a MissionsUpdated event");
    }

    #[tokio::test]
    async fn synthesize_results_stores_the_summary() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let cfg = config(tx, simple_team());
        let tool = SynthesizeResultsTool::new(cfg.clone());
        let args = serde_json::json!({ "summary": "done, fix applied and verified" });
        let result = tool.execute(args, &ctx()).await.unwrap();
        assert!(matches!(result, aivyx_types::ToolOutput::Ok(_)));
        assert_eq!(
            cfg.plan.lock().unwrap().summary,
            Some("done, fix applied and verified".to_string())
        );
    }

    #[tokio::test]
    async fn synthesize_results_emits_missions_updated() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let cfg = config(tx, simple_team());
        let tool = SynthesizeResultsTool::new(cfg);
        let args = serde_json::json!({ "summary": "done, fix applied and verified" });
        tool.execute(args, &ctx()).await.unwrap();

        let mut found = false;
        while let Ok(event) = rx.try_recv() {
            if let AgentEvent::MissionsUpdated(plan) = event {
                assert_eq!(
                    plan.summary,
                    Some("done, fix applied and verified".to_string())
                );
                found = true;
            }
        }
        assert!(found, "expected a MissionsUpdated event");
    }
}
```

- [ ] **Step 11: Run to verify all `mission_tools` tests pass**

Run: `cargo test -p aivyx-core mission_tools`
Expected: all 11 tests pass (8 existing + 3 new).

- [ ] **Step 12: Re-export `SpecialistSessionSummary` and wire `agent_builder.rs`**

In `crates/aivyx-core/src/lib.rs`, change:

```rust
pub use specialist_sessions::{
    CloseSpecialistTool, QuerySpecialistTool, SpawnSpecialistTool, SpecialistSessionPool,
    SpecialistSessionsConfig,
};
```

to:

```rust
pub use specialist_sessions::{
    CloseSpecialistTool, QuerySpecialistTool, SpawnSpecialistTool, SpecialistSessionPool,
    SpecialistSessionSummary, SpecialistSessionsConfig,
};
```

In `crates/aivyx/src/agent_builder.rs`, find:

```rust
        let mission_tools_config = aivyx_core::MissionToolsConfig {
            team: team.clone(),
            plan: mission_plan,
        };
```

and replace it with:

```rust
        let mission_tools_config = aivyx_core::MissionToolsConfig {
            team: team.clone(),
            plan: mission_plan,
            events_tx: events_tx.clone(),
        };
```

- [ ] **Step 13: Full workspace verification**

Run: `cargo build --workspace`
Expected: PASS.

Run: `cargo test --workspace`
Expected: all tests PASS (aside from the pre-existing, unrelated `git_pr::tests::check_gh_authenticated_spawns_through_the_confiner` flake under full-suite parallelism if the `gh` CLI isn't installed in this environment — confirmed pre-existing/environment-only in earlier phases of this initiative).

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: clean.

Run file-scoped rustfmt checks on every file touched in this task:

```bash
rustfmt --edition 2024 --check crates/aivyx-core/src/agent/types.rs
rustfmt --edition 2024 --check crates/aivyx-core/src/specialist_sessions.rs
rustfmt --edition 2024 --check crates/aivyx-core/src/mission_tools.rs
rustfmt --edition 2024 --check crates/aivyx-core/src/lib.rs
rustfmt --edition 2024 --check crates/aivyx/src/agent_builder.rs
```

Expected: clean for lines this task touched. `agent_builder.rs` has documented pre-existing rustfmt drift elsewhere in the file (unrelated to this task's own edits) — if the check reports a diff, confirm via `git diff` that the flagged lines are NOT ones this task changed before concluding it's pre-existing; do not run a whole-file or package-scoped reformat.

- [ ] **Step 14: Commit**

```bash
git add crates/aivyx-core/src/agent/types.rs crates/aivyx-core/src/specialist_sessions.rs crates/aivyx-core/src/mission_tools.rs crates/aivyx-core/src/lib.rs crates/aivyx/src/agent_builder.rs
git commit -m "feat: emit MissionsUpdated/SpecialistSessionsUpdated events"
```

---

### Task 2: Terminal TUI — the Mission panel

**Files:**
- Modify: `crates/aivyx-tui/src/app.rs` (imports, new `App` fields, `App::new`, `handle_agent_event`, `render`, two new helper functions, one new constant, 6 new tests)

**Interfaces:**
- Consumes: `aivyx_core::{AgentEvent::MissionsUpdated, AgentEvent::SpecialistSessionsUpdated, SpecialistSessionSummary}` (Task 1); `aivyx_types::{MissionPlan, MissionStep, StepStatus}`.

- [ ] **Step 1: Write the failing tests for `handle_agent_event`'s two new arms**

In `crates/aivyx-tui/src/app.rs`, find the `mod tests` block (search for `mod tests {`) and, immediately after the `task()` helper function (search for `fn task(id: u32, text: &str, status: TaskStatus) -> Task`), insert a new helper:

```rust
    fn mission_step(id: u32, member: &str, task: &str, status: StepStatus) -> MissionStep {
        MissionStep {
            id,
            member: member.to_string(),
            task: task.to_string(),
            status,
            notes: None,
        }
    }
```

Then, immediately after the existing `task_window_of_all_done_tasks_shows_the_head` test (the last of the three `task_window_*` tests), insert these six new tests:

```rust
    #[test]
    fn missions_updated_stores_the_new_plan() {
        let mut app = App::new(None, PlanMode::new());
        let plan = MissionPlan {
            mission: "fix the bug".to_string(),
            steps: vec![mission_step(
                1,
                "implementer",
                "write the fix",
                StepStatus::Pending,
            )],
            summary: None,
        };
        app.handle_agent_event(AgentEvent::MissionsUpdated(plan.clone()));
        assert_eq!(app.mission_plan, Some(plan));
    }

    #[test]
    fn specialist_sessions_updated_stores_the_new_list() {
        let mut app = App::new(None, PlanMode::new());
        let sessions = vec![SpecialistSessionSummary {
            session_id: "abc123".to_string(),
            member: "implementer".to_string(),
        }];
        app.handle_agent_event(AgentEvent::SpecialistSessionsUpdated(sessions.clone()));
        assert_eq!(app.open_specialist_sessions, sessions);
    }

    #[test]
    fn conversation_cleared_resets_mission_state() {
        let mut app = App::new(None, PlanMode::new());
        app.handle_agent_event(AgentEvent::MissionsUpdated(MissionPlan {
            mission: "fix the bug".to_string(),
            steps: vec![mission_step(
                1,
                "implementer",
                "write the fix",
                StepStatus::Pending,
            )],
            summary: None,
        }));
        app.handle_agent_event(AgentEvent::SpecialistSessionsUpdated(vec![
            SpecialistSessionSummary {
                session_id: "abc123".to_string(),
                member: "implementer".to_string(),
            },
        ]));

        app.handle_agent_event(AgentEvent::ConversationCleared);

        assert_eq!(app.mission_plan, None);
        assert!(app.open_specialist_sessions.is_empty());
    }

    #[test]
    fn mission_step_window_shows_everything_when_it_fits() {
        let steps = vec![
            mission_step(1, "implementer", "a", StepStatus::Verified),
            mission_step(2, "implementer", "b", StepStatus::Pending),
        ];
        assert_eq!(mission_step_window(&steps, 6).len(), 2);
    }

    #[test]
    fn mission_step_window_skips_a_leading_run_of_verified_steps() {
        let mut steps: Vec<MissionStep> = (1..=6)
            .map(|i| mission_step(i, "implementer", "done", StepStatus::Verified))
            .collect();
        steps.push(mission_step(
            7,
            "implementer",
            "current",
            StepStatus::Pending,
        ));
        steps.push(mission_step(8, "implementer", "next", StepStatus::Pending));

        let window = mission_step_window(&steps, 6);

        assert_eq!(window.len(), 6);
        assert!(window.iter().any(|s| s.task == "current"));
        assert!(window.iter().any(|s| s.task == "next"));
    }

    #[test]
    fn mission_step_window_of_all_verified_steps_shows_the_head() {
        let steps: Vec<MissionStep> = (1..=9)
            .map(|i| mission_step(i, "implementer", "done", StepStatus::Verified))
            .collect();
        let window = mission_step_window(&steps, 6);
        assert_eq!(window.len(), 6);
        assert_eq!(window[0].id, 1);
    }
```

- [ ] **Step 2: Run to verify it fails to compile**

Run: `cargo check -p aivyx-tui`
Expected: FAIL — `MissionPlan`/`MissionStep`/`StepStatus`/`SpecialistSessionSummary` not in scope, `app.mission_plan`/`app.open_specialist_sessions` don't exist, `mission_step_window` doesn't exist.

- [ ] **Step 3: Add imports and the new constant**

In `crates/aivyx-tui/src/app.rs`, change:

```rust
use aivyx_core::{Agent, AgentEvent, SessionState, Task, TaskStatus};
use aivyx_sandbox::{
    InjectionFinding, InjectionTaint, PermissionRequest, PermissionTarget, PlanMode, UserResponse,
};
use aivyx_types::{ContentBlock, Message, Role, ToolCallSource, ToolOutput};
```

to:

```rust
use aivyx_core::{Agent, AgentEvent, SessionState, SpecialistSessionSummary, Task, TaskStatus};
use aivyx_sandbox::{
    InjectionFinding, InjectionTaint, PermissionRequest, PermissionTarget, PlanMode, UserResponse,
};
use aivyx_types::{
    ContentBlock, Message, MissionPlan, MissionStep, Role, StepStatus, ToolCallSource, ToolOutput,
};
```

Then, immediately after the existing constant:

```rust
const MAX_VISIBLE_TASKS: usize = 6;
```

add:

```rust
/// Mission-panel step rows before the panel stops growing and shows a
/// window into the list instead -- same rationale and value as
/// `MAX_VISIBLE_TASKS`.
const MAX_VISIBLE_MISSION_STEPS: usize = 6;
```

- [ ] **Step 4: Add the two new `App` fields**

Change the `App` struct from:

```rust
struct App {
    transcript: Vec<ChatLine>,
    input: TextArea<'static>,
    streaming_active: bool,
    pending_permission: Option<ModalRequest>,
    /// Latest `(used, limit)` context-token counts from the backend, shown
    /// in the status line. `None` until the first response reports usage.
    context_usage: Option<(u32, u32)>,
    /// The agent's task list, rendered as a panel between the transcript
    /// and the input box whenever it's non-empty.
    tasks: Vec<Task>,
    /// Shared with the gate (enforcement) and the agent (tool filtering +
    /// system-prompt note); the TUI owns the only toggle.
    plan_mode: PlanMode,
}
```

to:

```rust
struct App {
    transcript: Vec<ChatLine>,
    input: TextArea<'static>,
    streaming_active: bool,
    pending_permission: Option<ModalRequest>,
    /// Latest `(used, limit)` context-token counts from the backend, shown
    /// in the status line. `None` until the first response reports usage.
    context_usage: Option<(u32, u32)>,
    /// The agent's task list, rendered as a panel between the transcript
    /// and the input box whenever it's non-empty.
    tasks: Vec<Task>,
    /// The current mission plan, if `decompose_task` has been called this
    /// session -- rendered as part of the Mission panel. Not persisted
    /// across `--resume` (mission state is in-memory only, same as
    /// specialist sessions).
    mission_plan: Option<MissionPlan>,
    /// Every currently-open specialist session's (session_id, member) --
    /// rendered as a compact line in the Mission panel. Empty until the
    /// first `spawn_specialist` call.
    open_specialist_sessions: Vec<SpecialistSessionSummary>,
    /// Shared with the gate (enforcement) and the agent (tool filtering +
    /// system-prompt note); the TUI owns the only toggle.
    plan_mode: PlanMode,
}
```

In `App::new`, change:

```rust
        Self {
            transcript,
            input: new_input_box(),
            streaming_active: false,
            pending_permission: None,
            context_usage: None,
            tasks,
            plan_mode,
        }
```

to:

```rust
        Self {
            transcript,
            input: new_input_box(),
            streaming_active: false,
            pending_permission: None,
            context_usage: None,
            tasks,
            mission_plan: None,
            open_specialist_sessions: Vec::new(),
            plan_mode,
        }
```

- [ ] **Step 5: Add the two new `handle_agent_event` arms and update `ConversationCleared`'s reset**

In `handle_agent_event`, change:

```rust
            AgentEvent::TasksUpdated(tasks) => {
                self.tasks = tasks;
            }
            AgentEvent::ConversationCleared => {
                self.transcript.clear();
                self.tasks.clear();
                self.context_usage = None;
                self.streaming_active = false;
            }
```

to:

```rust
            AgentEvent::TasksUpdated(tasks) => {
                self.tasks = tasks;
            }
            AgentEvent::MissionsUpdated(plan) => {
                self.mission_plan = Some(plan);
            }
            AgentEvent::SpecialistSessionsUpdated(sessions) => {
                self.open_specialist_sessions = sessions;
            }
            AgentEvent::ConversationCleared => {
                self.transcript.clear();
                self.tasks.clear();
                self.mission_plan = None;
                self.open_specialist_sessions.clear();
                self.context_usage = None;
                self.streaming_active = false;
            }
```

- [ ] **Step 6: Run to verify Steps 1-5's tests pass**

Run: `cargo test -p aivyx-tui missions_updated_stores_the_new_plan specialist_sessions_updated_stores_the_new_list conversation_cleared_resets_mission_state`
Expected: PASS (the `mission_step_window` tests still fail — that helper is added in Step 8).

- [ ] **Step 7: Add the `mission_step_window` and `mission_step_line` helper functions**

Immediately after the existing `task_line` function (search for `fn task_line(task: &Task) -> Line<'static> {` and find its closing `}`), insert:

```rust
fn mission_step_window(steps: &[MissionStep], max: usize) -> &[MissionStep] {
    if steps.len() <= max {
        return steps;
    }
    let first_pending = steps
        .iter()
        .position(|s| s.status == StepStatus::Pending)
        .unwrap_or(0);
    let start = first_pending.min(steps.len() - max);
    &steps[start..start + max]
}

fn mission_step_line(step: &MissionStep) -> Line<'static> {
    let (marker, style) = match step.status {
        StepStatus::Pending => ("[ ]", Style::default()),
        StepStatus::Verified => ("[x]", Style::default().fg(Color::DarkGray)),
        StepStatus::Failed => ("[!]", Style::default().fg(Color::Red)),
    };
    Line::from(format!(
        "{marker} {}. [{}] {}",
        step.id, step.member, step.task
    ))
    .style(style)
}
```

- [ ] **Step 8: Run to verify all Task 2 tests pass so far**

Run: `cargo test -p aivyx-tui mission_step_window`
Expected: all 3 `mission_step_window_*` tests PASS.

- [ ] **Step 9: Add the Mission panel to `render`**

In `crates/aivyx-tui/src/app.rs`, replace the entire `render` function's layout-setup-through-Tasks-panel section — from the start of `fn render` through the closing `}` of the `if tasks_height > 0 { ... }` block that renders the Tasks panel (i.e. everything from `fn render(&self, frame: &mut ratatui::Frame) {` through the line `frame.render_widget(panel, layout[1]);` and its closing `}`) — with:

```rust
    fn render(&self, frame: &mut ratatui::Frame) {
        // The task panel only occupies a row of the layout while there are
        // tasks to show — an empty bordered box would just eat transcript
        // space for the (common) sessions that never use the task list.
        let tasks_height = if self.tasks.is_empty() {
            0
        } else {
            self.tasks.len().min(MAX_VISIBLE_TASKS) as u16 + 2 // + borders
        };
        // The Mission panel occupies its own conditional row, independent
        // of the Tasks panel above — either, both, or neither can be
        // present, so its layout index below is computed dynamically
        // (`panel_index`) rather than hardcoded, unlike the Tasks panel's
        // own pre-existing `layout[1]` (safe there only because Tasks was
        // always the sole optional row before this panel existed).
        let mission_step_count = self
            .mission_plan
            .as_ref()
            .map(|p| p.steps.len())
            .unwrap_or(0);
        let mission_height = if mission_step_count == 0 && self.open_specialist_sessions.is_empty()
        {
            0
        } else {
            let step_rows = mission_step_count.min(MAX_VISIBLE_MISSION_STEPS) as u16;
            let sessions_row: u16 = if self.open_specialist_sessions.is_empty() {
                0
            } else {
                1
            };
            step_rows + sessions_row + 2 // + borders
        };
        let mut constraints = vec![Constraint::Min(1)];
        if tasks_height > 0 {
            constraints.push(Constraint::Length(tasks_height));
        }
        if mission_height > 0 {
            constraints.push(Constraint::Length(mission_height));
        }
        constraints.push(Constraint::Length(3));
        constraints.push(Constraint::Length(1));
        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints(constraints)
            .split(frame.area());
        let (input_area, status_area) = (layout[layout.len() - 2], layout[layout.len() - 1]);

        let lines: Vec<Line> = self
            .transcript
            .iter()
            .flat_map(chat_line_to_lines)
            .collect();
        let viewport_height = layout[0].height.saturating_sub(2);
        // border chars, left + right
        let content_width = layout[0].width.saturating_sub(2);

        let transcript = Paragraph::new(lines).wrap(Wrap { trim: false });
        // `Paragraph::scroll` applies its offset *after* wrapping, so the
        // offset must be computed from the wrapped (post-wrap) row count —
        // `lines.len()` alone undercounts as soon as anything actually
        // wraps, and the transcript stops reaching the true bottom.
        //
        // `line_count` must be measured BEFORE `.block(...)` is attached:
        // once a block is set, ratatui adds the block's own vertical space
        // (2 rows for `Borders::ALL`) into the count, which would double
        // count against `viewport_height` (already border-excluded) and
        // over-scroll by exactly that many rows.
        let wrapped_rows = transcript.line_count(content_width) as u16;
        let scroll = wrapped_rows.saturating_sub(viewport_height);

        let transcript = transcript
            .block(Block::default().borders(Borders::ALL).title("aivyx-coder"))
            .scroll((scroll, 0));
        frame.render_widget(transcript, layout[0]);

        let mut panel_index = 1;
        if tasks_height > 0 {
            let done = self
                .tasks
                .iter()
                .filter(|t| t.status == TaskStatus::Done)
                .count();
            let task_lines: Vec<Line> = task_window(&self.tasks, MAX_VISIBLE_TASKS)
                .iter()
                .map(task_line)
                .collect();
            let panel = Paragraph::new(task_lines).block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(format!("Tasks ({done}/{})", self.tasks.len())),
            );
            frame.render_widget(panel, layout[panel_index]);
            panel_index += 1;
        }

        if mission_height > 0 {
            let mut lines: Vec<Line> = Vec::new();
            if let Some(plan) = &self.mission_plan {
                lines.extend(
                    mission_step_window(&plan.steps, MAX_VISIBLE_MISSION_STEPS)
                        .iter()
                        .map(mission_step_line),
                );
            }
            if !self.open_specialist_sessions.is_empty() {
                let members: Vec<&str> = self
                    .open_specialist_sessions
                    .iter()
                    .map(|s| s.member.as_str())
                    .collect();
                lines.push(Line::from(format!("Open: {}", members.join(", "))));
            }
            let title = match &self.mission_plan {
                Some(plan) => {
                    let verified = plan
                        .steps
                        .iter()
                        .filter(|s| s.status == StepStatus::Verified)
                        .count();
                    format!("Mission ({verified}/{} steps)", plan.steps.len())
                }
                None => "Mission".to_string(),
            };
            let panel =
                Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(title));
            frame.render_widget(panel, layout[panel_index]);
        }
```

Do not touch anything after this point in `render` (the input box, command hints, and status line rendering that follows are unaffected — they already use `input_area`/`status_area`, computed length-relative to `layout`, not a hardcoded index).

- [ ] **Step 10: Run to verify it compiles and all tests pass**

Run: `cargo test -p aivyx-tui`
Expected: all tests PASS, including the 6 new tests from Step 1 and the pre-existing `task_window_*`/`handle_agent_event`-related tests (no regressions).

- [ ] **Step 11: Full workspace verification**

Run: `cargo build --workspace`
Expected: PASS.

Run: `cargo test --workspace`
Expected: all tests PASS (aside from the known, pre-existing, unrelated `git_pr` flake noted in Task 1).

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: clean.

Run: `rustfmt --edition 2024 --check crates/aivyx-tui/src/app.rs`
Expected: clean.

- [ ] **Step 12: Manual smoke check**

Run: `cargo build -p aivyx` then `./target/debug/aivyx-coder --help`
Expected: builds and runs cleanly. (A full interactive smoke test of the rendered panel needs a real local LLM backend and a `[team] enabled = true` config, which may not be available in this environment — if so, note this in the report as an accepted, previously-documented limitation, consistent with every earlier phase of this initiative.)

- [ ] **Step 13: Commit**

```bash
git add crates/aivyx-tui/src/app.rs
git commit -m "feat: render mission plan and open specialist sessions in the TUI"
```

---

## Self-Review Notes

**Spec coverage:** Decision 1 (two new events, tool-side emission, five emission points, `query_specialist` emits neither) → Task 1's Steps 6 and 8. Decision 2 (`App`'s two new fields, `ConversationCleared` reset) → Task 2's Step 4-5. Decision 3 (one combined panel, OR visibility condition, title logic, windowing, trailing sessions line) → Task 2's Step 9. Decision 4 (testing mirrors existing patterns) → both tasks' test steps. "What this spec does not decide" items are all genuinely untouched: no ACP code anywhere in this plan (Task 1/2 touch only `aivyx-core`/`aivyx-tui`), no interactivity/keybinding added, no per-session detail beyond `(session_id, member)`, no persistence changes.

**Global Constraints deviation:** none — this plan matches the spec's decisions directly, no refinements needed at plan-writing time (unlike Phase 4's spec, which needed one disclosed refinement).

**Type consistency check:** `AgentEvent::MissionsUpdated(MissionPlan)` and `AgentEvent::SpecialistSessionsUpdated(Vec<SpecialistSessionSummary>)` (Task 1, Step 3) are constructed identically in every emission site (Task 1 Steps 6/8) and consumed identically in `handle_agent_event` (Task 2, Step 5) and the new tests (Task 2, Step 1) — same variant names, same payload types, no drift. `MissionToolsConfig`'s three-field shape (Task 1, Step 8) matches its one construction site (Task 1, Step 12) and its test fixture (Task 1, Step 10) exactly. `mission_step_window`/`mission_step_line`'s signatures (Task 2, Step 7) match their call sites in `render` (Task 2, Step 9) and their tests (Task 2, Step 1) exactly.

**Placeholder scan:** no TBD/TODO; every code step shows complete, real code; no "similar to Task N" references — Task 2's render replacement is written out in full rather than described.
