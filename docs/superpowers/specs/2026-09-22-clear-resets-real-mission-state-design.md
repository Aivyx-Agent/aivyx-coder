# `/clear` Resets Real Mission/Specialist-Session State Design

## Context

Nonagon Phase 6a's own final review flagged, but deliberately left
unfixed, a real gap: `/clear` resets the TUI's *displayed*
`mission_plan`/`open_specialist_sessions` (via `AgentEvent
::ConversationCleared`), but not the real `Arc<Mutex<MissionPlan>>`
`decompose_task`/`verify_output`/`synthesize_results` actually read and
write, nor the real `SpecialistSessionPool` `spawn_specialist`/
`query_specialist`/`close_specialist` operate on — so a later call to any
of those tools can "resurrect" the panel with stale, pre-clear content.
Tracked since as Nonagon deferred gap #6 of the original 7, third in the
user-approved close-out sequence.

## Grounding

Read directly in the current codebase, not assumed:

- **`/clear` is TUI-only today, confirmed by the code itself**:
  `aivyx-acp/src/translate.rs`'s own comment states
  `AgentEvent::ConversationCleared` "is only ever emitted by the TUI's
  `/clear` interception... this frontend doesn't wire that command up" —
  ACP has no `/clear` path at all today, so this fix's real scope is the
  TUI, not "both frontends" as the gap's original one-line summary might
  suggest.
- **`Agent::clear_conversation()`** (`crates/aivyx-core/src/agent/mod.rs`)
  is the entire real implementation: clears `history`, clears the shared
  `tasks: Arc<Mutex<Vec<Task>>>`, emits `ConversationCleared`, persists.
  It has no knowledge of `MissionPlan`/`SpecialistSessionPool` at all —
  neither is a field on `Agent` today; both live only in
  `MissionToolsConfig`/`SpecialistSessionsConfig`, constructed separately
  in `agent_builder.rs` and handed only to the mission/specialist-session
  tools.
- **The TUI's own `ConversationCleared` handler** (`aivyx-tui/src/app.rs`)
  already resets `self.mission_plan = None` and
  `self.open_specialist_sessions.clear()` — but these are the TUI's own
  *display* fields, not the real backing state.
- **`agent_builder.rs` already threads both pieces of real state out to
  `BuiltAgent`**, from the earlier `goal_achieved()` team-awareness fix:
  `mission_plan: Option<Arc<Mutex<MissionPlan>>>` and
  `specialist_session_pool: Option<SpecialistSessionPool>` are both
  already cloned into local variables (`mission_plan`,
  `specialist_session_pool`) by the time the lead `Agent::new(...)` is
  called — no new construction needed to also hand `Agent` itself a
  third clone of each.
- **`Agent` already has an established pattern for optional,
  post-construction state**: `set_repo_map`, `set_verification`,
  `set_injection_taint`, `set_broker_mode` — all `pub fn set_X(&mut self,
  ..)` setters called right after `Agent::new(...)` in `agent_builder.rs`,
  not required constructor parameters (avoiding a breaking change to
  every other `Agent::new` call site — `delegate.rs`,
  `delegate_to_specialist.rs`, `specialist_sessions.rs`, the MCP-server
  frontend's own session construction).
- **`close_specialist`'s real close sequence**, read directly
  (`CloseSpecialistTool::execute`): `pool.take(&session_id)` (removes
  from the map), `drop(session.agent)`, then `.await`s
  `session.forward_task` to completion before reporting success.
  `SpecialistSessionPool` has no bulk "close everything" method today —
  only `new`, `max_concurrent`, and `open_sessions` are public.
- **`clear_conversation()` is synchronous, not `async`** — awaiting each
  closed session's `forward_task` (as `close_specialist` does) isn't
  actually necessary for correctness: dropping a parked session's
  `Agent` closes the event channel its background forwarding task reads
  from, so that task's next `.recv()` returns `None` and it ends on its
  own. `close_specialist` awaits it only for its own tidiness (reporting
  "closed" deterministically to the model) — a synchronous `close_all`
  that just drops every session's `Agent` without awaiting anything is
  equally correct for `/clear`'s purposes.

## Decisions

**1. `Agent` gains two new, optional fields** — `mission_plan:
Option<Arc<Mutex<aivyx_types::MissionPlan>>>` and
`specialist_session_pool: Option<SpecialistSessionPool>` — plus two new
setters, `set_mission_plan_handle`/`set_specialist_session_pool_handle`,
matching the exact existing `set_repo_map`/`set_injection_taint`
convention.

**2. `agent_builder.rs` calls both new setters on the lead `Agent`**,
right alongside the existing `agent.set_injection_taint(...)`/
`agent.set_broker_mode(...)`/`agent.set_repo_map(...)` calls, passing the
*same* `mission_plan`/`specialist_session_pool` local `Option` values
already threaded to `BuiltAgent` — an additional cheap `Arc`/pool clone
each, no new construction.

**3. `SpecialistSessionPool` gains a new `close_all(&self)` method**,
mirroring `close_specialist`'s own session-removal logic (drop every
parked session's `Agent`) without the `.await` on `forward_task` (see
Grounding — not needed for correctness in this synchronous context).

**4. `clear_conversation()` resets both, if present**: the `MissionPlan`
handle is reset to its pristine default (`MissionPlan { mission:
String::new(), steps: vec![], summary: None }` — the exact same initial
value `agent_builder.rs` already constructs it with); the specialist
session pool, if present, gets `close_all()` called on it. No new
`AgentEvent` is emitted for either — the TUI's existing
`ConversationCleared` handler already resets the *display* correctly
today; this fix only makes the *real* backing state actually match it,
so any later `decompose_task`/`spawn_specialist` call naturally emits a
correct, non-stale `MissionsUpdated`/`SpecialistSessionsUpdated` event
going forward, through the existing, unmodified event path.

## What this spec does not decide

- Wiring `/clear` into the ACP frontend — confirmed out of scope; ACP has
  no `/clear` command at all today (a separate, pre-existing, already
  self-documented gap in `aivyx-acp/src/translate.rs`), not something
  this fix introduces or needs to resolve.
- Awaiting `close_all`'s dropped sessions' `forward_task`s before
  `clear_conversation()` returns — confirmed unnecessary for correctness
  (Grounding); each task ends on its own once its channel closes.
- Any change to `close_specialist`'s own single-session close path, or to
  `decompose_task`/`verify_output`/`synthesize_results`'s own logic —
  both already correct and unchanged; this spec only adds a reset path
  triggered by `/clear`.
- Evicting Always-Allow cache entries on `/clear` — a separate, already
  independently logged gap (`clear_conversation`'s own existing doc
  comment), not part of this fix.
