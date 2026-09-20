//! `DelegateToSpecialistTool`: an attenuated sibling to `delegate_task`
//! (`delegate.rs`) -- spins up a specialist `Agent` scoped to one
//! `aivyx_team::TeamMember`'s `effective_tool_allowlist` and `persona`,
//! reusing the exact same shared gate/confiner/checkpointer,
//! fresh-history, bounded-iteration mechanism `delegate_task` already
//! has. Registered conditionally in `agent_builder.rs`, gated on
//! `[team] enabled` (`TeamSettings`, off by default) -- see
//! `docs/superpowers/specs/2026-09-20-nonagon-team-entry-point-design.md`.
//! When enabled, the team is always `aivyx_team::default_coding_roster()`;
//! a custom-roster config format is still separate, later scope.
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
use aivyx_tools::{
    GitCheckpointer, Tool, ToolError, ToolExecutionContext, ToolExecutor, ToolRegistry,
};
use aivyx_types::{ToolDefinition, ToolOutput};
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use tokio::sync::mpsc;
use tokio::sync::mpsc::UnboundedSender;

use crate::agent::{Agent, AgentConfig, AgentEvent, EditFormat};

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

/// Mirrors `delegate.rs`'s own `CUTOFF_NOTICE` -- same wording, "sub-agent"
/// kept as-is rather than reworded to "specialist" (no behavioral
/// difference either way; consistency with the sibling tool's text won
/// out).
const CUTOFF_NOTICE: &str = "\n\n(sub-agent stopped: reached its iteration budget before \
finishing — the above is its best-effort partial result.)";

/// Mirrors `delegate.rs`'s own `INJECTION_CUTOFF_NOTICE` -- see that
/// constant's doc comment for why this outer loop's own taint check is
/// the only place that can stop a second delegated round-trip's tool call
/// once a result is flagged, given the specialist's own `AgentConfig`
/// fixes `max_tool_iterations` at 1 just like `delegate_task`'s sub-agent.
const INJECTION_CUTOFF_NOTICE: &str = "\n\n(sub-agent stopped: a tool result was flagged as a \
possible prompt injection — the above is its best-effort partial result.)";

const NO_TEXT_RESPONSE: &str = "(the sub-agent produced no text response)";

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
    /// construction time (before this tool itself is registered onto
    /// it, in `agent_builder.rs`, mirroring `delegate_task`'s own
    /// recursion-prevention structure -- see the module doc comment).
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

    // Offered during plan mode too, not hidden: see `delegate.rs`'s
    // identical override on `DelegateTaskTool` for the full rationale --
    // the specialist's own turn loop reads the same shared `PlanMode`
    // flag baked into `self.config.plan_mode`, so a specialist spawned
    // during plan mode automatically only sees read-only tools (further
    // narrowed by `compute_specialist_registry`'s attenuation on top) --
    // it degrades gracefully rather than needing this tool hidden
    // outright.
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
        arguments: serde_json::Value,
        ctx: &ToolExecutionContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: DelegateToSpecialistArgs = serde_json::from_value(arguments)
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;

        let Some(member) = self
            .config
            .team
            .members
            .iter()
            .find(|m| m.name == args.member)
        else {
            return Ok(ToolOutput::Error(format!(
                "unknown team member: {:?}",
                args.member
            )));
        };

        let specialist_registry = compute_specialist_registry(member, &self.config.parent_registry);
        let mut sub_executor = ToolExecutor::new(
            specialist_registry,
            Arc::clone(&self.config.gate),
            Arc::clone(&self.config.confiner),
        );
        if let Some(checkpointer) = &self.config.checkpointer {
            sub_executor.set_checkpointer(Arc::clone(checkpointer));
        }

        // See `delegate.rs`'s own `execute()` for the full rationale --
        // the specialist gets its own event channel, forwarded onto the
        // parent's real channel wrapped in `AgentEvent::SubAgentActivity`,
        // with `TextDelta` content accumulated separately into
        // `accumulated` since `AgentEvent` doesn't expose the specialist's
        // assembled response text directly.
        let (sub_tx, mut sub_rx) = mpsc::unbounded_channel();
        let accumulated = Arc::new(std::sync::Mutex::new(String::new()));
        let accumulated_for_task = Arc::clone(&accumulated);
        let parent_tx = self.config.events_tx.clone();
        let forward_task = tokio::spawn(async move {
            while let Some(event) = sub_rx.recv().await {
                if let AgentEvent::TextDelta(text) = &event {
                    accumulated_for_task.lock().unwrap().push_str(text);
                }
                let _ = parent_tx.send(AgentEvent::SubAgentActivity(Box::new(event)));
            }
        });

        let mut specialist = Agent::new(
            Arc::clone(&self.config.llm),
            sub_executor,
            member.persona.clone(),
            AgentConfig {
                // Deliberately 1, not `self.config.max_iterations` -- see
                // `delegate.rs`'s identical `max_tool_iterations: 1`
                // comment for why: the outer loop below bounds the
                // specialist's total LLM round-trip budget instead.
                max_tool_iterations: 1,
                context_tokens: self.config.context_tokens,
                edit_format: self.config.edit_format,
            },
            Arc::default(),
            self.config.plan_mode.clone(),
            self.config.autonomous_mode.clone(),
            sub_tx,
        );
        if let Some((map, budget)) = &self.config.repo_map {
            specialist.set_repo_map(Arc::clone(map), *budget);
        }
        if let Some((command, max_retries)) = &self.config.verification {
            specialist.set_verification(command.clone(), *max_retries);
        }
        // Must be the parent's own *shared* instance -- see
        // `DelegateTaskConfig::injection_taint`'s doc comment in
        // `delegate.rs` for why a fresh, disconnected instance here would
        // let a specialist's own ingested content poison autonomous mode
        // with the guard never seeing it.
        specialist.set_injection_taint(self.config.injection_taint.clone());
        specialist.set_broker_mode(self.config.broker_mode);

        let max_iterations = self.config.max_iterations.max(1);
        let mut result = specialist
            .run_turn(args.task, &ctx.cwd, ctx.cancellation.clone())
            .await;
        let mut iterations_used = 1u32;
        let is_injection_tainted = || {
            self.config.autonomous_mode.active() && self.config.injection_taint.current().is_some()
        };
        while result.is_ok()
            && specialist.last_turn_paused()
            && iterations_used < max_iterations
            && !ctx.cancellation.is_cancelled()
            && !is_injection_tainted()
        {
            iterations_used += 1;
            result = specialist
                .run_turn("continue".to_string(), &ctx.cwd, ctx.cancellation.clone())
                .await;
        }
        let paused = result.is_ok() && specialist.last_turn_paused();
        let cap_hit = paused && !is_injection_tainted();
        let injection_hit = paused && is_injection_tainted();

        // Dropping the specialist drops its `sub_tx` (the only remaining
        // sender), closing the channel so `forward_task`'s `recv()` loop
        // ends and it can be awaited to completion.
        drop(specialist);
        let _ = forward_task.await;

        match result {
            Err(err) => Ok(ToolOutput::Error(format!("sub-agent failed: {err}"))),
            Ok(()) => {
                let mut text = Arc::try_unwrap(accumulated)
                    .map(|m| m.into_inner().unwrap())
                    .unwrap_or_default();
                if injection_hit {
                    text.push_str(INJECTION_CUTOFF_NOTICE);
                } else if cap_hit {
                    text.push_str(CUTOFF_NOTICE);
                }
                if text.trim().is_empty() {
                    text = NO_TEXT_RESPONSE.to_string();
                }
                Ok(ToolOutput::Ok(text))
            }
        }
    }
}

/// Shared lightweight test fixtures for building a synthetic, non-empty
/// `ToolRegistry` -- used by both `registry_attenuation_tests` (which
/// exercises `compute_specialist_registry` in isolation) and
/// `delegation_tests` (which needs a *non-empty* `parent_registry` to
/// prove attenuation reaches all the way through `execute()`). Shared
/// here rather than duplicated so the two modules can't drift apart on
/// what a "named tool" fixture even means.
#[cfg(test)]
mod test_support {
    use super::*;

    // A minimal Tool impl for building a test registry -- named tools
    // with no real behavior, matching this crate's existing test
    // conventions of lightweight stand-ins rather than the real
    // aivyx-tools types (which this crate cannot depend on -- see the
    // parent plan's Global Constraints on dependency direction).
    pub(super) struct NamedTool(pub(super) &'static str);
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

    pub(super) fn registry_with(names: &[&'static str]) -> ToolRegistry {
        let mut registry = ToolRegistry::new();
        for name in names {
            registry.register(Arc::new(NamedTool(name)));
        }
        registry
    }
}

#[cfg(test)]
mod delegation_tests {
    use super::test_support::registry_with;
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
            let events = self
                .responses
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_default();
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
        let llm = Arc::new(MockBackend::new(vec![text_response(
            "done: the fix is applied",
        )]));
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
        // An unknown member must short-circuit before any specialist
        // `Agent` is ever constructed -- no request should have reached
        // the backend at all.
        assert!(
            llm.received.lock().unwrap().is_empty(),
            "no backend request should have been sent for an unknown member"
        );
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
        // ChatRequest's Debug output includes its `messages: Vec<Message>`
        // field (verified directly against aivyx-llm's real ChatRequest/
        // Message definitions), and the specialist's system prompt is
        // carried as a Message in that vec -- so the persona text shows
        // up in the request's Debug output.
        assert!(
            format!("{first_request:?}").contains("You are the implementer specialist"),
            "expected the member's persona in the outgoing request, got: {first_request:?}"
        );
    }

    /// The end-to-end proof that `execute()` actually wires
    /// `compute_specialist_registry`'s attenuated output into the
    /// specialist it constructs, rather than (say) handing the specialist
    /// `self.config.parent_registry.clone()` unattenuated. Task 1's own
    /// `registry_attenuation_tests` prove `compute_specialist_registry` is
    /// correct in isolation, and the other tests in this module prove
    /// `execute()` works end-to-end -- but every one of those uses an
    /// *empty* `parent_registry`, so none of them can distinguish "the
    /// attenuated registry was used" from "any registry, attenuated or
    /// not, was used" -- both look identical against an empty registry.
    /// This test uses a non-empty `parent_registry` and a member whose
    /// `tool_allowlist` is a strict subset of it, then inspects the real
    /// `ChatRequest` the specialist's backend call received: if `execute`
    /// were ever changed to bypass attenuation (e.g. cloning
    /// `parent_registry` directly instead of calling
    /// `compute_specialist_registry`), this is the test that would catch
    /// it -- the request's `tools` would carry all four parent tool names
    /// instead of just the member's two.
    #[tokio::test]
    async fn attenuation_reaches_the_specialists_actual_chat_request() {
        let llm = Arc::new(MockBackend::new(vec![text_response("ok")]));
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let team = TeamConfig {
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
                    tool_allowlist: vec!["read_file".to_string(), "grep".to_string()],
                    extra_deny_paths: vec![],
                },
            ],
        };
        let mut config = base_config(Arc::clone(&llm) as Arc<dyn LlmBackend>, tx, team);
        config.parent_registry = registry_with(&["read_file", "write_file", "grep", "run_command"]);
        let tool = DelegateToSpecialistTool::new(config);
        let ctx = exec_ctx(std::path::Path::new("."));
        let args = serde_json::json!({ "member": "implementer", "task": "read something" });
        let _ = tool.execute(args, &ctx).await.unwrap();

        let received = llm.received.lock().unwrap();
        let first_request = received.first().expect("one request should have been sent");
        let mut tool_names: Vec<&str> = first_request
            .tools
            .iter()
            .map(|t| t.name.as_str())
            .collect();
        tool_names.sort();
        assert_eq!(
            tool_names,
            vec!["grep", "read_file"],
            "expected exactly the member's allowed tools ([\"grep\", \"read_file\"]) in the \
             outgoing request's `tools` field -- not the full parent registry and not empty; \
             got: {tool_names:?}"
        );
    }
}

#[cfg(test)]
mod registry_attenuation_tests {
    use super::test_support::registry_with;
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
        let names: Vec<String> = attenuated
            .definitions()
            .into_iter()
            .map(|d| d.name)
            .collect();
        assert_eq!(names, vec!["read_file".to_string()]);
    }
}
