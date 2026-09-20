# Nonagon Team — Phase 6b (ACP Missions Surface) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Surface the Nonagon team's `MissionPlan` and open specialist sessions in Zed/VS Code (via ACP), merged with the existing task list into one combined ACP `Plan`.

**Architecture:** `aivyx-acp`'s `Session` struct gains three tracked-state fields (`tasks`, `mission_plan`, `open_specialist_sessions`). A new pure function in `translate.rs`, `translate_event_with_state`, intercepts `TasksUpdated`/`MissionsUpdated`/`SpecialistSessionsUpdated`, updates the relevant tracked value, and rebuilds the full merged `Plan` from all three (origin-prefixed entries: `[Task]`/`[Mission: member]`/`[Specialist: member]`) — every other event still passes through the existing, unchanged `translate_event`. `Session::translate_and_merge` is a thin wrapper threading `Session`'s own fields into that pure function, so the real merge logic never needs a full `Session`/`Agent` to test.

**Tech Stack:** Rust, `agent-client-protocol` (vendored `agent-client-protocol-schema` types: `Plan`, `PlanEntry`, `PlanEntryStatus`, `PlanEntryPriority`).

## Global Constraints

- ACP has no dedicated "mission" slot — `SessionUpdate::Plan` (whole-list-replace) is the only usable list-shaped variant (`PlanUpdate`/`PlanRemoved` require the `unstable_plan_operations` feature, not enabled in this crate).
- Every entry gets a uniform origin prefix: `"[Task] {text}"`, `"[Mission: {member}] {task}"` (or `"[Mission: {member}] (FAILED) {task}"` for a failed step), `"[Specialist: {member}] session open"`.
- Status mapping: `Task`'s existing mapping unchanged. `StepStatus::Pending` → `PlanEntryStatus::Pending`; `StepStatus::Verified` → `PlanEntryStatus::Completed`; `StepStatus::Failed` → `PlanEntryStatus::Pending` (never `Completed` — `PlanEntryStatus` has no failure state, so `Pending` plus the `(FAILED)` text marker is the honest choice). Open specialist sessions map to `PlanEntryStatus::InProgress`.
- Entry order is fixed: tasks first, then mission steps, then open specialist sessions — never interleaved or re-sorted. `PlanEntryPriority` stays hardcoded `Medium` for every entry from every source, matching the existing `TasksUpdated` convention (none of the three source types carry a priority concept).
- Exactly one ACP session per process (already enforced elsewhere in this crate) — the tracked state is three plain fields on `Session`, no `HashMap<SessionId, _>`.
- **Disclosed deviation from the design spec's literal testing section**: the spec sketched "`session.rs` gains one new test confirming a `MissionsUpdated` event updates the correct `Session` field... proving the merge." This plan instead extracts the entire state-update-and-merge logic into a pure free function (`translate_event_with_state`, taking `&mut Vec<Task>`/`&mut Option<MissionPlan>`/`&mut Vec<SpecialistSessionSummary>` directly rather than `&mut Session`), fully testable in `translate.rs`'s existing lightweight test style with no `Agent`/`Session` construction needed at all. `Session::translate_and_merge` becomes a trivial one-call wrapper with no branching logic of its own, so it does not get a separate dedicated test — this crate's test module has no existing precedent for constructing a full `Session` (which requires a real `Agent`), and inventing that fixture just to re-test logic already fully covered by the pure-function tests would not be proportionate. The user-visible outcome (the merge happens correctly) is identical either way; only where the test coverage lives has changed.
- File-scoped `rustfmt --edition 2024 --check <path>` / `rustfmt --edition 2024 <path>` only — **never** a package-scoped `cargo fmt -p <crate>` or `cargo fmt --check -p <crate>` command with no file argument. This exact mistake has happened multiple times already in this larger initiative and had to be reverted every time.
- `cargo build --workspace`, `cargo test --workspace`, and `cargo clippy --workspace --all-targets -- -D warnings` must all stay clean.

---

### Task 1: Stateful merge in `translate.rs` + `Session` wiring

**Files:**
- Modify: `crates/aivyx-acp/src/translate.rs` (new `build_merged_plan`/`translate_event_with_state` functions, `TasksUpdated` moved into the existing drop-bucket, new imports, tests)
- Modify: `crates/aivyx-acp/src/session.rs` (`Session` gains 3 fields, its one construction site updated, new `impl Session` block, 3 call sites switched to the new method, new imports)

**Interfaces:**
- Produces: `pub(crate) fn build_merged_plan(tasks: &[Task], mission_plan: Option<&MissionPlan>, open_specialist_sessions: &[SpecialistSessionSummary]) -> Plan`; `pub(crate) fn translate_event_with_state(session_id: &SessionId, tasks: &mut Vec<Task>, mission_plan: &mut Option<MissionPlan>, open_specialist_sessions: &mut Vec<SpecialistSessionSummary>, event: &AgentEvent) -> Option<SessionUpdate>` (both in `translate.rs`); `Session::translate_and_merge(&mut self, event: &AgentEvent) -> Option<SessionUpdate>` (in `session.rs`, thin wrapper over the above).

- [ ] **Step 1: Write the failing tests for `build_merged_plan`**

In `crates/aivyx-acp/src/translate.rs`, find the `#[cfg(test)] mod tests { ... }` block and, immediately after the existing `tasks_updated_becomes_a_plan` test, insert:

```rust
    #[test]
    fn merged_plan_prefixes_tasks_and_maps_their_status() {
        let tasks = vec![
            Task { id: 1, text: "write tests".to_string(), status: TaskStatus::Done },
            Task { id: 2, text: "write code".to_string(), status: TaskStatus::InProgress },
        ];
        let plan = build_merged_plan(&tasks, None, &[]);
        assert_eq!(plan.entries.len(), 2);
        assert_eq!(plan.entries[0].content, "[Task] write tests");
        assert_eq!(plan.entries[0].status, PlanEntryStatus::Completed);
        assert_eq!(plan.entries[1].content, "[Task] write code");
        assert_eq!(plan.entries[1].status, PlanEntryStatus::InProgress);
    }

    #[test]
    fn merged_plan_prefixes_mission_steps_with_their_member() {
        let mission = MissionPlan {
            mission: "fix the bug".to_string(),
            steps: vec![MissionStep {
                id: 1,
                member: "implementer".to_string(),
                task: "write the fix".to_string(),
                status: StepStatus::Pending,
                notes: None,
            }],
            summary: None,
        };
        let plan = build_merged_plan(&[], Some(&mission), &[]);
        assert_eq!(plan.entries.len(), 1);
        assert_eq!(plan.entries[0].content, "[Mission: implementer] write the fix");
        assert_eq!(plan.entries[0].status, PlanEntryStatus::Pending);
    }

    #[test]
    fn merged_plan_maps_a_failed_step_to_pending_with_a_failed_marker() {
        let mission = MissionPlan {
            mission: "fix the bug".to_string(),
            steps: vec![MissionStep {
                id: 1,
                member: "implementer".to_string(),
                task: "write the fix".to_string(),
                status: StepStatus::Failed,
                notes: Some("does not compile".to_string()),
            }],
            summary: None,
        };
        let plan = build_merged_plan(&[], Some(&mission), &[]);
        assert_eq!(plan.entries.len(), 1);
        assert_eq!(
            plan.entries[0].content,
            "[Mission: implementer] (FAILED) write the fix"
        );
        // Never Completed -- a failed step must never look like a success.
        assert_eq!(plan.entries[0].status, PlanEntryStatus::Pending);
    }

    #[test]
    fn merged_plan_maps_a_verified_step_to_completed() {
        let mission = MissionPlan {
            mission: "fix the bug".to_string(),
            steps: vec![MissionStep {
                id: 1,
                member: "implementer".to_string(),
                task: "write the fix".to_string(),
                status: StepStatus::Verified,
                notes: Some("looks good".to_string()),
            }],
            summary: None,
        };
        let plan = build_merged_plan(&[], Some(&mission), &[]);
        assert_eq!(plan.entries[0].content, "[Mission: implementer] write the fix");
        assert_eq!(plan.entries[0].status, PlanEntryStatus::Completed);
    }

    #[test]
    fn merged_plan_prefixes_open_specialist_sessions_as_in_progress() {
        let sessions = vec![SpecialistSessionSummary {
            session_id: "abc123".to_string(),
            member: "reviewer".to_string(),
        }];
        let plan = build_merged_plan(&[], None, &sessions);
        assert_eq!(plan.entries.len(), 1);
        assert_eq!(plan.entries[0].content, "[Specialist: reviewer] session open");
        assert_eq!(plan.entries[0].status, PlanEntryStatus::InProgress);
    }

    #[test]
    fn merged_plan_orders_tasks_then_mission_steps_then_sessions() {
        let tasks = vec![Task { id: 1, text: "a task".to_string(), status: TaskStatus::Pending }];
        let mission = MissionPlan {
            mission: "fix the bug".to_string(),
            steps: vec![MissionStep {
                id: 1,
                member: "implementer".to_string(),
                task: "a step".to_string(),
                status: StepStatus::Pending,
                notes: None,
            }],
            summary: None,
        };
        let sessions = vec![SpecialistSessionSummary {
            session_id: "abc123".to_string(),
            member: "reviewer".to_string(),
        }];
        let plan = build_merged_plan(&tasks, Some(&mission), &sessions);
        assert_eq!(plan.entries.len(), 3);
        assert!(plan.entries[0].content.starts_with("[Task]"));
        assert!(plan.entries[1].content.starts_with("[Mission:"));
        assert!(plan.entries[2].content.starts_with("[Specialist:"));
    }

    #[test]
    fn merged_plan_with_no_sources_is_empty() {
        let plan = build_merged_plan(&[], None, &[]);
        assert!(plan.entries.is_empty());
    }
```

- [ ] **Step 2: Run to verify it fails to compile**

Run: `cargo check -p aivyx-acp`
Expected: FAIL — `build_merged_plan` does not exist yet, `MissionPlan`/`MissionStep`/`StepStatus`/`SpecialistSessionSummary` are not imported in `translate.rs`'s test module.

- [ ] **Step 3: Add imports and `build_merged_plan`**

In `crates/aivyx-acp/src/translate.rs`, change the top-level imports from:

```rust
use agent_client_protocol::schema::v1::{
    ContentBlock, ContentChunk, Plan, PlanEntry, PlanEntryPriority, PlanEntryStatus, SessionId,
    SessionUpdate, StopReason, TextContent, ToolCall as AcpToolCall, ToolCallContent,
    ToolCallStatus, ToolCallUpdate, ToolCallUpdateFields, ToolKind,
};
use aivyx_core::AgentEvent;
use aivyx_types::{ToolOutput, TaskStatus};
```

to:

```rust
use agent_client_protocol::schema::v1::{
    ContentBlock, ContentChunk, Plan, PlanEntry, PlanEntryPriority, PlanEntryStatus, SessionId,
    SessionUpdate, StopReason, TextContent, ToolCall as AcpToolCall, ToolCallContent,
    ToolCallStatus, ToolCallUpdate, ToolCallUpdateFields, ToolKind,
};
use aivyx_core::{AgentEvent, SpecialistSessionSummary};
use aivyx_types::{MissionPlan, StepStatus, Task, TaskStatus, ToolOutput};
```

Then, immediately after the `text_chunk` function (before `translate_event`), insert:

```rust
/// Builds the single merged ACP `Plan` from three tracked state sources
/// -- tasks first, then mission steps, then open specialist sessions,
/// each origin-prefixed so they're distinguishable in one flat list,
/// since ACP has no dedicated "mission" slot (only `Plan`, which is
/// whole-list-replace). See `docs/superpowers/specs/
/// 2026-09-21-nonagon-team-acp-missions-surface-design.md`.
pub(crate) fn build_merged_plan(
    tasks: &[Task],
    mission_plan: Option<&MissionPlan>,
    open_specialist_sessions: &[SpecialistSessionSummary],
) -> Plan {
    let mut entries = Vec::new();
    for task in tasks {
        let status = match task.status {
            TaskStatus::Pending => PlanEntryStatus::Pending,
            TaskStatus::InProgress => PlanEntryStatus::InProgress,
            TaskStatus::Done => PlanEntryStatus::Completed,
        };
        entries.push(PlanEntry::new(
            format!("[Task] {}", task.text),
            PlanEntryPriority::Medium,
            status,
        ));
    }
    if let Some(plan) = mission_plan {
        for step in &plan.steps {
            let (content, status) = match step.status {
                StepStatus::Pending => (
                    format!("[Mission: {}] {}", step.member, step.task),
                    PlanEntryStatus::Pending,
                ),
                StepStatus::Verified => (
                    format!("[Mission: {}] {}", step.member, step.task),
                    PlanEntryStatus::Completed,
                ),
                // Never Completed -- PlanEntryStatus has no failure state,
                // so Pending (not a false success) plus a text marker is
                // the honest mapping, mirroring the same reasoning behind
                // the TUI's own Failed-step handling (Phase 6a).
                StepStatus::Failed => (
                    format!("[Mission: {}] (FAILED) {}", step.member, step.task),
                    PlanEntryStatus::Pending,
                ),
            };
            entries.push(PlanEntry::new(content, PlanEntryPriority::Medium, status));
        }
    }
    for session in open_specialist_sessions {
        entries.push(PlanEntry::new(
            format!("[Specialist: {}] session open", session.member),
            PlanEntryPriority::Medium,
            // No real "done" concept for a parked session -- it's open or
            // it's gone (closed/evicted sessions simply aren't present in
            // this slice, they don't get a terminal status here).
            PlanEntryStatus::InProgress,
        ));
    }
    Plan::new(entries)
}
```

- [ ] **Step 4: Run to verify Step 1's tests pass**

Run: `cargo test -p aivyx-acp merged_plan`
Expected: all 7 new tests PASS.

- [ ] **Step 5: Write the failing tests for `translate_event_with_state`**

In the same `mod tests` block, immediately after the tests added in Step 1, insert:

```rust
    #[test]
    fn translate_event_with_state_updates_tasks_and_returns_the_merged_plan() {
        let mut tasks = Vec::new();
        let mut mission_plan = None;
        let mut open_specialist_sessions = Vec::new();
        let new_tasks = vec![Task { id: 1, text: "write tests".to_string(), status: TaskStatus::Done }];

        let update = translate_event_with_state(
            &sid(),
            &mut tasks,
            &mut mission_plan,
            &mut open_specialist_sessions,
            &AgentEvent::TasksUpdated(new_tasks.clone()),
        )
        .unwrap();

        assert_eq!(tasks, new_tasks);
        let SessionUpdate::Plan(plan) = update else {
            panic!("expected Plan");
        };
        assert_eq!(plan.entries.len(), 1);
        assert_eq!(plan.entries[0].content, "[Task] write tests");
    }

    #[test]
    fn translate_event_with_state_preserves_other_sources_when_one_changes() {
        let mut tasks = vec![Task { id: 1, text: "a task".to_string(), status: TaskStatus::Pending }];
        let mut mission_plan = None;
        let mut open_specialist_sessions = Vec::new();
        let mission = MissionPlan {
            mission: "fix the bug".to_string(),
            steps: vec![MissionStep {
                id: 1,
                member: "implementer".to_string(),
                task: "a step".to_string(),
                status: StepStatus::Pending,
                notes: None,
            }],
            summary: None,
        };

        let update = translate_event_with_state(
            &sid(),
            &mut tasks,
            &mut mission_plan,
            &mut open_specialist_sessions,
            &AgentEvent::MissionsUpdated(mission.clone()),
        )
        .unwrap();

        assert_eq!(mission_plan, Some(mission));
        // The pre-existing task must still be present in the re-merged plan
        // -- this is the whole point of the union merge (a MissionsUpdated
        // event must not clobber the Tasks entries).
        let SessionUpdate::Plan(plan) = update else {
            panic!("expected Plan");
        };
        assert_eq!(plan.entries.len(), 2);
        assert!(plan.entries[0].content.starts_with("[Task]"));
        assert!(plan.entries[1].content.starts_with("[Mission:"));
    }

    #[test]
    fn translate_event_with_state_updates_specialist_sessions() {
        let mut tasks = Vec::new();
        let mut mission_plan = None;
        let mut open_specialist_sessions = Vec::new();
        let sessions = vec![SpecialistSessionSummary {
            session_id: "abc123".to_string(),
            member: "reviewer".to_string(),
        }];

        let update = translate_event_with_state(
            &sid(),
            &mut tasks,
            &mut mission_plan,
            &mut open_specialist_sessions,
            &AgentEvent::SpecialistSessionsUpdated(sessions.clone()),
        )
        .unwrap();

        assert_eq!(open_specialist_sessions, sessions);
        let SessionUpdate::Plan(plan) = update else {
            panic!("expected Plan");
        };
        assert_eq!(plan.entries[0].content, "[Specialist: reviewer] session open");
    }

    #[test]
    fn translate_event_with_state_passes_other_events_straight_through() {
        let mut tasks = Vec::new();
        let mut mission_plan = None;
        let mut open_specialist_sessions = Vec::new();

        let update = translate_event_with_state(
            &sid(),
            &mut tasks,
            &mut mission_plan,
            &mut open_specialist_sessions,
            &AgentEvent::TextDelta("hi".to_string()),
        )
        .unwrap();

        assert!(matches!(update, SessionUpdate::AgentMessageChunk(_)));
    }
```

- [ ] **Step 6: Run to verify it fails to compile**

Run: `cargo check -p aivyx-acp`
Expected: FAIL — `translate_event_with_state` does not exist yet.

- [ ] **Step 7: Add `translate_event_with_state` and fold `TasksUpdated` into `translate_event`'s drop-bucket**

In `crates/aivyx-acp/src/translate.rs`, find `translate_event`'s existing `TasksUpdated` arm:

```rust
        AgentEvent::TasksUpdated(tasks) => SessionUpdate::Plan(Plan::new(
            tasks
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
        )),
```

Delete this arm entirely (its behavior is superseded by `build_merged_plan`/`translate_event_with_state` below — `translate_event` alone must no longer handle `TasksUpdated`, since doing so would produce an un-prefixed, un-merged `Plan` that bypasses the whole point of this phase).

Then find the existing drop-bucket:

```rust
        AgentEvent::TurnComplete
        | AgentEvent::TurnPaused(_)
        | AgentEvent::ContextUsage { .. }
        | AgentEvent::ConversationCleared
        // No ACP `SessionUpdate` variant maps to a mission plan or a
        // specialist-session list -- this editor frontend doesn't have a
        // missions panel (that's TUI-only, see the TUI Missions Surface
        // design spec), so these are silently dropped here, same as
        // `ConversationCleared` above.
        | AgentEvent::MissionsUpdated(_)
        | AgentEvent::SpecialistSessionsUpdated(_) => return None,
```

and replace it with:

```rust
        AgentEvent::TurnComplete
        | AgentEvent::TurnPaused(_)
        | AgentEvent::ContextUsage { .. }
        | AgentEvent::ConversationCleared
        // Tasks/mission/specialist-session state all feed a single,
        // merged ACP Plan (see `build_merged_plan`) -- handled
        // exclusively by `translate_event_with_state`, which tracks
        // last-known state and rebuilds the union on every change. This
        // stateless function must never handle any of the three, or a
        // direct call here would silently bypass the merge and produce
        // an un-prefixed, single-source Plan.
        | AgentEvent::TasksUpdated(_)
        | AgentEvent::MissionsUpdated(_)
        | AgentEvent::SpecialistSessionsUpdated(_) => return None,
```

Then, immediately after `translate_event`'s closing `}` (before `terminal_stop_reason`), insert:

```rust
/// Session-aware wrapper around `translate_event`: `TasksUpdated`/
/// `MissionsUpdated`/`SpecialistSessionsUpdated` update the relevant
/// tracked value and return the full re-merged `Plan`
/// (`build_merged_plan`); every other event passes straight through to
/// `translate_event`, unchanged. Takes the three tracked-state slots by
/// `&mut` directly (not a `&mut Session`) so this whole mechanism stays
/// testable with plain values, no `Agent`/`Session` construction needed
/// -- `Session::translate_and_merge` in `session.rs` is a thin wrapper
/// over this.
pub(crate) fn translate_event_with_state(
    session_id: &SessionId,
    tasks: &mut Vec<Task>,
    mission_plan: &mut Option<MissionPlan>,
    open_specialist_sessions: &mut Vec<SpecialistSessionSummary>,
    event: &AgentEvent,
) -> Option<SessionUpdate> {
    match event {
        AgentEvent::TasksUpdated(new_tasks) => {
            *tasks = new_tasks.clone();
            Some(SessionUpdate::Plan(build_merged_plan(
                tasks,
                mission_plan.as_ref(),
                open_specialist_sessions,
            )))
        }
        AgentEvent::MissionsUpdated(plan) => {
            *mission_plan = Some(plan.clone());
            Some(SessionUpdate::Plan(build_merged_plan(
                tasks,
                mission_plan.as_ref(),
                open_specialist_sessions,
            )))
        }
        AgentEvent::SpecialistSessionsUpdated(sessions) => {
            *open_specialist_sessions = sessions.clone();
            Some(SessionUpdate::Plan(build_merged_plan(
                tasks,
                mission_plan.as_ref(),
                open_specialist_sessions,
            )))
        }
        other => translate_event(session_id, other),
    }
}
```

- [ ] **Step 8: Run to verify all `translate.rs` tests pass**

Run: `cargo test -p aivyx-acp translate`
Expected: all tests PASS — the pre-existing `tasks_updated_becomes_a_plan` test now exercises `translate_event` returning `None` for `TasksUpdated` (a real, intentional behavior change from Step 7); update that test now:

Find:

```rust
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
```

and replace it with:

```rust
    #[test]
    fn tasks_updated_is_not_handled_by_the_stateless_translate_event() {
        // TasksUpdated is handled exclusively by translate_event_with_state
        // now (see merged_plan_prefixes_tasks_and_maps_their_status and
        // translate_event_with_state_updates_tasks_and_returns_the_merged_plan
        // above) -- translate_event alone must return None for it, or a
        // direct call here would silently bypass the merge.
        let tasks = vec![Task { id: 1, text: "write tests".to_string(), status: TaskStatus::Done }];
        assert!(translate_event(&sid(), &AgentEvent::TasksUpdated(tasks)).is_none());
    }
```

Run: `cargo test -p aivyx-acp translate`
Expected: all tests PASS (the renamed test plus every test added in Steps 1 and 5).

- [ ] **Step 9: Add tracked-state fields to `Session` and wire its construction site**

In `crates/aivyx-acp/src/session.rs`, change the top-level imports from:

```rust
use aivyx_core::{Agent, AgentEvent};
use aivyx_sandbox::{InjectionFinding, PlanMode};
```

to:

```rust
use aivyx_core::{Agent, AgentEvent, SpecialistSessionSummary};
use aivyx_sandbox::{InjectionFinding, PlanMode};
use aivyx_types::{MissionPlan, Task};
```

Also add `SessionUpdate` to the existing `agent_client_protocol::schema::v1::{...}` import block — change:

```rust
use agent_client_protocol::schema::v1::{
    AgentCapabilities, ContentBlock, InitializeRequest, InitializeResponse, NewSessionRequest,
    NewSessionResponse, PromptRequest, PromptResponse, SessionId, SessionMode, SessionModeId,
    SessionModeState, SessionNotification, SetSessionModeRequest, SetSessionModeResponse,
    StopReason,
};
```

to:

```rust
use agent_client_protocol::schema::v1::{
    AgentCapabilities, ContentBlock, InitializeRequest, InitializeResponse, NewSessionRequest,
    NewSessionResponse, PromptRequest, PromptResponse, SessionId, SessionMode, SessionModeId,
    SessionModeState, SessionNotification, SessionUpdate, SetSessionModeRequest,
    SetSessionModeResponse, StopReason,
};
```

Change the `Session` struct from:

```rust
struct Session {
    agent: Agent,
    events_rx: mpsc::UnboundedReceiver<AgentEvent>,
    cwd: PathBuf,
    session_id: SessionId,
}
```

to:

```rust
struct Session {
    agent: Agent,
    events_rx: mpsc::UnboundedReceiver<AgentEvent>,
    cwd: PathBuf,
    session_id: SessionId,
    /// Last-known state from each of the three sources
    /// `translate::build_merged_plan` unions into one ACP `Plan` --
    /// updated by `translate_and_merge` below. Not persisted across
    /// process restart (in-memory only, same as the TUI's own equivalent
    /// fields from Phase 6a).
    tasks: Vec<Task>,
    mission_plan: Option<MissionPlan>,
    open_specialist_sessions: Vec<SpecialistSessionSummary>,
}
```

Then find the `Session` construction site inside the `NewSessionRequest` handler:

```rust
                *guard = Some(Session {
                    agent: built.agent,
                    events_rx: built.events_rx,
                    // The client's declared session `cwd` is authoritative
                    // for the protocol (mandatory on `NewSessionRequest`),
                    // not `AcpSessionConfig.cwd` (the directory `build_agent`
                    // happened to be constructed with before any session
                    // existed) — the two are expected to match under
                    // Decision 4 (editor spawns one `aivyx --acp` process
                    // per session/cwd) but the wire value wins if they ever
                    // diverge.
                    cwd: req.cwd.clone(),
                    session_id: session_id.clone(),
                });
```

and replace it with:

```rust
                *guard = Some(Session {
                    agent: built.agent,
                    events_rx: built.events_rx,
                    // The client's declared session `cwd` is authoritative
                    // for the protocol (mandatory on `NewSessionRequest`),
                    // not `AcpSessionConfig.cwd` (the directory `build_agent`
                    // happened to be constructed with before any session
                    // existed) — the two are expected to match under
                    // Decision 4 (editor spawns one `aivyx --acp` process
                    // per session/cwd) but the wire value wins if they ever
                    // diverge.
                    cwd: req.cwd.clone(),
                    session_id: session_id.clone(),
                    tasks: Vec::new(),
                    mission_plan: None,
                    open_specialist_sessions: Vec::new(),
                });
```

- [ ] **Step 10: Run to verify it compiles**

Run: `cargo check -p aivyx-acp`
Expected: PASS (the new fields compile; `Session::translate_and_merge` doesn't exist yet, but nothing calls it yet either).

- [ ] **Step 11: Add `Session::translate_and_merge` and switch the three real call sites to use it**

In `crates/aivyx-acp/src/session.rs`, immediately after the `Session` struct definition (before `extract_prompt_text`), insert:

```rust
impl Session {
    /// Thin wrapper over `translate::translate_event_with_state`,
    /// threading this session's own tracked-state fields into it. See
    /// that function's doc comment for why the real merge logic lives
    /// there (testable with plain values) rather than here.
    fn translate_and_merge(&mut self, event: &AgentEvent) -> Option<SessionUpdate> {
        crate::translate::translate_event_with_state(
            &self.session_id,
            &mut self.tasks,
            &mut self.mission_plan,
            &mut self.open_specialist_sessions,
            event,
        )
    }
}
```

Then, inside the `PromptRequest` handler, find the first of the three real call sites (inside the `tokio::select!` loop):

```rust
                                Some(event) = session.events_rx.recv() => {
                                    if let Some(reason) = crate::translate::terminal_stop_reason(&event) {
                                        stop_reason = Some(reason);
                                    }
                                    if let Some(update) = crate::translate::translate_event(&session.session_id, &event) {
                                        let _ = spawn_connection.send_notification(SessionNotification::new(
                                            session.session_id.clone(),
                                            update,
                                        ));
                                    }
                                }
```

and replace it with:

```rust
                                Some(event) = session.events_rx.recv() => {
                                    if let Some(reason) = crate::translate::terminal_stop_reason(&event) {
                                        stop_reason = Some(reason);
                                    }
                                    if let Some(update) = session.translate_and_merge(&event) {
                                        let _ = spawn_connection.send_notification(SessionNotification::new(
                                            session.session_id.clone(),
                                            update,
                                        ));
                                    }
                                }
```

Then find the second (the post-loop drain):

```rust
                    while let Ok(event) = session.events_rx.try_recv() {
                        if let Some(reason) = crate::translate::terminal_stop_reason(&event) {
                            stop_reason = Some(reason);
                        }
                        if let Some(update) = crate::translate::translate_event(&session.session_id, &event) {
                            let _ = spawn_connection.send_notification(SessionNotification::new(
                                session.session_id.clone(),
                                update,
                            ));
                        }
                    }
```

and replace it with:

```rust
                    while let Ok(event) = session.events_rx.try_recv() {
                        if let Some(reason) = crate::translate::terminal_stop_reason(&event) {
                            stop_reason = Some(reason);
                        }
                        if let Some(update) = session.translate_and_merge(&event) {
                            let _ = spawn_connection.send_notification(SessionNotification::new(
                                session.session_id.clone(),
                                update,
                            ));
                        }
                    }
```

The third call site (the injection-taint notice, always an `AgentEvent::Error`) is **not** one of the three stateful event kinds and can stay exactly as-is — do not change it:

```rust
                    if let Some(finding) = session.agent.injection_taint().current() {
                        let notice = AgentEvent::Error(interactive_injection_notice(&finding));
                        if let Some(update) =
                            crate::translate::translate_event(&session.session_id, &notice)
                        {
```

(Leave this block untouched. `AgentEvent::Error` always passes straight through `translate_event` unchanged either way, so calling the plain function here is correct and simpler than routing through `translate_and_merge` for no benefit.)

- [ ] **Step 12: Run to verify it compiles and all tests pass**

Run: `cargo test -p aivyx-acp`
Expected: all tests PASS, no regressions in existing `session.rs`/`translate.rs` tests.

- [ ] **Step 13: Full workspace verification**

Run: `cargo build --workspace`
Expected: PASS.

Run: `cargo test --workspace`
Expected: all tests PASS (aside from the pre-existing, unrelated `git_pr::tests::check_gh_authenticated_spawns_through_the_confiner` flake under full-suite parallelism if the `gh` CLI isn't installed in this environment — confirmed pre-existing/environment-only in earlier phases of this initiative).

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: clean.

Run file-scoped rustfmt checks on both touched files:

```bash
rustfmt --edition 2024 --check crates/aivyx-acp/src/translate.rs
rustfmt --edition 2024 --check crates/aivyx-acp/src/session.rs
```

Expected: clean.

- [ ] **Step 14: Commit**

```bash
git add crates/aivyx-acp/src/translate.rs crates/aivyx-acp/src/session.rs
git commit -m "feat: merge tasks/mission plan/specialist sessions into one ACP Plan"
```

---

## Self-Review Notes

**Spec coverage:** Decision 1 (three tracked fields on `Session`, union-merge on every change) → Steps 9-11. Decision 2 (uniform prefixing) → Step 3's `build_merged_plan`. Decision 3 (status mapping, including `Failed` → `Pending`) → Step 3. Decision 4 (fixed entry order, hardcoded `Medium` priority) → Step 3. Decision 5 (testing) → Steps 1, 5, 8, with the disclosed deviation (pure-function testability instead of a `Session`-level test) recorded in Global Constraints. "What this spec does not decide" items are all genuinely untouched: no TUI changes (only `crates/aivyx-acp/*` touched), no `PlanUpdate`/`PlanRemoved` usage, no new ACP capability/config/mode, no multi-session support.

**Type consistency check:** `build_merged_plan`'s signature (Step 3) matches every call site inside `translate_event_with_state` (Step 7) and every test call (Steps 1, 5) exactly. `translate_event_with_state`'s signature (Step 7) matches `Session::translate_and_merge`'s call into it (Step 11) and every direct test call (Step 5) exactly — same parameter order, same types. `Session`'s three new field names (`tasks`, `mission_plan`, `open_specialist_sessions`, Step 9) match `translate_and_merge`'s field accesses (Step 11) exactly.

**Placeholder scan:** no TBD/TODO; every code step shows complete, real code; no "similar to Task N" references (this is a single-task plan, so this check is trivially satisfied, but every step's code is still written out in full rather than referenced).
