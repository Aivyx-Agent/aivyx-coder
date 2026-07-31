# Slash Command Framework — Design

**Status:** Approved by user 2026-08-01.

## Context

`aivyx-coder` already has three ad hoc slash commands — `/council`
(`crates/aivyx-core/src/council.rs`), `/wiki`
(`crates/aivyx-core/src/wiki.rs`), and `/architect`
(`crates/aivyx-core/src/architect.rs`) — each with its own
`parse_command(input: &str) -> Option<...>` function, chained together in
`Agent::run_turn` (`crates/aivyx-core/src/agent/mod.rs:973-987`). There is
no generic slash-command framework: no shared metadata, no TUI-side
awareness that `/`-prefixed input is special (the TUI just sends whatever
text the user types straight through `input_tx`, per
`crates/aivyx-tui/src/app.rs:244-251`), no autocomplete/discoverability,
and no built-in utility commands (`/help`, `/clear`, `/quit`) — Ctrl+C is
the only way to quit today, and there's no way to reset a conversation
short of restarting the binary.

This chapter builds a real (but deliberately simple — no dynamic plugin
registry) framework: a shared static command table for discoverability,
three new built-in commands, and a TUI autocomplete hint, while migrating
the three existing commands' duplicated word-boundary parsing logic onto
one shared helper.

**Architecture wrinkle that shapes the design**: the TUI does not hold a
direct reference to `Agent`. `Agent` runs on a background task
(`crates/aivyx-tui/src/app.rs:150`, spawned inside `run`), driven purely
by messages received over an `input_rx` channel
(`crates/aivyx-tui/src/app.rs:189: while let Some(input) =
input_rx.recv().await { ... agent.run_turn(input, ...).await; }`). The
render loop itself only holds `input_tx` (to send) and
`agent_events_rx` (to receive) — it never touches `Agent` directly. This
is why the design below has three tiers, not two: a command needing real
`Agent` state (`/clear`) must be intercepted where `agent` is actually in
scope (the background task's own receive loop), not in the render loop
where `/help`/`/quit` are handled.

## Decisions

### Three dispatch tiers, chosen by what each command actually needs

```rust
pub enum CommandTier {
    /// Goes through Agent::run_turn as today — /council, /wiki,
    /// /architect, and any future command needing the model.
    AgentTurn,
    /// Touches real Agent state directly, bypassing run_turn entirely —
    /// currently just /clear. Never calls the model.
    AgentState,
    /// Needs nothing from Agent at all — /help, /quit.
    FrontendOnly,
}
```

1. **`AgentTurn`** (`/council`, `/wiki`, `/architect`): unchanged in
   spirit. Still flows `input_tx.send()` → the background task →
   `Agent::run_turn`'s existing dispatch chain
   (`agent/mod.rs:973-987`). The only change is each command's own
   `parse_command` stops reimplementing the boundary check and calls a
   shared helper instead (see below).
2. **`AgentState`** (`/clear`): intercepted inside the background task's
   `input_rx.recv()` loop (`app.rs:189`), *before* `run_turn` is called
   — `agent` is already in scope there. Calls a new
   `Agent::clear_conversation()` method directly. Bypasses `run_turn`
   entirely, so it structurally cannot trigger a model call.
3. **`FrontendOnly`** (`/help`, `/quit`): intercepted in the render loop
   itself, at the point `KeyCode::Enter` currently calls
   `app.take_input()`/`push_user_message`/`input_tx.send` (`app.rs:244-251`)
   — before any of that fires. Never reaches the background task,
   `input_tx`, or `Agent` at all.

**Scope**: this chapter is TUI-only. `aivyx-acp` keeps getting
`AgentTurn` commands for free (they already flow through `run_turn`
unconditionally, with no frontend-specific gating), but `AgentState` and
`FrontendOnly` commands, plus the autocomplete hint, are not wired into
the ACP frontend — an editor hosting ACP has its own UI paradigms for
"new conversation" etc.

### `aivyx_core::commands` — one shared static table, not a dynamic registry

A new module, `crates/aivyx-core/src/commands.rs`:

```rust
pub struct CommandInfo {
    pub name: &'static str,
    pub description: &'static str,
    pub tier: CommandTier,
}

pub const COMMANDS: &[CommandInfo] = &[
    CommandInfo { name: "/council", description: "...", tier: CommandTier::AgentTurn },
    CommandInfo { name: "/wiki", description: "...", tier: CommandTier::AgentTurn },
    CommandInfo { name: "/architect", description: "...", tier: CommandTier::AgentTurn },
    CommandInfo { name: "/clear", description: "...", tier: CommandTier::AgentState },
    CommandInfo { name: "/help", description: "...", tier: CommandTier::FrontendOnly },
    CommandInfo { name: "/quit", description: "...", tier: CommandTier::FrontendOnly },
];

/// Shared boundary-aware parser: `/foo` matches only as a whole word —
/// `/foobar` does not match `/foo`. Replaces the three near-identical
/// copies in council.rs/wiki.rs/architect.rs.
pub fn parse_slash_command<'a>(input: &'a str, name: &str) -> Option<&'a str> {
    let trimmed = input.trim();
    let rest = trimmed.strip_prefix(name)?;
    if rest.is_empty() || rest.starts_with(char::is_whitespace) {
        Some(rest.trim())
    } else {
        None
    }
}
```

This is a plain compile-time table, deliberately not a dynamic/pluggable
registry — adding a future command is still a couple of explicit lines
(a `CommandInfo` entry, plus whichever dispatch-tier code path it needs),
matching this project's existing preference for explicit, simple code
over abstraction nobody asked for.

`council::parse_command`/`wiki::parse_command`/`architect::parse_command`
keep existing as their own functions (`wiki`'s does real extra parsing —
bare vs. named-module forms — beyond the boundary check), but each now
calls `commands::parse_slash_command(input, "/council")` etc. internally
instead of duplicating the `strip_prefix`/whitespace-boundary logic.

### `Agent::clear_conversation(&mut self)` — new, `AgentState`-tier

```rust
pub fn clear_conversation(&mut self) {
    self.history.clear();
    self.tasks.lock().unwrap().clear();
    self.emit(AgentEvent::TasksUpdated(Vec::new()));
    self.persist();
}
```

Clears `history` (`agent/mod.rs:147`) and the shared `tasks` list
(`agent/mod.rs:161`), emits `AgentEvent::TasksUpdated(vec![])` so the
TUI's task panel updates live (mirroring how `set_tasks` already drives
that panel), then calls the existing private `persist()`
(`agent/mod.rs:620`) so a crash immediately after `/clear` doesn't reload
the old conversation via `--resume`. `plan_mode` is deliberately
untouched — it's a mode setting, not conversation content, and there's
no reason `/clear` should silently exit plan mode.

### TUI wiring

**Render loop** (`app.rs`, right before the existing `KeyCode::Enter`
handling at `app.rs:244-251`): check the typed text against `COMMANDS`
entries tagged `FrontendOnly`, using `parse_slash_command`.

- `/help` → pushes one `ChatLine::Notice` (the same variant existing
  notices like `goal_achieved_notice` already use) listing every entry in
  `COMMANDS` with its description — one source of truth, so this listing
  can never drift from what actually exists. Consumes the input; nothing
  is sent anywhere.
- `/quit` → breaks the render loop directly, the same effect as today's
  Ctrl+C-with-no-active-turn path. Consumes the input.

**Background task's `input_rx.recv()` loop** (`app.rs:189`, where `agent`
is already in scope): before calling `run_turn`, check for `/clear`
(tagged `AgentState`) via the same helper — call
`agent.clear_conversation()`, then `agent.notify(...)` with a short
confirmation message so the TUI shows the conversation was reset, and
skip `run_turn` entirely for that input.

**`AgentTurn` commands**: no change to this path — they fall through to
`run_turn` exactly as today, unaffected by any of the above.

### Autocomplete hint — visual-only, no Tab-complete

While the input box's content starts with `/` and contains no space yet
(still composing the command name), render a small list below the input
box of every `COMMANDS` entry whose name starts with what's typed so far,
each with its description. Purely visual — no Tab-complete or
arrow-key-driven selection in this pass, avoiding new keybindings that
could conflict with `tui-textarea`'s own key handling. The user still
types the full command and presses Enter as normal; the list disappears
once there's a space or the text no longer starts with `/`. A richer
Tab-to-complete interaction is a natural future increment, not in scope
here.

## Out of scope for this chapter

- ACP-side equivalents of `/help`/`/clear`/`/quit`, or the autocomplete
  hint, on the `aivyx-acp` frontend.
- Tab-complete or arrow-key-driven autocomplete selection.
- Any user-defined/custom command mechanism.
- Any behavior change to `/council`/`/wiki`/`/architect` themselves,
  beyond the internal parsing de-duplication.

## Testing / verification

- `commands::parse_slash_command`: unit tests for exact match, bare form,
  and word-boundary rejection (`/clearfoo` must not match `/clear`),
  mirroring the existing council/wiki/architect test shapes
  (`council.rs`'s `parse_command_recognizes_bare_and_question_forms`/
  `parse_command_rejects_lookalikes_and_normal_messages` and their
  equivalents).
- `Agent::clear_conversation`: a test building an `Agent` with non-empty
  `history`/`tasks`, calling it, and asserting both are empty afterward
  and that an `AgentEvent::TasksUpdated(vec![])` event was emitted.
- TUI: a test exercising the render-loop interception for `/help` (the
  transcript gets the listing notice, no `input_tx` send happens) and
  `/quit` (the loop breaks, no send happens); a test for `/clear`'s
  background-task interception confirming `Agent::run_turn` is *not*
  invoked (no model call attempted) and history/tasks end up empty.

**Live E2E follow-up** (this project's standing practice for a feature
this size): drive the real TUI through the existing pyte-based PTY
harness (see the `feedback_live_e2e_grading` project memory for
established pitfalls to avoid), type `/help` and confirm the listing
renders, type `/clear` mid-conversation and confirm the transcript/task
panel resets and the next turn doesn't carry old context.

## Documentation

`README.md` gets a new "Slash commands" subsection consolidating all six
(currently `/council`/`/wiki`/`/architect` are each only documented in
their own separate sections) plus the three new ones. `ROADMAP.md` gets
a shipped entry. `docs/HISTORY.md` gets a chapter, including the
`council.rs`/`wiki.rs`/`architect.rs` parsing de-duplication as a small
worthwhile side effect of this work.
