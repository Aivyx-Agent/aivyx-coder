//! Tool trait + execution pipeline.
//!
//! `ToolExecutor::dispatch` centralizes the permission check rather than
//! leaving it to individual `Tool::execute` impls, so a tool author can't
//! forget to gate a dangerous action. `read_file`/`write_file`/`edit_file`
//! (this pass) are the first concrete tools; shell-exec/grep/git are a
//! later pass.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use aivyx_sandbox::{ActionKind, ExecutionConfiner, PermissionGate, PermissionRequest, PermissionTarget};
use aivyx_types::{ToolCall, ToolDefinition, ToolOutput, ToolResult};
use async_trait::async_trait;
use thiserror::Error;
use tokio_util::sync::CancellationToken;

mod diff;
mod lsp;
mod mcp;
mod memory_topic;
mod path_resolve;
mod process;
mod pty;
mod tools;
pub mod web;
pub mod wiki;

pub use aivyx_checkpoint::GitCheckpointer;
pub use lsp::LspClient;
pub use mcp::{McpClient, ToolInfo};
pub use process::{CommandSpec, run};
pub use tools::{
    CoderTextCompleter, DeleteFileTool, EditFileTool, FindReferencesTool, GenerateSvgTool,
    GetMcpPromptTool, GitBranchTool, GitCommitTool, GitPrTool, GitPushTool, GitReadTool, GlobTool,
    GoToDefinitionTool, GrepTool, ListMcpPromptsTool, ListMcpResourcesTool, McpToolAdapter,
    MemoryForgetTool, MemoryReadTool, MemoryWriteTool, MoveFileTool, PatchFileTool, ReadFileTool,
    ReadMcpResourceTool, RememberPreferenceTool, ReplResizeTarget, ReplSendTool, ReplStartTool,
    ReplStopTool, RunCommandTool, RunShellTool, SetTasksTool, SharedReplSession, WebFetchTool,
    WebSearchTool, WriteFileTool, new_shared_repl_session,
};

#[derive(Debug, Error)]
pub enum ToolError {
    #[error("invalid arguments for tool: {0}")]
    InvalidArguments(String),
    #[error("tool execution failed: {0}")]
    ExecutionFailed(String),
    #[error("no tool registered with name: {0}")]
    NotFound(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

pub struct ToolExecutionContext {
    pub cwd: PathBuf,
    pub confiner: Arc<dyn ExecutionConfiner>,
    pub cancellation: CancellationToken,
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn definition(&self) -> ToolDefinition;

    /// Whether this tool can mutate anything outside the agent's own
    /// session state (filesystem, processes, network). Decides whether the
    /// tool is offered to the model at all while plan mode is active — the
    /// static, argument-free counterpart of `permission_request`'s
    /// `ActionKind`. Defaults to `true` (fail-closed): a new tool stays
    /// hidden in plan mode unless it explicitly declares itself safe.
    ///
    /// Does NOT by itself decide whether a call is checkpointed — see
    /// `needs_checkpoint` below for that, separate, question.
    fn mutates_outside_session(&self) -> bool {
        true
    }

    /// Whether a call to this tool should be preceded by a git checkpoint.
    /// Defaults to `mutates_outside_session()`, which is correct for every
    /// tool that actually touches the filesystem — a checkpoint exists so a
    /// mutating call can be rolled back. `web_fetch`/`web_search` are the
    /// deliberate exception: they must stay `mutates_outside_session() ==
    /// true` (network is not session-local, so plan mode must still hide
    /// them and the gate must still treat them as `ActionKind::Network`),
    /// but a network read cannot mutate the worktree, so checkpointing one
    /// only wastes a `git add -A` + `write-tree` and — because
    /// `GitCheckpointer` dedups by tree hash — can cause a network call
    /// that runs before a real edit in the same batch to be the one that
    /// "mints" the checkpoint, misattributing it as an edit in the
    /// batch-rollback notice (`agent/mod.rs`'s `batch_touched_paths`) while
    /// the real edit goes unlisted. Override this (not
    /// `mutates_outside_session`) to `false` on a tool that is gated as
    /// mutating for plan-mode purposes but never actually changes the
    /// worktree.
    fn needs_checkpoint(&self) -> bool {
        self.mutates_outside_session()
    }

    /// Inspect (already schema-validated) arguments and describe the
    /// permission needed. May do a bounded read (e.g. to build a diff
    /// preview) but must not perform the actual mutating side effect.
    fn permission_request(
        &self,
        arguments: &serde_json::Value,
        cwd: &Path,
    ) -> Result<PermissionRequest, ToolError>;

    /// The argument-parsing-and-path-resolution slice of `permission_request`
    /// — just enough to compute the `(ActionKind, PermissionTarget)` pair
    /// that determines an Always-Allow cache key, with none of the
    /// additional fallible, *current-filesystem-state*-dependent checks
    /// (file exists, diff/preview content, "destination doesn't exist yet")
    /// that the full `permission_request` layers on top.
    ///
    /// This distinction matters because some tools' `permission_request`
    /// is not idempotent with respect to their own prior execution: calling
    /// it again *after* the tool already ran can fail purely because the
    /// tool's own effect changed the filesystem state being inspected (e.g.
    /// `edit_file` re-parsing `old_string` against a file that no longer
    /// contains it, `delete_file` statting a path it just removed,
    /// `move_file` statting a `from` that no longer exists). Such a tool
    /// overrides this method with just the pure parse-and-resolve step so
    /// `ToolExecutor::reconstruct_permission_request` — which only ever
    /// needs the key, never the preview — can still recompute a historical
    /// call's cache key after the call has already run.
    ///
    /// Defaults to delegating to `permission_request` and discarding
    /// everything but `action`/`target`, which is correct for any tool
    /// whose full implementation has no such execution-state dependency
    /// (e.g. `write_file`, `run_command`) — override only when it does.
    fn permission_target(
        &self,
        arguments: &serde_json::Value,
        cwd: &Path,
    ) -> Result<(ActionKind, PermissionTarget), ToolError> {
        self.permission_request(arguments, cwd)
            .map(|request| (request.action, request.target))
    }

    /// Only ever invoked by `ToolExecutor` after `PermissionGate::check`
    /// returns `Allow`/`AllowAlways`.
    async fn execute(
        &self,
        arguments: serde_json::Value,
        ctx: &ToolExecutionContext,
    ) -> Result<ToolOutput, ToolError>;
}

#[derive(Default, Clone)]
pub struct ToolRegistry {
    tools: Vec<Arc<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        self.tools.push(tool);
    }

    pub fn get(&self, name: &str) -> Option<&Arc<dyn Tool>> {
        self.tools.iter().find(|tool| tool.name() == name)
    }

    /// Fed into `ChatRequest.tools` so the model knows what it can call.
    pub fn definitions(&self) -> Vec<ToolDefinition> {
        self.tools.iter().map(|tool| tool.definition()).collect()
    }

    /// The subset of `definitions` offered while plan mode is active: only
    /// tools that cannot mutate anything outside the session. Withholding
    /// the rest (instead of offering them and denying at the gate) matters
    /// for small local models, which retry-loop on unavailable actions.
    pub fn plan_definitions(&self) -> Vec<ToolDefinition> {
        self.tools
            .iter()
            .filter(|tool| !tool.mutates_outside_session())
            .map(|tool| tool.definition())
            .collect()
    }

    /// Removes every registered tool whose name matches an entry in
    /// `names`, in place. Absent names are silently ignored — a caller
    /// excluding a tool that was never registered (e.g. because a
    /// feature flag left it out) is not an error condition.
    pub fn exclude(&mut self, names: &[&str]) {
        self.tools.retain(|tool| !names.contains(&tool.name()));
    }
}

pub struct ToolExecutor {
    registry: ToolRegistry,
    gate: Arc<dyn PermissionGate>,
    confiner: Arc<dyn ExecutionConfiner>,
    /// When set, the worktree is snapshotted before every mutating tool
    /// call (see `checkpoint.rs`). `None` = disabled (config, or not a
    /// git repository).
    checkpointer: Option<Arc<GitCheckpointer>>,
}

impl ToolExecutor {
    pub fn new(
        registry: ToolRegistry,
        gate: Arc<dyn PermissionGate>,
        confiner: Arc<dyn ExecutionConfiner>,
    ) -> Self {
        Self {
            registry,
            gate,
            confiner,
            checkpointer: None,
        }
    }

    pub fn set_checkpointer(&mut self, checkpointer: Arc<GitCheckpointer>) {
        self.checkpointer = Some(checkpointer);
    }

    /// The most recent checkpoint ref, or `None` if no checkpointer is
    /// configured (checkpointing disabled, or `cwd` isn't a git repo) or
    /// none has been taken yet. `Agent` (Phase 11c's autonomous discard
    /// path) uses this to remember "state right before the first unverified
    /// edit" without needing to know `GitCheckpointer` exists.
    pub async fn latest_checkpoint_ref(&self, cancellation: &CancellationToken) -> Option<String> {
        self.checkpointer.as_ref()?.latest_ref(cancellation).await
    }

    /// Restores the worktree to `ref_name` — see
    /// `GitCheckpointer::restore_to` for exactly what that means. `Err` if
    /// no checkpointer is configured (nothing to restore from) or the
    /// underlying git operation fails.
    pub async fn restore_to_checkpoint(
        &self,
        ref_name: &str,
        cancellation: &CancellationToken,
    ) -> Result<(), String> {
        let checkpointer = self
            .checkpointer
            .as_ref()
            .ok_or("no checkpointer is configured")?;
        checkpointer.restore_to(ref_name, cancellation).await
    }

    pub fn definitions(&self) -> Vec<ToolDefinition> {
        self.registry.definitions()
    }

    /// See `ToolRegistry::plan_definitions`.
    pub fn plan_definitions(&self) -> Vec<ToolDefinition> {
        self.registry.plan_definitions()
    }

    /// Rebuilds the `(action, target)` pair a historical `ToolCall` (e.g.
    /// one about to be dropped from history by compaction) would have
    /// produced, by replaying it through `tool.permission_target` — the
    /// same pure parse-and-resolve step `dispatch_inner`'s own call to
    /// `tool.permission_request` runs internally before layering any
    /// execution-state-dependent checks on top. This is what makes the
    /// resulting `PermissionKey` provably identical to whatever key was
    /// cached for the original call: it isn't a hand-rolled reconstruction
    /// that merely mirrors that logic, it's a second call to the same
    /// underlying derivation with the same recorded arguments and the same
    /// `cwd`. Deliberately calls `permission_target`, not
    /// `permission_request`, directly: several tools' full
    /// `permission_request` (`edit_file`, `delete_file`, `move_file`,
    /// `patch_file`) re-derives its result from *current* filesystem state
    /// that the tool's own prior execution already changed (e.g. `edit_file`
    /// re-matching `old_string` against a file that no longer contains it),
    /// so calling `permission_request` here would spuriously fail for
    /// exactly the calls this function most needs to succeed for — see Task
    /// 8 review Finding 1 (security audit, 2026-09-16).
    ///
    /// Builds a minimal `PermissionRequest` around that pair — `preview`/
    /// `diff` are `None` and `arguments_preview` is the raw call arguments,
    /// since eviction only ever needs `action`/`target` (what
    /// `PermissionKey::from_request` reads) and never renders anything to a
    /// human.
    ///
    /// `None` if the tool is no longer registered, or its arguments no
    /// longer parse against it (e.g. `run_command` naming an
    /// `allowed_commands` entry removed from config since the call was
    /// made) — both rare. The caller should log when this happens: unlike
    /// the case this used to (incorrectly) claim, there generally *is*
    /// something worth evicting in this situation, since a cached
    /// Always-Allow entry doesn't disappear just because reconstruction
    /// failed to name it.
    pub fn reconstruct_permission_request(
        &self,
        call: &ToolCall,
        cwd: &Path,
    ) -> Option<PermissionRequest> {
        let tool = self.registry.get(&call.name)?;
        let (action, target) = tool.permission_target(&call.arguments, cwd).ok()?;
        Some(PermissionRequest {
            tool_name: call.name.clone(),
            action,
            target,
            arguments_preview: call.arguments.clone(),
            preview: None,
            diff: None,
        })
    }

    /// Evicts a cached Always-Allow decision matching `request`, if any —
    /// see `PermissionGate::forget_always_allow`.
    pub fn forget_permission(&self, request: &PermissionRequest) {
        self.gate.forget_always_allow(request);
    }

    pub async fn dispatch(
        &self,
        call: ToolCall,
        cwd: &std::path::Path,
        cancellation: CancellationToken,
    ) -> ToolResult {
        let output = self.dispatch_inner(&call, cwd, cancellation).await;
        let output = output.unwrap_or_else(|err| ToolOutput::Error(err.to_string()));
        ToolResult {
            call_id: call.id,
            output,
        }
    }

    async fn dispatch_inner(
        &self,
        call: &ToolCall,
        cwd: &std::path::Path,
        cancellation: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let tool = self
            .registry
            .get(&call.name)
            .ok_or_else(|| ToolError::NotFound(call.name.clone()))?;

        let permission_request = tool.permission_request(&call.arguments, cwd)?;

        use aivyx_sandbox::PermissionDecision;
        match self.gate.check(&permission_request).await {
            PermissionDecision::Allow | PermissionDecision::AllowAlways => {}
            PermissionDecision::Deny(reason) => {
                // The gate's reason (plan mode, deny_paths, user refusal)
                // gives the model something to adapt to; without one, fall
                // back to the generic message.
                return Ok(ToolOutput::Denied(reason.unwrap_or_else(|| {
                    format!("permission denied for tool `{}`", call.name)
                })));
            }
        }

        // Snapshot the worktree before anything that can actually mutate
        // it — after the gate (denied calls change nothing worth
        // checkpointing), before the effect. Best-effort: a failed
        // checkpoint logs and the call proceeds. Deliberately
        // `needs_checkpoint()`, not `mutates_outside_session()`: the latter
        // also drives plan-mode filtering and must stay `true` for
        // `web_fetch`/`web_search` (network is not session-local), but
        // those two never touch the worktree, so checkpointing them would
        // be pure waste and — worse — can cause a network call to
        // misattribute itself as the "edit" a later rollback undoes (see
        // `needs_checkpoint`'s doc comment on the `Tool` trait).
        if tool.needs_checkpoint()
            && let Some(checkpointer) = &self.checkpointer
        {
            checkpointer.checkpoint(&call.name, &cancellation).await;
        }

        let ctx = ToolExecutionContext {
            cwd: cwd.to_path_buf(),
            confiner: Arc::clone(&self.confiner),
            cancellation,
        };

        tool.execute(call.arguments.clone(), &ctx).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_definitions_offer_only_session_safe_tools() {
        // The full default tool set, registered in main.rs order: the plan
        // subset must be exactly the read/search tools plus set_tasks — a
        // newly added tool lands on the mutating (hidden) side unless it
        // explicitly opts out, so this test also catches an accidental
        // opt-out on something dangerous.
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(ReadFileTool));
        registry.register(Arc::new(WriteFileTool));
        registry.register(Arc::new(EditFileTool));
        registry.register(Arc::new(GrepTool::new(vec![])));
        registry.register(Arc::new(GlobTool::new(vec![])));
        registry.register(Arc::new(RunShellTool));
        registry.register(Arc::new(SetTasksTool::new(Arc::default())));
        registry.register(Arc::new(GitReadTool::new(vec![])));
        registry.register(Arc::new(GitCommitTool::new(vec![])));
        registry.register(Arc::new(ReplStartTool::new(
            new_shared_repl_session(),
            std::time::Duration::from_millis(1),
            std::time::Duration::from_millis(1),
            std::time::Duration::from_secs(1),
        )));
        registry.register(Arc::new(ReplSendTool::new(
            new_shared_repl_session(),
            std::time::Duration::from_millis(1),
            std::time::Duration::from_millis(1),
            None,
        )));
        registry.register(Arc::new(ReplStopTool::new(new_shared_repl_session())));
        // Task 1 (HIGH, 2026-09-16 audit) — web_fetch/web_search must land
        // on the mutating (hidden-in-plan-mode) side, same as every other
        // non-session-safe tool above; this is the regression guard the
        // task's own review asked for, since neither tool was registered
        // in this test before the fix.
        registry.register(Arc::new(WebFetchTool::new(5, false)));
        registry.register(Arc::new(WebSearchTool::new(None, 10, 5)));
        // Same regression-guard reasoning as web_fetch/web_search above,
        // for generate_svg (Aivyx-Vision adoption, 2026-09-18): a new
        // network-reaching tool must land on the mutating side here too,
        // not just be covered by its own unit test in isolation.
        struct NeverCalledCompleter;
        #[async_trait::async_trait]
        impl aivyx_vision_svg::TextCompleter for NeverCalledCompleter {
            async fn complete(
                &self,
                _prompt: &str,
            ) -> Result<String, aivyx_vision_svg::TextCompleterError> {
                unreachable!("this test only inspects tool definitions, never executes one")
            }
        }
        registry.register(Arc::new(GenerateSvgTool::new(Arc::new(NeverCalledCompleter))));

        let all: Vec<String> = registry.definitions().into_iter().map(|d| d.name).collect();
        let plan: Vec<String> = registry
            .plan_definitions()
            .into_iter()
            .map(|d| d.name)
            .collect();

        assert_eq!(all.len(), 15);
        assert_eq!(
            plan,
            vec![
                "read_file", "grep", "glob", "set_tasks", "git_read", "repl_send", "repl_stop"
            ],
            "web_fetch/web_search/generate_svg must NOT appear here -- network access is not session-safe"
        );
    }

    #[tokio::test]
    async fn write_approval_does_not_satisfy_a_forget_via_real_tool_output() {
        // aivyx-sandbox's own confirmation.rs has a regression test for this
        // exact finding (memory_write and memory_forget must not share an
        // Always-Allow cache entry on the same topic), but it drives the gate
        // with hand-built PermissionRequests, not the tools' own real
        // permission_request() output. This is the aivyx-tools-side half the
        // backlog asked for: both tools' REAL permission_request() calls,
        // through a REAL ConfirmationGate, closing the loop end-to-end so a
        // future change to either tool's target-string format can't silently
        // reopen the same hole without a test noticing.
        use aivyx_sandbox::{
            AutonomousMode, ConfirmationGate, PermissionDecision, PermissionPrompter, PlanMode,
            UserResponse,
        };
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct FakePrompter {
            response: UserResponse,
            calls: AtomicUsize,
        }
        #[async_trait]
        impl PermissionPrompter for FakePrompter {
            async fn prompt(&self, _request: &PermissionRequest) -> UserResponse {
                self.calls.fetch_add(1, Ordering::SeqCst);
                self.response
            }
        }

        let prompter = Arc::new(FakePrompter {
            response: UserResponse::AllowAlways,
            calls: AtomicUsize::new(0),
        });
        let gate = ConfirmationGate::new(
            prompter.clone(),
            vec![],
            vec![],
            PlanMode::new(),
            AutonomousMode::new(),
            PathBuf::from("/irrelevant"),
            false,
        );

        let write_tool = MemoryWriteTool::new(Arc::new(aivyx_recall::InMemoryRecall::new()));
        let write_request = write_tool
            .permission_request(
                &serde_json::json!({"topic": "global:editor", "body": "prefers tabs"}),
                Path::new("/irrelevant"),
            )
            .unwrap();
        let decision = gate.check(&write_request).await;
        assert_eq!(decision, PermissionDecision::AllowAlways);
        assert_eq!(prompter.calls.load(Ordering::SeqCst), 1);

        let forget_tool = MemoryForgetTool::new(Arc::new(aivyx_recall::InMemoryRecall::new()));
        let forget_request = forget_tool
            .permission_request(
                &serde_json::json!({"topic": "global:editor"}),
                Path::new("/irrelevant"),
            )
            .unwrap();
        let decision = gate.check(&forget_request).await;
        assert_eq!(decision, PermissionDecision::AllowAlways);
        // The approval for memory_write must NOT satisfy memory_forget on
        // the same topic -- the gate must prompt again.
        assert_eq!(prompter.calls.load(Ordering::SeqCst), 2);
    }

    /// Task 8 (security audit, 2026-09-16): the single most important
    /// correctness question in that task is whether
    /// `reconstruct_permission_request` really produces a `PermissionRequest`
    /// whose cache key is byte-for-byte identical to whatever `dispatch`
    /// itself used to cache the original approval — not merely "close" or
    /// "the same shape." This drives a REAL `ConfirmationGate` (not a test
    /// double) through both a `Command`-target tool (`run_command`) and a
    /// `Path`-target tool (`write_file`), and proves the equivalence
    /// directly: `forget_permission` fed the *reconstructed* request must
    /// actually evict the entry `dispatch` cached, forcing the gate to
    /// prompt again on a replayed identical call instead of silently
    /// reusing the stale approval.
    #[tokio::test]
    async fn reconstruct_permission_request_reproduces_the_exact_cache_key_dispatch_used() {
        use aivyx_sandbox::{
            AutonomousMode, ConfirmationGate, NoopConfiner, PermissionPrompter, PlanMode,
            UserResponse,
        };
        use aivyx_types::{ToolCall, ToolCallId, ToolCallSource};
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct CountingAlwaysAllowPrompter(AtomicUsize);
        #[async_trait]
        impl PermissionPrompter for CountingAlwaysAllowPrompter {
            async fn prompt(&self, _request: &PermissionRequest) -> UserResponse {
                self.0.fetch_add(1, Ordering::SeqCst);
                UserResponse::AllowAlways
            }
        }

        let cwd = std::env::temp_dir();

        // ---- Command target (run_command) ----
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(RunCommandTool::new(vec![CommandSpec {
            name: "deploy".to_string(),
            program: "true".to_string(),
            args: vec![],
            timeout: std::time::Duration::from_secs(5),
        }])));
        let prompter = Arc::new(CountingAlwaysAllowPrompter(AtomicUsize::new(0)));
        let gate: Arc<dyn PermissionGate> = Arc::new(ConfirmationGate::new(
            prompter.clone(),
            vec![],
            vec![],
            PlanMode::new(),
            AutonomousMode::new(),
            cwd.clone(),
            false,
        ));
        let executor = ToolExecutor::new(registry, gate, Arc::new(NoopConfiner));

        let call = ToolCall {
            id: ToolCallId("c1".to_string()),
            name: "run_command".to_string(),
            arguments: serde_json::json!({ "command": "deploy" }),
            source: ToolCallSource::Native,
        };
        // Cache a real AllowAlways approval through the real dispatch path.
        executor
            .dispatch(call.clone(), &cwd, CancellationToken::new())
            .await;
        assert_eq!(prompter.0.load(Ordering::SeqCst), 1);

        // Dispatching the identical call again must be a silent cache hit
        // (no second prompt) -- the baseline this test needs to disprove
        // once eviction runs below.
        executor
            .dispatch(call.clone(), &cwd, CancellationToken::new())
            .await;
        assert_eq!(
            prompter.0.load(Ordering::SeqCst),
            1,
            "sanity check: a repeat dispatch before eviction must be a cache hit"
        );

        // Reconstruct the request from the historical ToolCall alone (name
        // + arguments, exactly what a compacted history block retains) and
        // evict it.
        let reconstructed = executor
            .reconstruct_permission_request(&call, &cwd)
            .expect("run_command's permission_request must succeed on unchanged arguments");
        executor.forget_permission(&reconstructed);

        // If the reconstructed key were even slightly different from the
        // one `dispatch` actually cached, this would still be a silent
        // cache hit and the count would stay at 1.
        executor
            .dispatch(call, &cwd, CancellationToken::new())
            .await;
        assert_eq!(
            prompter.0.load(Ordering::SeqCst),
            2,
            "eviction via the reconstructed request must have forced a fresh prompt"
        );

        // ---- Path target (write_file), same proof shape ----
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(WriteFileTool));
        let prompter = Arc::new(CountingAlwaysAllowPrompter(AtomicUsize::new(0)));
        let gate: Arc<dyn PermissionGate> = Arc::new(ConfirmationGate::new(
            prompter.clone(),
            vec![],
            vec![],
            PlanMode::new(),
            AutonomousMode::new(),
            cwd.clone(),
            false,
        ));
        let executor = ToolExecutor::new(registry, gate, Arc::new(NoopConfiner));

        let file = tempfile::NamedTempFile::new_in(&cwd).unwrap();
        let path = file.path().to_path_buf();
        let call = ToolCall {
            id: ToolCallId("w1".to_string()),
            name: "write_file".to_string(),
            arguments: serde_json::json!({ "path": path, "content": "hello\n" }),
            source: ToolCallSource::Native,
        };
        executor
            .dispatch(call.clone(), &cwd, CancellationToken::new())
            .await;
        assert_eq!(prompter.0.load(Ordering::SeqCst), 1);

        let reconstructed = executor
            .reconstruct_permission_request(&call, &cwd)
            .expect("write_file's permission_request must succeed on unchanged arguments");
        executor.forget_permission(&reconstructed);

        executor
            .dispatch(call, &cwd, CancellationToken::new())
            .await;
        assert_eq!(
            prompter.0.load(Ordering::SeqCst),
            2,
            "eviction of a Path-target key via the reconstructed request must also \
             force a fresh prompt"
        );
    }

    /// Task 8 review Finding 1 (security audit, 2026-09-16): `run_command`
    /// and `write_file` above happen to be exactly the two tools whose
    /// `permission_request` is idempotent with respect to their own prior
    /// execution — which is why the test above passed even while eviction
    /// silently never fired for `delete_file`/`edit_file`/`move_file`.
    /// `delete_file`'s own `permission_request` re-derives its target by
    /// `std::fs::metadata`-ing the path, which its own successful execution
    /// just removed — before the fix, this made `reconstruct_permission_request`
    /// return `None` (`Err` swallowed via `.ok()`) for every real
    /// `delete_file` call once it had actually run, so its cache entry was
    /// never evicted no matter how many turn-groups compaction dropped.
    ///
    /// This drives a REAL `delete_file` dispatch through to completion (the
    /// file is genuinely deleted), then reconstructs a `PermissionRequest`
    /// from the resulting historical `ToolCall` and proves both that
    /// reconstruction succeeds post-execution and that the eviction it
    /// enables actually takes effect. This test fails before the Finding 1
    /// fix (`reconstructed` is `None`, the `.expect` panics) and passes
    /// after it.
    #[tokio::test]
    async fn reconstruct_permission_request_succeeds_for_delete_file_after_its_own_execution() {
        use aivyx_sandbox::{
            AutonomousMode, ConfirmationGate, NoopConfiner, PermissionPrompter, PlanMode,
            UserResponse,
        };
        use aivyx_types::{ToolCall, ToolCallId, ToolCallSource, ToolOutput};
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct CountingAlwaysAllowPrompter(AtomicUsize);
        #[async_trait]
        impl PermissionPrompter for CountingAlwaysAllowPrompter {
            async fn prompt(&self, _request: &PermissionRequest) -> UserResponse {
                self.0.fetch_add(1, Ordering::SeqCst);
                UserResponse::AllowAlways
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().to_path_buf();
        std::fs::write(dir.path().join("doomed.txt"), "bye\n").unwrap();

        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(DeleteFileTool));
        let prompter = Arc::new(CountingAlwaysAllowPrompter(AtomicUsize::new(0)));
        let gate: Arc<dyn PermissionGate> = Arc::new(ConfirmationGate::new(
            prompter.clone(),
            vec![],
            vec![],
            PlanMode::new(),
            AutonomousMode::new(),
            cwd.clone(),
            false,
        ));
        let executor = ToolExecutor::new(registry, Arc::clone(&gate), Arc::new(NoopConfiner));

        let call = ToolCall {
            id: ToolCallId("d1".to_string()),
            name: "delete_file".to_string(),
            arguments: serde_json::json!({ "path": "doomed.txt" }),
            source: ToolCallSource::Native,
        };

        let result = executor
            .dispatch(call.clone(), &cwd, CancellationToken::new())
            .await;
        assert!(
            matches!(result.output, ToolOutput::Ok(_)),
            "expected the delete to succeed, got {:?}",
            result.output
        );
        assert_eq!(prompter.0.load(Ordering::SeqCst), 1);
        assert!(
            !dir.path().join("doomed.txt").exists(),
            "the file must really be gone -- this is what breaks the old .ok()-swallowing code"
        );

        // The load-bearing assertion: reconstruction must succeed even
        // though delete_file's own permission_request would now fail
        // (the file it stats no longer exists).
        let reconstructed = executor
            .reconstruct_permission_request(&call, &cwd)
            .expect(
                "reconstruct_permission_request must succeed for a historical delete_file call \
                 even after its own execution removed the file -- this is exactly Finding 1's bug",
            );

        // The cache entry must still be live before eviction: checking the
        // reconstructed request directly must be a silent cache hit.
        use aivyx_sandbox::PermissionDecision;
        assert_eq!(
            gate.check(&reconstructed).await,
            PermissionDecision::AllowAlways
        );
        assert_eq!(
            prompter.0.load(Ordering::SeqCst),
            1,
            "sanity check: the cache entry must still be live before eviction"
        );

        executor.forget_permission(&reconstructed);

        // If eviction is genuinely wired up (not a vacuous no-op), a repeat
        // check of the exact same reconstructed target must now prompt again.
        assert_eq!(
            gate.check(&reconstructed).await,
            PermissionDecision::AllowAlways
        );
        assert_eq!(
            prompter.0.load(Ordering::SeqCst),
            2,
            "forget_permission on the reconstructed request must have evicted the cache entry"
        );
    }

    #[tokio::test]
    async fn dispatch_checkpoints_before_mutating_tools_only() {
        use aivyx_checkpoint::test_support::{git, init_repo};
        use aivyx_sandbox::{NoopConfiner, PermissionDecision};
        use aivyx_types::{ToolCall, ToolCallId, ToolCallSource};

        struct AllowAll;
        #[async_trait]
        impl PermissionGate for AllowAll {
            async fn check(&self, _r: &PermissionRequest) -> PermissionDecision {
                PermissionDecision::Allow
            }
        }

        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path()).await;
        let cwd = dir.path().canonicalize().unwrap();

        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(ReadFileTool));
        registry.register(Arc::new(WriteFileTool));
        let mut executor = ToolExecutor::new(registry, Arc::new(AllowAll), Arc::new(NoopConfiner));
        executor.set_checkpointer(Arc::new(
            GitCheckpointer::detect(&cwd, vec![]).await.unwrap(),
        ));

        let call = |name: &str, args: serde_json::Value| ToolCall {
            id: ToolCallId("c".to_string()),
            name: name.to_string(),
            arguments: args,
            source: ToolCallSource::Native,
        };
        async fn count_refs(dir: &std::path::Path) -> usize {
            let out = tokio::process::Command::new("git")
                .args(["for-each-ref", "refs/aivyx/checkpoints/"])
                .current_dir(dir)
                .output()
                .await
                .unwrap();
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .filter(|l| !l.is_empty())
                .count()
        }

        // A read dispatch: no checkpoint.
        executor
            .dispatch(
                call("read_file", serde_json::json!({ "path": "tracked.txt" })),
                &cwd,
                tokio_util::sync::CancellationToken::new(),
            )
            .await;
        assert_eq!(count_refs(&cwd).await, 0);

        // A write dispatch: exactly one checkpoint, taken BEFORE the write
        // (the snapshot holds the pre-write content).
        executor
            .dispatch(
                call(
                    "write_file",
                    serde_json::json!({ "path": "tracked.txt", "content": "overwritten\n" }),
                ),
                &cwd,
                tokio_util::sync::CancellationToken::new(),
            )
            .await;
        assert_eq!(count_refs(&cwd).await, 1);
        let ref_name = git(
            &cwd,
            &[
                "for-each-ref",
                "--format=%(refname)",
                "refs/aivyx/checkpoints/",
            ],
        )
        .await;
        let snapshot = git(&cwd, &["show", &format!("{}:tracked.txt", ref_name.trim())]).await;
        assert_eq!(snapshot, "v1\n", "checkpoint must hold pre-write content");
        assert_eq!(
            std::fs::read_to_string(cwd.join("tracked.txt")).unwrap(),
            "overwritten\n"
        );
    }

    #[tokio::test]
    async fn dispatch_does_not_checkpoint_web_fetch_but_still_checkpoints_write_file() {
        // Review-round regression test (final whole-branch review, Finding
        // 1): Task 1 correctly flipped `web_fetch`/`web_search`'s
        // `mutates_outside_session()` to `true` (closing a plan-mode
        // bypass), but `dispatch`'s checkpoint decision must NOT ride
        // along on that same flag — a network read cannot mutate the
        // worktree, so checkpointing it is pure waste and, worse, can
        // misattribute the checkpoint mint to the network call instead of
        // a real edit later in the same batch (see
        // `Tool::needs_checkpoint`'s doc comment). This asserts the
        // `needs_checkpoint()` split actually reaches `dispatch`: a
        // `web_fetch` call takes zero checkpoints, while a `write_file`
        // call in the very same executor still takes exactly one.
        use crate::web::test_support::spawn_mock_http_server;
        use aivyx_checkpoint::test_support::init_repo;
        use aivyx_sandbox::{NoopConfiner, PermissionDecision};
        use aivyx_types::{ToolCall, ToolCallId, ToolCallSource};

        struct AllowAll;
        #[async_trait]
        impl PermissionGate for AllowAll {
            async fn check(&self, _r: &PermissionRequest) -> PermissionDecision {
                PermissionDecision::Allow
            }
        }

        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path()).await;
        let cwd = dir.path().canonicalize().unwrap();

        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(WebFetchTool::new(5, true)));
        registry.register(Arc::new(WriteFileTool));
        let mut executor = ToolExecutor::new(registry, Arc::new(AllowAll), Arc::new(NoopConfiner));
        executor.set_checkpointer(Arc::new(
            GitCheckpointer::detect(&cwd, vec![]).await.unwrap(),
        ));

        async fn count_refs(dir: &std::path::Path) -> usize {
            let out = tokio::process::Command::new("git")
                .args(["for-each-ref", "refs/aivyx/checkpoints/"])
                .current_dir(dir)
                .output()
                .await
                .unwrap();
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .filter(|l| !l.is_empty())
                .count()
        }

        let body = "<html><body>ok</body></html>";
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        let addr = spawn_mock_http_server(Box::leak(response.into_boxed_str())).await;

        // A web_fetch dispatch: no checkpoint, even though
        // `mutates_outside_session()` is `true` for this tool.
        executor
            .dispatch(
                ToolCall {
                    id: ToolCallId("c1".to_string()),
                    name: "web_fetch".to_string(),
                    arguments: serde_json::json!({ "url": format!("http://{addr}/") }),
                    source: ToolCallSource::Native,
                },
                &cwd,
                tokio_util::sync::CancellationToken::new(),
            )
            .await;
        assert_eq!(
            count_refs(&cwd).await,
            0,
            "web_fetch must not take a checkpoint"
        );

        // A real mutating tool in the same executor: still checkpointed.
        executor
            .dispatch(
                ToolCall {
                    id: ToolCallId("c2".to_string()),
                    name: "write_file".to_string(),
                    arguments: serde_json::json!({ "path": "new.txt", "content": "hi\n" }),
                    source: ToolCallSource::Native,
                },
                &cwd,
                tokio_util::sync::CancellationToken::new(),
            )
            .await;
        assert_eq!(
            count_refs(&cwd).await,
            1,
            "write_file must still take exactly one checkpoint"
        );
    }

    #[tokio::test]
    async fn latest_checkpoint_ref_and_restore_delegate_to_the_checkpointer() {
        use aivyx_checkpoint::test_support::init_repo;
        use aivyx_sandbox::NoopConfiner;

        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path()).await;
        let cwd = dir.path().canonicalize().unwrap();

        let registry = ToolRegistry::new();
        let mut executor = ToolExecutor::new(
            registry,
            Arc::new(AllowAllForThisTest),
            Arc::new(NoopConfiner),
        );

        // No checkpointer configured: both wrappers degrade gracefully.
        assert!(
            executor
                .latest_checkpoint_ref(&CancellationToken::new())
                .await
                .is_none()
        );
        assert!(
            executor
                .restore_to_checkpoint("whatever", &CancellationToken::new())
                .await
                .is_err()
        );

        executor.set_checkpointer(Arc::new(
            GitCheckpointer::detect(&cwd, vec![]).await.unwrap(),
        ));
        std::fs::write(cwd.join("tracked.txt"), "v2\n").unwrap();
        executor
            .checkpointer
            .as_ref()
            .unwrap()
            .checkpoint("test", &CancellationToken::new())
            .await;

        let ref_name = executor
            .latest_checkpoint_ref(&CancellationToken::new())
            .await
            .expect("a checkpoint was just taken");

        std::fs::write(cwd.join("tracked.txt"), "broken\n").unwrap();
        executor
            .restore_to_checkpoint(&ref_name, &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(cwd.join("tracked.txt")).unwrap(),
            "v2\n"
        );
    }

    #[test]
    fn cloned_registry_is_independent_of_the_original() {
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(ReadFileTool));

        let sub_registry = registry.clone();
        // Registering onto the original after cloning must not affect the
        // already-taken clone — this is what lets main.rs snapshot "every
        // tool so far" for a sub-agent's registry, then keep adding
        // parent-only tools (like delegate_task itself) onto the original.
        registry.register(Arc::new(WriteFileTool));

        assert_eq!(sub_registry.definitions().len(), 1);
        assert_eq!(registry.definitions().len(), 2);
    }

    #[test]
    fn exclude_removes_the_named_tool_and_keeps_others() {
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(ReadFileTool));
        registry.register(Arc::new(WriteFileTool));

        registry.exclude(&["write_file"]);

        let names: Vec<String> = registry.definitions().into_iter().map(|d| d.name).collect();
        assert_eq!(names, vec!["read_file"]);
    }

    #[test]
    fn exclude_is_a_no_op_for_an_unregistered_name() {
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(ReadFileTool));

        registry.exclude(&["not_a_real_tool"]);

        let names: Vec<String> = registry.definitions().into_iter().map(|d| d.name).collect();
        assert_eq!(names, vec!["read_file"]);
    }

    #[test]
    fn exclude_removes_multiple_names_in_one_call() {
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(ReadFileTool));
        registry.register(Arc::new(WriteFileTool));
        registry.register(Arc::new(EditFileTool));

        registry.exclude(&["write_file", "edit_file"]);

        let names: Vec<String> = registry.definitions().into_iter().map(|d| d.name).collect();
        assert_eq!(names, vec!["read_file"]);
    }

    struct AllowAllForThisTest;
    #[async_trait]
    impl PermissionGate for AllowAllForThisTest {
        async fn check(&self, _r: &PermissionRequest) -> aivyx_sandbox::PermissionDecision {
            aivyx_sandbox::PermissionDecision::Allow
        }
    }
}
