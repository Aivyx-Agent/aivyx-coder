# Nonagon-Style Team — Phase 2 (Pool + Delegation) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `DelegateToSpecialistTool` — an attenuated sibling to the
existing `delegate_task` (`aivyx-core/src/delegate.rs`) that spins up a
specialist `Agent` scoped to one `TeamMember`'s `effective_tool_allowlist`
and `persona`, reusing the same shared gate/confiner/checkpointer,
fresh-history, bounded-iteration mechanism `delegate_task` already has.
Fully built and tested in isolation — **not** registered onto the
default agent's tool list this phase (that's Phase 5's job).

**Architecture:** A new file, `crates/aivyx-core/src/delegate_to_specialist.rs`,
mirroring `delegate.rs`'s shape closely. Two pieces: (1) a pure-ish
helper, `compute_specialist_registry`, that turns a `TeamMember` + the
parent's full `ToolRegistry` into an attenuated `ToolRegistry` (via
`aivyx_team::effective_tool_allowlist` + the existing
`ToolRegistry::exclude`) — independently testable; (2) `DelegateToSpecialistConfig`/
`DelegateToSpecialistTool`, whose `execute()` looks up the named member,
builds the attenuated registry, and spins up a specialist `Agent` the
same way `delegate_task` does, using the member's `persona` as its
system prompt instead of `delegate_task`'s generic constant.

**Tech Stack:** Rust, reusing `aivyx-core`'s existing `Agent`/`AgentConfig`,
`aivyx-tools`' `ToolRegistry`/`ToolExecutor`, and the new `aivyx-team`
crate (already a workspace member, from Phase 1).

## Global Constraints

- `cargo build --workspace`, `cargo test --workspace`,
  `cargo clippy --workspace --all-targets -- -D warnings` must stay
  clean. `cargo fmt --check` on touched files only (known, pre-existing,
  out-of-scope drift elsewhere in this repo).
- **No `agent_builder.rs` changes this phase.** `DelegateToSpecialistTool`
  is built, exported from `aivyx-core`, and fully tested — but never
  constructed or registered in the real binary's tool-building path.
  Real wiring (and how a user opts into team mode at all) is Phase 5's
  job. Do not add it to `agent_builder.rs`'s `build_agent` even as a
  disabled-by-default option — that decision belongs to Phase 5, not
  here.
- **Deny-paths attenuation is out of scope.** The specialist shares the
  lead's exact `deny_paths` (whatever the lead's own tool instances
  already have baked in) — do not attempt to rebuild any tool instance
  with a different `deny_paths` value. Only which tools are *visible* is
  attenuated (via `ToolRegistry::exclude`), not each tool's own internal
  configuration.
- `aivyx-core` already depends on `aivyx-tools`/`aivyx-llm`/`aivyx-sandbox`/
  `aivyx-types` (confirmed via `delegate.rs`'s own imports) — add
  `aivyx-team` as a new dependency of `aivyx-core` (it's dependency-light
  itself, per Phase 1's own constraints, so this is a safe, small
  addition, not a layering violation).
- Match `delegate.rs`'s existing conventions exactly: `CUTOFF_NOTICE`-style
  constants for the iteration-budget and injection-taint stop conditions,
  the same `MockBackend`/`AllowAllGate`/`text_response`/`exec_ctx` test
  harness shape (each new file needs its own copies — these are private
  to `delegate.rs`'s own `#[cfg(test)] mod tests`, not exported).

---

## Task 1: `compute_specialist_registry` + the tool's shell (args, definition, permission_request)

**Files:**
- Create: `/home/julian/Projects/Rust/aivyx-coder/crates/aivyx-core/src/delegate_to_specialist.rs`
- Modify: `/home/julian/Projects/Rust/aivyx-coder/crates/aivyx-core/src/lib.rs`
- Modify: `/home/julian/Projects/Rust/aivyx-coder/crates/aivyx-core/Cargo.toml`

**Interfaces:**
- Produces: `pub fn compute_specialist_registry(member: &aivyx_team::TeamMember, parent_registry: &ToolRegistry) -> ToolRegistry`;
  `pub struct DelegateToSpecialistConfig { ... }` (fields per the spec's
  Decision 3, listed in Step 4 below); `pub struct DelegateToSpecialistTool { config: DelegateToSpecialistConfig }`
  implementing `Tool`'s `name`/`definition`/`mutates_outside_session`/
  `permission_request` in this task (`execute` is Task 2's job — stub it
  with `todo!()` in this task, replaced for real in Task 2, so the file
  compiles and this task's own tests can run independently).

- [ ] **Step 1: Add `aivyx-team` as a dependency**

Read `crates/aivyx-core/Cargo.toml` in full first. Add:

```toml
aivyx-team = { path = "../aivyx-team" }
```

to `[dependencies]`, matching the existing path-dependency style already
used for `aivyx-tools`/`aivyx-sandbox` etc. in that same file (check
their exact syntax — version + path, or path-only — and match it).

- [ ] **Step 2: Write the failing tests for `compute_specialist_registry`**

```rust
#[cfg(test)]
mod registry_attenuation_tests {
    use super::*;
    use aivyx_team::TeamMember;

    fn member(tool_allowlist: &[&str]) -> TeamMember {
        TeamMember {
            name: "implementer".to_string(),
            role: "Implementer".to_string(),
            persona: "You write code.".to_string(),
            tool_allowlist: tool_allowlist.iter().map(|s| s.to_string()).collect(),
            extra_deny_paths: vec![],
        }
    }

    // A minimal Tool impl for building a test registry -- named tools
    // with no real behavior, matching this crate's existing test
    // conventions of lightweight stand-ins rather than the real
    // aivyx-tools types (which this crate cannot depend on -- see the
    // parent plan's Global Constraints on dependency direction).
    struct NamedTool(&'static str);
    #[async_trait::async_trait]
    impl Tool for NamedTool {
        fn name(&self) -> &str {
            self.0
        }
        fn definition(&self) -> aivyx_types::ToolDefinition {
            aivyx_types::ToolDefinition {
                name: self.0.to_string(),
                description: String::new(),
                parameters_schema: serde_json::json!({}),
            }
        }
        fn permission_request(
            &self,
            _arguments: &serde_json::Value,
            _cwd: &std::path::Path,
        ) -> Result<aivyx_sandbox::PermissionRequest, aivyx_tools::ToolError> {
            unreachable!("not exercised by these tests")
        }
        async fn execute(
            &self,
            _arguments: serde_json::Value,
            _ctx: &aivyx_tools::ToolExecutionContext,
        ) -> Result<aivyx_types::ToolOutput, aivyx_tools::ToolError> {
            unreachable!("not exercised by these tests")
        }
    }

    fn registry_with(names: &[&'static str]) -> ToolRegistry {
        let mut registry = ToolRegistry::new();
        for name in names {
            registry.register(Arc::new(NamedTool(name)));
        }
        registry
    }

    #[test]
    fn attenuated_registry_keeps_only_the_members_allowed_tools() {
        let parent = registry_with(&["read_file", "write_file", "grep", "run_command"]);
        let m = member(&["read_file", "grep"]);
        let attenuated = compute_specialist_registry(&m, &parent);
        let mut names: Vec<String> = attenuated
            .definitions()
            .into_iter()
            .map(|d| d.name)
            .collect();
        names.sort();
        assert_eq!(names, vec!["grep".to_string(), "read_file".to_string()]);
    }

    #[test]
    fn attenuated_registry_is_empty_for_a_member_with_no_tool_allowlist() {
        let parent = registry_with(&["read_file", "write_file"]);
        let m = member(&[]);
        let attenuated = compute_specialist_registry(&m, &parent);
        assert!(attenuated.definitions().is_empty());
    }

    #[test]
    fn attenuated_registry_silently_drops_an_allowlist_entry_not_in_the_parent() {
        // A member's tool_allowlist naming a tool the parent doesn't
        // actually have registered (e.g. a stale/misconfigured
        // TeamConfig) must not panic or error here -- it's simply not
        // in the parent's registry to begin with, so it's absent from
        // the attenuated result too. (Load-time TeamConfig::validate is
        // where this should have been caught already; this function
        // doesn't re-validate.)
        let parent = registry_with(&["read_file"]);
        let m = member(&["read_file", "nonexistent_tool"]);
        let attenuated = compute_specialist_registry(&m, &parent);
        let names: Vec<String> = attenuated.definitions().into_iter().map(|d| d.name).collect();
        assert_eq!(names, vec!["read_file".to_string()]);
    }
}
```

- [ ] **Step 3: Run to verify they fail**

```bash
cd /home/julian/Projects/Rust/aivyx-coder
cargo test -p aivyx-core registry_attenuation_tests
```

Expected: compile error — the file/function don't exist yet.

- [ ] **Step 4: Implement the file's shell**

```rust
//! `DelegateToSpecialistTool`: an attenuated sibling to `delegate_task`
//! (`delegate.rs`) -- spins up a specialist `Agent` scoped to one
//! `aivyx_team::TeamMember`'s `effective_tool_allowlist` and `persona`,
//! reusing the exact same shared gate/confiner/checkpointer,
//! fresh-history, bounded-iteration mechanism `delegate_task` already
//! has. Deliberately NOT registered onto the default agent's tool list
//! (see `docs/superpowers/plans/2026-09-20-nonagon-team-delegation.md`'s
//! Global Constraints) -- that's a later phase's job, once there's a
//! real way to load a `TeamConfig` and opt into team mode at all.
//!
//! Deny-paths attenuation is explicitly out of scope here: a specialist
//! shares the lead's exact `deny_paths` (baked into each tool instance
//! at construction time in `agent_builder.rs`, which this function
//! never touches) -- only which tools are *visible* is attenuated.

use std::path::Path;
use std::sync::Arc;

use aivyx_llm::LlmBackend;
use aivyx_repomap::RepoMap;
use aivyx_sandbox::{
    ActionKind, AutonomousMode, ExecutionConfiner, InjectionTaint, PermissionGate,
    PermissionRequest, PermissionTarget, PlanMode,
};
use aivyx_team::TeamConfig;
use aivyx_tools::{GitCheckpointer, Tool, ToolError, ToolExecutionContext, ToolExecutor, ToolRegistry};
use aivyx_types::{ToolDefinition, ToolOutput};
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use tokio::sync::mpsc::UnboundedSender;

use crate::agent::{AgentEvent, EditFormat};

#[derive(Deserialize, JsonSchema)]
struct DelegateToSpecialistArgs {
    /// The name of a `TeamConfig` member to delegate to -- must match
    /// one of `team.members`' own `name` fields.
    member: String,
    /// A complete, self-contained description of the task for the
    /// specialist -- it starts with no context beyond this text and the
    /// specialist's own persona.
    task: String,
}

/// Turns `member`'s `tool_allowlist` into an attenuated `ToolRegistry`:
/// every tool in `parent_registry` whose name is in
/// `aivyx_team::effective_tool_allowlist(member, ...)`, nothing else.
/// Does not re-validate `member`'s `tool_allowlist` against
/// `parent_registry` -- an allowlist entry naming a tool the parent
/// doesn't actually have is silently absent from the result (that's
/// `TeamConfig::validate`'s job, at config-load time, not this
/// function's).
pub fn compute_specialist_registry(
    member: &aivyx_team::TeamMember,
    parent_registry: &ToolRegistry,
) -> ToolRegistry {
    // Bound once, owned, so the borrowed &str names below have
    // somewhere to live for the rest of this function.
    let definitions = parent_registry.definitions();
    let parent_names: Vec<&str> = definitions.iter().map(|d| d.name.as_str()).collect();
    let effective = aivyx_team::effective_tool_allowlist(member, &parent_names);
    let effective_set: std::collections::HashSet<&str> =
        effective.iter().map(|s| s.as_str()).collect();
    let exclude_names: Vec<&str> = parent_names
        .into_iter()
        .filter(|name| !effective_set.contains(name))
        .collect();
    let mut attenuated = parent_registry.clone();
    attenuated.exclude(&exclude_names);
    attenuated
}

pub struct DelegateToSpecialistConfig {
    pub llm: Arc<dyn LlmBackend>,
    pub gate: Arc<dyn PermissionGate>,
    pub confiner: Arc<dyn ExecutionConfiner>,
    pub checkpointer: Option<Arc<GitCheckpointer>>,
    pub repo_map: Option<(Arc<RepoMap>, u32)>,
    pub events_tx: UnboundedSender<AgentEvent>,
    /// The parent's full tool registry -- `compute_specialist_registry`
    /// attenuates it per-member at call time, so (unlike
    /// `delegate_task`'s pre-cloned, pre-excluded `sub_agent_registry`)
    /// this is the *unfiltered* parent registry, cloned once here at
    /// construction time (before this tool itself would ever be
    /// registered onto it, mirroring `delegate_task`'s own recursion-
    /// prevention structure -- though this tool is not registered onto
    /// the default agent at all this phase, see the module doc comment).
    pub parent_registry: ToolRegistry,
    pub team: TeamConfig,
    pub plan_mode: PlanMode,
    pub autonomous_mode: AutonomousMode,
    pub injection_taint: InjectionTaint,
    pub context_tokens: u32,
    pub edit_format: EditFormat,
    pub verification: Option<(String, u32)>,
    pub max_iterations: u32,
    pub broker_mode: bool,
}

pub struct DelegateToSpecialistTool {
    config: DelegateToSpecialistConfig,
}

impl DelegateToSpecialistTool {
    pub fn new(config: DelegateToSpecialistConfig) -> Self {
        Self { config }
    }
}

#[async_trait]
impl Tool for DelegateToSpecialistTool {
    fn name(&self) -> &str {
        "delegate_to_specialist"
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name().to_string(),
            description: "Delegate a bounded task to a named team specialist -- a fresh \
                sub-agent scoped to that specialist's own role and tool access (narrower than \
                yours), with its own isolated conversation history. Write a complete, \
                self-contained task description: the specialist starts with no context beyond \
                what you write here plus its own persona. Returns the specialist's final answer \
                as text."
                .to_string(),
            parameters_schema: serde_json::Value::from(schemars::schema_for!(
                DelegateToSpecialistArgs
            )),
        }
    }

    fn mutates_outside_session(&self) -> bool {
        false
    }

    fn permission_request(
        &self,
        _arguments: &serde_json::Value,
        _cwd: &Path,
    ) -> Result<PermissionRequest, ToolError> {
        Ok(PermissionRequest {
            tool_name: self.name().to_string(),
            action: ActionKind::Internal,
            target: PermissionTarget::Other("delegate_to_specialist".to_string()),
            arguments_preview: serde_json::json!({}),
            preview: None,
            diff: None,
        })
    }

    async fn execute(
        &self,
        _arguments: serde_json::Value,
        _ctx: &ToolExecutionContext,
    ) -> Result<ToolOutput, ToolError> {
        todo!("Task 2 implements this")
    }
}
```

- [ ] **Step 5: Wire the module and its exports**

Add `pub mod delegate_to_specialist;` to `crates/aivyx-core/src/lib.rs`
near the existing `pub mod delegate;` line. Add
`pub use delegate_to_specialist::{DelegateToSpecialistConfig, DelegateToSpecialistTool};`
near the existing `pub use delegate::{DelegateTaskConfig, DelegateTaskTool};`
line, matching its exact style.

- [ ] **Step 6: Run to verify the registry tests pass**

```bash
cd /home/julian/Projects/Rust/aivyx-coder
cargo test -p aivyx-core registry_attenuation_tests
```

Expected: all 3 tests pass. (The crate as a whole will not yet build
cleanly under `cargo build --workspace` because of the `todo!()` in
`execute` — that's expected and fine; `cargo test -p aivyx-core
registry_attenuation_tests` runs only the tests this task added, which
don't call `execute` at all.)

- [ ] **Step 7: Commit**

```bash
cd /home/julian/Projects/Rust/aivyx-coder
git add crates/aivyx-core/Cargo.toml crates/aivyx-core/src/lib.rs crates/aivyx-core/src/delegate_to_specialist.rs
git commit -m "feat: add compute_specialist_registry and the DelegateToSpecialistTool shell

Attenuates a parent ToolRegistry down to one TeamMember's
effective_tool_allowlist, reusing the existing ToolRegistry::exclude
(no new registry API). Tool's name/definition/permission_request/
mutates_outside_session are implemented; execute() is a stub -- Task 2's
job. Not yet registered anywhere (this phase never wires it into
agent_builder.rs)."
```

---

## Task 2: `execute()` — spin up the attenuated specialist, and integration tests

**Files:**
- Modify: `/home/julian/Projects/Rust/aivyx-coder/crates/aivyx-core/src/delegate_to_specialist.rs`

**Interfaces:**
- Consumes: `compute_specialist_registry`, `DelegateToSpecialistConfig`/
  `DelegateToSpecialistTool` from Task 1.
- Produces: a real `execute()` implementation (no interface change from
  Task 1's `Tool` impl signature).

- [ ] **Step 1: Read `delegate.rs`'s `DelegateTaskTool::execute` and its full test module again**

Re-read `crates/aivyx-core/src/delegate.rs` end to end (both the real
`execute()` body and its `#[cfg(test)] mod tests`) immediately before
starting this task — Task 1 was written from the same research, but the
exact `Agent::new(...)` call, the `CUTOFF_NOTICE`/`INJECTION_CUTOFF_NOTICE`
constants' real text, and the test harness (`MockBackend`, `AllowAllGate`,
`text_response`, `exec_ctx`) all need to be re-derived from the live file,
not assumed from this plan's paraphrase.

- [ ] **Step 2: Write the failing tests**

```rust
#[cfg(test)]
mod delegation_tests {
    use super::*;
    use aivyx_llm::{ChatRequest, FinishReason, LlmError, StreamEvent};
    use aivyx_sandbox::{NoopConfiner, PermissionDecision};
    use aivyx_team::TeamMember;
    use async_trait::async_trait;
    use futures::StreamExt;
    use futures::stream::BoxStream;
    use std::sync::Mutex;
    use tokio_util::sync::CancellationToken;

    struct MockBackend {
        responses: Mutex<std::collections::VecDeque<Vec<StreamEvent>>>,
        received: Mutex<Vec<ChatRequest>>,
    }

    impl MockBackend {
        fn new(responses: Vec<Vec<StreamEvent>>) -> Self {
            Self {
                responses: Mutex::new(responses.into()),
                received: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl LlmBackend for MockBackend {
        fn model_id(&self) -> &str {
            "mock"
        }

        async fn stream_chat(
            &self,
            request: ChatRequest,
        ) -> Result<BoxStream<'static, Result<StreamEvent, LlmError>>, LlmError> {
            self.received.lock().unwrap().push(request);
            let events = self.responses.lock().unwrap().pop_front().unwrap_or_default();
            Ok(futures::stream::iter(events.into_iter().map(Ok::<StreamEvent, LlmError>)).boxed())
        }
    }

    struct AllowAllGate;
    #[async_trait]
    impl PermissionGate for AllowAllGate {
        async fn check(&self, _request: &PermissionRequest) -> PermissionDecision {
            PermissionDecision::Allow
        }
    }

    fn text_response(text: &str) -> Vec<StreamEvent> {
        vec![
            StreamEvent::TextDelta(text.to_string()),
            StreamEvent::Done {
                finish_reason: FinishReason::Stop,
            },
        ]
    }

    fn exec_ctx(cwd: &std::path::Path) -> ToolExecutionContext {
        ToolExecutionContext {
            cwd: cwd.to_path_buf(),
            confiner: Arc::new(NoopConfiner),
            cancellation: CancellationToken::new(),
        }
    }

    fn simple_team() -> TeamConfig {
        TeamConfig {
            lead: "coordinator".to_string(),
            members: vec![
                TeamMember {
                    name: "coordinator".to_string(),
                    role: "Lead".to_string(),
                    persona: "You delegate.".to_string(),
                    tool_allowlist: vec![],
                    extra_deny_paths: vec![],
                },
                TeamMember {
                    name: "implementer".to_string(),
                    role: "Implementer".to_string(),
                    persona: "You are the implementer specialist. You write code.".to_string(),
                    tool_allowlist: vec![],
                    extra_deny_paths: vec![],
                },
            ],
        }
    }

    fn base_config(
        llm: Arc<dyn LlmBackend>,
        events_tx: UnboundedSender<AgentEvent>,
        team: TeamConfig,
    ) -> DelegateToSpecialistConfig {
        DelegateToSpecialistConfig {
            llm,
            gate: Arc::new(AllowAllGate),
            confiner: Arc::new(NoopConfiner),
            checkpointer: None,
            repo_map: None,
            events_tx,
            parent_registry: ToolRegistry::new(),
            team,
            plan_mode: PlanMode::new(),
            autonomous_mode: AutonomousMode::new(),
            injection_taint: InjectionTaint::new(),
            context_tokens: 8192,
            edit_format: EditFormat::Native,
            verification: None,
            max_iterations: 3,
            broker_mode: false,
        }
    }

    #[tokio::test]
    async fn delegating_to_a_known_member_returns_the_specialists_final_answer() {
        let llm = Arc::new(MockBackend::new(vec![text_response("done: the fix is applied")]));
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let config = base_config(Arc::clone(&llm) as Arc<dyn LlmBackend>, tx, simple_team());
        let tool = DelegateToSpecialistTool::new(config);
        let ctx = exec_ctx(std::path::Path::new("."));
        let args = serde_json::json!({ "member": "implementer", "task": "fix the bug" });
        let result = tool.execute(args, &ctx).await.unwrap();
        match result {
            ToolOutput::Ok(text) => assert!(text.contains("done: the fix is applied")),
            other => panic!("expected Ok, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn delegating_to_an_unknown_member_returns_a_tool_error() {
        let llm = Arc::new(MockBackend::new(vec![]));
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let config = base_config(Arc::clone(&llm) as Arc<dyn LlmBackend>, tx, simple_team());
        let tool = DelegateToSpecialistTool::new(config);
        let ctx = exec_ctx(std::path::Path::new("."));
        let args = serde_json::json!({ "member": "nonexistent", "task": "do something" });
        let result = tool.execute(args, &ctx).await.unwrap();
        match result {
            ToolOutput::Error(msg) => assert!(msg.contains("nonexistent")),
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn specialist_system_prompt_is_the_members_persona() {
        let llm = Arc::new(MockBackend::new(vec![text_response("ok")]));
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let config = base_config(Arc::clone(&llm) as Arc<dyn LlmBackend>, tx, simple_team());
        let tool = DelegateToSpecialistTool::new(config);
        let ctx = exec_ctx(std::path::Path::new("."));
        let args = serde_json::json!({ "member": "implementer", "task": "do the thing" });
        let _ = tool.execute(args, &ctx).await.unwrap();
        let received = llm.received.lock().unwrap();
        let first_request = received.first().expect("one request should have been sent");
        // Exact system-prompt field/shape depends on ChatRequest's real
        // structure -- re-verify against aivyx-llm's actual ChatRequest
        // type before finalizing this assertion; the intent is to prove
        // the specialist's persona text ("You are the implementer
        // specialist...") reached the outgoing request, not
        // delegate_task's own SUB_AGENT_SYSTEM_PROMPT constant.
        assert!(
            format!("{first_request:?}").contains("You are the implementer specialist"),
            "expected the member's persona in the outgoing request, got: {first_request:?}"
        );
    }
}
```

- [ ] **Step 3: Run to verify they fail**

```bash
cd /home/julian/Projects/Rust/aivyx-coder
cargo test -p aivyx-core delegation_tests
```

Expected: fails at the `todo!()` panic inside `execute` (a runtime panic
during the async test, not a compile error, since `Tool::execute`'s
signature is already satisfied by the stub).

- [ ] **Step 4: Implement `execute()`**

Replace the `todo!("Task 2 implements this")` body with real logic,
closely mirroring `DelegateTaskTool::execute` in `delegate.rs` (re-read
it fresh per Step 1 before writing this — the exact `Agent::new(...)`
argument order/types, the `CUTOFF_NOTICE`/injection-taint handling, and
the `SubAgentActivity` event-forwarding task all need to match that
file's real, current code, not this plan's paraphrase). The real
differences from `delegate_task`'s version:

1. Parse `DelegateToSpecialistArgs` (has `member` + `task`, not just
   `task`).
2. Look up `self.config.team.members.iter().find(|m| m.name == args.member)`;
   if `None`, return `Ok(ToolOutput::Error(format!("unknown team member: \
   {:?}", args.member)))` immediately — no sub-agent is ever constructed
   for an unknown member.
3. Build the attenuated registry via
   `compute_specialist_registry(member, &self.config.parent_registry)`
   (Task 1), then wrap it in a fresh `ToolExecutor` exactly like
   `delegate_task` does (`ToolExecutor::new(...)`, `set_checkpointer` if
   configured).
4. Use `member.persona.as_str()` (or `.clone()`, matching whatever
   `Agent::new`'s `system_prompt: impl Into<String>` parameter needs) as
   the system prompt argument to `Agent::new(...)`, in place of
   `delegate.rs`'s `SUB_AGENT_SYSTEM_PROMPT` constant.
5. Everything else — the event-forwarding spawned task, the
   `AgentConfig { max_tool_iterations: 1, ... }` sub-agent construction,
   the outer iteration loop bounded by `self.config.max_iterations`, the
   injection-taint / cutoff-notice handling, the final `ToolOutput::Ok`/
   `ToolOutput::Error` mapping — should be as close to a literal copy of
   `delegate.rs`'s own logic as the two configs' field differences allow.
   Define local constants mirroring `delegate.rs`'s `CUTOFF_NOTICE`/
   `INJECTION_CUTOFF_NOTICE`/`NO_TEXT_RESPONSE` (same text is fine, or
   adjust wording to say "specialist" instead of "sub-agent" — your
   call, note which you chose in your task report).

- [ ] **Step 5: Run to verify they pass**

```bash
cd /home/julian/Projects/Rust/aivyx-coder
cargo test -p aivyx-core delegation_tests
cargo test -p aivyx-core registry_attenuation_tests
```

Expected: all tests from both tasks pass. If Step 2's third test
(`specialist_system_prompt_is_the_members_persona`) needs adjustment
because `ChatRequest`'s real `Debug` output doesn't include the system
prompt the way assumed, adjust the assertion to inspect whatever field
actually carries it (check `aivyx-llm`'s real `ChatRequest` struct
definition) — the intent (prove the persona reached the request, not
`delegate_task`'s constant) is what matters, not the exact assertion
mechanics.

- [ ] **Step 6: Run full workspace check**

```bash
cd /home/julian/Projects/Rust/aivyx-coder
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check -p aivyx-core
```

- [ ] **Step 7: Commit**

```bash
cd /home/julian/Projects/Rust/aivyx-coder
git add crates/aivyx-core/src/delegate_to_specialist.rs
git commit -m "feat: implement DelegateToSpecialistTool::execute

Mirrors delegate_task's sub-agent-spinning-up mechanism (shared gate/
confiner/checkpointer, fresh isolated history, bounded iterations,
SubAgentActivity event forwarding) with two differences: the specialist
registry is attenuated per-member via compute_specialist_registry
(Task 1), and the specialist's system prompt is the member's own
persona rather than delegate_task's generic constant. An unknown
member name returns ToolOutput::Error without ever constructing a
sub-agent. Still not registered anywhere in agent_builder.rs -- Phase 5's
job, per this plan's Global Constraints."
```

---

## Final verification

- [ ] Run the complete workspace check once more, after both tasks:

```bash
cd /home/julian/Projects/Rust/aivyx-coder
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check -p aivyx-core
```

Expected: everything clean/passing. Confirm via `grep -rn
"DelegateToSpecialist" crates/aivyx/src/agent_builder.rs` that this
task genuinely made no changes there (should return no matches) —
matching this plan's Global Constraint that registration is Phase 5's
job, not this phase's.

## Explicitly out of scope for this plan

(Copied forward from the design spec's own "What this spec does not
decide" section.)

- Deny-paths attenuation and the `agent_builder.rs` refactor it would
  need — separate, later, unscoped work.
- Where a `TeamConfig` is actually loaded from at runtime, and how a
  user opts into team mode at all — Phase 5.
- Registering `DelegateToSpecialistTool` in `agent_builder.rs` at all —
  Phase 5.
- Phases 3, 4, 6 (mission structure, message bus, TUI) — each gets its
  own spec/plan cycle when reached.
