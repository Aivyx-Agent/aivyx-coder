# Architect/Editor Model-Pairing — Design

**Status:** Approved by user 2026-07-14. Phase 9 stretch goal, next item after
sub-agent delegation.

## Problem

Phase 9's roadmap describes "a stronger model plans in prose, a faster local
model executes the mechanical edit" — directly enabled once the prompted
SEARCH/REPLACE edit format existed (Phase 2, now the A/B-decided default in
one config: `edit_format = "prompted"` remains model-dependent, not load-bearing
here). Two models with genuinely different roles collaborate on one task: an
"architect" (presumably a stronger, possibly remote-capable model) produces an
implementation plan in prose; the existing primary/local model (the "editor")
turns that plan into actual tool calls and edits, exactly as it would any
other turn.

This is a new capability shape distinct from existing precedents:
- `/council` (Phase 11a) is read-only deliberation among several models,
  synthesizing one message into history — never continues into tool
  execution in the same turn.
- `delegate_task` (Phase 9) spawns a fully isolated nested `Agent` with its
  own conversation history, for context-isolated exploration — not a
  division of labor on the same task.

Architect/editor pairing needs the architect's output to feed directly and
immediately into the *existing* primary agent's own execution — one
continuous action from the user's point of view, not two separate agents.

## Command & trigger

`/architect <task>` — a slash command, parsed in `Agent::run_turn` alongside
`/council` and `/wiki`'s existing `parse_command`-style dispatch, via a new
`crate::architect` module (`aivyx-core/src/architect.rs`).

- Requires an explicit task argument. Bare `/architect` (no argument) emits a
  usage note and ends the turn — unlike `/council`'s bare form (which reviews
  the last assistant message), there is no natural "plan a review of what we
  just discussed" fallback for a mode whose whole purpose is producing a
  fresh implementation plan for a stated task.
- `parse_command` mirrors `council::parse_command`'s recognition rules
  exactly (rejects lookalikes like `/architecture`, rejects mid-sentence
  occurrences, trims whitespace).

## Turn flow

`run_architect_turn(&mut self, subject: &str, cwd: &Path, cancellation: CancellationToken) -> Result<(), AgentError>`:

1. **Unconfigured guard.** If `self.architect` is `None` (built once at
   `Agent` construction from `ArchitectSettings::configured()`, exactly
   mirroring how `self.council` is built), emit
   `AgentEvent::ArchitectNote` explaining how to configure `[architect]` in
   `config.toml`, then `AgentEvent::TurnComplete` and return `Ok(())` without
   touching history. No editor execution happens.

2. **Planning call.** One no-tools chat request to the architect's
   `LlmBackend`:
   - System prompt: a new `ARCHITECT_PROMPT` constant — "You are a senior
     engineer producing a concrete implementation plan for the task below, to
     be executed by a separate, faster model. Describe what to change and
     why, file by file where relevant. Do not write the actual diffs or full
     file contents — the executing model will produce those. Keep the plan
     concrete and actionable, not exploratory."
   - User prompt: the repo map (if `repo_map.enabled`, reusing
     `self.repo_map` exactly as the main turn loop does) + a tail digest of
     recent conversation (reusing `council::tail_digest`, budgeted by a new
     `architect.tail_budget_tokens` config field — see Config below) + the
     task subject.
   - Streamed live: every text delta is accumulated and, on completion,
     wrapped as `AgentEvent::ArchitectNote(text)` (the same "emit the whole
     collected answer as one note" shape `Council::convene` uses for a
     member's answer — not per-token streaming, since prose plans are short
     enough that one final note reads better than a fragmented stream).
   - `<think>...</think>` spans stripped via `council::strip_think`
     (visibility changed from private to `pub(crate)` — no behavior change).

3. **Failure handling.** A backend error, or a stripped response shorter than
   `council::MIN_ANSWER_CHARS`, is treated as a failed architect call: emit
   `AgentEvent::ArchitectNote` describing the failure, then `TurnComplete`,
   return `Ok(())`. Nothing is injected into history and the editor is never
   invoked — an unrequested fallback to "just run the raw task through the
   editor alone" would silently do something the user didn't ask for.

4. **Injection and hand-off.** On success, format:
   ```
   [Architect plan — {model} planned this task; you are executing it]

   Task: {subject}

   Plan:
   {plan}
   ```
   and call `self.run_turn_inner(formatted, cwd, cancellation).await` —
   **directly, not via a new code path**. `run_turn_inner` already pushes
   whatever string it receives as a `Role::User` message and runs the normal
   iteration loop (repo map refresh, tool-call loop, edit-format handling,
   plan-mode/autonomous-mode filtering, compaction — all of it, unmodified).
   This is the entire mechanism that makes pairing feel like one continuous
   action: the editor model sees the plan exactly as if it were the user's
   next message and begins calling tools immediately, in the same
   `run_architect_turn` call, without the user sending anything further.

`run_architect_turn`'s return value **is** `run_turn_inner`'s return value —
no separate `TurnComplete` needed after step 4, since `run_turn_inner` already
emits it on every path.

## Configuration

New `ArchitectSettings` in `aivyx-config/src/lib.rs`, added as
`pub architect: ArchitectSettings` on `Settings`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ArchitectSettings {
    pub base_url: String,
    pub model: String,
    pub api_key: Option<String>,
    /// Token budget for the conversation-tail digest the architect sees
    /// alongside the task — separate from `council.tail_budget_tokens`
    /// since the two features are configured and toggled independently.
    pub tail_budget_tokens: u32,
}

impl Default for ArchitectSettings {
    fn default() -> Self {
        Self {
            base_url: String::new(),
            model: String::new(),
            api_key: None,
            tail_budget_tokens: 3072,
        }
    }
}

impl ArchitectSettings {
    /// Off until both `base_url` and `model` are set — no separate enable
    /// flag, matching `VerificationSettings`'s "setting the value is the
    /// opt-in" convention.
    pub fn configured(&self) -> bool {
        !self.base_url.is_empty() && !self.model.is_empty()
    }
}
```

`Agent` gains a `architect: Option<ArchitectSeat>` field (constructed once,
alongside `council`, from `ArchitectSettings` + a constructed `LlmBackend`) —
mirroring `CouncilSeat`'s shape (`model: String`, `backend: Arc<dyn LlmBackend>`)
but named `ArchitectSeat` since there's no council to share the type with.

## Streaming / UI

New `AgentEvent::ArchitectNote(String)` variant in `aivyx-core/src/agent.rs`
(mirrors `CouncilNote` — one text-carrying variant, no structured fields).

In `aivyx-tui/src/app.rs`:
- New `ChatLine::Architect(String)` variant.
- Rendered with `Color::Cyan` and prefix `"architect> "` — distinct from
  `council>` (magenta) and `  sub-agent> ` (light yellow).
- `AgentEvent::ArchitectNote(text) => self.transcript.push(ChatLine::Architect(text))`,
  wired next to the existing `AgentEvent::CouncilNote` arm.

The editor's subsequent tool calls and text render exactly as normal agent
activity — no special prefix, since the editor *is* the primary agent
continuing its own turn, not a distinct entity the way a sub-agent or council
member is.

## Interaction with existing modes

- **Plan mode / autonomous mode**: no new logic. Once `run_turn_inner` takes
  over in step 4, it already re-evaluates `plan_mode.active()` and
  `autonomous_mode.active()` per iteration exactly as any other turn — a plan
  produced under plan mode can only be acted on with read-only tools by the
  editor, the same graceful-degradation shape `delegate_task` already
  exhibits.
- **`edit_format` (native vs. prompted)**: untouched. The editor is a normal
  turn; whichever `edit_format` is configured already applies to however it
  chooses to act on the injected plan.
- **Cancellation**: one `CancellationToken` spans both the architect call and
  the subsequent `run_turn_inner` call. Cancelling mid-plan stops before any
  editor execution starts (mirrors `Council::convene`'s cancellation
  checks); cancelling mid-execution behaves exactly like cancelling any
  other turn.
- **`/council`, `/wiki`**: unaffected. `/architect` is dispatched from the
  same top-level `match` in `run_turn`, as a third sibling arm — parse order
  is `/council`, then `/wiki`, then `/architect`, then plain `run_turn_inner`.

## Testing strategy

- `architect::parse_command` recognizes `/architect <task>` and rejects
  lookalikes/bare invocation/mid-sentence occurrences (unit tests, mirrors
  `council::parse_command`'s test shape).
- Unconfigured `[architect]` → `ArchitectNote` explaining configuration,
  `TurnComplete`, no history mutation, mock editor backend never invoked.
- Configured, mock architect backend returns a plan → history contains
  exactly one injected `Role::User` message with the expected marker/format,
  and the mock *editor* backend's subsequent tool call(s) appear afterward in
  the same turn — proves the `run_turn_inner` hand-off actually executes
  tools, not just that the plan text was injected.
- Architect backend failure (`LlmError`) → `ArchitectNote` describing the
  failure, nothing injected, editor backend never invoked.
- Architect response that is `<think>`-only (stripped to empty) → treated as
  a failure via the `MIN_ANSWER_CHARS` check, same as council's empty-answer
  handling.
- Plan-mode regression: architect plan injected, editor's own turn correctly
  filters to read-only tool definitions (no new filtering logic exists to
  test, but the composition of two existing mechanisms deserves its own
  test rather than trusting it by inspection).
- Cancellation mid-architect-call: `CancellationToken` cancelled during the
  planning request → note explaining cancellation, no injection, editor
  never invoked.
- One live E2E through the real binary (PTY harness, same pattern as
  delegation/wiki Task 6): a real architect-configured + editor-configured
  run of `/architect <task>` against a scratch repo, confirming the
  `architect>`-prefixed plan appears live, followed by real tool-call
  confirmation modals for the editor's edits.

## Non-goals

- No deliberation, ranking, or multi-seat architect config — exactly one
  architect seat, unlike council's N-member structure.
- No new permission tier — the architect itself never calls tools (no
  permission surface at all), and the editor's execution uses the existing
  `ConfirmationGate` unchanged.
- No streaming of the architect's plan token-by-token — one accumulated note
  on completion, matching council's per-stage note granularity.
