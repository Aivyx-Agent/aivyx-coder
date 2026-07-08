//! Security boundary for tool execution.
//!
//! Foundation pass: trait signatures only. `PermissionGate` is the
//! decision point every tool call must pass through before `Tool::execute`
//! runs; `ExecutionConfiner` is the (currently no-op) hook for OS-level
//! process confinement (Linux landlock/bubblewrap), kept as a separate
//! trait so non-process tools (e.g. file read) never need a confiner at
//! all. Concrete implementations (`ConfirmationGate`, `LandlockConfiner`)
//! are deliberately deferred to the next pass — see the project plan.

use std::path::PathBuf;

use async_trait::async_trait;

/// What a tool is asking to do, described *before* any side effect happens.
#[derive(Debug, Clone)]
pub struct PermissionRequest {
    pub tool_name: String,
    pub action: ActionKind,
    pub target: PermissionTarget,
    pub arguments_preview: serde_json::Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionKind {
    Read,
    Write,
    Execute,
    Delete,
}

#[derive(Debug, Clone)]
pub enum PermissionTarget {
    Path(PathBuf),
    Command { program: String, args: Vec<String> },
    Other(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionDecision {
    Allow,
    AllowAlways,
    Deny,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserResponse {
    Allow,
    AllowAlways,
    Deny,
}

/// Decides allow/deny for every tool call, uniformly, before `Tool::execute`
/// runs. The default (and only, this pass) backend prompts the user via a
/// `PermissionPrompter`; a future OS-enforced backend can implement this
/// same trait without the agent loop or `ToolExecutor` changing at all.
#[async_trait]
pub trait PermissionGate: Send + Sync {
    async fn check(&self, request: &PermissionRequest) -> PermissionDecision;
}

/// The human-facing side of a `PermissionGate` decision — implemented by
/// `aivyx-tui` so a gate can pop a confirmation modal without depending on
/// the UI crate directly.
#[async_trait]
pub trait PermissionPrompter: Send + Sync {
    async fn prompt(&self, request: &PermissionRequest) -> UserResponse;
}

/// Wraps/restricts an about-to-spawn process. The v1 (this pass) caller
/// uses a no-op identity confiner; a Linux landlock/bubblewrap backend can
/// be dropped in later behind the `landlock-backend` feature without
/// touching any tool implementation.
pub trait ExecutionConfiner: Send + Sync {
    fn confine(&self, command: tokio::process::Command) -> tokio::process::Command;
}

pub struct NoopConfiner;

impl ExecutionConfiner for NoopConfiner {
    fn confine(&self, command: tokio::process::Command) -> tokio::process::Command {
        command
    }
}

/// Denies every request. A safe stand-in wherever a `PermissionGate` is
/// required but no real one (e.g. `ConfirmationGate`) has been wired up
/// yet: since it fails closed, forgetting to swap it in later breaks a
/// tool call loudly instead of silently allowing it.
pub struct AlwaysDenyGate;

#[async_trait]
impl PermissionGate for AlwaysDenyGate {
    async fn check(&self, _request: &PermissionRequest) -> PermissionDecision {
        PermissionDecision::Deny
    }
}
