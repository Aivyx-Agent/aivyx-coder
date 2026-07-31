# Slash Command Framework Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a shared slash-command framework (a static metadata table
for discoverability, three new built-in commands, TUI autocomplete),
while migrating the three existing ad hoc commands' duplicated parsing
logic onto one helper.

**Architecture:** Three dispatch tiers, chosen by what a command needs:
`AgentTurn` (`/council`/`/wiki`/`/architect`, unchanged, flows through
`Agent::run_turn`), `AgentState` (`/clear`, intercepted in the TUI's
background task before `run_turn`, calls a new `Agent::clear_conversation`
directly), `FrontendOnly` (`/help`/`/quit`, intercepted in the render loop,
never reaches `Agent` at all). A new `aivyx_core::commands` module is the
single shared metadata table all three tiers read from.

**Tech Stack:** Rust, no new dependencies.

## Global Constraints

- No new crate dependency anywhere in this plan.
- `commands::COMMANDS` is a plain compile-time table, not a dynamic
  registry — adding a future command is still explicit code, not
  configuration.
- **Refinement beyond the approved design spec, noted explicitly**: the
  spec's `Agent::clear_conversation` example emitted
  `AgentEvent::TasksUpdated(Vec::new())`. That alone doesn't address a
  real gap the spec didn't cover: the TUI's own *visible transcript*
  (`App.transcript`, a field private to `aivyx-tui`) also needs to be
  wiped when a conversation is cleared, or the chat window would keep
  showing every old message after `/clear`. This plan instead adds a new
  `AgentEvent::ConversationCleared` (unit variant) that the TUI's
  `handle_agent_event` reacts to by clearing `transcript`, `tasks`, and
  `context_usage` all at once — one event covers everything the TUI owns
  that needs resetting, replacing the spec's `TasksUpdated(Vec::new())`
  approach with something that actually fixes the display, not just the
  task panel. Because `AgentEvent` is matched exhaustively in two other
  places (`aivyx-tui/src/app.rs`'s `handle_agent_event` and
  `aivyx-acp/src/translate.rs`'s `translate_event`), both must be
  updated in Task 2 or the workspace won't compile.
- Events sent on the same channel preserve order (`mpsc` is FIFO): the
  background task must call `agent.clear_conversation()` (which emits
  `ConversationCleared`, wiping the transcript) *before*
  `agent.notify("Conversation cleared.")` (which emits an `Error`-backed
  notice) — this ordering guarantees the confirmation message survives
  the wipe rather than being erased by it.
- `/help`/`/quit`/`/clear`'s own text must never be sent to the model —
  `/help`/`/quit` never reach `input_tx` at all; `/clear` reaches
  `input_tx` (so it flows through the same channel as everything else)
  but is intercepted before `agent.run_turn` is called.

---

### Task 1: `aivyx_core::commands` module + de-duplicate existing parsers

**Files:**
- Create: `crates/aivyx-core/src/commands.rs`
- Modify: `crates/aivyx-core/src/lib.rs`
- Modify: `crates/aivyx-core/src/council.rs`
- Modify: `crates/aivyx-core/src/wiki.rs`
- Modify: `crates/aivyx-core/src/architect.rs`

**Interfaces:**
- Produces: `pub enum CommandTier { AgentTurn, AgentState, FrontendOnly }`,
  `pub struct CommandInfo { pub name: &'static str, pub description:
  &'static str, pub tier: CommandTier }`, `pub const COMMANDS: &[CommandInfo]`,
  `pub fn parse_slash_command<'a>(input: &'a str, name: &str) -> Option<&'a str>`
  — all consumed by Tasks 2-4.

- [ ] **Step 1: Write the failing tests**

Create `crates/aivyx-core/src/commands.rs` with just the test module
first (the rest of the file comes in Step 3):

```rust
//! Shared slash-command metadata and parsing — the single source of
//! truth for `/help`'s listing, the TUI's autocomplete hint, and the
//! word-boundary check every command's own parser uses. See
//! `docs/superpowers/specs/2026-08-01-slash-commands-design.md`.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_slash_command_recognizes_bare_and_argument_forms() {
        assert_eq!(parse_slash_command("/foo", "/foo"), Some(""));
        assert_eq!(parse_slash_command("  /foo  ", "/foo"), Some(""));
        assert_eq!(
            parse_slash_command("/foo some args here", "/foo"),
            Some("some args here")
        );
    }

    #[test]
    fn parse_slash_command_rejects_lookalikes_and_normal_messages() {
        assert_eq!(parse_slash_command("/foobar", "/foo"), None);
        assert_eq!(parse_slash_command("run /foo for me", "/foo"), None);
        assert_eq!(parse_slash_command("foo", "/foo"), None);
    }

    #[test]
    fn commands_table_has_no_duplicate_names() {
        let mut names: Vec<&str> = COMMANDS.iter().map(|c| c.name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(
            names.len(),
            COMMANDS.len(),
            "COMMANDS must not list the same name twice"
        );
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p aivyx-core --test-threads=1 commands::`
Expected: compile failure — `parse_slash_command` and `COMMANDS` don't
exist yet.

- [ ] **Step 3: Write the module**

Add to `crates/aivyx-core/src/commands.rs`, above the `#[cfg(test)]`
block:

```rust
/// Which dispatch path a command takes — see
/// `docs/superpowers/specs/2026-08-01-slash-commands-design.md`'s
/// "Three dispatch tiers" decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandTier {
    /// Goes through `Agent::run_turn` as today — needs the model.
    AgentTurn,
    /// Touches real `Agent` state directly, bypassing `run_turn` and the
    /// model entirely.
    AgentState,
    /// Needs nothing from `Agent` at all — handled entirely by the
    /// frontend.
    FrontendOnly,
}

pub struct CommandInfo {
    pub name: &'static str,
    pub description: &'static str,
    pub tier: CommandTier,
}

/// Every known slash command, in the order `/help` lists them. Adding a
/// command means adding an entry here plus whichever dispatch-tier code
/// path it needs — this table itself has no dynamic registration.
pub const COMMANDS: &[CommandInfo] = &[
    CommandInfo {
        name: "/council",
        description: "Convene the configured council on a subject (or the last assistant message if bare)",
        tier: CommandTier::AgentTurn,
    },
    CommandInfo {
        name: "/wiki",
        description: "Regenerate stale wiki pages, or one named page",
        tier: CommandTier::AgentTurn,
    },
    CommandInfo {
        name: "/architect",
        description: "Have the configured architect model produce a plan for a task, then execute it",
        tier: CommandTier::AgentTurn,
    },
    CommandInfo {
        name: "/clear",
        description: "Start a fresh conversation (clears history and tasks)",
        tier: CommandTier::AgentState,
    },
    CommandInfo {
        name: "/help",
        description: "List available commands",
        tier: CommandTier::FrontendOnly,
    },
    CommandInfo {
        name: "/quit",
        description: "Exit aivyx-coder",
        tier: CommandTier::FrontendOnly,
    },
];

/// Shared boundary-aware parser: `name` matches only as a whole word —
/// `/foobar` does not match `/foo`. Replaces the three near-identical
/// copies that used to live in council.rs/wiki.rs/architect.rs. Returns
/// the trimmed remainder after `name` (empty for the bare form).
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

- [ ] **Step 4: Register the module**

In `crates/aivyx-core/src/lib.rs`, replace:

```rust
pub mod agent;
pub mod architect;
pub mod council;
pub mod delegate;
pub mod edit_blocks;
pub mod editor_context;
pub mod session;
pub mod wiki;
```

with:

```rust
pub mod agent;
pub mod architect;
pub mod commands;
pub mod council;
pub mod delegate;
pub mod edit_blocks;
pub mod editor_context;
pub mod session;
pub mod wiki;
```

- [ ] **Step 5: Run the new tests to verify they pass**

Run: `cargo test -p aivyx-core --test-threads=1 commands::`
Expected: all three pass.

- [ ] **Step 6: De-duplicate the three existing parsers onto the shared helper**

In `crates/aivyx-core/src/council.rs`, replace:

```rust
/// Recognizes `/council` / `/council <question>` (and nothing else — a
/// message merely starting with those letters is a normal turn). Returns
/// the question, empty for the bare form.
pub fn parse_command(input: &str) -> Option<&str> {
    let trimmed = input.trim();
    let rest = trimmed.strip_prefix("/council")?;
    if rest.is_empty() || rest.starts_with(char::is_whitespace) {
        Some(rest.trim())
    } else {
        None
    }
}
```

with:

```rust
/// Recognizes `/council` / `/council <question>` (and nothing else — a
/// message merely starting with those letters is a normal turn). Returns
/// the question, empty for the bare form.
pub fn parse_command(input: &str) -> Option<&str> {
    crate::commands::parse_slash_command(input, "/council")
}
```

In `crates/aivyx-core/src/architect.rs`, replace:

```rust
/// Recognizes `/architect` / `/architect <task>` (and nothing else — a
/// message merely starting with those letters is a normal turn). Returns
/// the task, empty for the bare form — callers decide what an empty task
/// means (see `Agent::run_architect_turn`, which treats it as a usage
/// note rather than falling back to reviewing prior conversation the way
/// `/council`'s bare form does).
pub fn parse_command(input: &str) -> Option<&str> {
    let trimmed = input.trim();
    let rest = trimmed.strip_prefix("/architect")?;
    if rest.is_empty() || rest.starts_with(char::is_whitespace) {
        Some(rest.trim())
    } else {
        None
    }
}
```

with:

```rust
/// Recognizes `/architect` / `/architect <task>` (and nothing else — a
/// message merely starting with those letters is a normal turn). Returns
/// the task, empty for the bare form — callers decide what an empty task
/// means (see `Agent::run_architect_turn`, which treats it as a usage
/// note rather than falling back to reviewing prior conversation the way
/// `/council`'s bare form does).
pub fn parse_command(input: &str) -> Option<&str> {
    crate::commands::parse_slash_command(input, "/architect")
}
```

In `crates/aivyx-core/src/wiki.rs`, replace:

```rust
/// Recognizes `/wiki` / `/wiki <page>` (and nothing else — a message merely
/// starting with those letters is a normal turn), mirroring
/// `council::parse_command` exactly.
pub fn parse_command(input: &str) -> Option<WikiCommand> {
    let trimmed = input.trim();
    let rest = trimmed.strip_prefix("/wiki")?;
    if rest.is_empty() {
        Some(WikiCommand::Batch)
    } else if rest.starts_with(char::is_whitespace) {
        let page = rest.trim();
        if page.is_empty() {
            Some(WikiCommand::Batch)
        } else {
            Some(WikiCommand::Forced(page.to_string()))
        }
    } else {
        None
    }
}
```

with:

```rust
/// Recognizes `/wiki` / `/wiki <page>` (and nothing else — a message merely
/// starting with those letters is a normal turn).
pub fn parse_command(input: &str) -> Option<WikiCommand> {
    let page = crate::commands::parse_slash_command(input, "/wiki")?;
    if page.is_empty() {
        Some(WikiCommand::Batch)
    } else {
        Some(WikiCommand::Forced(page.to_string()))
    }
}
```

- [ ] **Step 7: Run every affected test to confirm nothing broke**

Run: `cargo test -p aivyx-core --test-threads=1 council:: wiki:: architect:: commands::`
Expected: all pass unchanged — `council`'s/`wiki`'s/`architect`'s own
`parse_command_recognizes_...`/`parse_command_rejects_...` tests were not
modified and must still pass verbatim, since the observable behavior of
each function is unchanged, only its internals.

Then the full crate:

Run: `cargo test -p aivyx-core --test-threads=1`
Expected: all pass.

- [ ] **Step 8: Commit**

```bash
git add crates/aivyx-core/src/commands.rs crates/aivyx-core/src/lib.rs \
        crates/aivyx-core/src/council.rs crates/aivyx-core/src/wiki.rs \
        crates/aivyx-core/src/architect.rs
git commit -m "Add aivyx_core::commands; de-duplicate slash-command parsing"
```

---

### Task 2: `AgentEvent::ConversationCleared` + `Agent::clear_conversation`

**Files:**
- Modify: `crates/aivyx-core/src/agent/types.rs`
- Modify: `crates/aivyx-core/src/agent/mod.rs`
- Modify: `crates/aivyx-core/src/agent/tests.rs`
- Modify: `crates/aivyx-acp/src/translate.rs`

**Interfaces:**
- Produces: `AgentEvent::ConversationCleared` (unit variant),
  `pub fn Agent::clear_conversation(&mut self)` — consumed by Task 3.

- [ ] **Step 1: Write the failing test**

In `crates/aivyx-core/src/agent/tests.rs`, add (near the other
`Agent`-behavior tests — placement anywhere in the file's test module is
fine, this doesn't depend on surrounding tests):

```rust
    #[tokio::test]
    async fn clear_conversation_empties_history_and_tasks_and_emits_one_event() {
        let (mut agent, mut rx, _mock) = build_agent(
            vec![vec![StreamEvent::Done {
                finish_reason: FinishReason::Stop,
            }]],
            ToolRegistry::new(),
            10,
        );
        agent
            .run_turn("hello".to_string(), Path::new("."), CancellationToken::new())
            .await
            .unwrap();
        assert!(!agent.history.is_empty(), "a real turn should have added history");
        agent.tasks.lock().unwrap().push(Task {
            id: 1,
            text: "do the thing".to_string(),
            status: crate::session::TaskStatus::Pending,
        });
        drain(&mut rx); // discard events from the turn and the push above

        agent.clear_conversation();

        assert!(agent.history.is_empty());
        assert!(agent.tasks.lock().unwrap().is_empty());
        let events = drain(&mut rx);
        assert!(
            matches!(events.as_slice(), [AgentEvent::ConversationCleared]),
            "expected exactly one ConversationCleared event, got: {events:?}"
        );
    }
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p aivyx-core --test-threads=1 clear_conversation`
Expected: compile failure — `AgentEvent::ConversationCleared` and
`Agent::clear_conversation` don't exist yet.

- [ ] **Step 3: Add the `AgentEvent` variant**

In `crates/aivyx-core/src/agent/types.rs`, replace:

```rust
    /// The task list changed during this turn (the model called
    /// `set_tasks`) — carries the full new list for the TUI's task panel.
    TasksUpdated(Vec<Task>),
```

with:

```rust
    /// The task list changed during this turn (the model called
    /// `set_tasks`) — carries the full new list for the TUI's task panel.
    TasksUpdated(Vec<Task>),
    /// `Agent::clear_conversation` ran (the `/clear` command) — the
    /// frontend should reset whatever display state it owns (transcript,
    /// task panel, context-usage indicator). Carries no payload: the new
    /// state is simply "empty" in every dimension.
    ConversationCleared,
```

- [ ] **Step 4: Add `Agent::clear_conversation`**

In `crates/aivyx-core/src/agent/mod.rs`, directly after the existing
`notify` method:

```rust
    /// Sends an informational notice into the transcript without going through
    /// a model turn — used by the autonomous driver to report why it stopped
    /// (goal achieved, budget exhausted, cancelled), since none of those are
    /// otherwise visible once the loop stops producing turns.
    pub fn notify(&self, message: impl Into<String>) {
        self.emit(AgentEvent::Error(message.into()));
    }
```

add:

```rust

    /// Starts a fresh conversation: clears `history` and the shared task
    /// list, persists the now-empty session (so a crash immediately after
    /// doesn't reload the old conversation via `--resume`), and emits
    /// `ConversationCleared` so the frontend resets its own display state.
    /// Never calls the model — this is the `AgentState`-tier `/clear`
    /// command's entire implementation. `plan_mode` is deliberately
    /// untouched: it's a mode setting, not conversation content.
    pub fn clear_conversation(&mut self) {
        self.history.clear();
        self.tasks.lock().unwrap().clear();
        self.emit(AgentEvent::ConversationCleared);
        self.persist();
    }
```

- [ ] **Step 5: Update `aivyx-acp`'s exhaustive `AgentEvent` match**

In `crates/aivyx-acp/src/translate.rs`, replace:

```rust
        // Turn-terminal (handled by `terminal_stop_reason` instead) or
        // deliberately non-notification events — not surfaced as a
        // SessionUpdate.
        AgentEvent::TurnComplete | AgentEvent::TurnPaused(_) | AgentEvent::ContextUsage { .. } => {
            return None
        }
```

with:

```rust
        // Turn-terminal (handled by `terminal_stop_reason` instead) or
        // deliberately non-notification events — not surfaced as a
        // SessionUpdate. `ConversationCleared` is only ever emitted by the
        // TUI's `/clear` interception (see the design spec's scope note —
        // this frontend doesn't wire that command up), but the match must
        // still be exhaustive.
        AgentEvent::TurnComplete
        | AgentEvent::TurnPaused(_)
        | AgentEvent::ContextUsage { .. }
        | AgentEvent::ConversationCleared => return None,
```

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test -p aivyx-core --test-threads=1 clear_conversation`
Expected: PASS.

Then the full workspace, since `AgentEvent` is a public type other crates
match on exhaustively:

Run: `cargo build --workspace`
Expected: builds cleanly (confirms `aivyx-tui` and `aivyx-acp` both still
compile against the new variant — `aivyx-tui`'s own match gets its arm in
Task 3, so a `non_exhaustive_match`/missing-arm compile error here for
`aivyx-tui` specifically is expected until Task 3 lands; if `aivyx-acp`
fails to build, Step 5 above was not applied correctly).

Run: `cargo test -p aivyx-core -p aivyx-acp --test-threads=1`
Expected: all pass.

- [ ] **Step 7: Commit**

```bash
git add crates/aivyx-core/src/agent/types.rs crates/aivyx-core/src/agent/mod.rs \
        crates/aivyx-core/src/agent/tests.rs crates/aivyx-acp/src/translate.rs
git commit -m "Add AgentEvent::ConversationCleared and Agent::clear_conversation"
```

---

### Task 3: Wire `/help`, `/quit`, `/clear` into the TUI

**Files:**
- Modify: `crates/aivyx-tui/src/app.rs`

**Interfaces:**
- Consumes: `aivyx_core::commands::{COMMANDS, parse_slash_command}` (Task 1),
  `AgentEvent::ConversationCleared`, `Agent::clear_conversation` (Task 2).

- [ ] **Step 1: Write the failing tests**

Add to `crates/aivyx-tui/src/app.rs`'s existing `#[cfg(test)] mod tests`
block (anywhere among the other tests):

```rust
    #[test]
    fn conversation_cleared_event_resets_transcript_tasks_and_context_usage() {
        let mut app = App::new(None, PlanMode::new());
        app.transcript.push(ChatLine::User("hi".to_string()));
        app.transcript.push(ChatLine::Assistant("hello".to_string()));
        app.tasks.push(Task {
            id: 1,
            text: "a task".to_string(),
            status: TaskStatus::Pending,
        });
        app.context_usage = Some((100, 1000));
        app.streaming_active = true;

        app.handle_agent_event(AgentEvent::ConversationCleared);

        assert!(app.transcript.is_empty());
        assert!(app.tasks.is_empty());
        assert_eq!(app.context_usage, None);
        assert!(!app.streaming_active);
    }

    #[test]
    fn show_help_lists_every_known_command_with_its_description() {
        let mut app = App::new(None, PlanMode::new());
        app.show_help();

        assert_eq!(app.transcript.len(), 1);
        let ChatLine::Notice(text) = &app.transcript[0] else {
            panic!("expected a Notice line");
        };
        for cmd in aivyx_core::commands::COMMANDS {
            assert!(
                text.contains(cmd.name) && text.contains(cmd.description),
                "help text missing {}: {text}",
                cmd.name
            );
        }
    }
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p aivyx-tui --test-threads=1 conversation_cleared show_help`
Expected: compile failure — `AgentEvent::ConversationCleared` has no
arm in `handle_agent_event` yet (non-exhaustive match), and `show_help`
doesn't exist.

- [ ] **Step 3: Add the `handle_agent_event` arm**

In `crates/aivyx-tui/src/app.rs`, replace:

```rust
            AgentEvent::TasksUpdated(tasks) => {
                self.tasks = tasks;
            }
```

with:

```rust
            AgentEvent::TasksUpdated(tasks) => {
                self.tasks = tasks;
            }
            AgentEvent::ConversationCleared => {
                self.transcript.clear();
                self.tasks.clear();
                self.context_usage = None;
                self.streaming_active = false;
            }
```

- [ ] **Step 4: Add `App::show_help`**

Directly after the existing `push_user_message` method:

```rust
    fn push_user_message(&mut self, text: String) {
        self.transcript.push(ChatLine::User(text));
        self.streaming_active = true;
    }
```

add:

```rust

    /// Renders the `/help` command: every known slash command with its
    /// one-line description, sourced from `aivyx_core::commands::COMMANDS`
    /// — the same table `/clear`'s and the autocomplete hint's own logic
    /// reads, so this listing can never drift from what actually exists.
    fn show_help(&mut self) {
        let mut lines = vec!["Available commands:".to_string()];
        for cmd in aivyx_core::commands::COMMANDS {
            lines.push(format!("  {} — {}", cmd.name, cmd.description));
        }
        self.transcript.push(ChatLine::Notice(lines.join("\n")));
    }
```

- [ ] **Step 5: Intercept `/quit` and `/help` in the render loop**

In `crates/aivyx-tui/src/app.rs`, replace:

```rust
                        (KeyCode::Enter, KeyModifiers::NONE) => {
                            let text = app.take_input();
                            if !text.is_empty() {
                                app.push_user_message(text.clone());
                                let _ = input_tx.send(text);
                            }
                            continue;
                        }
```

with:

```rust
                        (KeyCode::Enter, KeyModifiers::NONE) => {
                            let text = app.take_input();
                            if !text.is_empty() {
                                if aivyx_core::commands::parse_slash_command(&text, "/quit").is_some() {
                                    break;
                                }
                                if aivyx_core::commands::parse_slash_command(&text, "/help").is_some() {
                                    app.show_help();
                                } else {
                                    app.push_user_message(text.clone());
                                    let _ = input_tx.send(text);
                                }
                            }
                            continue;
                        }
```

(`/clear` deliberately gets no special case here — it falls into the
`else` branch, gets sent through `input_tx` exactly like any other
message, and is intercepted one step later, in the background task
below. It will briefly appear as a `ChatLine::User("/clear")` line before
`ConversationCleared` wipes the transcript a moment later — harmless,
and simpler than special-casing it here too.)

- [ ] **Step 6: Intercept `/clear` in the background task, before `run_turn`**

In `crates/aivyx-tui/src/app.rs`, replace:

```rust
        } else {
            while let Some(input) = input_rx.recv().await {
                let cancellation = CancellationToken::new();
                *background_cancellation.lock().unwrap() = Some(cancellation.clone());
                let _ = agent.run_turn(input, &cwd, cancellation).await;
                *background_cancellation.lock().unwrap() = None;
            }
        }
```

with:

```rust
        } else {
            while let Some(input) = input_rx.recv().await {
                if aivyx_core::commands::parse_slash_command(&input, "/clear").is_some() {
                    // AgentState tier: never touches run_turn, never calls
                    // the model. Order matters — clear_conversation's own
                    // ConversationCleared event must be emitted (and thus
                    // received by the render loop) before this notify's
                    // Error event, or the confirmation message would be
                    // wiped by the clear that follows it. mpsc channels
                    // preserve send order, so calling these sequentially
                    // here is sufficient.
                    agent.clear_conversation();
                    agent.notify("Conversation cleared.");
                    continue;
                }
                let cancellation = CancellationToken::new();
                *background_cancellation.lock().unwrap() = Some(cancellation.clone());
                let _ = agent.run_turn(input, &cwd, cancellation).await;
                *background_cancellation.lock().unwrap() = None;
            }
        }
```

- [ ] **Step 7: Run the tests to verify they pass**

Run: `cargo test -p aivyx-tui --test-threads=1`
Expected: all pass, including the two new tests.

Then the full workspace:

Run: `cargo build --workspace`
Expected: clean build (this closes out the non-exhaustive-match gap left
open at the end of Task 2's Step 6).

- [ ] **Step 8: Commit**

```bash
git add crates/aivyx-tui/src/app.rs
git commit -m "Wire /help, /quit, /clear into the TUI"
```

---

### Task 4: Autocomplete hint

**Files:**
- Modify: `crates/aivyx-tui/src/app.rs`

**Interfaces:**
- Consumes: `aivyx_core::commands::{COMMANDS, CommandInfo}` (Task 1).
- Produces: nothing consumed elsewhere — this is the final, additive UI
  layer.

- [ ] **Step 1: Write the failing tests**

Add to `crates/aivyx-tui/src/app.rs`'s test module:

```rust
    #[test]
    fn command_hint_matches_narrows_as_the_user_types_and_stops_after_a_space() {
        let mut app = App::new(None, PlanMode::new());
        assert!(app.command_hint_matches().is_empty(), "empty input has no hints");

        app.input.insert_str("/");
        assert_eq!(app.command_hint_matches().len(), aivyx_core::commands::COMMANDS.len());

        app.input.insert_str("cl");
        let matches = app.command_hint_matches();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].name, "/clear");

        app.input.insert_str(" ");
        assert!(
            app.command_hint_matches().is_empty(),
            "a space means the command name is done being composed"
        );
    }

    #[test]
    fn command_hint_matches_is_empty_for_plain_text() {
        let mut app = App::new(None, PlanMode::new());
        app.input.insert_str("fix the bug");
        assert!(app.command_hint_matches().is_empty());
    }

    #[test]
    fn command_hint_rect_sits_directly_above_the_input_area_and_never_goes_negative() {
        let input_area = Rect { x: 0, y: 10, width: 80, height: 3 };
        let rect = command_hint_rect(input_area, 2);
        assert_eq!(rect.height, 4); // 2 matches + 2 borders
        assert_eq!(rect.y, 6); // 10 - 4
        assert_eq!(rect.x, input_area.x);
        assert_eq!(rect.width, input_area.width);

        // Near the top of the frame: must clamp, not underflow/panic.
        let near_top = Rect { x: 0, y: 1, width: 80, height: 3 };
        let clamped = command_hint_rect(near_top, 6);
        assert_eq!(clamped.y, 0);
    }
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p aivyx-tui --test-threads=1 command_hint`
Expected: compile failure — `command_hint_matches` and
`command_hint_rect` don't exist yet.

- [ ] **Step 3: Add `App::command_hint_matches`**

Directly after `App::show_help` (added in Task 3):

```rust

    /// Slash-command suggestions for the input box's current
    /// (uncommitted) content — shown while the user is still composing a
    /// command name: starts with `/`, no whitespace yet. Empty once a
    /// space appears or the text doesn't start with `/`.
    fn command_hint_matches(&self) -> Vec<&'static aivyx_core::commands::CommandInfo> {
        let text = self.input.lines().join("\n");
        if !text.starts_with('/') || text.contains(char::is_whitespace) {
            return Vec::new();
        }
        aivyx_core::commands::COMMANDS
            .iter()
            .filter(|c| c.name.starts_with(text.as_str()))
            .collect()
    }
```

- [ ] **Step 4: Add `command_hint_rect` and `render_command_hint`**

Directly after the existing `centered_rect` function:

```rust
/// The area for the command-hint popup: anchored directly above
/// `input_area`, same width, one row per match plus borders — clamped so
/// it never extends above the top of the frame (`saturating_sub` avoids
/// an underflow panic when `input_area.y` is small).
fn command_hint_rect(input_area: Rect, match_count: u16) -> Rect {
    let height = match_count + 2;
    let y = input_area.y.saturating_sub(height);
    Rect {
        x: input_area.x,
        y,
        width: input_area.width,
        height,
    }
}

fn render_command_hint(
    frame: &mut ratatui::Frame,
    area: Rect,
    matches: &[&aivyx_core::commands::CommandInfo],
) {
    frame.render_widget(Clear, area);
    let lines: Vec<Line> = matches
        .iter()
        .map(|c| Line::from(format!("{} — {}", c.name, c.description)))
        .collect();
    let block = Block::default().borders(Borders::ALL).title("Commands");
    frame.render_widget(Paragraph::new(lines).block(block), area);
}
```

- [ ] **Step 5: Wire it into `render`**

In `crates/aivyx-tui/src/app.rs`, replace:

```rust
        frame.render_widget(&self.input, input_area);

        let base = if self.streaming_active {
```

with:

```rust
        frame.render_widget(&self.input, input_area);

        let hints = self.command_hint_matches();
        if !hints.is_empty() {
            let hint_area = command_hint_rect(input_area, hints.len() as u16);
            render_command_hint(frame, hint_area, &hints);
        }

        let base = if self.streaming_active {
```

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test -p aivyx-tui --test-threads=1`
Expected: all pass, including the three new tests.

Then the full workspace:

Run: `cargo build --workspace && cargo clippy --workspace --all-targets`
Expected: clean.

- [ ] **Step 7: Commit**

```bash
git add crates/aivyx-tui/src/app.rs
git commit -m "Add slash-command autocomplete hint to the TUI"
```

---

### Task 5: Documentation

**Files:**
- Modify: `README.md`
- Modify: `ROADMAP.md`
- Modify: `docs/HISTORY.md`

**Interfaces:**
- Consumes: nothing (documentation only); should be the last task since
  it describes the finished feature.

- [ ] **Step 1: Add a "Slash commands" section to `README.md`**

Find the `## Council mode` section header (search for `## Council mode`)
and insert a new section directly before it:

```markdown
## Slash commands

Typed at the start of a message (with a space or nothing after — see
below for exact forms):

| Command | Tier | What it does |
|---|---|---|
| `/council` | needs the model | Convenes the configured council on a subject, or the last assistant message if bare. See "Council mode" below. |
| `/wiki` | needs the model | Regenerates stale wiki pages, or `/wiki <page>` forces one named page. |
| `/architect` | needs the model | Has the configured architect model produce a plan for `/architect <task>`, then hands it to the primary model to execute. |
| `/clear` | agent state, no model call | Starts a fresh conversation — clears history and the task list, keeps plan mode as-is. |
| `/help` | frontend only | Lists all of the above. |
| `/quit` | frontend only | Exits `aivyx-coder` (same as Ctrl+C). |

While composing a command (input starts with `/`, no space yet), the TUI
shows a small hint listing matching commands and their descriptions —
purely visual, keep typing and press Enter as normal. `/help`/`/clear`/
`/quit` and the hint are TUI-only; the ACP editor-integration frontend
doesn't wire them up (an editor hosting ACP has its own UI for
equivalent actions), though `/council`/`/wiki`/`/architect` work there
too since they flow through the same `Agent::run_turn` path either way.

## Council mode
```

- [ ] **Step 2: Update `ROADMAP.md`**

Find the end of the "## Current status" section (the Real PTY shipped
paragraph, ending "...calling `open()` on any `/dev/pts/*` path itself."
followed by "See `docs/HISTORY.md` for the full phase-by-phase narrative
behind every item above." then "## Backlog") and insert a new shipped
paragraph:

```markdown

**Slash command framework — shipped.** `/council`/`/wiki`/`/architect`
existed as three separately-implemented ad hoc commands with no shared
metadata and no TUI-side awareness that `/`-prefixed input was special.
Added a shared `aivyx_core::commands` table (name, description, and
which of three dispatch tiers each belongs to) that both a new `/help`
listing and a new TUI autocomplete hint read from, plus two new built-in
commands: `/clear` (starts a fresh conversation, via a new
`Agent::clear_conversation` that never calls the model) and `/quit`. The
three existing commands' triplicated word-boundary parsing logic was
de-duplicated onto one shared helper as a side effect. TUI-only —
`/council`/`/wiki`/`/architect` still work identically under the ACP
frontend (unchanged, they already flowed through `Agent::run_turn`
unconditionally), but `/help`/`/clear`/`/quit` and the autocomplete hint
are not wired into ACP.

See `docs/HISTORY.md` for the full phase-by-phase narrative behind
every item above.

## Backlog — capability opportunities, not yet scheduled
```

Also update the `_Last updated:_` line at the top of `ROADMAP.md` to
today's actual date, if it is not already today's date.

- [ ] **Step 3: Add a `docs/HISTORY.md` chapter**

Append a new chapter after the last chapter in the file:

```markdown
### Slash command framework — ✅ shipped

`/council`, `/wiki`, and `/architect` existed as three independently
implemented ad hoc commands (`council.rs`, `wiki.rs`, `architect.rs`),
each with its own `parse_command` reimplementing the identical
strip-prefix-then-check-word-boundary logic, chained together in
`Agent::run_turn`. There was no generic framework: no shared metadata for
discoverability, no TUI-side awareness that `/`-prefixed input was
special (the TUI just sent whatever text the user typed straight
through), and no built-in utility commands — Ctrl+C was the only way to
quit, and there was no way to reset a conversation short of restarting
the binary.

**Three dispatch tiers**, chosen by what a command actually needs: a new
`aivyx_core::commands` module's `CommandTier` enum distinguishes
`AgentTurn` (`/council`/`/wiki`/`/architect`, unchanged — still flows
through `Agent::run_turn`), `AgentState` (`/clear`, touches real `Agent`
state but never calls the model), and `FrontendOnly` (`/help`/`/quit`,
need nothing from `Agent` at all). A single static `COMMANDS` table
(name, description, tier) is the shared source of truth `/help`'s
listing and the TUI's autocomplete hint both read from — a plain
compile-time table, not a dynamic registry, matching this project's
preference for explicit code over unrequested abstraction. The three
existing commands' triplicated boundary-parsing logic was de-duplicated
onto one shared `parse_slash_command` helper as a natural side effect,
with zero change to their own tests (each function's observable
behavior is unchanged).

**A real architecture wrinkle shaped where each tier gets intercepted**:
the TUI doesn't hold a direct reference to `Agent` — it runs on a
background task, driven only by messages received over a channel. This
is why `/clear` (which needs real `Agent` state: clearing `history` and
the task list, persisting the now-empty session) is intercepted inside
that background task's own receive loop, right before `run_turn` would
otherwise be called — not in the render loop, where `Agent` isn't
reachable at all. `/help`/`/quit`, needing nothing from `Agent`, are
intercepted in the render loop itself, before anything is even sent
through the channel.

**A gap in the original design spec, caught and fixed during planning,
not left for implementation to improvise**: the spec's
`Agent::clear_conversation` sketch emitted `AgentEvent::TasksUpdated(Vec::new())`
alone, which resets the TUI's task panel but does nothing about the
TUI's own visible transcript — after `/clear`, the chat window would
still show every prior message, defeating the point of a "fresh
conversation" command. Fixed by adding a dedicated
`AgentEvent::ConversationCleared` instead, handled by the TUI to reset
transcript, task panel, and context-usage indicator together. Because
`AgentEvent` is matched exhaustively in two other places (`aivyx-tui`'s
`handle_agent_event` and `aivyx-acp`'s `translate_event`), both needed
updating for the new variant — `aivyx-acp`'s match routes it to "no
`SessionUpdate`" (the ACP frontend never triggers `/clear`, since this
chapter is TUI-only, but the match must still be exhaustive).

**Autocomplete hint**: a small popup rendered directly above the input
box while the user is still composing a command name (starts with `/`,
no space yet), listing matching commands with their descriptions.
Deliberately visual-only in this pass — no Tab-complete or arrow-key
selection, avoiding new keybindings that could conflict with
`tui-textarea`'s own handling; a richer interactive version is a natural
future increment.

Scope: TUI-only throughout. `/council`/`/wiki`/`/architect` still work
identically under the ACP editor-integration frontend (they always
flowed through `Agent::run_turn` unconditionally, with no
frontend-specific gating), but `/help`/`/clear`/`/quit` and the
autocomplete hint were not wired into ACP — an editor hosting ACP has
its own UI paradigms for equivalent actions.
```

- [ ] **Step 4: Commit**

```bash
git add README.md ROADMAP.md docs/HISTORY.md
git commit -m "Document the slash command framework"
```
