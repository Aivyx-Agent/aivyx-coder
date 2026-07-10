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

mod diff;
mod path_resolve;
mod process;
mod tools;

pub use process::CommandSpec;
pub use tools::{
    EditFileTool, GlobTool, GrepTool, ReadFileTool, RunCommandTool, RunShellTool, SetTasksTool,
    WriteFileTool,
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
        }
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

        let all: Vec<String> = registry.definitions().into_iter().map(|d| d.name).collect();
        let plan: Vec<String> = registry
            .plan_definitions()
            .into_iter()
            .map(|d| d.name)
            .collect();

        assert_eq!(all.len(), 7);
        assert_eq!(plan, vec!["read_file", "grep", "glob", "set_tasks"]);
    }
}
