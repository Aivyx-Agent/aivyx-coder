# Nonagon-Style Team — Phase 4 (Specialist Sessions) Design

## Context

Phases 1, 2, 3, and 5 are shipped: `aivyx-team` (schema/attenuation),
`DelegateToSpecialistTool` (attenuated one-shot sub-agent delegation),
`decompose_task`/`verify_output`/`synthesize_results` (mission-plan
bookkeeping), and `[team] enabled` config-gated registration —
`aivyx-coder` has a real, live team-delegation and mission-planning
capability. This spec covers what the original 6-phase roadmap called
"Phase 4 (message bus)" — reworked, per a grounded architectural finding
below, into "specialist sessions" instead of a literal port of
`aivyx-pa`'s message bus.

**Grounded finding that reshapes this phase's scope**: `aivyx-pa`'s
Nonagon message bus (`send_message`/`read_messages`/`query_agent`, a
`tokio::broadcast` channel) exists to let **concurrently executing**
specialists exchange messages, or let the lead interject on a specialist
still mid-mission. Confirmed directly against `aivyx-core/src/agent/mod.rs`:
`aivyx-coder`'s turn loop dispatches tool calls strictly one at a time
(a `for` loop awaiting each `dispatch(...)` fully before the next begins,
no `join_all`/task-fan-out), and `DelegateToSpecialistTool::execute`
(`delegate_to_specialist.rs`) constructs a specialist `Agent`, runs it to
completion via a bounded loop of `run_turn` calls, then `drop`s it — all
synchronously inside one tool call. There is no point at which two
specialists, or the lead and a specialist, are alive at the same moment.
A literal message-bus port would therefore have no reachable scenario to
attach to — the same "ships correctly but is structurally inert" shape
Phases 1-2 had before Phase 5 gave them an entry point.

Presented to the project owner as an explicit fork (not assumed): defer
this phase indefinitely, build a different narrower capability, or skip
straight to Phase 6 (TUI). Chosen: build **long-lived specialist
sessions** — the real prerequisite a future genuine dialogue/message-bus
phase would need anyway, and independently useful on its own (a
resumable back-and-forth with one specialist, instead of always
re-delegating from scratch).

## Grounding

Read directly, not assumed:

- `aivyx-core/src/agent/mod.rs`'s tool-call loop — confirmed strictly
  sequential (see above); no fan-out/join primitive exists anywhere in
  this codebase's dispatch path.
- `aivyx-core/src/delegate_to_specialist.rs`'s `execute()` — confirmed
  the exact mechanism this phase reuses: `Agent::run_turn(text, cwd,
  cancellation)` is already called **repeatedly on the same `Agent`
  instance** (the existing "continue" loop, bounded by
  `max_iterations`, run once per exchange today) — i.e. `Agent` is
  already naturally resumable across multiple `run_turn` calls with no
  new capability needed in `aivyx-core`'s `Agent` itself. The only
  change this phase needs is **not dropping** the specialist between
  exchanges, and exposing a session id so a later call can find it again.
- `aivyx-tools/src/tools/repl.rs` — the closest existing precedent for
  state addressable across multiple tool calls (`repl_start`/
  `repl_send`/`repl_stop`, a persistent OS process). Confirmed: single
  global slot (`Arc<tokio::sync::Mutex<Option<ReplSession>>>`), not a
  keyed map — this phase deliberately diverges (a keyed map, see Decision
  2) since a single slot would cap the lead at one resumable specialist
  at a time, defeating the point. Confirmed reusable: the
  gate-the-start-call-fully / gate-follow-ups-lightly split
  (`ActionKind::Execute` + real `ConfirmationGate` check for the first
  call, `ActionKind::Interact`-equivalent auto-allow for follow-ups) —
  though this phase's equivalent is simpler still, since
  `delegate_to_specialist` is already `ActionKind::Internal` end to end
  (the real gating happens inside the specialist's own nested tool
  calls via the shared gate, unchanged by this phase). Confirmed
  **not** reusable: everything premised on a pty/child process
  (idle-timeout's resource-cost rationale, output-draining reader task,
  process-group kill) — a parked specialist `Agent` costs only
  in-memory conversation history while idle, not a live process/port.
- `aivyx-config/src/lib.rs`'s `TeamSettings`/`ReplSettings` — confirmed
  current shape (`TeamSettings` is just `{ enabled: bool }` today;
  `ReplSettings.idle_timeout_secs` defaults to `600`, the precedent this
  phase's own idle-timeout default follows).
- `aivyx-team/src/lib.rs`'s `default_coding_roster()` — confirmed 3
  non-lead specialists (implementer/reviewer/tester) under the
  `coordinator` lead — the precedent this phase's default concurrent-
  session cap is sized against.

## Decisions

**1. Additive, not a replacement.** `delegate_to_specialist` is
untouched — it remains the simple one-shot path. Three new tools,
`spawn_specialist`/`query_specialist`/`close_specialist`, are a separate,
resumable path for when the lead needs a back-and-forth with a
specialist instead of a single exchange. Matches this initiative's
consistent pattern of adding tools rather than reworking shipped ones.

**2. Multiple, keyed concurrent sessions — not a single global slot.**
A `SpecialistSessionPool` (`Arc<tokio::sync::Mutex<HashMap<String,
ParkedSpecialistSession>>>`), keyed by a freshly generated `session_id`
per `spawn_specialist` call, shared across the three new `Tool` impls
(constructed once in `agent_builder.rs`, same sharing shape `repl.rs`
uses for its own single `Arc`). This is what makes genuine multi-
specialist dialogue possible later (the lead can keep e.g. `implementer`
and `reviewer` both parked and relay between them) — a single global
slot, `repl.rs`'s exact pattern, was considered and rejected: it would
only yield a resumable one-shot delegation, not real peer capability.

**3. Session mechanics reuse `Agent::run_turn`'s existing resumability
verbatim — no new `aivyx-core::Agent` capability.**
- `spawn_specialist(member, task)`: identical member validation to
  `delegate_to_specialist` (rejects the lead; rejects an unknown member,
  both reusing the existing `specialists`/`specialist_names` helpers),
  identical `Agent` construction (same attenuated registry via
  `compute_specialist_registry`, same shared gate/confiner/checkpointer/
  injection-taint/plan-mode/autonomous-mode, same event-forwarding
  `tokio::spawn` task relaying `AgentEvent::SubAgentActivity`), then runs
  the *same* bounded "continue" loop `delegate_to_specialist::execute`
  already runs for one exchange. Difference: instead of `drop(specialist)`
  at the end, the `Agent`, its `forward_task` handle, and the shared
  `accumulated` text buffer are stored in the pool under a new
  `session_id`, returned alongside the exchange's result. Errors clearly
  (naming the configured cap) if the concurrent-session cap is already
  reached.
- `query_specialist(session_id, message)`: looks up the parked session
  (clear `ToolOutput::Error` if unknown or already closed — matching
  `verify_output`'s unknown-`step_id` convention), drains/resets the
  accumulator (not `Arc::try_unwrap`, since the session persists after
  this call — a real change from `delegate_to_specialist`'s one-shot
  teardown), calls `run_turn(message, ...)` on the *same* `Agent`
  instance (its internal history already carries every prior exchange —
  this is the load-bearing reuse this whole phase rests on), runs the
  same bounded loop, updates `last_activity`, returns the result. Session
  stays parked afterward.
- `close_specialist(session_id)`: removes the session from the pool,
  then performs today's exact teardown (`drop(specialist)` + await
  `forward_task` to let its channel close and drain).

All three tools are `ActionKind::Internal`, `mutates_outside_session() ==
false` — identical to `delegate_to_specialist` today. The real gating
continues to happen inside each specialist's own nested tool calls via
the shared `ConfirmationGate`/`ExecutionConfiner`/`GitCheckpointer`,
unchanged by this phase — normal per-call checkpointing already applies
(unlike `repl.rs`, which needed a special lazy-checkpoint hack because a
raw pty write bypasses the `Tool` abstraction entirely; a specialist's
tool calls always go through `Tool::execute`, so no equivalent hack is
needed here).

**4. Concurrent-session cap and idle timeout, both configurable.**
`TeamSettings` (currently `{ enabled: bool }`) gains two fields:
`max_concurrent_specialist_sessions: usize` (default `3`, matching
`default_coding_roster()`'s exact non-lead specialist count) and
`specialist_session_idle_timeout_secs: u64` (default `600`, matching
`ReplSettings.idle_timeout_secs`'s existing precedent). A background
idle-watcher task, spawned once alongside the pool, polls every 30
seconds (deliberately much less aggressive than `repl.rs`'s 1-second
poll, since a parked specialist has no real-time output stream needing
prompt draining) and closes any session whose `last_activity` exceeds
the configured timeout, running the same teardown `close_specialist`
performs. An idle timeout was a genuine judgment call here — a parked
specialist costs only in-memory history while idle (no process/port,
unlike `repl.rs`'s actual resource-cost rationale) — but kept anyway, as
a safety net against a model that spawns sessions and forgets to close
them, consistent with `repl.rs`'s own precedent even though the
underlying cost argument differs.

**5. Registered under the same `[team] enabled` gate, no new config
knob for on/off.** The `SpecialistSessionPool` and all three tools are
constructed and registered in `agent_builder.rs`'s existing
`if settings.team.enabled` block, alongside `delegate_to_specialist` and
the mission-structure tools — inert/unregistered when team mode is off,
exactly like its siblings.

## What this spec does not decide

- Real peer-to-peer dialogue (a specialist directly messaging another
  specialist, or a genuine `tokio::broadcast` bus) — this phase gives the
  lead multiple parked specialists it can relay between via
  `query_specialist`, but the lead remains the mandatory intermediary,
  consistent with Phase 3's "no auto-orchestration, the lead drives
  everything" principle. A literal specialist-to-specialist channel is
  future, unscoped work if ever revisited.
- Any DAG/concurrent-branch execution engine — still separate, later,
  unscoped work; not built or assumed here. This phase does not make
  specialist *execution* concurrent, only specialist *existence* (a
  parked `Agent` sitting in memory between calls) concurrent.
- Phase 6 (TUI missions surface) — parked specialist sessions are not
  rendered or persisted anywhere; a human watching the TUI has no
  visibility into them, same deferral `MissionPlan` already has.
- Deny-paths attenuation for specialists — still deferred from Phase 2,
  unrelated to and unchanged by this phase.
- Session persistence across process restart — a parked specialist lives
  only in process memory; restarting `aivyx-coder` loses all sessions,
  same as every other in-memory `Agent` state today.
