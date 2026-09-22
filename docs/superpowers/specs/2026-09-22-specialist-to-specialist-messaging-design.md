# Specialist-to-Specialist Messaging Design

## Context

Nonagon deferred gap #5 of the original 7: today, if specialist A's
response needs to inform specialist B, the only path is the *lead*
manually relaying text between two separate `query_specialist` calls —
the lead is a mandatory intermediary for any specialist-to-specialist
coordination, even though nothing in this codebase's dispatch model is
actually concurrent (a prior phase confirmed tool calls run strictly one
at a time; there is never a moment when two specialists, or the lead and
a specialist, are alive simultaneously — this is *why* the original
roadmap's "Phase 4 message bus" was replaced with the long-lived
specialist sessions this phase builds on). Fifth in the user-approved
close-out order; one gap remains after this (ACP entry-prefix
spoofability).

## Grounding

Read directly in the current codebase, not assumed:

- **A specialist's own tool registry cannot reach `spawn_specialist`/
  `query_specialist`/`close_specialist`/the three mission tools today —
  structurally, not by policy.** `agent_builder.rs`'s own code comment,
  read directly, explicitly anticipates this phase: *"Because this
  snapshot is taken before decompose_task/verify_output/
  synthesize_results/spawn_specialist/query_specialist/close_specialist
  are registered further down, a specialist's own attenuated registry can
  never include any of those six tools either... a phase adding custom
  rosters will need to revisit where this snapshot is taken if
  specialists should ever be granted them."* `compute_specialist_registry`
  (`delegate_to_specialist.rs`) only ever *removes* tools from a cloned
  parent registry (`attenuated.exclude(&exclude_names)`) — it has no
  mechanism to register a *new* tool instance, which this phase needs.
- **`SpawnSpecialistTool`/`QuerySpecialistTool`/`CloseSpecialistTool` are
  each constructed once, bound to one `SpecialistSessionsConfig`,** which
  itself holds the one shared `SpecialistSessionPool`
  (`crates/aivyx-core/src/specialist_sessions.rs`). Cloning the *same*
  tool instances into a specialist's registry would work mechanically
  (the pool `Clone`s cheaply over an `Arc<Mutex<_>>`), but gives every
  specialist an identical, undifferentiated `caller` identity — wrong for
  ownership tracking (below). This phase needs `build_specialist_agent`
  to construct *fresh* tool instances per specialist, bound to a
  purpose-built child config.
- **A specialist's own session_id doesn't exist yet at the point
  `build_specialist_agent` runs.** `SpawnSpecialistTool::execute`
  currently generates the session's `session_id` *after* calling
  `build_specialist_agent`. This phase needs the id available *before*,
  so it can be threaded into the specialist's own child config as its
  `caller` identity for any sessions *it* later opens. Requires
  reordering that one call site.
- **`SessionPoolState`/`ParkedSpecialistSession`/`PersistedSpecialistSession`
  have no owner concept at all today** — a flat map, any caller with the
  tools can target any `session_id`. The restart-persistence phase
  (shipped immediately prior) added `PersistedSpecialistSession
  {session_id, member, history}`; this phase adds a fourth field to that
  same type, so the two phases' formats must stay compatible.
- **`TeamConfig::validate`'s `UnknownTool` check is driven entirely by
  the `available_tools` list callers pass in** (`aivyx-team/src/lib.rs`)
  — the function itself needs no logic change; only what
  `agent_builder.rs` passes as `available_tools` needs to additionally
  include the three session-tool names by literal string, since they
  aren't present in the registry snapshot taken at that point.
- **The existing "cannot spawn the team's own lead" check** (in
  `SpawnSpecialistTool::execute`, checking `args.member ==
  self.config.team.lead`) is untouched by and sufficient alongside this
  phase — no additional self-spawn restriction is needed (see "What this
  spec does not decide").

## Decisions

**1. `SpecialistSessionsConfig` gains two new fields**: `spawn_depth: u32`
(`0` for the lead's own top-level config, constructed once in
`agent_builder.rs`) and `caller: SessionOwner` (a new, small enum —
`SessionOwner::Lead` or `SessionOwner::Specialist(String)`, the latter
holding the calling specialist's own `session_id`). Both derive `Clone`;
`SessionOwner` also derives `Debug, PartialEq, Eq, Serialize, Deserialize`
(needed for persistence, Decision 5).

**2. `SpawnSpecialistTool::execute` is reordered**: generate `session_id`
*before* calling `build_specialist_agent`, so it can be threaded through.
Every newly-created `ParkedSpecialistSession` gains an `owner: SessionOwner`
field, set to `self.config.caller.clone()`.

**3. `build_specialist_agent` gains a new registration step**, run after
`compute_specialist_registry`'s existing attenuation, that conditionally
registers fresh `SpawnSpecialistTool`/`QuerySpecialistTool`/
`CloseSpecialistTool` instances onto the specialist's own registry — one
tool at a time, each gated independently on whether `member.tool_allowlist`
names it (a plain string check against the member's own list, not
`effective_tool_allowlist`, since availability here is gated by depth,
not by presence in the attenuated snapshot) — **and only if
`config.spawn_depth < MAX_SPECIALIST_SPAWN_DEPTH`** (a new constant,
`1`). Each registered instance is bound to a *child*
`SpecialistSessionsConfig`: same `parent_registry`/`team`/`llm`/etc. as
the specialist's own config, but `spawn_depth: config.spawn_depth + 1`
and `caller: SessionOwner::Specialist(this_specialist's_own_session_id)`.
A specialist at `spawn_depth == MAX_SPECIALIST_SPAWN_DEPTH` gets no
session tools registered at all, regardless of its own `tool_allowlist`
— structurally impossible to nest further, the same shape as
`delegate_task`'s own recursion prevention, just enforced at
registration time via a depth check rather than via the parent snapshot
never containing the tool in the first place (which doesn't work here,
since these tools genuinely *should* exist for a depth-0 specialist).

**4. Ownership enforcement in `query_specialist`/`close_specialist`**:
after finding a target session (live or, per the restart-persistence
phase, dehydrated), compare its `owner` against `self.config.caller`. On
a mismatch: return a clear `ToolOutput::Error` naming that the session
wasn't opened by the caller, and — critical — **put the session back
exactly as found** before returning (`pool.put_back(...)` for a live hit,
`pool.seed_dehydrated(vec![persisted])` for a dehydrated hit) so a
rejected, unauthorized query never has the side effect of removing or
disturbing the session. The lead's own calls are unaffected: the lead's
top-level config has `caller: SessionOwner::Lead`, matching every
lead-opened session's own `owner`.

**5. `PersistedSpecialistSession` gains a fourth field**, `owner:
SessionOwner`, `#[serde(default)]` — defaulting to `SessionOwner::Lead`
(`SessionOwner` implements `Default` returning `Lead`) so a session file
written by the immediately-prior restart-persistence phase (which has no
concept of specialist-opened sessions at all) still loads correctly: any
session persisted before this phase existed was necessarily lead-opened.

**6. `agent_builder.rs`'s `available_tools` list** (passed to
`resolve_team_config`/`TeamConfig::validate`) gains the three literal
strings `"spawn_specialist"`, `"query_specialist"`, `"close_specialist"`,
added explicitly alongside the existing `registry.definitions()`-derived
list, with a doc comment explaining why (matches this exact codebase's
own established practice of extending derived-list constants alongside a
short rationale, e.g. `resolve_team_config`'s own precedent).

## What this spec does not decide

- `decompose_task`/`verify_output`/`synthesize_results` remain fully
  excluded from every specialist's registry, unchanged — this phase is
  scoped to the three session-management tools only, per explicit user
  choice during brainstorming.
- Any TUI/ACP display change — `open_sessions()`/
  `SpecialistSessionsUpdated`/the merged ACP Plan panel show a
  specialist-opened session identically to a lead-opened one; no new
  "opened by" indicator. Deliberately out of scope, matching this
  codebase's established pattern of not touching display surfaces unless
  a phase specifically targets them.
- A specialist spawning another session of its *own* member type (e.g.
  "implementer" spawning a second "implementer") needs no new
  restriction beyond the existing "cannot spawn the team's own lead"
  check — treated as an independent, legitimate session, same as any
  other pairing.
- Raising `MAX_SPECIALIST_SPAWN_DEPTH` above `1`, or making it
  configurable — a fixed constant for this phase; revisit only if a real
  need for deeper nesting emerges.
- Any change to `max_concurrent_specialist_sessions`'s existing semantics
  — the shared pool and its cap (already counting dehydrated sessions,
  per the immediately-prior phase) are untouched; specialist-opened
  sessions simply count against the same cap as lead-opened ones.
- Any change to the restart-persistence phase's own union/carry-forward
  logic (`snapshot_for_persistence`, `seed_dehydrated`, `close_all`) — only
  the one new `owner` field is added to the type it already persists.
