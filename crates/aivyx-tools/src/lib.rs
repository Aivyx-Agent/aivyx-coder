//! Tool trait + execution pipeline.
//!
//! `ToolExecutor::dispatch` centralizes the permission check rather than
//! leaving it to individual `Tool::execute` impls, so a tool author can't
//! forget to gate a dangerous action. `read_file`/`write_file`/`edit_file`
//! (this pass) are the first concrete tools; shell-exec/grep/git are a
//! later pass.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use aivyx_sandbox::{ExecutionConfiner, PermissionGate, PermissionRequest};
use aivyx_types::{ToolCall, ToolDefinition, ToolOutput, ToolResult};
use async_trait::async_trait;
use thiserror::Error;
use tokio_util::sync::CancellationToken;

mod checkpoint;
mod diff;
mod path_resolve;
mod process;
mod tools;
mod wiki;

pub use checkpoint::GitCheckpointer;
pub use process::CommandSpec;
pub use tools::{
    EditFileTool, GitCommitTool, GitReadTool, GlobTool, GrepTool, ReadFileTool, RunCommandTool,
    RunShellTool, SetTasksTool, WriteFileTool,
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
    fn mutates_outside_session(&self) -> bool {
        true
    }

    /// Inspect (already schema-validated) arguments and describe the
    /// permission needed. May do a bounded read (e.g. to build a diff
    /// preview) but must not perform the actual mutating side effect.
    fn permission_request(
        &self,
        arguments: &serde_json::Value,
        cwd: &Path,
    ) -> Result<PermissionRequest, ToolError>;

    /// Only ever invoked by `ToolExecutor` after `PermissionGate::check`
    /// returns `Allow`/`AllowAlways`.
    async fn execute(
        &self,
        arguments: serde_json::Value,
        ctx: &ToolExecutionContext,
    ) -> Result<ToolOutput, ToolError>;
}

#[derive(Default)]
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

        // Snapshot the worktree before anything that can mutate outside the
        // session — after the gate (denied calls change nothing worth
        // checkpointing), before the effect. Best-effort: a failed
        // checkpoint logs and the call proceeds.
        if tool.mutates_outside_session()
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

        let all: Vec<String> = registry.definitions().into_iter().map(|d| d.name).collect();
        let plan: Vec<String> = registry
            .plan_definitions()
            .into_iter()
            .map(|d| d.name)
            .collect();

        assert_eq!(all.len(), 9);
        assert_eq!(
            plan,
            vec!["read_file", "grep", "glob", "set_tasks", "git_read"]
        );
    }

    #[tokio::test]
    async fn dispatch_checkpoints_before_mutating_tools_only() {
        use crate::checkpoint::test_support::{git, init_repo};
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
    async fn latest_checkpoint_ref_and_restore_delegate_to_the_checkpointer() {
        use crate::checkpoint::test_support::init_repo;
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

    struct AllowAllForThisTest;
    #[async_trait]
    impl PermissionGate for AllowAllForThisTest {
        async fn check(&self, _r: &PermissionRequest) -> aivyx_sandbox::PermissionDecision {
            aivyx_sandbox::PermissionDecision::Allow
        }
    }
}
