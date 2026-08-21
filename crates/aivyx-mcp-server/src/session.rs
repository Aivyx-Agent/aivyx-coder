//! Fresh, isolated, tier-filtered `Agent` construction for one MCP
//! session -- structurally `delegate_task`'s own shape
//! (`aivyx-core/src/delegate.rs`), invoked externally instead of mid-turn.

use std::path::Path;
use std::sync::Arc;

use aivyx_core::{Agent, AgentConfig, AgentEvent, EditFormat};
use aivyx_sandbox::{
    AutonomousMode, ConfirmationGate, PermissionGate, PermissionPrompter, PermissionRequest,
    PlanMode, UserResponse,
};
use aivyx_tools::{GitCheckpointer, ToolExecutor, ToolRegistry};
use async_trait::async_trait;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::tiers::AccessLevel;

/// Auto-resolves any request from a tool already in this session's own
/// tier-filtered registry -- belt-and-braces with the registry exclusion
/// itself (Task 2's `mcp_registry` + `AccessLevel::excluded_tool_names`):
/// even if the model somehow invents a call to an excluded tool's name,
/// `ToolExecutor::dispatch` fails with "unknown tool" before this prompter
/// is ever reached (the tool isn't in the registry at all), and for any
/// call that IS reachable, `allowed_names` is checked again here as a
/// second, independent guard against exactly that scenario.
pub(crate) struct TieredPrompter {
    allowed_names: std::collections::HashSet<String>,
}

impl TieredPrompter {
    pub(crate) fn new(allowed_names: impl IntoIterator<Item = String>) -> Self {
        Self {
            allowed_names: allowed_names.into_iter().collect(),
        }
    }
}

#[async_trait]
impl PermissionPrompter for TieredPrompter {
    async fn prompt(&self, request: &PermissionRequest) -> UserResponse {
        if self.allowed_names.contains(&request.tool_name) {
            UserResponse::Allow
        } else {
            UserResponse::Deny
        }
    }
}

/// Everything `build_session_agent` needs, captured once at server startup
/// from `BuiltAgent`'s new fields (Task 2) -- mirrors
/// `DelegateTaskConfig`'s own "gathered once, stable for the server's
/// whole lifetime" shape.
pub struct SessionConfig {
    pub llm: Arc<dyn aivyx_llm::LlmBackend>,
    pub confiner: Arc<dyn aivyx_sandbox::ExecutionConfiner>,
    pub checkpointer: Option<Arc<GitCheckpointer>>,
    pub repo_map: Option<(Arc<aivyx_repomap::RepoMap>, u32)>,
    /// `BuiltAgent::kv_cache_handles` -- the same shared pool/store every
    /// other `Agent` in this process uses, so every MCP session's own
    /// checkout draws from one `total_slots`-sized pool rather than each
    /// session getting its own (see `agent_builder.rs`'s own doc comment).
    pub kv_cache_handles: Option<(
        Arc<aivyx_llm::KvSlotPool>,
        Arc<aivyx_kvcache::LlamaServerSlotStore>,
        String,
    )>,
    /// Task 2's `mcp_registry` -- the delegate-shaped base set every
    /// session's own tier-filtered registry is cloned and excluded from.
    pub base_registry: ToolRegistry,
    pub deny_paths: Vec<std::path::PathBuf>,
    pub cwd: std::path::PathBuf,
    pub context_tokens: u32,
    pub edit_format: EditFormat,
}

const SESSION_SYSTEM_PROMPT: &str = "You are aivyx-coder, delegated a bounded coding task by \
another agent over MCP. Work the task to completion within your granted access level, then give \
a clear, complete final answer describing what you found or did -- this is the only part of your \
work the caller sees directly.";

/// Filters `base` down to exactly `level`'s tier -- the one line that
/// actually enforces tier boundaries. Extracted so it's independently
/// testable against a realistic registry (see the module tests), not
/// just provable-in-isolation via `AccessLevel::excluded_tool_names`'s
/// own tests in `tiers.rs`.
fn tier_registry(base: &ToolRegistry, level: AccessLevel) -> ToolRegistry {
    let mut registry = base.clone();
    registry.exclude(&level.excluded_tool_names());
    registry
}

/// Builds a fresh, isolated `Agent` for one MCP session at `level` --
/// no turn run yet. A fresh `PlanMode`/`AutonomousMode`/`ConfirmationGate`
/// per session (never shared, never the outer `BuiltAgent`'s own) since
/// each session's tier is independent and AutonomousMode is never reused
/// per this project's own Global Constraints.
///
/// `plan_mode`/`autonomous_mode` are each bound once and `.clone()`d into
/// their two consumers (`ConfirmationGate::new` and `Agent::new`) so both
/// hold the *same* shared handle -- mirrors `agent_builder.rs`'s own
/// `plan_mode.clone()` passed to both. A second, independent `PlanMode`
/// instance passed to `Agent::new` would silently desync from the one
/// baked into `ConfirmationGate`, since `run_turn`'s own tool-definition
/// choice reads `self.plan_mode` (the `Agent::new` parameter), not
/// `ConfirmationGate`'s copy.
pub async fn build_session_agent(
    config: &SessionConfig,
    level: AccessLevel,
    events_tx: mpsc::UnboundedSender<AgentEvent>,
) -> Agent {
    let registry = tier_registry(&config.base_registry, level);

    let allowed_names: Vec<String> = registry.definitions().into_iter().map(|d| d.name).collect();
    let prompter: Arc<dyn PermissionPrompter> = Arc::new(TieredPrompter::new(allowed_names));

    let plan_mode = PlanMode::new();
    // "plan" tier reuses the existing plan-mode mechanism verbatim (zero
    // new filtering logic for this tier): run_turn's own
    // `if plan_mode.active() { plan_definitions() } else { definitions() }`
    // branch does the work, and ConfirmationGate's plan-mode-deny branch
    // is the backstop if the model invents a mutating call anyway.
    plan_mode.set_active(level == AccessLevel::Plan);
    let autonomous_mode = AutonomousMode::new(); // always inactive -- never reused, see Global Constraints

    let gate: Arc<dyn PermissionGate> = Arc::new(ConfirmationGate::new(
        prompter,
        config.deny_paths.clone(),
        Vec::new(), // no pre-approved commands -- everything in-tier already auto-allows via TieredPrompter
        plan_mode.clone(),
        autonomous_mode.clone(),
        config.cwd.clone(),
        false, // no editor-approval integration for this frontend
    ));

    let mut executor = ToolExecutor::new(registry, Arc::clone(&gate), Arc::clone(&config.confiner));
    if let Some(checkpointer) = &config.checkpointer {
        executor.set_checkpointer(Arc::clone(checkpointer));
    }

    let mut agent = Agent::new(
        Arc::clone(&config.llm),
        executor,
        SESSION_SYSTEM_PROMPT,
        AgentConfig {
            // Fixed at 1, exactly like delegate_task's own inner agent --
            // run_bounded_turn's outer loop (below) is what bounds the
            // total round-trip budget via max_iterations, not this field.
            max_tool_iterations: 1,
            context_tokens: config.context_tokens,
            edit_format: config.edit_format,
        },
        Arc::default(), // fresh, empty task list -- this session's own, never the outer BuiltAgent's
        plan_mode,
        autonomous_mode,
        events_tx,
    );
    if let Some((map, budget)) = &config.repo_map {
        agent.set_repo_map(Arc::clone(map), *budget);
    }
    if let Some((pool, store, build_hash)) = &config.kv_cache_handles {
        agent.set_kv_cache(
            Arc::clone(pool),
            Arc::clone(store),
            "llama-server".to_string(),
            config.llm.model_id().to_string(),
            build_hash.clone(),
        );
    }
    agent
}

/// Runs `input` to completion against `agent`, bounded by `max_iterations`
/// outer round-trips -- the exact same shape as `delegate_task`'s own
/// outer "continue" loop (`aivyx-core/src/delegate.rs`), reused here since
/// `AgentConfig.max_tool_iterations` is fixed at 1 per `build_session_agent`
/// above. Drains `agent`'s own event channel concurrently with each round
/// trip (mirrors `aivyx-acp/src/session.rs`'s own `tokio::select!` pattern)
/// to accumulate the final text answer, appending a cutoff notice if the
/// budget runs out before a natural finish -- returns `Ok` even then,
/// since the session may have done real, useful, incomplete work.
pub async fn run_bounded_turn(
    agent: &mut Agent,
    events_rx: &mut mpsc::UnboundedReceiver<AgentEvent>,
    input: String,
    cwd: &Path,
    max_iterations: u32,
    cancellation: CancellationToken,
) -> (Result<(), aivyx_core::AgentError>, String) {
    let mut accumulated = String::new();
    let max_iterations = max_iterations.max(1);

    let mut next_input = Some(input);
    let mut result = Ok(());
    let mut iterations_used = 0u32;
    while let Some(turn_input) = next_input.take() {
        iterations_used += 1;
        result = {
            let run = agent.run_turn(turn_input, cwd, cancellation.clone());
            tokio::pin!(run);
            loop {
                tokio::select! {
                    r = &mut run => break r,
                    Some(event) = events_rx.recv() => {
                        if let AgentEvent::TextDelta(text) = &event {
                            accumulated.push_str(text);
                        }
                    }
                }
            }
        };
        // Drain anything buffered right at this round trip's completion
        // (e.g. a final TextDelta that arrived after `run` resolved but
        // before select! polled the channel again) before deciding
        // whether to continue.
        while let Ok(event) = events_rx.try_recv() {
            if let AgentEvent::TextDelta(text) = &event {
                accumulated.push_str(text);
            }
        }
        if result.is_ok()
            && agent.last_turn_paused()
            && iterations_used < max_iterations
            && !cancellation.is_cancelled()
        {
            next_input = Some("continue".to_string());
        }
    }
    let cap_hit = result.is_ok() && agent.last_turn_paused();

    if cap_hit {
        accumulated.push_str(
            "\n\n(session stopped: reached its iteration budget before finishing -- the above is its best-effort partial result.)",
        );
    }
    (result, accumulated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aivyx_llm::{ChatRequest, FinishReason, LlmBackend, LlmError, StreamEvent};
    use aivyx_sandbox::NoopConfiner;
    use aivyx_tools::{ReadFileTool, RunCommandTool, RunShellTool, WriteFileTool};
    use async_trait::async_trait;
    use futures::stream::BoxStream;
    use futures::StreamExt;
    use std::sync::Mutex;

    struct MockBackend {
        responses: Mutex<std::collections::VecDeque<Vec<StreamEvent>>>,
    }
    impl MockBackend {
        fn says(text: &str) -> Arc<Self> {
            Arc::new(Self {
                responses: Mutex::new(std::collections::VecDeque::from(vec![vec![
                    StreamEvent::TextDelta(text.to_string()),
                    // NOTE: the task-3 brief's draft had an extra
                    // `usage: None` field here that doesn't exist on the
                    // real `StreamEvent::Done` (checked against
                    // `aivyx-llm/src/backend.rs`, and consistent with
                    // `aivyx-core/src/delegate.rs`'s own precedent test
                    // and this same file's `LoopingBackend` test below) --
                    // dropped to match the real type.
                    StreamEvent::Done { finish_reason: FinishReason::Stop },
                ]])),
            })
        }
    }
    #[async_trait]
    impl LlmBackend for MockBackend {
        fn model_id(&self) -> &str {
            "mock"
        }
        async fn stream_chat(
            &self,
            _request: ChatRequest,
        ) -> Result<BoxStream<'static, Result<StreamEvent, LlmError>>, LlmError> {
            let events = self.responses.lock().unwrap().pop_front().unwrap_or_default();
            Ok(futures::stream::iter(events.into_iter().map(Ok)).boxed())
        }
    }

    fn config(base_registry: ToolRegistry) -> SessionConfig {
        SessionConfig {
            llm: MockBackend::says("done"),
            confiner: Arc::new(NoopConfiner),
            checkpointer: None,
            repo_map: None,
            kv_cache_handles: None,
            base_registry,
            deny_paths: Vec::new(),
            cwd: std::env::temp_dir(),
            context_tokens: 8192,
            edit_format: EditFormat::Native,
        }
    }

    fn full_registry() -> ToolRegistry {
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(ReadFileTool));
        registry.register(Arc::new(WriteFileTool));
        registry.register(Arc::new(RunCommandTool::new(Vec::new())));
        registry
    }

    #[test]
    fn tier_registry_actually_filters_a_realistic_base_registry() {
        let mut base = ToolRegistry::new();
        base.register(Arc::new(ReadFileTool));
        base.register(Arc::new(WriteFileTool));
        base.register(Arc::new(RunShellTool));

        let names = |level: AccessLevel| -> Vec<String> {
            tier_registry(&base, level).definitions().into_iter().map(|d| d.name).collect()
        };

        let plan_names = names(AccessLevel::Plan);
        assert!(plan_names.contains(&"read_file".to_string()), "plan must include read_file");
        assert!(!plan_names.contains(&"write_file".to_string()), "plan must exclude write_file");
        assert!(!plan_names.contains(&"run_shell".to_string()), "plan must exclude run_shell");

        let edit_names = names(AccessLevel::Edit);
        assert!(edit_names.contains(&"write_file".to_string()), "edit must include write_file");
        assert!(!edit_names.contains(&"run_shell".to_string()), "edit must exclude run_shell");

        let execute_names = names(AccessLevel::Execute);
        assert!(execute_names.contains(&"write_file".to_string()), "execute must include write_file");
        assert!(execute_names.contains(&"run_shell".to_string()), "execute must include run_shell");
    }

    #[tokio::test]
    async fn run_bounded_turn_returns_accumulated_text_on_a_single_round_trip() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut agent = build_session_agent(&config(full_registry()), AccessLevel::Plan, tx).await;
        let (result, text) = run_bounded_turn(
            &mut agent,
            &mut rx,
            "say hello".to_string(),
            &std::env::temp_dir(),
            10,
            CancellationToken::new(),
        )
        .await;
        assert!(result.is_ok());
        assert_eq!(text, "done");
    }

    #[test]
    fn tiered_prompter_allows_only_names_it_was_given() {
        // Direct unit proof of the belt-and-braces defense described in
        // TieredPrompter's own doc comment -- independent of whether
        // ToolRegistry::exclude is ever miswired upstream.
        let prompter = TieredPrompter::new(vec!["read_file".to_string()]);
        let allowed = PermissionRequest {
            tool_name: "read_file".to_string(),
            action: aivyx_sandbox::ActionKind::Read,
            target: aivyx_sandbox::PermissionTarget::Other("x".to_string()),
            arguments_preview: serde_json::json!({}),
            preview: None,
            diff: None,
        };
        let denied = PermissionRequest { tool_name: "run_command".to_string(), ..allowed.clone() };
        // PermissionRequest has no #[derive(Clone)] guarantee beyond what
        // aivyx-sandbox already declares (Debug, Clone -- confirmed in
        // lib.rs) so `..allowed.clone()` above is valid; if a future
        // change to PermissionRequest drops Clone, construct `denied`
        // with its own full literal instead.
        let allow = futures::executor::block_on(prompter.prompt(&allowed));
        let deny = futures::executor::block_on(prompter.prompt(&denied));
        assert_eq!(allow, UserResponse::Allow);
        assert_eq!(deny, UserResponse::Deny);
    }

    #[tokio::test]
    async fn run_bounded_turn_appends_a_cutoff_notice_on_budget_exhaustion() {
        // Mirrors aivyx-core/src/delegate.rs's own
        // cap_exhaustion_returns_ok_with_a_cutoff_notice_not_an_error test
        // exactly: every response is a tool call with no final answer, so
        // the agent pauses every single iteration and never finishes
        // naturally -- proving run_bounded_turn's outer loop actually
        // stops at max_iterations and appends the cutoff notice, the same
        // behavior delegate_task's own outer loop has.
        use aivyx_llm::FinishReason;
        use aivyx_types::{ToolCall, ToolCallId, ToolCallSource};

        struct LoopingBackend;
        #[async_trait]
        impl aivyx_llm::LlmBackend for LoopingBackend {
            fn model_id(&self) -> &str {
                "looping"
            }
            async fn stream_chat(
                &self,
                _request: aivyx_llm::ChatRequest,
            ) -> Result<futures::stream::BoxStream<'static, Result<StreamEvent, aivyx_llm::LlmError>>, aivyx_llm::LlmError>
            {
                Ok(futures::stream::iter([
                    Ok(StreamEvent::ToolCallComplete(ToolCall {
                        id: ToolCallId("c".to_string()),
                        name: "nonexistent_tool".to_string(),
                        arguments: serde_json::json!({}),
                        source: ToolCallSource::Native,
                    })),
                    Ok(StreamEvent::Done { finish_reason: FinishReason::ToolCalls }),
                ])
                .boxed())
            }
        }

        let mut cfg = config(full_registry());
        cfg.llm = std::sync::Arc::new(LoopingBackend);
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut agent = build_session_agent(&cfg, AccessLevel::Plan, tx).await;
        let (result, text) = run_bounded_turn(
            &mut agent,
            &mut rx,
            "loop forever".to_string(),
            &std::env::temp_dir(),
            3,
            CancellationToken::new(),
        )
        .await;
        assert!(result.is_ok(), "budget exhaustion must return Ok, not an error");
        assert!(
            text.contains("reached its iteration budget"),
            "expected a cutoff notice, got: {text}"
        );
    }
}
