# Architect/Editor Model-Pairing Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `/architect <task>`, a slash command that has a separately
configured, stronger model produce a prose implementation plan, then hands
that plan directly to the existing primary ("editor") model's own turn loop
so it starts calling edit tools immediately — one continuous action, no
re-prompt.

**Architecture:** A new `crate::architect` module (`ArchitectSeat`,
`Architect`, `parse_command`, `plan()`) mirrors `crate::council`'s shape.
`Agent::run_architect_turn` makes one no-tools planning call, then calls
`self.run_turn_inner(formatted_plan, cwd, cancellation).await` directly —
`run_turn_inner` is unmodified; it already pushes whatever string it's given
as a `Role::User` message and runs the normal iteration loop, so this one
call *is* the hand-off to the editor.

**Tech Stack:** Rust, tokio, existing `aivyx-core`/`aivyx-config`/`aivyx-tui`/`aivyx` crates. No new dependencies.

## Global Constraints

- `ArchitectSettings::configured()` is `true` only when both `base_url` and
  `model` are non-empty — no separate enable flag (matches
  `VerificationSettings`'s convention).
- Default `[architect]` is fully empty/off (`base_url = ""`, `model = ""`).
- `architect.tail_budget_tokens` defaults to `3072` (same default as
  `council.tail_budget_tokens`, configured independently).
- The formatted hand-off message is exactly:
  `"[Architect plan — {model} planned this task; you are executing it]\n\nTask: {subject}\n\nPlan:\n{plan}"`.
- `run_turn_inner` must not be modified — the hand-off is a direct call to
  the existing, unmodified method.
- `run_architect_turn`'s return value is `run_turn_inner`'s return value on
  the success path (no separate `TurnComplete` emitted after the hand-off).
- New `ChatLine::Architect` renders with `Color::Cyan` and prefix
  `"architect> "` — distinct from `council>` (magenta) and
  `"  sub-agent> "` (light yellow).
- Command dispatch order in `Agent::run_turn` is: `/council`, then `/wiki`,
  then `/architect`, then plain `run_turn_inner`.

---

### Task 1: `[architect]` config section

**Files:**
- Modify: `crates/aivyx-config/src/lib.rs`

**Interfaces:**
- Produces: `pub struct ArchitectSettings { pub base_url: String, pub model: String, pub api_key: Option<String>, pub tail_budget_tokens: u32 }`, `impl ArchitectSettings { pub fn configured(&self) -> bool }`, `Settings.architect: ArchitectSettings`.

- [ ] **Step 1: Write the failing tests**

Add to the `mod tests` block in `crates/aivyx-config/src/lib.rs` (find the
existing `council_block_parses_members_and_chairman` test and add these
after it):

```rust
    #[test]
    fn architect_settings_default_is_unconfigured() {
        let settings = Settings::default();
        assert!(!settings.architect.configured());
        assert_eq!(settings.architect.base_url, "");
        assert_eq!(settings.architect.model, "");
        assert_eq!(settings.architect.tail_budget_tokens, 3072);
    }

    #[test]
    fn architect_block_parses_and_reports_configured() {
        let raw = r#"
            [architect]
            base_url = "http://localhost:11434/v1"
            model = "qwen3.6:27b"
            tail_budget_tokens = 2048
        "#;
        let settings: Settings = toml::from_str(raw).unwrap();
        assert!(settings.architect.configured());
        assert_eq!(settings.architect.base_url, "http://localhost:11434/v1");
        assert_eq!(settings.architect.model, "qwen3.6:27b");
        assert_eq!(settings.architect.tail_budget_tokens, 2048);
    }

    #[test]
    fn architect_needs_both_base_url_and_model_to_be_configured() {
        let only_model = r#"
            [architect]
            model = "qwen3.6:27b"
        "#;
        let settings: Settings = toml::from_str(only_model).unwrap();
        assert!(!settings.architect.configured());

        let only_base_url = r#"
            [architect]
            base_url = "http://localhost:11434/v1"
        "#;
        let settings: Settings = toml::from_str(only_base_url).unwrap();
        assert!(!settings.architect.configured());
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p aivyx-config architect --lib`
Expected: FAIL to compile — `Settings` has no field `architect`.

- [ ] **Step 3: Implement `ArchitectSettings`**

In `crates/aivyx-config/src/lib.rs`, add `pub architect: ArchitectSettings,`
to the `Settings` struct (next to `pub council: CouncilSettings,`):

```rust
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub backend: BackendSettings,
    pub permissions: PermissionSettings,
    pub sandbox: SandboxSettings,
    pub git: GitSettings,
    pub repo_map: RepoMapSettings,
    pub council: CouncilSettings,
    pub architect: ArchitectSettings,
    pub verification: VerificationSettings,
    pub autonomous: AutonomousSettings,
    pub sub_agent: SubAgentSettings,
}
```

Then add the new struct just after `CouncilMember`'s definition:

```rust
/// Architect/editor model-pairing (`/architect <task>`, ROADMAP.md Phase 9):
/// a single, separately configured model produces a prose implementation
/// plan, which is then handed directly to the primary/editor model's own
/// turn loop. Off until configured: an empty `base_url` or `model` means
/// `/architect` explains how to enable itself instead of running — no
/// separate enable flag, matching `VerificationSettings`'s convention.
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
    pub fn configured(&self) -> bool {
        !self.base_url.is_empty() && !self.model.is_empty()
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p aivyx-config architect --lib`
Expected: PASS (3 new tests).

- [ ] **Step 5: Run the full config crate test suite**

Run: `cargo test -p aivyx-config --lib`
Expected: all tests pass (no regressions to existing `council`/`autonomous`/`sub_agent` tests).

- [ ] **Step 6: Commit**

```bash
git add crates/aivyx-config/src/lib.rs
git commit -m "Config: add [architect] section for model-pairing (Phase 9)"
```

---

### Task 2: `architect` module + `Agent` wiring

**Files:**
- Modify: `crates/aivyx-core/src/council.rs` (visibility only — no behavior change)
- Create: `crates/aivyx-core/src/architect.rs`
- Modify: `crates/aivyx-core/src/agent.rs`
- Modify: `crates/aivyx-core/src/lib.rs`

**Interfaces:**
- Consumes: `ArchitectSettings` (Task 1, not directly — this task only adds
  the runtime types; config→runtime construction is Task 4). `crate::council::tail_digest(history: &[Message], budget_chars: usize) -> Option<String>` (already `pub`).
- Produces: `pub struct ArchitectSeat { pub model: String, pub backend: Arc<dyn LlmBackend> }`, `pub struct Architect { pub seat: ArchitectSeat, pub tail_budget_tokens: u32 }`, `pub fn architect::parse_command(input: &str) -> Option<&str>`, `pub(crate) async fn architect::plan(seat: &ArchitectSeat, subject: &str, context: Option<String>, events: &UnboundedSender<AgentEvent>, cancellation: &CancellationToken) -> Option<String>`, `Agent::set_architect(&mut self, architect: Architect)`, `AgentEvent::ArchitectNote(String)`.

#### Step 1: Loosen `council.rs` visibility (no behavior change)

`architect::plan` needs to reuse council's existing text-collection helper
and constants rather than duplicating them (DRY). These are currently
private to `council.rs`; widen to `pub(crate)` (visible anywhere in
`aivyx-core`, not exported from the crate).

- [ ] In `crates/aivyx-core/src/council.rs`, change:

```rust
const MIN_ANSWER_CHARS: usize = 20;
```
to:
```rust
pub(crate) const MIN_ANSWER_CHARS: usize = 20;
```

- [ ] Change:
```rust
fn strip_think(text: &str) -> String {
```
to:
```rust
pub(crate) fn strip_think(text: &str) -> String {
```

- [ ] Change:
```rust
enum CollectError {
    Cancelled,
    Backend(String),
}
```
to:
```rust
pub(crate) enum CollectError {
    Cancelled,
    Backend(String),
}
```

- [ ] Change:
```rust
async fn collect_text(
    backend: &dyn LlmBackend,
    request: ChatRequest,
    cancellation: &CancellationToken,
) -> Result<String, CollectError> {
```
to:
```rust
pub(crate) async fn collect_text(
    backend: &dyn LlmBackend,
    request: ChatRequest,
    cancellation: &CancellationToken,
) -> Result<String, CollectError> {
```

- [ ] Run `cargo build -p aivyx-core` to confirm this compiles with no
  warnings about now-unused-more-visible items (there won't be any — these
  are still used within `council.rs` itself).

#### Step 2: Write `architect.rs`'s failing tests (the pure/unit-testable pieces)

- [ ] Create `crates/aivyx-core/src/architect.rs` with this initial content
  (module doc, types, `parse_command`, and its tests — `plan()` comes in
  Step 4):

```rust
//! Architect/editor model-pairing (`/architect <task>`, ROADMAP.md Phase 9):
//! a single, separately configured model ("architect") produces a prose
//! implementation plan for a stated task; the plan is then handed directly
//! to the primary/editor model's own turn loop (see
//! `Agent::run_architect_turn` in `agent.rs`), which begins calling edit
//! tools on it immediately. Unlike `/council`, there is no deliberation and
//! no ranking — exactly one seat, one planning call.

use std::sync::Arc;

use aivyx_llm::{ChatRequest, LlmBackend};
use aivyx_types::{Message, Role};
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

use crate::agent::AgentEvent;
use crate::council::CollectError;

const ARCHITECT_PROMPT: &str = "You are a senior engineer producing a concrete implementation plan \
     for the task below, to be executed by a separate, faster model. Describe what to change and \
     why, file by file where relevant. Do not write the actual diffs or full file contents — the \
     executing model will produce those. Keep the plan concrete and actionable, not exploratory.";

/// One architect seat: a display name for the transcript plus the backend
/// that produces the plan. Mirrors `council::CouncilSeat`'s shape.
pub struct ArchitectSeat {
    pub model: String,
    pub backend: Arc<dyn LlmBackend>,
}

/// The configured architect, if any — `tail_budget_tokens` lives here
/// (rather than on `ArchitectSeat`) exactly as `council::Council` wraps its
/// seats with `tail_budget_tokens`, since a bare seat carries no notion of
/// how much conversation context it should see.
pub struct Architect {
    pub seat: ArchitectSeat,
    pub tail_budget_tokens: u32,
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_command_recognizes_bare_and_task_forms() {
        assert_eq!(parse_command("/architect"), Some(""));
        assert_eq!(parse_command("  /architect  "), Some(""));
        assert_eq!(
            parse_command("/architect refactor the auth module"),
            Some("refactor the auth module")
        );
    }

    #[test]
    fn parse_command_rejects_lookalikes_and_normal_messages() {
        assert_eq!(parse_command("/architecture"), None);
        assert_eq!(parse_command("run /architect for me"), None);
        assert_eq!(parse_command("architect"), None);
    }
}
```

- [ ] **Run the tests to verify they pass** (this step's tests are
  self-contained — no dependency on the rest of the plan yet):

Run: `cargo test -p aivyx-core architect:: --lib`
Expected: PASS (2 tests) — but the crate won't yet compile as a whole since
`architect.rs` isn't wired into `lib.rs`. Fix that now:

- [ ] In `crates/aivyx-core/src/lib.rs`, add `pub mod architect;` (next to
  `pub mod agent;`) and `pub use architect::{Architect, ArchitectSeat};`
  (next to the `council` re-export line):

```rust
pub mod agent;
pub mod architect;
pub mod council;
pub mod delegate;
pub mod edit_blocks;
pub mod session;
pub mod wiki;

pub use agent::{Agent, AgentConfig, AgentError, AgentEvent, EditFormat};
pub use architect::{Architect, ArchitectSeat};
pub use council::{Council, CouncilSeat};
pub use delegate::{DelegateTaskConfig, DelegateTaskTool};
pub use session::{SessionState, Task, TaskStatus};
```

- [ ] Re-run: `cargo test -p aivyx-core architect:: --lib`
Expected: PASS (2 tests), crate compiles.

#### Step 3: Add `AgentEvent::ArchitectNote` and the `architect` field to `Agent`

- [ ] In `crates/aivyx-core/src/agent.rs`, add a new variant to `AgentEvent`
  right after `CouncilNote(String),`:

```rust
    /// One block of architect-mode output (the planning-in-progress note,
    /// the produced plan, or a failure note) — mirrors `CouncilNote`
    /// exactly, but for the single-seat architect/editor pairing feature.
    /// See `crate::architect::plan`.
    ArchitectNote(String),
```

- [ ] Add a new field to the `Agent` struct, right after the `council`
  field:

```rust
    /// `/architect` support when configured; `None` makes the command
    /// explain how to enable itself instead of running.
    architect: Option<crate::architect::Architect>,
```

- [ ] Initialize it in `Agent::new`'s constructor body, right after
  `council: None,`:

```rust
            architect: None,
```

- [ ] Add a public setter right after `set_council`:

```rust
    /// Enables `/architect` (ROADMAP.md Phase 9). The caller builds the
    /// seat from config — see `crate::architect::Architect`.
    pub fn set_architect(&mut self, architect: crate::architect::Architect) {
        self.architect = Some(architect);
    }
```

- [ ] Run: `cargo build -p aivyx-core`
Expected: compiles (the new field/variant/setter are all unused-but-valid
additions so far — no dispatch wired yet).

#### Step 4: Implement `architect::plan` and wire dispatch/`run_architect_turn`

- [ ] Add `plan` to `crates/aivyx-core/src/architect.rs`, right after
  `parse_command` (before the `#[cfg(test)]` block):

```rust
/// Makes one no-tools planning call to the architect's backend and returns
/// the produced plan text, or `None` on any failure (backend error, an
/// empty/near-empty response after `<think>`-stripping, or cancellation) —
/// every failure path emits its own `AgentEvent::ArchitectNote` explaining
/// why, mirroring `council::convene`'s "a failed stage is a transcript
/// note, not an `AgentError`" contract.
pub(crate) async fn plan(
    seat: &ArchitectSeat,
    subject: &str,
    context: Option<String>,
    events: &UnboundedSender<AgentEvent>,
    cancellation: &CancellationToken,
) -> Option<String> {
    let note = |text: String| {
        let _ = events.send(AgentEvent::ArchitectNote(text));
    };

    note(format!(
        "{} is planning… (cold model loads can take a while)",
        seat.model
    ));

    let mut user_prompt = String::new();
    if let Some(context) = &context {
        user_prompt.push_str(context);
        user_prompt.push_str("\n\n");
    }
    user_prompt.push_str("The task to plan:\n");
    user_prompt.push_str(subject);

    let request = ChatRequest::new(vec![
        Message::text(Role::System, ARCHITECT_PROMPT),
        Message::text(Role::User, user_prompt),
    ]);

    match crate::council::collect_text(seat.backend.as_ref(), request, cancellation).await {
        Ok(text) => {
            let text = crate::council::strip_think(&text);
            if text.chars().count() < crate::council::MIN_ANSWER_CHARS {
                note(format!(
                    "{} produced no usable plan — nothing was executed",
                    seat.model
                ));
                None
            } else {
                note(format!("plan from {}:\n{text}", seat.model));
                Some(text)
            }
        }
        Err(CollectError::Cancelled) => {
            note("architect planning cancelled — nothing was executed".to_string());
            None
        }
        Err(CollectError::Backend(err)) => {
            note(format!(
                "{} failed to produce a plan ({err}) — nothing was executed",
                seat.model
            ));
            None
        }
    }
}
```

- [ ] In `crates/aivyx-core/src/agent.rs`, update the command dispatch in
  `run_turn` (the `match crate::council::parse_command(&user_input)` block)
  to add `/architect` as a third arm, after `/wiki`:

```rust
        let result = match crate::council::parse_command(&user_input) {
            Some(subject) => {
                let subject = subject.to_string();
                self.run_council_turn(&subject, cancellation).await
            }
            None => match crate::wiki::parse_command(&user_input) {
                Some(command) => self.run_wiki_turn(command, cwd, cancellation).await,
                None => match crate::architect::parse_command(&user_input) {
                    Some(subject) => {
                        let subject = subject.to_string();
                        self.run_architect_turn(&subject, cwd, cancellation).await
                    }
                    None => self.run_turn_inner(user_input, cwd, cancellation).await,
                },
            },
        };
```

- [ ] Add `run_architect_turn` as a new method on `Agent`, right after
  `run_council_turn` (before `run_wiki_turn`'s doc comment):

```rust
    /// Runs `/architect`: makes one planning call to the configured
    /// architect, then hands the plan directly to `run_turn_inner` so the
    /// primary (editor) model begins acting on it in the same call — one
    /// continuous action, no re-prompt needed. A missing task argument or
    /// any planning failure ends the turn without ever invoking the editor.
    async fn run_architect_turn(
        &mut self,
        subject: &str,
        cwd: &Path,
        cancellation: CancellationToken,
    ) -> Result<(), AgentError> {
        if self.architect.is_none() {
            self.emit(AgentEvent::ArchitectNote(
                "no architect is configured — add [architect] base_url and model to \
                 config.toml (see the README's Architect/editor section)"
                    .to_string(),
            ));
            self.emit(AgentEvent::TurnComplete);
            return Ok(());
        }
        if subject.trim().is_empty() {
            self.emit(AgentEvent::ArchitectNote(
                "usage: /architect <task> — describe the task you want planned, e.g. \
                 /architect refactor the auth module to use the new token type"
                    .to_string(),
            ));
            self.emit(AgentEvent::TurnComplete);
            return Ok(());
        }

        self.refresh_repo_map().await;
        let architect = self.architect.as_ref().expect("checked above");
        let budget_chars =
            (architect.tail_budget_tokens as f64 * self.chars_per_token) as usize;
        let digest = crate::council::tail_digest(&self.history, budget_chars);

        let mut context = String::new();
        if let Some(map) = &self.repo_map_text {
            context.push_str(map);
            context.push_str("\n\n");
        }
        if let Some(digest) = &digest {
            context.push_str(digest);
        }
        let context = if context.is_empty() { None } else { Some(context) };

        let seat = &self.architect.as_ref().expect("checked above").seat;
        let plan_text = crate::architect::plan(
            seat,
            subject,
            context,
            &self.events_tx,
            &cancellation,
        )
        .await;

        let Some(plan_text) = plan_text else {
            self.emit(AgentEvent::TurnComplete);
            return Ok(());
        };
        let model_name = seat.model.clone();

        let formatted = format!(
            "[Architect plan — {model_name} planned this task; you are executing it]\n\n\
             Task: {subject}\n\nPlan:\n{plan_text}"
        );
        self.run_turn_inner(formatted, cwd, cancellation).await
    }
```

- [ ] Run: `cargo build -p aivyx-core`
Expected: compiles cleanly. (If the borrow checker objects to `seat`/
`architect` living across the `.await` alongside a later `&mut self` use,
double-check `model_name` is cloned *before* `self.run_turn_inner` is
called — the code above already orders it that way.)

#### Step 5: Write and pass the integration tests

- [ ] Add these helpers and tests to the `mod tests` block in
  `crates/aivyx-core/src/agent.rs`, right after the existing
  `council_notes` helper and its constants (near
  `council_command_without_configuration_notes_and_ends_the_turn`):

```rust
    // ----- architect/editor pairing (Phase 9) -----

    fn architect_seat(
        model: &str,
        responses: Vec<Vec<StreamEvent>>,
    ) -> (crate::architect::ArchitectSeat, Arc<MockBackend>) {
        let mock = Arc::new(MockBackend::new(responses));
        (
            crate::architect::ArchitectSeat {
                model: model.to_string(),
                backend: mock.clone(),
            },
            mock,
        )
    }

    fn architect_notes(events: &[AgentEvent]) -> Vec<&str> {
        events
            .iter()
            .filter_map(|e| match e {
                AgentEvent::ArchitectNote(text) => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    // Long enough to clear MIN_ANSWER_CHARS in council.rs.
    const PLAN_TEXT: &str = "1. Add a TokenV2 struct in auth/token.rs. 2. Update verify() to accept it.";

    #[tokio::test]
    async fn architect_command_without_configuration_notes_and_ends_the_turn() {
        let (mut agent, mut rx, main_mock) = build_agent(vec![], ToolRegistry::new(), 10);

        agent
            .run_turn(
                "/architect refactor auth".to_string(),
                Path::new("."),
                CancellationToken::new(),
            )
            .await
            .unwrap();

        let events = drain(&mut rx);
        assert!(
            architect_notes(&events)
                .iter()
                .any(|n| n.contains("no architect is configured"))
        );
        assert!(events.iter().any(|e| matches!(e, AgentEvent::TurnComplete)));
        assert!(agent.history.is_empty(), "command must not enter history");
        assert!(
            main_mock.received.lock().unwrap().is_empty(),
            "no LLM request may be made without an architect"
        );
    }

    #[tokio::test]
    async fn bare_architect_command_notes_usage_and_ends_the_turn() {
        let (mut agent, mut rx, _main_mock) = build_agent(vec![], ToolRegistry::new(), 10);
        let (seat, _) = architect_seat("model-architect", vec![text_response(PLAN_TEXT)]);
        agent.set_architect(crate::architect::Architect {
            seat,
            tail_budget_tokens: 3072,
        });

        agent
            .run_turn("/architect".to_string(), Path::new("."), CancellationToken::new())
            .await
            .unwrap();

        let events = drain(&mut rx);
        assert!(architect_notes(&events).iter().any(|n| n.contains("usage:")));
        assert!(events.iter().any(|e| matches!(e, AgentEvent::TurnComplete)));
        assert!(agent.history.is_empty());
    }

    #[tokio::test]
    async fn architect_plan_hands_off_to_the_editor_in_the_same_turn() {
        // The editor's mock backend replies with one tool call (read_file)
        // then a plain stop, so the test can prove the hand-off actually
        // reached the tool-dispatch loop, not just that text was injected.
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(aivyx_tools::ReadFileTool));
        let editor_responses = vec![
            vec![StreamEvent::ToolCallComplete(tool_call("c1", "read_file"))],
            text_response("done"),
        ];
        let (mut agent, mut rx, editor_mock) = build_agent(editor_responses, registry, 10);
        let (seat, architect_mock) = architect_seat("model-architect", vec![text_response(PLAN_TEXT)]);
        agent.set_architect(crate::architect::Architect {
            seat,
            tail_budget_tokens: 3072,
        });

        agent
            .run_turn(
                "/architect refactor the auth module".to_string(),
                Path::new("."),
                CancellationToken::new(),
            )
            .await
            .unwrap();

        assert_eq!(
            architect_mock.received.lock().unwrap().len(),
            1,
            "the architect backend must be called exactly once"
        );
        assert_eq!(
            editor_mock.received.lock().unwrap().len(),
            2,
            "the editor backend must run its normal iteration loop after the hand-off"
        );

        // History carries the injected plan message and the editor's own
        // tool-call round-trip, in that order — proving the hand-off is one
        // continuous turn, not two disjoint actions.
        let plan_index = agent
            .history
            .iter()
            .position(|m| {
                m.content.iter().any(|b| matches!(b, ContentBlock::Text(t) if t.contains("[Architect plan")))
            })
            .expect("plan message must be in history");
        assert!(
            agent.history[plan_index]
                .content
                .iter()
                .any(|b| matches!(b, ContentBlock::Text(t) if t.contains(PLAN_TEXT))),
            "injected message must contain the architect's plan text"
        );
        assert_eq!(count_tool_calls(&agent.history[plan_index..]), 1);

        let events = drain(&mut rx);
        assert!(
            architect_notes(&events).iter().any(|n| n.contains(PLAN_TEXT)),
            "the plan must stream live as an ArchitectNote"
        );
        assert!(events.iter().any(|e| matches!(e, AgentEvent::ToolCallDetected(_))));
    }

    #[tokio::test]
    async fn architect_backend_failure_ends_the_turn_without_invoking_the_editor() {
        let (mut agent, mut rx, editor_mock) = build_agent(vec![], ToolRegistry::new(), 10);
        let (seat, architect_mock) = architect_seat("model-architect", vec![]);
        agent.set_architect(crate::architect::Architect {
            seat,
            tail_budget_tokens: 3072,
        });

        agent
            .run_turn(
                "/architect refactor auth".to_string(),
                Path::new("."),
                CancellationToken::new(),
            )
            .await
            .unwrap();

        let events = drain(&mut rx);
        assert!(
            architect_notes(&events)
                .iter()
                .any(|n| n.contains("no usable plan")),
            "an empty response (below MIN_ANSWER_CHARS) must be treated as a failure"
        );
        assert!(events.iter().any(|e| matches!(e, AgentEvent::TurnComplete)));
        assert!(agent.history.is_empty());
        assert_eq!(architect_mock.received.lock().unwrap().len(), 1);
        assert!(
            editor_mock.received.lock().unwrap().is_empty(),
            "the editor must never be invoked after a failed plan"
        );
    }

    #[tokio::test]
    async fn architect_plan_mode_regression_editor_only_gets_read_only_tools() {
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(aivyx_tools::ReadFileTool));
        registry.register(Arc::new(aivyx_tools::WriteFileTool));

        let (tx, mut rx) = unbounded_channel();
        let mock = Arc::new(MockBackend::new(vec![text_response("noted")]));
        let llm: Arc<dyn LlmBackend> = mock.clone();
        let gate: Arc<dyn PermissionGate> = Arc::new(AllowAllGate);
        let confiner: Arc<dyn ExecutionConfiner> = Arc::new(NoopConfiner);
        let executor = ToolExecutor::new(registry, gate, confiner);
        let plan_mode = PlanMode::new();
        plan_mode.set_active(true);
        let mut agent = Agent::new(
            llm,
            executor,
            "system",
            AgentConfig {
                max_tool_iterations: 10,
                ..Default::default()
            },
            Arc::default(),
            plan_mode,
            AutonomousMode::new(),
            tx,
        );
        let (seat, _architect_mock) = architect_seat("model-architect", vec![text_response(PLAN_TEXT)]);
        agent.set_architect(crate::architect::Architect {
            seat,
            tail_budget_tokens: 3072,
        });

        agent
            .run_turn(
                "/architect refactor auth".to_string(),
                Path::new("."),
                CancellationToken::new(),
            )
            .await
            .unwrap();

        let received = mock.received.lock().unwrap();
        let tool_names: Vec<&str> = received[0].tools.iter().map(|t| t.name.as_str()).collect();
        assert!(tool_names.contains(&"read_file"));
        assert!(
            !tool_names.contains(&"write_file"),
            "plan mode must still filter the editor's own tool list after an architect hand-off"
        );
        drop(rx.try_recv()); // drain isn't needed for this assertion; silence unused warning
    }

    #[tokio::test]
    async fn architect_planning_cancellation_ends_the_turn_without_invoking_the_editor() {
        let (mut agent, mut rx, editor_mock) = build_agent(vec![], ToolRegistry::new(), 10);
        let (seat, _architect_mock) = architect_seat("model-architect", vec![text_response(PLAN_TEXT)]);
        agent.set_architect(crate::architect::Architect {
            seat,
            tail_budget_tokens: 3072,
        });
        let cancellation = CancellationToken::new();
        cancellation.cancel();

        agent
            .run_turn(
                "/architect refactor auth".to_string(),
                Path::new("."),
                cancellation,
            )
            .await
            .unwrap();

        let events = drain(&mut rx);
        assert!(
            architect_notes(&events)
                .iter()
                .any(|n| n.contains("cancelled")),
            "a pre-cancelled token must abort planning with an explanatory note"
        );
        assert!(agent.history.is_empty());
        assert!(editor_mock.received.lock().unwrap().is_empty());
    }
```

- [ ] **Run the tests to verify they pass**

Run: `cargo test -p aivyx-core --lib architect`
Expected: PASS (8 tests: 2 `parse_command` + 6 integration).

- [ ] **Run the full `aivyx-core` test suite**

Run: `cargo test -p aivyx-core --lib`
Expected: all tests pass, no regressions to `council`/`wiki`/`delegate` tests.

- [ ] **Commit**

```bash
git add crates/aivyx-core/src/architect.rs crates/aivyx-core/src/council.rs \
        crates/aivyx-core/src/agent.rs crates/aivyx-core/src/lib.rs
git commit -m "Core: /architect command, architect/editor pairing hand-off (Phase 9)"
```

---

### Task 3: TUI — `ChatLine::Architect` rendering

**Files:**
- Modify: `crates/aivyx-tui/src/app.rs`

**Interfaces:**
- Consumes: `AgentEvent::ArchitectNote(String)` (Task 2).
- Produces: `ChatLine::Architect(String)` variant, rendered via
  `chat_line_to_lines`.

- [ ] **Step 1: Write the failing test**

Add to the `mod tests` block in `crates/aivyx-tui/src/app.rs`, right after
`sub_agent_tool_call_is_prefixed_and_still_distinguished`:

```rust
    #[test]
    fn architect_note_renders_as_a_distinguished_chat_line() {
        let mut app = App::new(None, PlanMode::new());
        app.handle_agent_event(AgentEvent::ArchitectNote(
            "plan from model-architect:\n1. Add TokenV2.".to_string(),
        ));

        assert!(matches!(
            app.transcript.last(),
            Some(ChatLine::Architect(text)) if text.contains("Add TokenV2")
        ));

        let lines = chat_line_to_lines(app.transcript.last().unwrap());
        assert!(lines[0].to_string().starts_with("architect> "));
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p aivyx-tui architect_note --lib`
Expected: FAIL to compile — no `ChatLine::Architect` variant, no
`AgentEvent::ArchitectNote` arm in `handle_agent_event`.

- [ ] **Step 3: Add the `ChatLine::Architect` variant**

In `crates/aivyx-tui/src/app.rs`, add to the `ChatLine` enum, right after
`Council(String),`:

```rust
    /// One block of `/architect` output (the planning-in-progress note or
    /// the produced plan) — visually distinct from `Council`'s
    /// deliberation and the parent's own transcript, since the architect is
    /// a separate model producing a plan, not "aivyx" speaking.
    Architect(String),
```

- [ ] **Step 4: Wire the event dispatch and rendering**

In `handle_agent_event`, add a new arm right after the `CouncilNote` arm:

```rust
            AgentEvent::ArchitectNote(text) => {
                self.transcript.push(ChatLine::Architect(text));
            }
```

In `chat_line_to_lines`, add a new arm right after the `ChatLine::Council`
arm:

```rust
        ChatLine::Architect(text) => {
            prefixed_lines(text, "architect> ", Style::default().fg(Color::Cyan))
        }
```

- [ ] **Step 5: Run the test to verify it passes**

Run: `cargo test -p aivyx-tui architect_note --lib`
Expected: PASS.

- [ ] **Step 6: Run the full `aivyx-tui` test suite**

Run: `cargo test -p aivyx-tui --lib`
Expected: all tests pass, no regressions.

- [ ] **Step 7: Commit**

```bash
git add crates/aivyx-tui/src/app.rs
git commit -m "TUI: render /architect notes with a distinguished chat line (Phase 9)"
```

---

### Task 4: `main.rs` wiring

**Files:**
- Modify: `crates/aivyx/src/main.rs`

**Interfaces:**
- Consumes: `settings.architect: ArchitectSettings` (Task 1),
  `aivyx_core::{Architect, ArchitectSeat}` (Task 2),
  `OpenAiCompatBackend::with_idle_timeout` (existing, used identically for
  council), `agent.set_architect` (Task 2).

- [ ] **Step 1: Import the new types**

In `crates/aivyx/src/main.rs`, update the `aivyx_core` import line:

```rust
use aivyx_core::{Agent, AgentConfig, Architect, ArchitectSeat, Council, CouncilSeat, EditFormat, session};
```

- [ ] **Step 2: Construct the architect seat from config**

Right after the existing `/council` wiring block (the `if
settings.council.configured() { ... }` block that ends with
`agent.set_council(...)`), add:

```rust
    // `/architect` needs base_url + model; anything less and the command
    // explains itself instead (the agent handles the None case).
    if settings.architect.configured() {
        agent.set_architect(Architect {
            seat: ArchitectSeat {
                model: settings.architect.model.clone(),
                backend: Arc::new(OpenAiCompatBackend::with_idle_timeout(
                    settings.architect.base_url.clone(),
                    settings.architect.model.clone(),
                    settings.architect.api_key.clone(),
                    COUNCIL_IDLE_TIMEOUT,
                )),
            },
            tail_budget_tokens: settings.architect.tail_budget_tokens,
        });
    }
```

(Reusing `COUNCIL_IDLE_TIMEOUT` deliberately — the same reasoning applies:
an architect seat may be a cold-loaded or remote model that tolerates a
much longer silence than the interactive editor backend.)

- [ ] **Step 3: Build the workspace**

Run: `cargo build --workspace`
Expected: builds cleanly, no warnings.

- [ ] **Step 4: Manual config smoke check**

Run: `cargo run -p aivyx -- --help` (or equivalent entry point already used
by this project to sanity-check `main.rs` changes) just to confirm the
binary still starts without panicking on config load with a default
(unconfigured) `[architect]` section. Expected: normal startup, no error
about a missing `[architect]` section (config is `#[serde(default)]`
throughout, so an absent section is fine).

- [ ] **Step 5: Commit**

```bash
git add crates/aivyx/src/main.rs
git commit -m "Wire /architect construction into main.rs (Phase 9)"
```

---

### Task 5: Docs + live E2E verification + final check

**Files:**
- Modify: `README.md`
- Modify: `ROADMAP.md`

**Interfaces:**
- Consumes: the complete, merged-locally feature from Tasks 1–4.

- [ ] **Step 1: Live E2E through the real binary**

Using the same PTY-harness pattern as the delegation and wiki phases (a
Python `pty`/`select`/`subprocess` driver that types a slash command into
the real interactive `aivyx` TUI, auto-approves confirmation modals, and
grades from files on disk/persisted session JSON rather than raw screen
text): configure a scratch project's `config.toml` with a real `[architect]`
section pointing at a locally-running model distinct from `[backend]`
(e.g. a larger model via Ollama, or a second llama-server instance), plus
`[backend]` as the editor. Run `/architect <task that clearly implies an
edit, e.g. "add a doc comment to the fib function in alpha-crate/src/lib.rs
explaining its complexity">` against a small scratch git repo with one fake
crate.

Confirm, in order:
1. An `architect>`-prefixed planning note appears live in the raw pty
   capture (use a short, robust marker per this project's own
   ratatui-fragmentation lesson — e.g. matching on `"architect"` or
   `"Plan:"` rather than a longer literal phrase).
2. A real tool-call confirmation modal appears for the editor's resulting
   `edit_file`/`write_file` call (same `Tool:`/`Target:`/`quired` marker
   set already used by the delegation/wiki harnesses).
3. After approving, the target file is actually modified on disk with
   content addressing the task — confirming the hand-off produced a real
   edit, not just a plan that was never acted on.
4. The persisted session JSON's history contains, in order: the injected
   `"[Architect plan"`-prefixed message, then the tool-call/tool-result
   pair for the edit — confirming both halves landed in the same
   continuous turn.

Record the harness script and its output (or a summary of both) for the
final report; no need to keep the scratch config/repo afterward.

- [ ] **Step 2: Update `README.md`**

Add a new paragraph after the existing "Sub-agent delegation" paragraph
(match the surrounding paragraphs' style: one paragraph, cross-reference
where relevant), documenting: `/architect <task>`, the `[architect]` config
section and its off-by-default behavior, the single-seat (no deliberation)
shape distinguishing it from `/council`, and the direct hand-off into the
editor's own turn (one continuous action, no re-prompt).

- [ ] **Step 3: Update `ROADMAP.md`**

In the Phase 9 section, insert a "built and live-verified
(`<today's date>`)" paragraph — matching the style of the existing
"Sub-agent delegation built and live-verified (2026-07-13)" paragraph
immediately above it — covering: the `crate::architect` module's shape
mirroring `crate::council`, the direct `run_turn_inner` hand-off mechanism
and why it needed no changes to `run_turn_inner` itself, the
`Architect`/`ArchitectSeat` split (tail budget on the wrapper, not the
seat) mirroring `Council`/`CouncilSeat`, the test count delta, and the
live E2E results from Step 1.

- [ ] **Step 4: Final full-workspace check**

Run: `cargo test --workspace`
Expected: all tests pass (Task 1's 3 + Task 2's 8 + Task 3's 1 = 12 new
tests, on top of the pre-existing baseline).

Run: `cargo clippy --workspace --all-targets`
Expected: clean, no warnings.

- [ ] **Step 5: Commit**

```bash
git add README.md ROADMAP.md
git commit -m "Docs: architect/editor pairing + live E2E verification (Phase 9)"
```
