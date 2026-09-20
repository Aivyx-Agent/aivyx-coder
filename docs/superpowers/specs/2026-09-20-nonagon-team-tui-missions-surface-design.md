# Nonagon-Style Team — Phase 6a (TUI Missions Surface) Design

## Context

Phases 1, 2, 3, 4, and 5 are shipped: `aivyx-team`, `DelegateToSpecialistTool`,
mission-structure tools (`decompose_task`/`verify_output`/
`synthesize_results`), specialist sessions (`spawn_specialist`/
`query_specialist`/`close_specialist`), and `[team] enabled` config-gated
registration. All of this state — a `MissionPlan` and a pool of parked
specialist sessions — is real and mutates as the lead works, but is
completely invisible to a human watching the terminal TUI: Phase 3
deliberately deferred rendering ("Agent's own struct, persistence, and
the TUI gain no new knowledge of mission state in this phase... that's
Phase 6's job"). This spec is that phase, split into two independently
shippable halves per the original roadmap's own decomposition: **6a**
(this spec) covers event plumbing + the terminal TUI; **6b** (separate,
later spec) covers the Zed/VS Code (ACP) side.

**Two grounded findings that shape this spec's design:**

1. **`AgentEvent::TasksUpdated` (the closest existing precedent) is
   *not* emitted by the `set_tasks` tool itself.** `Agent` owns the
   shared `tasks: Arc<Mutex<Vec<Task>>>` state directly as a constructor
   field and polls/emits `TasksUpdated` from its own turn loop after
   each tool dispatch (`agent/mod.rs:2118`). Mirroring this exactly for
   `MissionPlan`/specialist sessions would mean adding two new fields to
   `Agent::new`'s constructor, rippling into every call site that builds
   an `Agent` (`agent_builder.rs`, `delegate_task`,
   `delegate_to_specialist`, and every test fixture across the crate) —
   a real architectural change, not a small addition. Confirmed with the
   project owner: **do not** do this. Instead, reuse the *other*
   existing, already-proven pattern: `delegate_task`/
   `delegate_to_specialist`/the specialist-session tools already hold
   their own `events_tx: UnboundedSender<AgentEvent>` and emit events
   (`AgentEvent::SubAgentActivity`) directly from inside their own
   `execute()` bodies, with zero `Agent`-side involvement. This phase's
   tools do the same for two new event kinds.
2. **`MissionToolsConfig` (Phase 3) has no `events_tx` field today** —
   unlike `SpecialistSessionsConfig` (Phase 4), which already has one.
   This spec adds one, a small, disclosed, additive change to an
   existing struct (not a new file, not new tools).

## Grounding

Read directly, not assumed:

- `crates/aivyx-core/src/agent/types.rs:38-45` — `AgentEvent::TasksUpdated`'s
  and `ConversationCleared`'s exact doc comments and shapes, the
  templates this spec's two new variants and their reset behavior follow.
- `crates/aivyx-core/src/agent/mod.rs:2118` — confirmed `TasksUpdated`'s
  real emission site (the turn loop, not the tool), and why this spec
  deliberately does not mirror that mechanism (see Finding 1 above).
- `crates/aivyx-core/src/mission_tools.rs:31-34` — `MissionToolsConfig`'s
  exact current shape (`{ team: TeamConfig, plan: Arc<Mutex<MissionPlan>> }`,
  no `events_tx`) — confirms the field addition this spec requires.
- `crates/aivyx-core/src/specialist_sessions.rs` — `SpecialistSessionsConfig`
  already has `events_tx`; the existing `open_sessions_description()`
  helper (added in Phase 4's final-review fix wave) already computes a
  `(session_id, member)` listing for cap-exceeded error messages — this
  spec's new `open_sessions()` method returns the same data
  structurally, and `open_sessions_description()` is refactored to build
  its string from it (removing duplication, not adding new logic).
- `crates/aivyx-tui/src/app.rs:655-671` (`render`'s Tasks panel block),
  `app.rs:811` (`task_window`), `app.rs:823` (`task_line`),
  `app.rs:585-587` (`TasksUpdated`'s `handle_agent_event` arm) — the
  exact, proven template this spec's Mission panel mirrors: a
  conditional layout row occupying space only when non-empty, a capped/
  windowed row list, a titled bordered `Paragraph` block.
- `crates/aivyx-tui/src/app.rs:443,446` — confirmed `App`'s existing
  field shape (plain fields set by `handle_agent_event` match arms, no
  polled `Arc<Mutex<...>>` anywhere in the render path) — this spec's
  two new `App` fields follow the identical shape.
- Confirmed via direct crate survey: `aivyx-tui` has no `View`/`Mode`/
  `Panel` enum anywhere — a single fixed vertical layout, matching the
  Tasks-panel precedent this spec follows rather than inventing a
  toggleable secondary view.

## Decisions

**1. Two new `AgentEvent` variants, emitted directly by the tools that
own the state, not by `Agent`.**

- `MissionsUpdated(MissionPlan)` — carries the full new plan (mirrors
  `TasksUpdated`'s "carries the full new list" convention). Emitted by
  `decompose_task`, `verify_output`, and `synthesize_results`
  (`mission_tools.rs`), each right after mutating `self.config.plan`,
  via a new `events_tx: UnboundedSender<AgentEvent>` field added to
  `MissionToolsConfig`.
- `SpecialistSessionsUpdated(Vec<SpecialistSessionSummary>)` — a new
  `pub struct SpecialistSessionSummary { pub session_id: String, pub member: String }`
  defined in `specialist_sessions.rs`. Emitted by `spawn_specialist` and
  `close_specialist` only — the open-session *set* changes on those two,
  not on `query_specialist` (which only exchanges messages with an
  already-open session, changing nothing about which sessions exist).
  Backed by a new `SpecialistSessionPool::open_sessions(&self) -> Vec<SpecialistSessionSummary>`
  method (locks, evicts stale entries, builds the list) — the existing
  `open_sessions_description()` helper is refactored to build its
  string from this same method, removing duplicated iteration logic.

**2. `App` gains two new fields, updated by two new `handle_agent_event`
match arms, following the exact shape `tasks: Vec<Task>` already has:**

```rust
mission_plan: Option<MissionPlan>,               // None until first decompose_task
open_specialist_sessions: Vec<SpecialistSessionSummary>,  // empty until first spawn_specialist
```

`MissionsUpdated(plan) => self.mission_plan = Some(plan)`;
`SpecialistSessionsUpdated(sessions) => self.open_specialist_sessions = sessions`.
Both reset (`mission_plan = None`, `open_specialist_sessions = vec![]`)
on `ConversationCleared`, matching how the Tasks panel's own state resets
on `/clear`.

**3. One combined "Mission" panel, not two separate ones — mirroring the
Tasks panel's exact template.** A new conditional layout row (only
occupies vertical space when there's something to show), visible when
`mission_plan.is_some() || !open_specialist_sessions.is_empty()` —
deliberately an OR, not requiring a plan to exist, since `decompose_task`
and `spawn_specialist` are independent, loosely-coupled tools (Phase 3
Decision 2: no auto-orchestration) and a lead may spawn a specialist
session without ever calling `decompose_task` first.

- Title: step progress when a plan exists, e.g. `"Mission (2/5 steps)"`
  (counting `StepStatus::Verified`, mirroring `Tasks ({done}/{total})`'s
  own counting convention) — falls back to a plain `"Mission"` title
  when only open sessions exist and no plan has been set yet.
- Step list: capped and windowed identically to `task_window`/
  `MAX_VISIBLE_TASKS` (a new `MAX_VISIBLE_MISSION_STEPS` constant, same
  value, same windowing helper shape — skip a leading run of `Verified`
  steps the same way `task_window` skips a leading run of `Done` tasks).
  Each row shows a status icon (mirroring `task_line`'s icon convention),
  the step's `member`, and its `task` text.
- Open specialist sessions: one trailing compact line when any are open
  (e.g. `"Open: implementer, reviewer"` — member names only, not full
  session ids, to stay compact), shown regardless of whether a plan
  exists.

**4. Testing mirrors existing patterns exactly, no new testing
approach introduced.** `mission_tools.rs`/`specialist_sessions.rs`:
extend each tool's existing test fixture helpers with a channel,
assert the right event fires with the right payload after each of the
five emission points, plus one test proving `query_specialist` emits
no `SpecialistSessionsUpdated`. `app.rs`: tests for the two new
`handle_agent_event` arms (including `ConversationCleared` reset) and
for the new windowing helper, mirroring `task_window`'s own existing
test coverage.

## What this spec does not decide

- Phase 6b (the ACP/Zed side) — a separate, later spec. This phase adds
  no ACP-facing code at all.
- Any interactivity (selection, scrolling within the panel, a dedicated
  keybinding) — the panel is purely a passive, always-rendered-when-
  nonempty display, exactly like the Tasks panel. A richer, toggleable,
  interactive master/detail view (closer to `aivyx-pa`'s own Missions
  panel) was considered and explicitly deferred as future, unscoped
  work — this phase's job is observability parity with what already
  exists for Tasks, not a new UI paradigm.
- Per-session detail beyond `(session_id, member)` (e.g. last-active
  time, a preview of the session's most recent exchange) — out of
  scope; the panel shows *that* sessions are open and *which*
  specialist, nothing more.
- Session persistence, autonomous-mode `goal_achieved()` awareness of
  `MissionPlan`/specialist sessions — unrelated, unchanged, still
  deliberately deferred gaps from earlier phases.
