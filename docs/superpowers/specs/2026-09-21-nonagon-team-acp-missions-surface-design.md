# Nonagon-Style Team — Phase 6b (ACP Missions Surface) Design

## Context

Phase 6a (shipped, `main` commit `cb43c72`) surfaced `MissionPlan` and open
specialist sessions in the terminal TUI only, deliberately deferring the
Zed/VS Code (ACP) side as a separate spec — see that phase's own design
doc for the full decomposition rationale. This spec is that deferred half:
wiring `AgentEvent::MissionsUpdated`/`SpecialistSessionsUpdated` (both
already defined, already emitted by the mission-structure and
specialist-session tools) into `aivyx-acp`'s ACP translation layer, so the
same state shows up in Zed's Agent panel.

**The grounded constraint that shapes this whole spec**: ACP's
`SessionUpdate` enum has no dedicated "mission" slot. The one existing
precedent — `AgentEvent::TasksUpdated` mapping to ACP's
`SessionUpdate::Plan(Plan{entries})` — is **whole-list-replace,
single-slot-per-session**: confirmed via the vendored
`agent-client-protocol-schema` crate, and confirmed that the only
alternative, more granular ACP variants (`PlanUpdate`/`PlanRemoved`,
per-entry patch/remove operations) aren't even compiled into this crate
today (`aivyx-acp/Cargo.toml` doesn't enable the `unstable_plan_operations`
feature they require). Since `TasksUpdated`, `MissionsUpdated`, and
`SpecialistSessionsUpdated` are three independent event streams that would
each naively map to the same single `Plan` slot, sending any one of them
directly (as `translate_event` currently does for `TasksUpdated`) would
have each overwrite the others in Zed's panel. This spec's core mechanism
is a stateful union-merge: track the latest known value of all three, and
re-emit the full combined `Plan` whenever any one of them changes.

## Grounding

Read directly, not assumed:

- `crates/aivyx-acp/src/translate.rs` — `translate_event(session_id: &SessionId, event: &AgentEvent) -> Option<SessionUpdate>` is currently a **pure, stateless free function** (confirmed: no session-level state threaded in at all, `session_id` itself is unused in the match body). `TasksUpdated` maps to `SessionUpdate::Plan(Plan::new(tasks.iter().map(|task| PlanEntry::new(task.text.clone(), PlanEntryPriority::Medium, status)).collect()))` — every entry hardcodes `PlanEntryPriority::Medium` (`Task` carries no priority concept). `MissionsUpdated`/`SpecialistSessionsUpdated` currently map to `None` (Phase 6a's own permanent no-op, with a comment explicitly deferring to this spec).
- `agent-client-protocol-schema-1.5.0/src/v1/plan.rs` — `PlanEntry`'s real fields: `content: String`, `priority: PlanEntryPriority` (High/Medium/Low), `status: PlanEntryStatus` (**Pending/InProgress/Completed — no Failed/error state**), `meta: Option<Meta>`.
- `agent-client-protocol-schema-1.5.0/src/v1/client.rs` — `SessionUpdate`'s full variant list confirmed unchanged since Phase 6a's own research: `UserMessageChunk`, `AgentMessageChunk`, `AgentThoughtChunk`, `ToolCall`, `ToolCallUpdate`, `Plan`, `PlanUpdate`/`PlanRemoved` (feature-gated, not enabled here), `AvailableCommandsUpdate`, `CurrentModeUpdate`, `ConfigOptionUpdate`, `SessionInfoUpdate`, `UsageUpdate` — none besides `Plan` offer a remotely suitable encoding for list-shaped mission/session state (the others are slash-command palettes, session metadata, or config toggles).
- `crates/aivyx-acp/src/session.rs` — confirmed exactly **one session per process** (`NewSessionRequest`'s handler rejects a second session; the module's own doc comment states this explicitly), held as `Arc<Mutex<Option<Session>>>`. This means the merge state can live as **plain fields directly on the `Session` struct** — no `HashMap<SessionId, _>` needed, since there is structurally never more than one session to key against. The event-drain loop (the `tokio::select!` loop plus its final drain) already holds `&mut session` at both points where `AgentEvent`s are consumed, so adding field updates there requires no new locking/borrowing pattern.
- Phase 6a's own final-review fix to the TUI's `mission_step_window` (anchoring windowing on "not `Verified`" rather than "`Pending`", specifically so a `Failed` step isn't hidden) — the precedent this spec's own `StepStatus::Failed` → `PlanEntryStatus::Pending` mapping decision follows: a failed step still needs attention, so it must not be represented as `Completed` (misleadingly implying success) or dropped from the list.

## Decisions

**1. `Session` gains three new fields tracking last-known state**:
`tasks: Vec<Task>`, `mission_plan: Option<MissionPlan>`,
`open_specialist_sessions: Vec<SpecialistSessionSummary>` — all starting
empty/`None`, mirroring `App`'s own equivalent fields in the Phase 6a TUI
implementation. The event-drain loop's `TasksUpdated`/`MissionsUpdated`/
`SpecialistSessionsUpdated` arms update the corresponding field, then call
a new merge function that rebuilds the **full** `SessionUpdate::Plan` from
the union of all three current values — this becomes the ACP update
actually sent, replacing `translate_event`'s current direct
`TasksUpdated → Plan` mapping for these three event kinds specifically.
Every other `AgentEvent` variant continues through `translate_event`
exactly as today, unchanged — only these three become session-aware.

**2. Uniform origin prefix on every entry**, per the project owner's
explicit choice (a full re-scope from "tasks unprefixed" was considered
and rejected in favor of visual consistency across all three sources):
- Tasks: `"[Task] {task.text}"`.
- Mission steps: `"[Mission: {step.member}] {step.task}"` — or, for a
  failed step specifically, `"[Mission: {step.member}] (FAILED) {step.task}"`.
- Open specialist sessions: `"[Specialist: {session.member}] session open"`.

**3. Status mapping**: `Task`'s existing status mapping is unchanged.
`MissionStep`'s `StepStatus::Pending` → `PlanEntryStatus::Pending`;
`StepStatus::Verified` → `PlanEntryStatus::Completed`;
**`StepStatus::Failed` → `PlanEntryStatus::Pending`** (not `Completed`,
which would misrepresent a failure as success; `PlanEntryStatus` has no
failure state at all, so the content-string `(FAILED)` marker above is
what actually communicates the failure to a human reading Zed's panel).
Open specialist sessions map to `PlanEntryStatus::InProgress` — there is
no real "done" concept for a parked session; it is either open or it no
longer exists in the list at all (closed/evicted sessions simply aren't
present, matching `open_sessions()`'s own existing "current snapshot"
semantics from Phase 6a's final-review fix, which already evicts stale
entries and sorts deterministically).

**4. Entry order is fixed and predictable**: tasks first, then mission
steps, then open specialist sessions — never interleaved, never
re-sorted by content. `PlanEntryPriority` stays hardcoded `Medium`
across all three sources, matching `TasksUpdated`'s own existing
convention (none of `Task`/`MissionStep`/`SpecialistSessionSummary`
carry a priority concept to map from).

**5. Testing mirrors `translate.rs`'s own existing unit-test style**,
extended to the new merge function: independent-source tests (tasks-only,
mission-only, sessions-only each populated with the other two empty),
a combined test asserting fixed ordering and correct prefixes/statuses
across all three simultaneously, and a dedicated test for the
`Failed` → `Pending`-with-`(FAILED)`-marker mapping. `session.rs` gains
one new test confirming a `MissionsUpdated` (or `SpecialistSessionsUpdated`)
event updates the correct `Session` field and the resulting emitted
`Plan` reflects the full union (not just the single changed source) —
proving the merge, not just the individual field write.

## What this spec does not decide

- Any change to the terminal TUI (Phase 6a's own, already-shipped
  surface) — this spec only touches `aivyx-acp`.
- Real-time editing of Plan entries from the Zed side (ACP's
  `PlanUpdate`/`PlanRemoved` unstable operations, not enabled in this
  crate) — out of scope, whole-replace only, matching the existing
  `TasksUpdated` precedent.
- Any new ACP capability negotiation, session mode, or config option
  related to missions — the merge is unconditional whenever `[team]` is
  enabled and any of the three event kinds fires; there is no opt-out
  toggle on the ACP side (mirrors `[team] enabled` itself being the only
  gate, exactly as the TUI panel has no separate visibility toggle
  either).
- Multi-session support for this merge state — deliberately relies on
  the "exactly one session per process" invariant already enforced
  elsewhere in this crate; if a future phase ever supports multiple
  concurrent ACP sessions per process, this spec's plain-field-on-`Session`
  approach would need revisiting (a `HashMap<SessionId, _>` at that point).
