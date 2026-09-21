# Autonomous `goal_achieved()` Team-Awareness Design

## Context

`aivyx-coder`'s Nonagon-style multi-agent team capability (`[team]
enabled = true`) shipped its full 6-phase roadmap 2026-09-21. Its Phase 3
("Mission structure") final whole-branch review found and explicitly
flagged, but deliberately did not fix, a real gap: autonomous mode's stop
signal (`aivyx-tui/src/app.rs`'s `goal_achieved()`) only checks the
`set_tasks`-backed task list. A model that plans a team mission purely via
`decompose_task` under `--auto` + `[team] enabled` never populates that
list, so autonomous mode never sees the goal as achieved and runs to its
iteration budget instead of stopping cleanly. This spec is that fix — the
first of the Nonagon initiative's 7 deliberately-deferred gaps to be
picked up, chosen by the project owner as the highest real-world-impact
item (a correctness bug, not just a missing feature).

## Grounding

Read directly in the current codebase, not assumed:

- **`goal_achieved(tasks: &[Task]) -> bool`** (`aivyx-tui/src/app.rs:39`):
  `!tasks.is_empty() && tasks.iter().all(|t| t.status == TaskStatus::Done)`
  — deliberately requires at least one task to have ever been set, so "no
  tasks ever set" means "never done, keep going until budget exhausts,"
  not "trivially achieved." Called from `next_autonomous_message`, in turn
  called once per driver-loop iteration inside a `tokio::spawn`ed
  background task in `run()` (line ~285).
- **The driver loop is a genuinely separate task from the TUI's render
  loop.** The render loop owns `self.mission_plan: Option<MissionPlan>`
  and `self.open_specialist_sessions: Vec<SpecialistSessionSummary>`
  (populated via `AgentEvent::MissionsUpdated`/`SpecialistSessionsUpdated`,
  Nonagon Phase 6a) — but the background driver task has no access to
  `self`. It already solves the identical problem for tasks via
  `AutonomousRun.tasks: Arc<Mutex<Vec<Task>>>`, `Arc::clone`d directly in
  `main.rs` from `agent_builder.rs`'s `BuiltAgent.tasks` — the same `Arc`
  the `set_tasks` tool itself writes into, read via
  `.lock().unwrap().clone()` once per iteration. This is the pattern to
  replicate, not the TUI-event mirror (which is also affected by deferred
  gap #6, `/clear` not resetting the real `MissionPlan` — deliberately
  out of scope here, see below).
- **`agent_builder.rs` already constructs exactly what's needed, but never
  surfaces it.** Inside `if settings.team.enabled { ... }` (line ~655):
  `let mission_plan = Arc::new(std::sync::Mutex::new(MissionPlan { .. }))`
  is moved directly into `MissionToolsConfig.plan` with no clone kept;
  `let specialist_session_pool = SpecialistSessionPool::new(..)` is moved
  directly into `SpecialistSessionsConfig.pool`, same way. Both moves
  happen before `BuiltAgent` is constructed, and `BuiltAgent` has no field
  for either today.
- **`SpecialistSessionPool` is cheap to clone and share.** `#[derive(Clone)]`
  wrapping `inner: Arc<Mutex<SessionPoolState>>` — a clone shares real
  state, not a snapshot. Its existing `pub fn open_sessions(&self) ->
  Vec<SpecialistSessionSummary>` already does lazy idle-timeout eviction
  before returning, so polling it never sees stale/timed-out sessions
  incorrectly block completion.
- **`MissionPlan`'s own type already encodes a natural completion
  signal**: `pub summary: Option<String>`, doc-commented "Set by
  `synthesize_results` — `None` until the lead has explicitly synthesized
  a final deliverable." This is the lead's own explicit judgment call, not
  a mechanical step-count computation — confirmed with the project owner
  as the right signal over "all steps `Verified`" (a mission can
  reasonably finish with some steps intentionally left unverified).
- **Autonomous mode is TUI-only today** (confirmed via `CLAUDE.md`/`main.rs`:
  `--auto` is "not yet supported together" with `--acp`) — this fix's
  scope is entirely `aivyx-tui`/`main.rs`/`agent_builder.rs`; no ACP or
  MCP-server frontend changes are needed.

## Decisions

**1. `BuiltAgent` gains two new `Option`-typed fields**:
`mission_plan: Option<Arc<std::sync::Mutex<aivyx_types::MissionPlan>>>` and
`specialist_session_pool: Option<aivyx_core::SpecialistSessionPool>`, both
`None` when `[team] enabled = false` (matching how neither type is
constructed at all in that branch today). Populated inside the existing
`if settings.team.enabled { .. }` block in `agent_builder.rs` by cloning
the `Arc`/pool *before* each is moved into its respective tool config —
zero behavior change to the tools themselves, purely an additional
read-only handle surfaced outward.

**2. `AutonomousRun` gains the identical two fields**, threaded through in
`main.rs` from `built.mission_plan`/`built.specialist_session_pool` at
construction, exactly matching how `built.tasks` is threaded today.

**3. `goal_achieved()` becomes 3-argument**, combining independent
per-source signals where an unused source is a no-op rather than a
blocker:

```rust
fn goal_achieved(
    tasks: &[Task],
    mission_plan: Option<&MissionPlan>,
    open_specialist_sessions: &[SpecialistSessionSummary],
) -> bool {
    if !open_specialist_sessions.is_empty() {
        return false;
    }
    let tasks_signal = (!tasks.is_empty()).then(|| tasks.iter().all(|t| t.status == TaskStatus::Done));
    let mission_signal = mission_plan.map(|p| p.summary.is_some());
    match (tasks_signal, mission_signal) {
        (None, None) => false,
        _ => tasks_signal.unwrap_or(true) && mission_signal.unwrap_or(true),
    }
}
```

An open specialist session is always a hard block, regardless of the
other two signals — a live, un-closed specialist session is inherently
evidence of unfinished business. `next_autonomous_message` and the driver
loop's call site are updated to pass a `mission_plan` snapshot (locked and
cloned once per iteration, same pattern as `tasks_snapshot`) and
`open_specialist_sessions` (via `.open_sessions()`) alongside the existing
`tasks_snapshot`.

**4. Exact backward compatibility for `[team] enabled = false`**: in that
configuration `mission_plan` is always `None` and
`open_specialist_sessions` is always empty, so the function reduces to
`(!tasks.is_empty() && all done)` — byte-identical to today's behavior.
The 4 existing unit tests for `goal_achieved`/`next_autonomous_message`
stay unchanged (updated only for the new call signature) and must
continue passing unmodified in their assertions, proving this.

## What this spec does not decide

- Any of the other 6 deferred Nonagon gaps (custom-roster loading,
  deny-paths attenuation, a real specialist-to-specialist channel,
  parked-session persistence across restart, `/clear` not resetting the
  real `MissionPlan`, ACP entry-prefix spoofability) — each is separate,
  future scope, picked up individually if/when prioritized.
- Any change to `AgentEvent::MissionsUpdated`/`SpecialistSessionsUpdated`,
  the TUI Mission panel, or the ACP merged-Plan translation — this fix
  reads the real shared state directly in the background driver task, not
  through the event/TUI-mirror path those surfaces use.
- Any change to `verify_output`'s step-level `StepStatus` semantics — a
  mission with `Failed` or `Pending` steps can still be considered
  complete once `synthesize_results` sets `summary`, matching the lead's
  own explicit judgment as designed.
- ACP or MCP-server autonomous-mode support — neither frontend supports
  `--auto` today; out of scope for this fix.
