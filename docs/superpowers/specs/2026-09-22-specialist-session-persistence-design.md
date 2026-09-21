# Parked Specialist-Session Persistence Across Restart Design

## Context

Nonagon deferred gap #4 of the original 7: `SpecialistSessionPool`
(`crates/aivyx-core/src/specialist_sessions.rs`) holds parked specialist
sessions purely in-memory — each `ParkedSpecialistSession` owns a live
`Agent`, a `tokio::task::JoinHandle<()>` (`forward_task`), an
`Arc<Mutex<String>>` accumulator, a barrier-sync channel sender, and a
`last_active: Instant`. None of this is literally serializable (a live
`Agent` holds a running LLM backend handle, tool registry, gate/confiner).
If the process restarts — a crash, or a normal relaunch with `--resume` —
every parked specialist session is lost, and `query_specialist`/
`close_specialist` against its old `session_id` fails. Fourth in the
user-approved close-out order, after deny-paths attenuation, custom-roster
loading, and `/clear` reset (all shipped); two gaps remain after this
(specialist channel, ACP spoofability).

## Grounding

Read directly in the current codebase, not assumed:

- **The failure mode is already clean, not a crash or a hang.**
  `QuerySpecialistTool`/`CloseSpecialistTool` (`specialist_sessions.rs:651-655`,
  `727-730`) already return a clear `ToolOutput::Error` on an unknown
  `session_id`, explicitly telling the model to call `spawn_specialist`
  again. This scopes the real risk down to "the lead's own restored
  conversation might still mention an old `session_id`," not "the process
  is left in a broken state."
- **The lead's own session already persists this way**: `SessionState`
  (`session.rs`) is a plain, serializable `{version, history: Vec<Message>,
  tasks: Vec<Task>, plan_mode_active: bool}`, written by `Agent::persist()`
  after most turns (`agent/mod.rs:456`, `:1333`) and restored via
  `Agent::restore()` only when `--resume` is passed
  (`agent_builder.rs:1183-1199`). `#[serde(default)]` on `plan_mode_active`
  is the established precedent for adding a new field without breaking old
  session files.
- **`Agent` already holds the exact handle this needs**: Task #17
  (`/clear` resets real mission state) added
  `specialist_session_pool: Option<SpecialistSessionPool>` to `Agent`,
  set via `set_specialist_session_pool_handle` right alongside
  `set_mission_plan_handle` in `agent_builder.rs`. `Agent::persist()` can
  read the same handle to snapshot every parked session's history.
- **`build_specialist_agent`** (`specialist_sessions.rs:345-`) is the one
  function that already constructs a specialist's `Agent` + `forward_task`
  + `accumulated` + `barrier_tx` tuple, shared by `spawn_specialist` today.
  Rehydrating a dehydrated session reuses it as-is — the only new step is
  restoring history onto the freshly-built `Agent` before parking it.
- **`Agent::history` is private** (`agent/mod.rs`) — no accessor exists
  yet for another module in the same crate to read or set it directly.
  `Agent::restore()` sets the whole `SessionState` at once; a specialist
  needs only its own `history`, so this needs two small, new, dedicated
  methods rather than reusing `SessionState`/`restore()` for a different
  actor's data.
- **Pool construction happens before `--resume` is read**:
  `specialist_session_pool_handle` is constructed at
  `agent_builder.rs:840` (inside the `if settings.team.enabled` block);
  `session::load`/`agent.restore()` happens much later, at
  `agent_builder.rs:1183-1199`, gated on `cli.resume`. Seeding dehydrated
  sessions into the pool has to happen at that second site, using the
  `specialist_session_pool: Option<SpecialistSessionPool>` variable
  already in scope for the whole function (not the more locally-scoped
  `_handle` binding).
- **The pool's existing cap/description machinery**
  (`has_room()`, `open_sessions_description()`, `SessionPoolState`) only
  knows about the live `sessions: HashMap`. Counting dehydrated sessions
  against `max_concurrent` (Decision 4 below) means both need to read a
  second map too.

## Decisions

**1. `SessionState` gains a new field**, `specialist_sessions:
Vec<PersistedSpecialistSession>` (`#[serde(default)]`, matching
`plan_mode_active`'s own precedent), where `PersistedSpecialistSession`
is a new, plain serializable type — `{session_id: String, member: String,
history: Vec<Message>}` — defined in `session.rs` alongside
`SessionState`/`Task` (the crate's existing persistence-format types),
imported into `specialist_sessions.rs`. No `last_active`: a dehydrated
session doesn't expire from inactivity (nothing is consuming resources
while it sits as inert JSON) — the idle-timeout mechanism stays scoped to
the live map exactly as it works today.

**2. `Agent` gains two small, dedicated methods** —
`pub fn history_snapshot(&self) -> Vec<Message>` and
`pub fn restore_history(&mut self, history: Vec<Message>)` — rather than
reusing `SessionState`/`restore()` (which model the *lead's* own
persistence concept, not a specialist's). `Agent::persist()` calls the
new `SpecialistSessionPool::snapshot_for_persistence()` (Decision 3) and
includes its result in the `SessionState` it writes, only when
`self.specialist_session_pool` is `Some`.

**3. `SpecialistSessionPool` gains a second internal map**,
`dehydrated: HashMap<String, PersistedSpecialistSession>`, alongside the
existing live `sessions` map, plus:
   - `pub fn seed_dehydrated(&self, sessions: Vec<PersistedSpecialistSession>)`
     — called once in `agent_builder.rs`, only inside the existing
     `if let Some(state) = &restored { agent.restore(state.clone()); ... }`
     block, right after `agent.restore(...)`, using the persisted
     `state.specialist_sessions`.
   - `pub fn snapshot_for_persistence(&self) -> Vec<PersistedSpecialistSession>`
     — called from `Agent::persist()`. **Must union both maps**: every
     live session (id, member, `agent.history_snapshot()`) *and* every
     still-untouched `dehydrated` entry, carried forward unchanged. If
     this only snapshotted live sessions, a dehydrated session would
     survive exactly one restart and then silently vanish on the *next*
     save, since nothing else would carry it forward — unioning both
     means a dehydrated session survives indefinitely, across any number
     of restarts, until it's resumed (moves into the live map from then
     on) or explicitly closed.
   - A private `take_dehydrated(&self, id: &str) -> Option<PersistedSpecialistSession>`,
     mirroring `take`'s remove-and-return shape.

**4. Dehydrated sessions count against `max_concurrent`.** `has_room()`
and `open_sessions_description()` both read `sessions.len() +
dehydrated.len()` against the cap. Rationale: a dehydrated session is
still a conceptually-open conversation the model might reference from its
own restored history; not counting it risks unbounded growth of the
persisted file across many restart-without-closing cycles (spawn to the
cap, restart without closing, spawn to the cap again, repeat). The
cap-exceeded error message enumerates both groups by member so a blocked
model always has a concrete, actionable next step:
`"cannot open a new specialist session: 3 are already open (2 live:
implementer, reviewer; 1 dehydrated from a previous run: tester) — close
one with close_specialist first"`.

**5. `query_specialist`'s rehydration flow**: on an id not found in the
live map, check `dehydrated`. If found:
   - Look up `persisted.member` in `self.config.team.members`. If it no
     longer exists (roster changed since the crash), discard the record
     (already removed by `take_dehydrated`) and return a clear error
     naming the missing member — fails closed, consistent with this
     project's existing posture (custom-roster loading, deny-paths
     attenuation).
   - Otherwise, call `build_specialist_agent(member, &self.config,
     &ctx.cwd)` (the same function `spawn_specialist` uses), then
     `agent.restore_history(persisted.history)`, then fall into the exact
     same "run the exchange, re-park" code path `query_specialist`
     already uses for a live hit — no special-casing beyond that point.

**6. `close_specialist` on a dehydrated-only id** just discards the
record via `take_dehydrated` and reports success — no `Agent` is ever
built for a session that's only being closed, not resumed.

## What this spec does not decide

- **TUI/ACP display of dehydrated sessions** — the panels
  (`SpecialistSessionsUpdated`, `open_sessions()`) continue to show only
  live sessions, unchanged. The model already has visibility via its own
  restored conversation history (the original `spawn_specialist` result,
  containing the `session_id`, is itself a persisted `Message`) — no new
  discovery surface is needed for this phase.
- **Preserving specialist sessions across a `[team] enabled` toggle-off
  interval.** If a user disables team mode with dehydrated sessions still
  on disk, the next `persist()` call reads `specialist_session_pool:
  None` and writes an empty specialist-sessions list, silently dropping
  them. Narrow, deliberately accepted as a documented limitation rather
  than engineered around (would require reading and merging the
  session file's existing field even while the owning feature is off).
- Any change to the live-session idle-timeout mechanism,
  `close_specialist`'s existing live-session-found code path, or the
  lead's own `SessionState`/`--resume` mechanics beyond the one new field
  — all unchanged.
- The other two remaining Nonagon deferred gaps (a real
  specialist-to-specialist message channel, ACP entry-prefix
  spoofability) — untouched here.
