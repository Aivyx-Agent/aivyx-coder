# Sub-agent delegation (`delegate_task`) — design

## Context

Phase 9 ("Stretch goals," ROADMAP.md) lists sub-agent delegation for
context-isolated exploration alongside architect/editor model-pairing, LSP
integration, and MCP support, and explicitly prioritizes delegation and
pairing above LSP/MCP because "both compound with the project's existing
small-context-window discipline (repo map, compaction, `LlmBackend`
interchangeability already proven by council mode) rather than requiring
new architecture to support."

This project has three existing multi-turn/multi-model precedents worth
distinguishing from what this design builds:

- **`/council`** (Phase 11a): several `LlmBackend`s independently answer a
  read-only, no-tools question; only the chairman's synthesis enters
  history. User-triggered, tool-free.
- **`/wiki`** (Phase 11b): drives several sequential `Agent` turns (one per
  stale page) through the existing `TurnPaused`-continuation mechanism,
  reusing `run_turn_inner` directly. User-triggered, full tool access.
- **`--auto`** (Phase 11c): a distinct `AutonomousMode` trust tier for
  unattended tool use, with its own `ConfirmationGate` tier.

None of these is "sub-agent delegation" in the Phase 9 sense: a task the
*model itself* decides mid-turn to hand off to a fresh, isolated context, so
the parent's own context window doesn't have to hold the raw
exploration/edit trail. This is a genuinely new capability shape, not a
variant of an existing one — it's the first place a tool call itself needs
to trigger a nested multi-turn agentic loop.

## Goals

- A new tool, `delegate_task`, callable by the model mid-turn like any
  other tool.
- The delegated sub-agent gets full tool access — same trust boundary as
  the parent (same `ConfirmationGate`, `deny_paths`, checkpoint timeline,
  `plan_mode`/`autonomous_mode` flags) — not a security-restricted subset.
  "Context-isolated" means a fresh LLM conversation history, not a fresh
  trust boundary; this sidesteps the kind of dedicated security-profile
  design pass Phase 11c needed.
- The sub-agent's own activity (tool calls, tool results, streaming text)
  renders live in the transcript, visually distinguished from the parent's
  own activity, so a confirmation modal triggered by the sub-agent has
  visible context instead of appearing with no lead-up.
- Only the sub-agent's final distilled result enters the parent's history
  (as the `delegate_task` tool call's result) — its own turn-by-turn
  history never does.

## Non-goals (v1)

- Recursive delegation — a sub-agent's own tool list excludes
  `delegate_task`, capping delegation at one level.
- A different/cheaper `LlmBackend` for the sub-agent — the sub-agent uses
  the same backend/model as the parent. Model-swapping for a
  faster/cheaper worker is closer to the separately-tracked
  architect/editor pairing stretch goal, not this one.
- Any parent-conversation context seeding beyond the task description
  string itself (no tail-digest, unlike `/council`) — full context
  isolation, not partial.

## Architecture

### The gap: `Tool` doesn't have Agent-level reach

`Tool::execute(&self, arguments: Value, ctx: &ToolExecutionContext) ->
Result<ToolOutput, ToolError>` — `ToolExecutionContext` today is just
`{ cwd, confiner, cancellation }`. Every existing multi-turn mechanism
(`/council`, `/wiki`) avoids this limitation by intercepting raw user text
*before* a turn starts, inside `Agent::run_turn` — a path that only exists
for the very first message of a turn. `delegate_task` is triggered by the
*model*, from inside `run_turn_inner`'s normal tool-dispatch loop, so that
interception point doesn't apply here; this is the first tool that needs
to reach back up into Agent-level machinery (an `LlmBackend`, a way to
emit `AgentEvent`s, a bounded sub-agent tool registry) that the `Tool`
trait was deliberately kept ignorant of.

**Resolution (revised during plan-writing — see Decision log):** every
one of these dependencies (`LlmBackend`, `PermissionGate`,
`ExecutionConfiner`, the checkpointer, the repo map, the events sender,
the sub-agent registry, `max_iterations`) is stable for the whole
session — constructed once in `main.rs`, never varying call to call. This
is exactly the shape `RunCommandTool::new(command_specs)` already
established: tool-specific configuration baked into the tool's own struct
at registration time, not threaded through the generic
`ToolExecutionContext` every tool receives. `DelegateTaskTool::new(llm,
gate, confiner, checkpointer, repo_map, events_tx, sub_agent_registry,
max_iterations)` holds everything it needs as its own fields;
`ToolExecutionContext` is untouched, and so is every other `Tool` impl and
its tests — the only per-call input `delegate_task` needs beyond what
`ToolExecutionContext` already provides (`cwd`, `cancellation`) is the
`task` argument itself, already available via `execute()`'s existing
`arguments: Value` parameter.

**Crate placement (also revised during plan-writing):** `aivyx-tools` has
no dependency on `aivyx-core` or `aivyx-llm` — the dependency graph runs
the other way (`aivyx-core` depends on `aivyx-tools`, `aivyx-llm`, and
`aivyx-repomap`). `DelegateTaskTool` needs `aivyx_tools::Tool` (the trait
it implements), `aivyx_llm::LlmBackend`, and `aivyx_core::Agent` (to
construct the nested agent) simultaneously in scope, which only
`aivyx-core` can see all three of. `DelegateTaskTool` is therefore defined
in `aivyx-core` (e.g. `crates/aivyx-core/src/delegate.rs`), implementing
the foreign `aivyx_tools::Tool` trait for a type local to `aivyx-core` —
allowed under Rust's orphan rule — rather than living alongside the
simple, LLM-agnostic tools in `crates/aivyx-tools/src/tools/`. This also
better matches what the tool actually is: an Agent-level orchestration
mechanism, not a dumb filesystem/process action.

`DelegateTaskTool::execute()` uses its own
held fields to construct a fresh `ToolExecutor` (sharing the parent's
`gate`, `confiner`, `checkpointer`) and a fresh `Agent` on top of it
(sharing the parent's `llm`, `repo_map`, `events_tx`) — drive it turn by
turn until either a natural final answer or the iteration cap, and return
its final text as the tool's `ToolOutput::Ok(text)`.

### Trust and shared state

The sub-agent's `ToolExecutor` is constructed with the **same**
`gate: Arc<dyn PermissionGate>`, `confiner: Arc<dyn ExecutionConfiner>`,
and `checkpointer: Option<Arc<GitCheckpointer>>` as the parent's — same
Always-Allow cache, same `deny_paths`, same checkpoint ref timeline (a
human reviewing `refs/aivyx/checkpoints/*` afterward sees one unified
history regardless of whether the parent or a sub-agent made a given
edit). This is the concrete meaning of "context-isolated, not
security-isolated": no new `ConfirmationGate` tier, no new trust-profile
design pass.

`delegate_task` itself is always auto-allowed at the gate —
`PermissionTarget::Other("delegate_task")` with `ActionKind::Internal`,
the same tier `set_tasks`/`read_file` already resolve through. Kicking off
delegation isn't the sensitive action; every mutation the sub-agent
actually attempts is separately gated when it happens, through the same
shared gate the parent uses for its own actions.

`delegate_task.mutates_outside_session()` returns `false` — it stays
**offered** during plan mode rather than hidden. This is deliberate,
not an oversight: the sub-agent's own per-turn tool list is built with the
identical `if self.plan_mode.active() { plan_definitions() } else {
definitions() }` logic the parent's `run_turn_inner` already uses (both
read the same shared `PlanMode` flag), so a sub-agent spawned while the
parent is in plan mode automatically only sees read-only tools — it
degrades gracefully to a read-only exploration helper rather than needing
to be hidden outright. This differs from why `write_file`/`edit_file` are
hidden from the *parent* specifically (offering a tool that would always
be denied invites the small-model retry-loop Phase 8 found) — here, the
sub-agent's own tool list is already correctly filtered before it ever
sees the option, so nothing mysterious fails.

Verification (`[verification].command`, if configured) runs **inside**
the sub-agent, using its own `unverified_edits`/`verify_retries` state,
because the sub-agent is a genuine `Agent` instance with its own turn
loop, not a stripped-down variant. By the time `delegate_task` returns to
the parent, that cycle has already resolved (passed, or exhausted with a
loud notice folded into the sub-agent's final text) — the parent's own
`unverified_edits` is never involved, since the parent never personally
called a mutating tool for those edits.

If repo-map is enabled, the sub-agent shares the parent's `Arc<RepoMap>` —
this is project-structural context the sub-agent needs to be useful at
all, not parent-conversation leakage the way a history digest would be.

### Live visibility

**Revised during plan-writing:** `/council`'s `CouncilNote` precedent
doesn't transfer directly — council emits its own coarse-grained notes
itself, at stage boundaries, and never has to forward a *separate* nested
agent's full token-by-token event stream. A sub-agent is a genuine nested
`Agent` with its own internal turn loop, so its `ToolCallDetected`/
`ToolResult`/`TextDelta` events need to reach the transcript without being
indistinguishable from the parent's own (which sharing one `events_tx`
literally would produce, since both would emit the exact same
`AgentEvent` variants). Resolution: the sub-agent gets its own
`mpsc::unbounded_channel()`; `DelegateTaskTool::execute()` spawns a
background task for the duration of the delegated call that drains the
sub-agent's receiver and forwards each event, wrapped in a new
`AgentEvent::SubAgentActivity(Box<AgentEvent>)` variant, onto the parent's
real `events_tx` (baked into `DelegateTaskTool` at construction). The TUI
unwraps this variant and renders the inner event through a
visually-distinguished path (a new `ChatLine` variant, analogous to
`ChatLine::Council`) instead of the normal one. This gives true live,
token-by-token streaming with proper visual distinction — a confirmation
modal triggered by the sub-agent's own `write_file`/`run_command` call has
visible lead-up explaining what's being attempted and why, rather than
appearing with no context.

### Stopping conditions

The sub-agent has its own iteration budget, `[sub_agent] max_iterations`
(default 10) — deliberately separate from and smaller than the parent's
`max_tool_iterations_per_turn` (a delegated task is meant to be a scoped,
bounded piece of work, not a full session). If the sub-agent reaches a
natural final answer (no further tool calls) before the cap, that text is
the result. If it hits the cap without finishing, `delegate_task` still
returns `ToolOutput::Ok` — never an error — carrying the sub-agent's last
response plus a clear cutoff notice, so the parent sees this as a normal
(if incomplete) tool result and can choose to delegate again with a
narrower task, adjust its own approach, or proceed with partial
information. Any edits the sub-agent already made before hitting the cap
are not discarded (no rewind — that mechanism is `--auto`'s, gated on
`autonomous_mode.active()`, which is never true here); the checkpoint
timeline already covers them the same way it covers any other edit.

## Config

```toml
[sub_agent]
max_iterations = 10   # a delegated task's own tool-call budget, separate
                       # from the parent's per-turn max_tool_iterations
```

## Testing strategy

Mirrors the `/council`/`/wiki` precedent:

- **`delegate_task` drives a nested `Agent` to completion**: mock
  `LlmBackend`, assert the tool call's result matches the sub-agent's
  final text, assert the sub-agent's own history never enters the
  parent's history.
- **Cap exhaustion**: mock backend that never stops issuing tool calls;
  assert `delegate_task` returns `Ok` with a cutoff notice, not an error.
- **Recursion is structurally impossible**: assert `delegate_task` is
  absent from the sub-agent's own offered tool definitions.
- **Plan-mode filtering**: with the parent's `PlanMode` active, assert the
  sub-agent's own tool list excludes `write_file`/`edit_file`/etc., the
  same as the parent's would.
- **Shared checkpoint timeline**: real-git fixture test proving an edit
  made by the sub-agent produces a checkpoint ref indistinguishable in
  mechanism from one the parent would have made.
- **Live event streaming**: assert `AgentEvent`s emitted during the
  sub-agent's turn reach the shared `events_tx` with the sub-agent's
  activity distinguishable from the parent's.
- One live E2E through the real binary before calling this phase done,
  matching every other phase's bar in ROADMAP.md.

## Decision log

| Fork | Decision |
|---|---|
| Trigger | A tool the model calls mid-turn, not a human-triggered slash command |
| Tool scope | Full tool access, same trust boundary as the parent — not a read-only-only restriction |
| Visibility | Streams live into the transcript, visually distinguished, not hidden until the final result |
| Streaming mechanism | A spawned forwarding task drains the sub-agent's own `AgentEvent` channel and re-emits each event wrapped in `AgentEvent::SubAgentActivity(Box<AgentEvent>)` onto the parent's channel, for true token-by-token live streaming with visual distinction (revised during plan-writing after `/council`'s simpler same-channel precedent turned out not to apply to a genuinely nested `Agent`) |
| Recursion | Capped at one level — a sub-agent's tool list excludes `delegate_task` |
| Budget | New dedicated `[sub_agent] max_iterations` config, smaller default than the parent's per-turn cap |
| Cap exhaustion | Best-effort partial result with a clear cutoff notice, never an error |
| Context seed | Task description only — no parent-conversation digest |
| `ToolExecutionContext` gap | Baked into `DelegateTaskTool::new(...)` at registration time instead — matches `RunCommandTool`'s existing config-at-construction pattern, zero changes to `ToolExecutionContext`/`ToolExecutor`/any other `Tool` impl (revised during plan-writing after the field-extension approach turned out to touch 8 existing construction sites for no per-call benefit — every dependency is session-stable, not per-call) |
| `delegate_task`'s own gate treatment | Always auto-allowed (`Internal`/`Other`), offered during plan mode (degrades gracefully via the shared `PlanMode` flag) |
| Crate placement | Defined in `aivyx-core` (e.g. `delegate.rs`), not `aivyx-tools/src/tools/` — `aivyx-tools` has no dependency on `aivyx-core`/`aivyx-llm`, so it cannot see `Agent` or `LlmBackend` at all; only `aivyx-core` can see both plus the `aivyx_tools::Tool` trait it implements (revised during plan-writing) |
