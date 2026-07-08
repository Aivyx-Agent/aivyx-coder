//! Tool trait + execution pipeline.
//!
//! Foundation pass: signatures only, no concrete tools (`ReadFile`,
//! `WriteFile`, shell-exec, grep, git) yet — those land alongside the
//! permission-gate implementation next pass. The one piece of real
//! behavior worth having now is `ToolExecutor::dispatch`'s shape: the
//! permission check is centralized here rather than inside individual
//! `Tool::execute` impls, so a future tool author can't forget to gate a
//! dangerous action.

use std::path::PathBuf;
use std::sync::Arc;

use aivyx_sandbox::{ExecutionConfiner, PermissionGate, PermissionRequest};
use aivyx_types::{ToolCall, ToolDefinition, ToolOutput, ToolResult};
use async_trait::async_trait;
use thiserror::Error;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Error)]
pub enum ToolError {
    #[error("invalid arguments for tool: {0}")]
    InvalidArguments(String),
    #[error("tool execution failed: {0}")]
    ExecutionFailed(String),
    #[error("no tool registered with name: {0}")]
    NotFound(String),
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

    /// Inspect (already schema-validated) arguments and describe the
    /// permission needed, with NO side effects performed.
    fn permission_request(
        &self,
        arguments: &serde_json::Value,
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

        let permission_request = tool.permission_request(&call.arguments)?;

        use aivyx_sandbox::PermissionDecision;
        match self.gate.check(&permission_request).await {
            PermissionDecision::Allow | PermissionDecision::AllowAlways => {}
            PermissionDecision::Deny => {
                return Ok(ToolOutput::Denied(format!(
                    "permission denied for tool `{}`",
                    call.name
                )));
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
