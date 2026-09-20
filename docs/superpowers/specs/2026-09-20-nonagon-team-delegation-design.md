# Nonagon-Style Team — Phase 2 (Pool + Delegation) Design

## Context

Phase 1 ("Foundation" — `docs/superpowers/specs/2026-09-20-nonagon-team-foundation-design.md`,
shipped on `main` as the `aivyx-team` crate) built the `TeamConfig`/
`TeamMember` schema and the attenuation invariant
(`effective_deny_paths`, `tool_allowlist_is_subset`/
`effective_tool_allowlist`) but wired nothing into a real delegation
path. This spec covers Phase 2 of the approved 6-phase roadmap: spinning
up an attenuated specialist `Agent` in-process and the tools that
delegate to it.

**Major discovery that reshapes this phase's scope**: `aivyx-coder`
already ships a complete, production sub-agent delegation mechanism —
`aivyx-core/src/delegate.rs` (`delegate_task`, ROADMAP.md Phase 9, its
own design doc `docs/superpowers/specs/2026-07-13-subagent-delegation-design.md`).
The model calls `delegate_task` mid-turn; it spins up a fresh `Agent`
with an isolated conversation history, sharing the parent's *same*
`PermissionGate`/`ExecutionConfiner`/`GitCheckpointer` (never rebuilt),
gets a *cloned* copy of the parent's tool registry (with `delegate_task`
itself excluded, making recursion structurally impossible), runs bounded
by an iteration budget, and streams activity back to the parent as
`AgentEvent::SubAgentActivity`. The gap: today's sub-agent gets the
parent's full, unfiltered registry — no attenuation at all. Phase 2 is
therefore not "build delegation from scratch" but "add an attenuated
sibling to an already-shipped mechanism."

## Grounding

Read directly, not assumed:

- `aivyx-coder/crates/aivyx-core/src/delegate.rs` (849 lines) — full
  read. `DelegateTaskConfig` (the bundle of collaborators gathered once
  at registration time: `llm`, `gate`, `confiner`, `checkpointer`,
  `repo_map`, `events_tx`, `sub_agent_registry: ToolRegistry`,
  `plan_mode`, `autonomous_mode`, `injection_taint`, `context_tokens`,
  `edit_format`, `verification`, `max_iterations`, `broker_mode`).
  `DelegateTaskTool::execute` constructs a fresh `Agent::new(...)` with
  `max_tool_iterations: 1` (the OUTER loop here, not the inner agent,
  bounds the total round-trip budget — see the file's own extensive
  comment on why: capping the inner agent at `max_iterations` too would
  give `max_iterations²` worst-case round trips), a `SUB_AGENT_SYSTEM_PROMPT`
  constant, its own event channel forwarded to the parent as
  `SubAgentActivity`, and accumulates `TextDelta` content across every
  internal round-trip as the tool's return value.
- `aivyx-coder/crates/aivyx-tools/src/lib.rs:157-186` — `ToolRegistry`'s
  real API: `register`, `get`, `definitions()`, `plan_definitions()`
  (already filters by `!mutates_outside_session()`), and
  `exclude(&mut self, names: &[&str])` (denylist-based, in place,
  silently ignores absent names). No allowlist/`retain`-style method
  exists today.
- `aivyx-coder/crates/aivyx/src/agent_builder.rs:576-640` — the real
  wiring point: `sub_agent_registry` is built by cloning the parent's
  registry *before* `delegate_task` itself is registered onto the
  parent's own `registry` (structural recursion prevention), then
  `DelegateTaskConfig` is assembled and `DelegateTaskTool` registered.
  This is the exact pattern a new sibling tool follows.
- `aivyx-coder/crates/aivyx/src/agent_builder.rs:153-475` (approximate —
  `build_agent`'s full body) — **every path-aware tool has `deny_paths`
  baked in at construction time** (`GrepTool::new(deny_paths.clone())`,
  `MoveFileTool::new(deny_paths.clone())`, `GitCommitTool::new(deny_paths.clone())`,
  etc.), inline in one large function. There is no reusable,
  `deny_paths`-parameterized "build a registry" helper — confirmed by
  reading the function in full. This means attenuating a specialist's
  `deny_paths` (as opposed to which tools it can see at all) would
  require refactoring this function, not just filtering an
  already-built registry — explicitly deferred (see Decisions).
- `aivyx-coder/crates/aivyx-team/src/lib.rs` (Phase 1) —
  `effective_tool_allowlist(member: &TeamMember, lead_tools: &[&str]) -> Vec<String>`
  already computes exactly the intersection this phase needs;
  `default_coding_roster()` is the ready-to-use `TeamConfig` this phase
  wires in, with no loading mechanism yet (Phase 5's job per Foundation's
  own explicit scope note).

## Decisions

**1. `DelegateToSpecialistTool` is a new, separate tool — not an
extension of `DelegateTaskTool`.** Confirmed with the project owner:
keeps the two use cases (general-purpose sub-agent delegation vs.
team-mission delegation) structurally separate rather than growing
`delegate_task`'s args/config with an optional team-member branch.
Lives in `aivyx-core/src/delegate.rs` alongside `DelegateTaskTool` (same
crate-placement rationale: needs `Agent`/`LlmBackend`/`Tool` in scope
together) or a new sibling file in the same crate — file organization
decided at plan time based on how large the addition turns out to be.

**2. Args add one field over `delegate_task`'s**: `member: String`
(must match a `TeamConfig` member's `name`) alongside `task: String`.
An unknown `member` name returns `ToolOutput::Error`, matching
`delegate_task`'s own error-return convention for sub-agent failures
(never a hard tool-dispatch error).

**3. `DelegateToSpecialistConfig` mirrors `DelegateTaskConfig` field-for-field**,
reusing every shared collaborator unchanged (`llm`, `gate`, `confiner`,
`checkpointer`, `repo_map`, `events_tx`, `plan_mode`, `autonomous_mode`,
`injection_taint`, `context_tokens`, `edit_format`, `verification`,
`broker_mode`), plus two new fields: `team: aivyx_team::TeamConfig` and
the parent's full tool registry (to compute the attenuated subset from
at call time, mirroring how `sub_agent_registry` is a pre-clone for
`delegate_task`).

**4. Tool-allowlist attenuation only, this phase — deny_paths
attenuation is explicitly deferred.** A specialist's visible tool set is
computed via `effective_tool_allowlist`, then applied by computing an
exclude-list (`all registered tool names` minus the effective allowlist)
and calling `ToolRegistry`'s existing `exclude()` — no new `ToolRegistry`
API needed. A specialist's `deny_paths` remain identical to the lead's
own (no widening) — the real per-member `extra_deny_paths` attenuation
`aivyx-team`'s `effective_deny_paths` already computes stays unused by
any real caller until a later, focused phase refactors
`agent_builder.rs`'s registry construction to be `deny_paths`-parameterized.
Confirmed with the project owner as this phase's explicit scope
boundary, not an oversight.

**5. The specialist's system prompt is the member's `persona`**, not
`delegate_task`'s generic `SUB_AGENT_SYSTEM_PROMPT` constant.

**6. Not registered onto the default agent's tool list this phase.**
Registering a brand-new tool unconditionally in `agent_builder.rs` (the
way `delegate_task` itself is registered) would make every existing
session's model suddenly able to call it, using a hardcoded
`aivyx_team::default_coding_roster()` — a real behavior change for every
user, not just ones who asked for team mode. Phase 2 builds and fully
tests `DelegateToSpecialistTool`/`DelegateToSpecialistConfig` (wired in
tests against `default_coding_roster()`) but does **not** call
`registry.register(...)` for it in `agent_builder.rs`'s always-on path.
Real registration (and the "how does a user opt into team mode"
question) is Phase 5's job — the same phase that decides where a
`TeamConfig` actually comes from at runtime.

## What this spec does not decide

- File organization within `aivyx-core` (new file vs. extending
  `delegate.rs`) — a plan-time call based on the actual code's size.
- `deny_paths` attenuation and the `agent_builder.rs` refactor it would
  need — separate, later, unscoped work (Decision 4).
- Where a `TeamConfig` is actually loaded from at runtime, and how a
  user opts into team mode at all — Phase 5's concern, unchanged from
  Foundation's own deferral.
- Whether `DelegateToSpecialistTool` should eventually replace/merge
  with `DelegateTaskTool` once team mode is fully wired — not
  reconsidered here; Decision 1 keeps them separate for now.
- Phases 3-6 (mission structure, message bus, entry point, TUI) — each
  gets its own spec/plan cycle when reached.
