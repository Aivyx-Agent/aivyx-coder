# Reasoning Visibility Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Show a reasoning-capable model's chain-of-thought live in the TUI
transcript, styled distinctly from the final answer, without ever
persisting it to conversation history or the session JSON.

**Architecture:** A new `reasoning_content` wire field flows through three
parallel enums that already carry every other kind of turn content —
`StreamEvent` (`aivyx-llm`) → `AgentEvent` (`aivyx-core`) → `ChatLine`
(`aivyx-tui`) — mirroring `TextDelta`'s own existing path at each hop,
except it never touches `Agent`'s own `assistant_text`/`self.history`.

**Tech Stack:** Rust, serde, ratatui. No new dependencies.

## Global Constraints

- Wire field is `reasoning_content` (confirmed empirically against a real
  llama-server + Qwen3.5-9B-GGUF response with thinking forced on via
  `chat_template_kwargs: {"enable_thinking": true}`) — not `reasoning`,
  correcting an inaccurate assumption in this file's own existing comments.
- Reasoning content **never** touches `Message`/`ContentBlock`/
  `self.history`/the session JSON — TUI-transcript-only, forgotten once
  displayed. Never sent back to the model on a later turn.
- No new config surface, no toggle to disable it — the mechanism is
  self-gating (a model/config that never sends `reasoning_content`
  triggers nothing).
- Every match this plan touches (`StreamEvent` consumption in
  `ToolCallAccumulator::consume`, the turn loop's streaming match in
  `run_turn_inner`, `handle_agent_event`, `chat_line_to_lines`,
  `sub_agent_event_text`) is **exhaustive with no wildcard arm** — adding a
  variant to `StreamEvent`/`AgentEvent`/`ChatLine` will produce a compiler
  error at every site that needs a new arm. Fix every site the compiler
  reports, not just the ones this plan enumerates — line numbers drift.
- Full test suite (`cargo test --workspace`) and `cargo clippy --workspace
  --all-targets` must stay clean (0 failures, 0 warnings) after every task.

---

### Task 1: `aivyx-llm` — wire field, `StreamEvent` variant, parsing

**Files:**
- Modify: `crates/aivyx-llm/src/openai_compat.rs` (`WireDelta`,
  `ToolCallAccumulator::consume`, 2 stale doc comments)
- Modify: `crates/aivyx-llm/src/backend.rs` (`StreamEvent`)

**Interfaces:**
- Produces: `StreamEvent::ReasoningDelta(String)`, a new variant sibling to
  `StreamEvent::TextDelta(String)` — Task 2 consumes this.

- [ ] **Step 1: Write the failing tests**

Add to `crates/aivyx-llm/src/openai_compat.rs`'s existing `#[cfg(test)] mod
tests` block (after `oversized_tool_call_arguments_abort_instead_of_growing_unbounded`,
matching that block's own `WireChunk`-via-`serde_json::from_value` style):

```rust
    #[test]
    fn reasoning_content_delta_produces_a_reasoning_delta_event() {
        let mut accumulator = ToolCallAccumulator::default();

        let chunk: WireChunk = serde_json::from_value(serde_json::json!({
            "choices": [{
                "delta": {"reasoning_content": "Thinking about the problem"},
                "finish_reason": null
            }]
        }))
        .unwrap();

        let events = accumulator.consume(chunk);
        assert_eq!(events.len(), 1);
        assert!(matches!(
            &events[0],
            Ok(StreamEvent::ReasoningDelta(text)) if text == "Thinking about the problem"
        ));
    }

    #[test]
    fn content_only_delta_produces_no_reasoning_delta_event() {
        let mut accumulator = ToolCallAccumulator::default();

        let chunk: WireChunk = serde_json::from_value(serde_json::json!({
            "choices": [{
                "delta": {"content": "4"},
                "finish_reason": null
            }]
        }))
        .unwrap();

        let events = accumulator.consume(chunk);
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], Ok(StreamEvent::TextDelta(text)) if text == "4"));
    }

    #[test]
    fn a_delta_carrying_both_fields_produces_both_events_content_first() {
        let mut accumulator = ToolCallAccumulator::default();

        let chunk: WireChunk = serde_json::from_value(serde_json::json!({
            "choices": [{
                "delta": {"content": "answer", "reasoning_content": "thought"},
                "finish_reason": null
            }]
        }))
        .unwrap();

        let events = accumulator.consume(chunk);
        assert_eq!(events.len(), 2);
        assert!(matches!(&events[0], Ok(StreamEvent::TextDelta(text)) if text == "answer"));
        assert!(matches!(&events[1], Ok(StreamEvent::ReasoningDelta(text)) if text == "thought"));
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p aivyx-llm reasoning -- --nocapture`
Expected: compile errors — `reasoning_content` isn't a field on `WireDelta`
yet, and `StreamEvent::ReasoningDelta` doesn't exist yet.

- [ ] **Step 3: Add the `StreamEvent::ReasoningDelta` variant**

In `crates/aivyx-llm/src/backend.rs`, find:

```rust
#[derive(Debug, Clone)]
pub enum StreamEvent {
    TextDelta(String),
    /// Emitted once a native tool call's streamed argument fragments have
    /// been fully reassembled into valid JSON.
    ToolCallComplete(ToolCall),
    Usage {
        prompt_tokens: u32,
        completion_tokens: u32,
    },
    Done {
        finish_reason: FinishReason,
    },
}
```

Replace with:

```rust
#[derive(Debug, Clone)]
pub enum StreamEvent {
    TextDelta(String),
    /// A reasoning-capable model's chain-of-thought, streamed separately
    /// from its final answer — see `docs/superpowers/specs/
    /// 2026-07-19-reasoning-visibility-design.md`. Never accumulated into
    /// anything persisted; display-only.
    ReasoningDelta(String),
    /// Emitted once a native tool call's streamed argument fragments have
    /// been fully reassembled into valid JSON.
    ToolCallComplete(ToolCall),
    Usage {
        prompt_tokens: u32,
        completion_tokens: u32,
    },
    Done {
        finish_reason: FinishReason,
    },
}
```

- [ ] **Step 4: Add the wire field and parsing**

In `crates/aivyx-llm/src/openai_compat.rs`, find:

```rust
#[derive(Deserialize, Default)]
struct WireDelta {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<WireToolCallDelta>>,
}
```

Replace with:

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

Find, inside `ToolCallAccumulator::consume`:

```rust
        if let Some(content) = choice.delta.content
            && !content.is_empty()
        {
            events.push(Ok(StreamEvent::TextDelta(content)));
        }
```

Replace with (the reasoning check is independent — checked regardless of
whether the `content` branch above fired, not an `else if` — a single
delta carrying both must produce both events, content event first, per
this task's own test above):

```rust
        if let Some(content) = choice.delta.content
            && !content.is_empty()
        {
            events.push(Ok(StreamEvent::TextDelta(content)));
        }

        if let Some(reasoning) = choice.delta.reasoning_content
            && !reasoning.is_empty()
        {
            events.push(Ok(StreamEvent::ReasoningDelta(reasoning)));
        }
```

- [ ] **Step 5: Fix the two stale doc comments**

In `crates/aivyx-llm/src/openai_compat.rs`, find (on `debug_log_from_env`):

```rust
/// Opt-in raw request/response capture for diagnosing local-model quirks —
/// e.g. malformed history assembly, or a reasoning-capable model's
/// `delta.reasoning` content, which `WireDelta` doesn't model and so
/// silently drops via serde's default behavior during normal parsing. Set
/// `AIVYX_DEBUG_LOG=<path>` to capture raw wire traffic there; unset by
/// default, so this has zero cost for normal use.
```

Replace with:

```rust
/// Opt-in raw request/response capture for diagnosing local-model quirks —
/// e.g. malformed history assembly, or any wire field a future backend
/// sends that `WireDelta` doesn't yet model and so silently drops via
/// serde's default behavior during normal parsing (a reasoning-capable
/// model's `delta.reasoning_content` used to be exactly this case, until
/// `WireDelta` started modeling it — see `docs/superpowers/specs/
/// 2026-07-19-reasoning-visibility-design.md`). Set `AIVYX_DEBUG_LOG=<path>`
/// to capture raw wire traffic there; unset by default, so this has zero
/// cost for normal use.
```

Find (in the SSE-event-logging comment inside `stream_chat`):

```rust
                        // Logged before typed parsing so fields `WireChunk`
                        // doesn't model (e.g. a reasoning model's
                        // `delta.reasoning`) still show up in the capture.
```

Replace with:

```rust
                        // Logged before typed parsing so fields `WireChunk`
                        // doesn't model still show up in the capture, even
                        // though `delta.reasoning_content` (a reasoning
                        // model's chain-of-thought) is one such field
                        // `WireChunk` now models — see `docs/superpowers/
                        // specs/2026-07-19-reasoning-visibility-design.md`.
```

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo test -p aivyx-llm -- --nocapture 2>&1 | tail -40`
Expected: all tests pass, including the 3 new ones from Step 1.

- [ ] **Step 7: Run the full workspace suite and clippy**

Run: `cargo test --workspace 2>&1 | grep -E "^test result|FAILED"`
Expected: every crate reports `ok`, 0 failed. `aivyx-core` and `aivyx-tui`
will fail to *compile* at this point (Tasks 2/3 haven't added their own
match arms yet for the new `StreamEvent`/eventual `AgentEvent` variant) —
that's expected and fine; this step's actual purpose is confirming
`aivyx-llm` itself is clean. If `cargo test --workspace` refuses to run at
all due to a workspace-wide compile failure, run `cargo test -p aivyx-llm`
instead and note in your report that the other two crates aren't
buildable yet (correct — Task 2 fixes `aivyx-core`).

Run: `cargo clippy -p aivyx-llm --all-targets 2>&1 | tail -20`
Expected: 0 warnings.

- [ ] **Step 8: Commit**

```bash
git add crates/aivyx-llm/src/backend.rs crates/aivyx-llm/src/openai_compat.rs
git commit -m "Parse delta.reasoning_content into StreamEvent::ReasoningDelta"
```

---

### Task 2: `aivyx-core` — `AgentEvent` variant, turn-loop wiring

**Files:**
- Modify: `crates/aivyx-core/src/agent/types.rs` (`AgentEvent`)
- Modify: `crates/aivyx-core/src/agent/mod.rs` (turn-loop streaming match)
- Modify: `crates/aivyx-core/src/agent/tests.rs` (1 new test)

**Interfaces:**
- Consumes: `StreamEvent::ReasoningDelta(String)` (Task 1).
- Produces: `AgentEvent::ReasoningDelta(String)` — Task 3 consumes this.

- [ ] **Step 1: Add the `AgentEvent::ReasoningDelta` variant**

In `crates/aivyx-core/src/agent/types.rs`, find:

```rust
pub enum AgentEvent {
    TextDelta(String),
    ToolCallDetected(ToolCall),
```

Replace with:

```rust
pub enum AgentEvent {
    TextDelta(String),
    /// A reasoning-capable model's chain-of-thought, streamed separately
    /// from its final answer — display-only, never accumulated into
    /// `Agent`'s own `history`. See `docs/superpowers/specs/
    /// 2026-07-19-reasoning-visibility-design.md`.
    ReasoningDelta(String),
    ToolCallDetected(ToolCall),
```

- [ ] **Step 2: Write the failing regression-guard test**

This is the core guarantee of the whole feature (Decision 2 of the spec):
reasoning must never end up in `self.history`. Add to
`crates/aivyx-core/src/agent/tests.rs`, following the file's own
`MockBackend`/`build_agent` precedent (read `tool_call_then_final_answer_produces_balanced_history`
first for the exact style, since this test mirrors its shape closely):

```rust
#[tokio::test]
async fn reasoning_delta_emits_but_never_enters_history() {
    let response = vec![
        StreamEvent::ReasoningDelta("Let me think about this".to_string()),
        StreamEvent::TextDelta("Here's my answer".to_string()),
        StreamEvent::Done {
            finish_reason: FinishReason::Stop,
        },
    ];
    let (mut agent, mut rx, _mock) = build_agent(vec![response], ToolRegistry::new(), 10);

    agent
        .run_turn("hello".to_string(), Path::new("."), CancellationToken::new())
        .await
        .unwrap();

    let events = drain(&mut rx);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::ReasoningDelta(text) if text == "Let me think about this")),
        "expected a ReasoningDelta event to have been emitted"
    );

    let history_text: String = agent
        .history
        .iter()
        .flat_map(|m| &m.content)
        .filter_map(|b| match b {
            ContentBlock::Text(text) => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        !history_text.contains("Let me think about this"),
        "reasoning content must never enter Agent's own history: {history_text}"
    );
    assert!(
        history_text.contains("Here's my answer"),
        "the real answer must still be recorded normally: {history_text}"
    );
}
```

- [ ] **Step 3: Run the test to verify it fails**

Run: `cargo test -p aivyx-core reasoning_delta_emits -- --nocapture`
Expected: compile error — `StreamEvent::ReasoningDelta`/`AgentEvent::
ReasoningDelta` exist after Task 1/this task's Step 1, but the turn loop's
own match isn't exhaustive over the new `StreamEvent` variant yet, so this
won't compile until Step 4 below.

- [ ] **Step 4: Wire the turn loop**

In `crates/aivyx-core/src/agent/mod.rs`, find, inside the turn loop's
streaming match (search for `Ok(StreamEvent::TextDelta(text)) =>`):

```rust
                match event {
                    Ok(StreamEvent::TextDelta(text)) => {
                        assistant_text.push_str(&text);
                        self.emit(AgentEvent::TextDelta(text));
                        if assistant_text.len() > MAX_ASSISTANT_TEXT_BYTES {
                            let err = AgentError::ResponseTooLarge;
                            self.emit(AgentEvent::Error(err.to_string()));
                            return Err(err);
                        }
                    }
                    Ok(StreamEvent::ToolCallComplete(call)) => {
```

Replace with (adding a new arm — do not touch the `TextDelta` arm itself):

```rust
                match event {
                    Ok(StreamEvent::TextDelta(text)) => {
                        assistant_text.push_str(&text);
                        self.emit(AgentEvent::TextDelta(text));
                        if assistant_text.len() > MAX_ASSISTANT_TEXT_BYTES {
                            let err = AgentError::ResponseTooLarge;
                            self.emit(AgentEvent::Error(err.to_string()));
                            return Err(err);
                        }
                    }
                    Ok(StreamEvent::ReasoningDelta(text)) => {
                        // Deliberately not accumulated into `assistant_text`
                        // and no size-cap check — reasoning never enters
                        // `self.history`, so there's no unbounded-growth
                        // risk on this side to guard against. See
                        // docs/superpowers/specs/
                        // 2026-07-19-reasoning-visibility-design.md.
                        self.emit(AgentEvent::ReasoningDelta(text));
                    }
                    Ok(StreamEvent::ToolCallComplete(call)) => {
```

- [ ] **Step 5: Fix every other non-exhaustive match the compiler reports**

Run: `cargo build -p aivyx-core --tests 2>&1 | grep -B3 "non-exhaustive"`

Fix each site the compiler reports by adding a `ReasoningDelta` arm
consistent with that match's own existing style for `TextDelta` (most
should just need a one-line pass-through or a no-op, matching how that
particular match already treats plain text). Re-run until clean.

- [ ] **Step 6: Run the test to verify it passes**

Run: `cargo test -p aivyx-core reasoning_delta_emits -- --nocapture`
Expected: PASS.

- [ ] **Step 7: Run the full aivyx-core suite and clippy**

Run: `cargo test -p aivyx-core 2>&1 | grep -E "^test result|FAILED"`
Expected: `ok`, 0 failed.

Run: `cargo clippy -p aivyx-core --all-targets 2>&1 | tail -20`
Expected: 0 warnings.

- [ ] **Step 8: Commit**

```bash
git add crates/aivyx-core/src/agent/types.rs crates/aivyx-core/src/agent/mod.rs crates/aivyx-core/src/agent/tests.rs
git commit -m "Wire StreamEvent::ReasoningDelta through to AgentEvent, never persisted to history"
```

---

### Task 3: `aivyx-tui` — `ChatLine` variant, rendering, sub-agent surfacing

**Files:**
- Modify: `crates/aivyx-tui/src/app.rs` (`ChatLine`, `handle_agent_event`,
  `chat_line_to_lines`, `sub_agent_event_text`, 2 new tests)

**Interfaces:**
- Consumes: `AgentEvent::ReasoningDelta(String)` (Task 2).

- [ ] **Step 1: Write the failing tests**

Add to `crates/aivyx-tui/src/app.rs`'s existing `#[cfg(test)] mod tests`
block, near `architect_note_renders_as_a_distinguished_chat_line` (read
that test and `sub_agent_text_delta_renders_as_a_distinguished_chat_line`
first — these two new tests mirror their exact style):

```rust
    #[test]
    fn reasoning_delta_renders_as_a_distinguished_chat_line() {
        let mut app = App::new(None, PlanMode::new());
        app.handle_agent_event(AgentEvent::ReasoningDelta(
            "considering the edge cases".to_string(),
        ));

        assert!(matches!(
            app.transcript.last(),
            Some(ChatLine::Reasoning(text)) if text == "considering the edge cases"
        ));

        let lines = chat_line_to_lines(app.transcript.last().unwrap());
        assert!(lines[0].to_string().contains("thinking:"));
    }

    #[test]
    fn reasoning_then_text_delta_starts_a_fresh_assistant_line() {
        let mut app = App::new(None, PlanMode::new());
        app.handle_agent_event(AgentEvent::ReasoningDelta("hmm".to_string()));
        app.handle_agent_event(AgentEvent::ReasoningDelta(", let me see".to_string()));
        app.handle_agent_event(AgentEvent::TextDelta("Here's the answer".to_string()));

        assert_eq!(app.transcript.len(), 2);
        assert!(matches!(
            &app.transcript[0],
            ChatLine::Reasoning(text) if text == "hmm, let me see"
        ));
        assert!(matches!(
            &app.transcript[1],
            ChatLine::Assistant(text) if text == "Here's the answer"
        ));
    }

    #[test]
    fn sub_agent_reasoning_delta_renders_as_a_distinguished_chat_line() {
        let mut app = App::new(None, PlanMode::new());
        app.handle_agent_event(AgentEvent::SubAgentActivity(Box::new(
            AgentEvent::ReasoningDelta("weighing two approaches".to_string()),
        )));

        assert!(matches!(
            app.transcript.last(),
            Some(ChatLine::SubAgent(text)) if text == "weighing two approaches"
        ));
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p aivyx-tui reasoning -- --nocapture`
Expected: compile errors — `ChatLine::Reasoning` doesn't exist yet, and
`handle_agent_event`/`chat_line_to_lines`/`sub_agent_event_text` aren't
exhaustive over the new `AgentEvent` variant.

- [ ] **Step 3: Add the `ChatLine::Reasoning` variant**

In `crates/aivyx-tui/src/app.rs`, find:

```rust
enum ChatLine {
    User(String),
    Assistant(String),
```

Replace with:

```rust
enum ChatLine {
    User(String),
    Assistant(String),
    /// A reasoning-capable model's chain-of-thought, streamed live and
    /// styled distinctly from the final answer — see
    /// `docs/superpowers/specs/2026-07-19-reasoning-visibility-design.md`.
    /// Deliberately never persisted (no `Message`/`ContentBlock`
    /// equivalent exists) — see that spec's Decision 2.
    Reasoning(String),
```

- [ ] **Step 4: Wire `handle_agent_event`**

Find, inside `handle_agent_event`:

```rust
            AgentEvent::TextDelta(text) => {
                if let Some(ChatLine::Assistant(existing)) = self.transcript.last_mut() {
                    existing.push_str(&text);
                } else {
                    self.transcript.push(ChatLine::Assistant(text));
                }
            }
            AgentEvent::ToolCallDetected(call) => {
```

Replace with (adding a new arm — do not touch the `TextDelta` arm itself):

```rust
            AgentEvent::TextDelta(text) => {
                if let Some(ChatLine::Assistant(existing)) = self.transcript.last_mut() {
                    existing.push_str(&text);
                } else {
                    self.transcript.push(ChatLine::Assistant(text));
                }
            }
            AgentEvent::ReasoningDelta(text) => {
                if let Some(ChatLine::Reasoning(existing)) = self.transcript.last_mut() {
                    existing.push_str(&text);
                } else {
                    self.transcript.push(ChatLine::Reasoning(text));
                }
            }
            AgentEvent::ToolCallDetected(call) => {
```

- [ ] **Step 5: Wire `chat_line_to_lines`**

Find, inside `chat_line_to_lines`:

```rust
        ChatLine::SubAgent(text) => {
            if text.is_empty() {
                return Vec::new();
            }
            prefixed_lines(text, "  sub-agent> ", Style::default().fg(Color::LightYellow))
        }
    }
}
```

Replace with (adding a new arm before the closing brace — do not touch the
`SubAgent` arm itself):

```rust
        ChatLine::SubAgent(text) => {
            if text.is_empty() {
                return Vec::new();
            }
            prefixed_lines(text, "  sub-agent> ", Style::default().fg(Color::LightYellow))
        }
        ChatLine::Reasoning(text) => prefixed_lines(
            text,
            "  thinking: ",
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::ITALIC),
        ),
    }
}
```

(`Color`/`Modifier` are already imported at the top of this file via `use
ratatui::style::{Color, Modifier, Style};` — no new import needed.)

- [ ] **Step 6: Wire `sub_agent_event_text`**

Find:

```rust
fn sub_agent_event_text(event: &AgentEvent) -> String {
    match event {
        AgentEvent::TextDelta(text) => text.clone(),
```

Replace with (merging into an or-pattern — both produce identical
behavior, so this is DRY rather than a separate arm):

```rust
fn sub_agent_event_text(event: &AgentEvent) -> String {
    match event {
        AgentEvent::TextDelta(text) | AgentEvent::ReasoningDelta(text) => text.clone(),
```

- [ ] **Step 7: Fix any other non-exhaustive match the compiler reports**

Run: `cargo build -p aivyx-tui --tests 2>&1 | grep -B3 "non-exhaustive"`

Fix each remaining site the same way — consistent with how that match
already treats `TextDelta`. Re-run until clean.

- [ ] **Step 8: Run the tests to verify they pass**

Run: `cargo test -p aivyx-tui reasoning -- --nocapture`
Expected: all 3 new tests pass.

- [ ] **Step 9: Run the full workspace suite and clippy**

Run: `cargo test --workspace 2>&1 | grep -E "^test result|FAILED"`
Expected: every crate `ok`, 0 failed — this is the first point in this
plan where the whole workspace compiles and passes together.

Run: `cargo clippy --workspace --all-targets 2>&1 | tail -20`
Expected: 0 warnings.

- [ ] **Step 10: Commit**

```bash
git add crates/aivyx-tui/src/app.rs
git commit -m "Render reasoning deltas as a distinct, dimmed transcript line"
```

---

### Task 4: Live E2E through the real binary

**Files:**
- None modified — verification only, via this project's established
  live-E2E method (PTY + `python-pyte`, graded from the rendered screen
  rather than the session JSON — this feature has nothing to check in the
  session JSON by design, since reasoning is never persisted there).

**Interfaces:**
- Consumes: the fully-wired feature from Tasks 1-3.

**This task requires temporarily reconfiguring the user's real,
currently-running llama-server** — confirmed necessary during this
feature's design (no per-request way exists to force thinking on; an
inline `/think` prompt directive was tested live and does not work; only
the server's own startup-time default does). Confirm with the user before
stopping their server, and confirm again once it's been restarted back to
its normal config at the end of this task — do not silently skip the
restore step on an early exit or failure partway through Steps 2-5.

- [ ] **Step 1: Confirm with the user, then note the current server config**

Before touching anything, tell the user you're about to stop their running
llama-server to restart it with thinking forced on for this test, and that
you'll restore it afterward. Record the exact command line/flags it's
currently running with (e.g. via `ps aux | grep llama-server` or however
it was started) so Step 5 can restore it exactly.

- [ ] **Step 2: Build the release binary**

```bash
cargo build --release -p aivyx 2>&1 | tail -10
```

Expected: succeeds.

- [ ] **Step 3: Restart llama-server with thinking forced on**

Stop the current llama-server process, then restart it with the same
flags plus `--default-chat-template-kwargs '{"enable_thinking":true}'` (or
whatever the equivalent current flag syntax is for the actually-running
llama-server version — re-verify against `llama-server --help` if the
flag name has changed since this plan was written). Confirm it's serving
again via `curl -s http://127.0.0.1:8001/v1/models` (adjust the port to
match the real config) before proceeding.

- [ ] **Step 4: Drive the real binary and confirm reasoning renders**

Follow this project's established live-E2E harness pattern (`python-pyte`
for rendering, explicit `TIOCSWINSZ` window sizing, paced keystrokes
~15ms apart, wait for the "Type a message..." readiness marker, wait for
both the ready-status text and the input-placeholder text before
considering a turn complete). Send a message likely to produce visible
reasoning, e.g.: `"What is 17 times 23? Show your reasoning."`

Confirm, from the pyte-rendered screen (not the session JSON — this
feature has nothing to check there by design):
- A dimmed/italic line containing `thinking:` appears in the transcript,
  before the final `aivyx>` answer line.
- The final answer line itself does not contain the word "thinking:" or
  otherwise look like it absorbed the reasoning content.

- [ ] **Step 5: Restore the server to its normal config**

Stop the thinking-forced-on llama-server, restart it with the exact
original flags recorded in Step 1. Confirm it's serving again via `curl`.
Tell the user explicitly that the server has been restored to its normal
daily-driver configuration.

- [ ] **Step 6: Clean up and report**

No commit for this task (verification only, no files modified). Report:
the pyte-rendered transcript excerpt showing the "thinking:" line and the
subsequent answer line, and explicit confirmation the server was restored
to its original config in Step 5.
