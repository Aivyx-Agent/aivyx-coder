# Nonagon-Style Team — Phase 3 (Mission Structure) Design

## Context

Phases 1, 2, and 5 are shipped: `aivyx-team` (schema/attenuation),
`DelegateToSpecialistTool` (attenuated sub-agent delegation), and
`[team] enabled` config-gated registration — `aivyx-coder` now has a
real, live team-delegation capability. This spec covers Phase 3
("Mission structure"): giving the lead explicit, structured tools for
decomposing a mission, verifying a step's output, and synthesizing a
final result — matching `aivyx-pa`'s own Nonagon tool set
(`decompose_task`/`verify_output`/`synthesize_results`), deliberately
scoped **sequential, no DAG, no auto-orchestration** (per the original
6-phase roadmap's own phase split — DAG/concurrency is Phase 4+
territory, not built here).

**Grounded finding that shapes this phase's scope**: the lead already
has `set_tasks` (an existing, shipped, flat todo-list tool) and, once
`[team] enabled`, `delegate_to_specialist` — both in its own full tool
registry (a specialist's `tool_allowlist` attenuation never applies to
the lead itself; the lead is the normal top-level `Agent` with the
complete registry). The lead could already informally decompose,
delegate, verify, and synthesize purely through its own prose reasoning
plus these two existing tools, with zero new tools. Confirmed with the
project owner: build the explicit tools anyway, matching `aivyx-pa`'s
design, for better observability (structured state a future TUI phase
can render) and more reliable behavior on small local models (explicit
tool schemas nudge structured workflows more reliably than prose alone)
— not because the informal path is impossible.

## Grounding

Read directly, not assumed:

- `aivyx-coder/crates/aivyx-tools/src/tools/set_tasks.rs` — the existing
  precedent for a lightweight, **state-recording** (not LLM-calling, not
  sub-agent-spinning) tool: whole-list replacement, `Arc<Mutex<Vec<Task>>>`
  shared state constructed once and handed to the tool, a compact
  "echo the accepted state back" convention, capped size
  (`MAX_TASKS = 50`) with a clear error rather than silent truncation.
  This is the template Phase 3's three new tools follow — none of them
  spin up an `Agent`/LLM call the way `delegate_task`/`delegate_to_specialist`
  do.
- `aivyx-coder/crates/aivyx-types/src/lib.rs:98-110` — `Task`/`TaskStatus`'s
  real definitions: plain, zero-logic shared types (no `schemars`/`Tool`
  dependency), matching this crate's own "shared wire/domain types with
  no logic" charter (per `aivyx-coder/CLAUDE.md`'s architecture table).
  The analogous `MissionPlan`/`MissionStep`/`StepStatus` types belong
  here too, not in `aivyx-core` or `aivyx-team`.
- `aivyx-coder/crates/aivyx-core/src/delegate_to_specialist.rs` (802
  lines as of Phase 5) — confirmed too large to extend further; the
  three new tools get their own new file.
- `aivyx-pa/docs/NONAGON.md` §6 (tool table) and §7 (execution flow) —
  `decompose_task` ("lead → a `MissionPlan`"), `verify_output` ("the
  Reflect/Gate quality check before a step proceeds"),
  `synthesize_results` ("weave specialist outputs into one deliverable")
  are lead-side tools; the DAG/concurrent-branch execution
  (`TeamRuntime`) is a separate, later piece of `aivyx-pa`'s own build
  (their J.4) this spec's Phase 3 does not replicate — matching this
  initiative's own Phase 4 ("message bus") being separate, later,
  unscoped work, and DAG/concurrency having no phase of its own yet
  in this initiative's roadmap either.
- Confirmed: `Agent`'s own struct (`agent/mod.rs`), session persistence,
  and the TUI have no knowledge of anything team-related today — Phase
  6 ("TUI missions surface") is the phase that would render mission
  state, and this spec deliberately does not touch `Agent`'s own struct,
  persistence, or the TUI to stay within Phase 3's scope.

## Decisions

**1. Three new tools, lightweight and state-recording, not
sub-agent-spinning** — `decompose_task`, `verify_output`,
`synthesize_results`. None of them call an LLM or spin up an `Agent`
themselves; each just validates and records structured state, exactly
matching `set_tasks`' own complexity class, not `delegate_task`'s.

- `decompose_task(mission: String, steps: Vec<{member: String, task: String}>)`
  — validates every `member` against the real, non-lead roster (reusing
  the same validation logic `delegate_to_specialist` already has —
  delegating a step to the lead is invalid here too, for the identical
  reason), stores a new `MissionPlan` (steps carry `member`, `task`, and
  a `status: StepStatus` — `Pending`/`InProgress`/`Done`/`Verified`/
  `Failed`), returns the plan echoed back with step numbers (mirroring
  `set_tasks`' own "echo the accepted state back compactly" convention).
- `verify_output(step_id: u32, verdict: "pass" | "fail", notes: String)`
  — records a verification judgment against one step (looked up by
  `step_id`), updates its status accordingly. An unknown `step_id` is a
  clear tool error, matching `delegate_to_specialist`'s own
  unknown-member error convention (never a silent no-op).
- `synthesize_results(summary: String)` — records the final deliverable
  text on the `MissionPlan`, marks the mission complete. Does not itself
  end the turn or force any particular subsequent model behavior — the
  lead's own next text response is still what the user/editor actually
  sees; this tool exists to make "I'm now synthesizing" a structured,
  auditable checkpoint rather than only implicit in prose.

**2. Loosely coupled to `delegate_to_specialist` — no auto-orchestration.**
The lead still calls `delegate_to_specialist` per step using its own
judgment about ordering and timing; `decompose_task`'s stored plan does
not automatically trigger delegation calls, and no new tool
automatically advances a step's status when `delegate_to_specialist`
returns. The lead is responsible for calling `verify_output` itself
after reviewing a specialist's result. This keeps the phase honest about
being "sequential, lead-directed, structured bookkeeping" — not a real
scheduler. A real DAG/auto-orchestration engine, if ever built, is
future, unscoped work.

**3. `MissionPlan`/`MissionStep`/`StepStatus` live in `aivyx-types`**,
mirroring `Task`/`TaskStatus`'s own placement and shape exactly — plain
data, no `schemars`/`Tool` dependency in that crate.

**4. The three tools live in a new file in `aivyx-core`**
(`crates/aivyx-core/src/mission_tools.rs` or similar — exact name
decided at plan time), not an extension of the already-802-line
`delegate_to_specialist.rs`.

**5. Shared `MissionPlan` state is tool-local, not `Agent`-owned.**
Unlike `Task` (which `Agent` itself owns as a field, since the agent
renders/persists it), the `MissionPlan`'s `Arc<Mutex<MissionPlan>>` is
constructed once in `agent_builder.rs` (inside the existing
`if settings.team.enabled` block) and shared only among the three new
tools — `Agent`'s own struct, session persistence, and the TUI gain no
new knowledge of mission state in this phase. Deliberately deferred:
rendering/persisting this state is Phase 6's job. A mission's plan is
therefore visible to the model (via each tool's own echoed-back text,
same as `set_tasks`) but not to a human watching the TUI, until Phase 6.

**6. Registered under the same `[team] enabled` gate, no new config
knob.** All three tools are constructed and registered in
`agent_builder.rs`'s existing `if settings.team.enabled` block,
alongside `delegate_to_specialist` — inert/unregistered when team mode
is off, exactly like that tool.

## What this spec does not decide

- Phase 4 (message bus) and any real DAG/concurrent-branch execution
  engine — separate, later, unscoped work; not built or assumed here.
- Phase 6 (TUI missions surface) — this phase's `MissionPlan` state is
  not rendered or persisted anywhere; that's Phase 6's job.
- Whether `verify_output`'s "pass/fail" judgment should ever gate
  anything automatically (e.g. blocking a subsequent `delegate_to_specialist`
  call) — per Decision 2, nothing is automated this phase; the lead's
  own judgment is what actually drives the conversation.
- Deny-paths attenuation for specialists — still deferred from Phase 2,
  unrelated to and unchanged by this phase.
