# Reasoning Visibility — Design

**Status:** Approved by user 2026-07-19. Second of 4 sub-projects closing the
capability gaps identified in a fresh audit of aivyx-coder's actual
code-writing ability (the first, multi-file edit atomicity, is merged; the
other two — structured verification memory, repo-map multi-language
support — are separate, later specs).

## Context

**Problem this spec solves:** a reasoning-capable model's chain-of-thought
is currently silently dropped. There is no first-class way to see *why* the
agent is about to do something — including right before approving a
mutating action, though that's a special case of the general problem, not
the whole scope (see Decision 1).

Facts confirmed against the current codebase, and empirically against a
real backend, before this design was written:

- `WireDelta` (`crates/aivyx-llm/src/openai_compat.rs:470-476`) is `{
  content: Option<String>, tool_calls: Option<Vec<WireToolCallDelta>> }` —
  no reasoning field at all, so serde's default `#[derive(Deserialize)]`
  behavior silently ignores any unrecognized JSON key during parsing. The
  file's own existing doc comment (lines 86-91) already flags this gap but
  names the field `delta.reasoning` — **this is factually wrong** (see
  next bullet); this spec both closes the gap and fixes the comment.
- **Empirically verified against a real llama-server + Qwen3.5-9B-GGUF
  request** (`chat_template_kwargs: {"enable_thinking": true}`, matching
  this project's own documented way to force thinking on): the actual wire
  field is `delta.reasoning_content`, e.g. `{"choices":[{"delta":
  {"reasoning_content":"Thinking"}}]}`. This is the DeepSeek API's original
  naming, since adopted by llama-server/vLLM for compatibility with
  DeepSeek-style clients — not `delta.reasoning`. With thinking disabled
  (this project's own daily-driver serving config, matching Phase 10 Part
  A's "explicit 16k window, thinking disabled" finding), no `content`
  field's sibling appears at all — the whole mechanism is naturally
  self-gating: a non-reasoning model, or a reasoning model with thinking
  turned off, simply never sends this field, and the feature does nothing.
- `StreamEvent` (`crates/aivyx-llm/src/backend.rs:48-59`) is `TextDelta(String)
  | ToolCallComplete(ToolCall) | Usage { .. } | Done { .. }` — the turn
  loop's streaming match (`crates/aivyx-core/src/agent/mod.rs:1055-1095`,
  inside `ToolCallAccumulator::consume` at `openai_compat.rs:212-269`) is
  exhaustive over every variant with no wildcard arm in either place, so a
  new variant requires a new arm at each site, not a silent no-op.
- `AgentEvent` (`crates/aivyx-core/src/agent/types.rs:9-40`) is the parallel
  TUI-facing event enum; `Agent`'s turn loop currently handles
  `StreamEvent::TextDelta` by *both* accumulating into `assistant_text`
  (which eventually becomes `ContentBlock::Text` in `self.history`) *and*
  emitting `AgentEvent::TextDelta` for the TUI (`agent/mod.rs:1056-1058`).
- `ChatLine` (`crates/aivyx-tui/src/app.rs:71-...`) is the TUI's own
  transcript-line enum; `handle_agent_event`'s `AgentEvent::TextDelta` arm
  (`app.rs:350-356`) appends to the last transcript line if it's already a
  matching variant, else pushes a new one — the same idiom this spec's new
  `ChatLine::Reasoning` variant will reuse. `chat_line_to_lines`
  (`app.rs:735-774`) renders every variant via `prefixed_lines(text,
  "prefix", style)`, with `Paused`/`Council`/`Architect`/`SubAgent` each
  already carrying their own distinct color to read as "not the normal
  assistant voice" — the established precedent this spec's new variant
  follows.
- `sub_agent_event_text` (`app.rs:579-592`) is a second exhaustive match
  over `AgentEvent`, used to render a `delegate_task` sub-agent's nested
  events into `ChatLine::SubAgent` text — a new `AgentEvent` variant needs
  an arm here too.
- Session persistence (`crates/aivyx-core/src/session.rs` and the
  `Role`/`ContentBlock`-driven history-reconstruction match at
  `app.rs:531-556`, which rebuilds `ChatLine`s from a resumed session's
  `Vec<Message>`) only ever iterates `Role::User`/`Role::Assistant`
  `ContentBlock::Text`/`ContentBlock::ToolCall`/`ContentBlock::ToolResult`
  — reasoning was never part of this data model, and this spec keeps it
  that way (Decision 2), so this reconstruction path needs no new code at
  all; a resumed session simply shows no reasoning for prior turns, exactly
  as today.

## Decisions (confirmed with user via one-at-a-time questions before this doc was written)

1. **Scope**: general visibility into the model's thinking throughout the
   transcript — not narrowly scoped to the permission-confirmation modal.
   The "see why before approving" use case falls out for free, since
   reasoning naturally streams immediately before the tool call it explains.
2. **Persistence**: reasoning content is **TUI-transcript-only** — it never
   touches `Message`/`ContentBlock`/`self.history`, is never sent back to
   the model on a later turn, and is never written to the session JSON. A
   resumed session (`--resume`) shows no reasoning for prior turns, only
   newly-streamed reasoning going forward. This matches how reasoning-model
   API providers themselves describe this content: meant for the human,
   not for replay as conversation history (re-sending it would also
   inflate context for no benefit).
3. **Presentation**: shown live as it streams, styled distinctly from the
   final answer — not collapsed-by-default, not gated behind a toggle.
4. **No new config surface**: no flag to disable reasoning display. The
   mechanism is inherently self-gating (see the "Facts" section above) —
   a model/serving config that never sends `reasoning_content` triggers
   nothing, so there is no case where enabling this "does something
   unwanted" that a toggle would need to suppress.
5. **Sub-agent reasoning surfaces too**: `delegate_task`'s nested
   `AgentEvent`s already get rendered into the parent's transcript via
   `sub_agent_event_text`/`ChatLine::SubAgent` — a sub-agent's own
   reasoning is treated the same way its `TextDelta` already is (rendered
   as text, prefixed as `sub-agent>` in the parent's transcript), matching
   Decision 1's "general visibility" framing rather than being scoped out.

## Changes

### 1. `WireDelta` gains the real field, and the stale comment is fixed

`crates/aivyx-llm/src/openai_compat.rs`:

```rust
#[derive(Deserialize, Default)]
struct WireDelta {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    reasoning_content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<WireToolCallDelta>>,
}
```

The existing doc comment on `debug_log_from_env` (lines 86-91) says
"`delta.reasoning` content, which `WireDelta` doesn't model" — both halves
are now wrong (wrong field name, and it's now modeled) and must be
corrected to reflect reality: `WireDelta` now models
`delta.reasoning_content`; the debug log's remaining purpose is capturing
genuinely unmodeled wire content (any future field this struct doesn't
yet know about), not this one specifically.

### 2. `StreamEvent` gains `ReasoningDelta`, populated in `ToolCallAccumulator::consume`

`crates/aivyx-llm/src/backend.rs`:

```rust
#[derive(Debug, Clone)]
pub enum StreamEvent {
    TextDelta(String),
    ReasoningDelta(String),
    ToolCallComplete(ToolCall),
    Usage { prompt_tokens: u32, completion_tokens: u32 },
    Done { finish_reason: FinishReason },
}
```

`crates/aivyx-llm/src/openai_compat.rs`'s `ToolCallAccumulator::consume`,
alongside the existing:

```rust
if let Some(content) = choice.delta.content
    && !content.is_empty()
{
    events.push(Ok(StreamEvent::TextDelta(content)));
}
```

add (checked independently, not as an `else if` — a single delta chunk
carrying both fields at once must emit both events, even though the real
llama-server response observed during this design never did):

```rust
if let Some(reasoning) = choice.delta.reasoning_content
    && !reasoning.is_empty()
{
    events.push(Ok(StreamEvent::ReasoningDelta(reasoning)));
}
```

### 3. `AgentEvent` gains `ReasoningDelta`, emitted straight through — never accumulated

`crates/aivyx-core/src/agent/types.rs`:

```rust
pub enum AgentEvent {
    TextDelta(String),
    ReasoningDelta(String),
    // ...unchanged variants
}
```

`crates/aivyx-core/src/agent/mod.rs`'s turn-loop streaming match gains a
new arm alongside the existing `Ok(StreamEvent::TextDelta(text))` one:

```rust
Ok(StreamEvent::ReasoningDelta(text)) => {
    self.emit(AgentEvent::ReasoningDelta(text));
}
```

Deliberately **not** mirroring `TextDelta`'s `assistant_text.push_str(&text)`
or its `MAX_ASSISTANT_TEXT_BYTES` size-cap check — per Decision 2, reasoning
never accumulates into anything `Agent` itself retains, so there is no
unbounded-growth risk on this side to guard against (each chunk is emitted
and forgotten, not appended to a growing buffer).

### 4. `ChatLine` gains `Reasoning`, rendered dimmed/italic

`crates/aivyx-tui/src/app.rs`:

```rust
enum ChatLine {
    // ...unchanged variants
    /// A reasoning-capable model's chain-of-thought, streamed live and
    /// styled distinctly from the final answer — see
    /// `docs/superpowers/specs/2026-07-19-reasoning-visibility-design.md`.
    /// Deliberately never persisted (no `Message`/`ContentBlock`
    /// equivalent exists) — see that spec's Decision 2.
    Reasoning(String),
}
```

`handle_agent_event` gains a new arm, following the exact accumulate-or-push
idiom `TextDelta`'s own arm already uses:

```rust
AgentEvent::ReasoningDelta(text) => {
    if let Some(ChatLine::Reasoning(existing)) = self.transcript.last_mut() {
        existing.push_str(&text);
    } else {
        self.transcript.push(ChatLine::Reasoning(text));
    }
}
```

Because this pushes into a *different* variant than `TextDelta`'s own arm,
the transition from reasoning to the real answer needs no extra logic: once
`TextDelta` starts arriving, the last transcript line is a `Reasoning`, not
an `Assistant`, so `TextDelta`'s own existing `else` branch naturally starts
a fresh `Assistant` line — the visual boundary falls out of the existing
code, not new code.

`chat_line_to_lines` gains a matching arm, dimmed/italic to read clearly as
"thinking," not the final answer:

```rust
ChatLine::Reasoning(text) => prefixed_lines(
    text,
    "  thinking: ",
    Style::default()
        .fg(Color::DarkGray)
        .add_modifier(Modifier::ITALIC),
),
```

### 5. Sub-agent reasoning surfaces through the existing nested-event path

`sub_agent_event_text` (`app.rs:579-592`), currently exhaustive over
`AgentEvent` with a wildcard-free match, needs a new arm. Per Decision 5,
treat it exactly like `TextDelta`'s own arm (surfaced as text, not
suppressed):

```rust
AgentEvent::TextDelta(text) | AgentEvent::ReasoningDelta(text) => text.clone(),
```

(Merging into the existing `TextDelta` arm via an or-pattern, rather than a
separate arm returning the same expression, since both produce identical
behavior here — DRY, and it's exactly what the or-pattern is for.)

## Out of scope for this spec

- Any config flag to disable reasoning display (Decision 4).
- Persisting reasoning to the session JSON or replaying it on `--resume`
  (Decision 2) — a resumed session shows no reasoning for prior turns.
- A collapsed/expandable rendering mode (Decision 3 chose live streaming).
- Scoping reasoning display to only appear in/around the permission
  modal (Decision 1 — general visibility instead).
- Any change to `AIVYX_DEBUG_LOG`'s own behavior — it remains a raw
  wire-capture mechanism for genuinely unmodeled content; this spec just
  corrects its doc comment's now-inaccurate description of what it's for.
- Handling any other backend-specific reasoning field name (e.g. a
  provider that used a different key entirely) — this spec targets the
  empirically-confirmed `reasoning_content` convention this project's own
  serving stack (llama-server, and by the same DeepSeek-compatible
  convention, vLLM) actually uses. A backend sending something else is
  indistinguishable from a non-reasoning model under this design (silently
  no reasoning shown) — acceptable, since there is no evidence this
  project's supported backends use anything else.

## Testing / verification

- Unit test on `ToolCallAccumulator::consume` (`aivyx-llm`): a wire chunk
  carrying `delta.reasoning_content` produces exactly one
  `StreamEvent::ReasoningDelta` with the right text; a chunk carrying both
  `content` and `reasoning_content` produces both events. The order is
  fixed by Change 2's own code (the `content` check runs first, so
  `TextDelta` is always pushed before `ReasoningDelta` when both are
  present in one chunk) — not something the wire format's JSON key order
  could affect, since `serde` deserializes into named struct fields, not
  an order-preserving map. The test should assert this fixed order, not
  treat it as an open question.
- Unit test confirming a chunk with only `content` (no `reasoning_content`
  key at all, matching a non-reasoning model or thinking-disabled config)
  produces no `ReasoningDelta` event — the self-gating behavior Decision 4
  relies on.
- Unit test on `Agent`'s turn loop confirming `StreamEvent::ReasoningDelta`
  emits `AgentEvent::ReasoningDelta` but does **not** appear in
  `self.history` afterward (Decision 2's core guarantee — a real
  regression-guard test, not just an absence-of-crash check).
- Unit test on `App::handle_agent_event` confirming consecutive
  `ReasoningDelta` events accumulate into one `ChatLine::Reasoning`, and
  that a subsequent `TextDelta` starts a fresh `ChatLine::Assistant` rather
  than appending to the reasoning line.
- Unit test on `sub_agent_event_text` confirming `AgentEvent::ReasoningDelta`
  renders as its own text (not empty string).
- Live E2E (through the real binary, PTY harness, per this project's
  established method) — **requires temporarily reconfiguring the real,
  currently-running llama-server**, confirmed necessary during this
  design: aivyx-coder's own client code has no `chat_template_kwargs`
  passthrough (out of scope for this spec, which is about displaying
  reasoning content, not controlling whether a model produces it), and an
  inline `/think`-style prompt directive was tested live against this
  project's actual dev server and does **not** trigger `reasoning_content`
  — only the server's own startup-time default does. The live-E2E task
  must therefore: stop the running llama-server, restart it with
  `--default-chat-template-kwargs '{"enable_thinking":true}'` (or
  whatever the equivalent current flag syntax is — re-verify against
  the actually-running llama-server version at implementation time), run
  the E2E, then restart llama-server back to its normal daily-driver
  config (thinking disabled) afterward, confirmed with the user before
  and after. Confirm the rendered transcript shows a dimmed "thinking:"
  line before the real "aivyx>" answer line, graded from the
  pyte-rendered screen (this is inherently a rendering/visual claim,
  unlike prior E2Es' session-JSON grading — the session JSON deliberately
  has nothing to check here, per Decision 2).

## Sequencing

Written now, at the user's request, as the second of four gap-closing
sub-projects (multi-file edit atomicity already shipped; structured
verification memory and repo-map multi-language support remain, in
whatever order the user picks after this one ships). The eventual
bare-metal test-rig trial remains the motivating context, not something
this spec itself designs.
