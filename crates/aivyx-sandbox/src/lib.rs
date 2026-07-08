//! Security boundary for tool execution.
//!
//! `PermissionGate` is the decision point every tool call must pass through
//! before `Tool::execute` runs; `ConfirmationGate` is the real (prompting)
//! implementation. `ExecutionConfiner` is the (currently no-op) hook for
//! OS-level process confinement (Linux landlock/bubblewrap), kept as a
//! separate trait so non-process tools (e.g. file read) never need a
//! confiner at all — a concrete `LandlockConfiner` is deliberately deferred
//! to a later pass.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;

#[cfg(feature = "sandbox-backend")]
mod confiner;
mod confirmation;
#[cfg(feature = "sandbox-backend")]
pub use confiner::LandlockConfiner;
pub use confirmation::ConfirmationGate;

/// What a tool is asking to do, described *before* any side effect happens.
#[derive(Debug, Clone)]
pub struct PermissionRequest {
    pub tool_name: String,
    pub action: ActionKind,
    pub target: PermissionTarget,
    pub arguments_preview: serde_json::Value,
    /// Human-renderable preview of the effect (e.g. a unified diff for a
    /// file write/edit). Computed by the tool, since only it has the old
    /// and new content — this crate and the UI treat it as an opaque string.
    pub preview: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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

/// Wraps/restricts an about-to-spawn process. `NoopConfiner` is the
/// identity fallback; `LandlockConfiner` (behind the `sandbox-backend`
/// feature, on by default) is the real Landlock + seccomp-bpf backend —
/// swapping between them never touches any tool implementation.
pub trait ExecutionConfiner: Send + Sync {
    fn confine(&self, command: tokio::process::Command) -> tokio::process::Command;
}

pub struct NoopConfiner;

impl ExecutionConfiner for NoopConfiner {
    fn confine(&self, command: tokio::process::Command) -> tokio::process::Command {
        command
    }
}

/// Builds the best confiner available for this build: `LandlockConfiner`
/// when the `sandbox-backend` feature is enabled (the default), otherwise
/// `NoopConfiner` — keeps the `#[cfg]` branching in one place rather than
/// in every caller.
#[cfg(feature = "sandbox-backend")]
pub fn default_confiner(
    cwd: &Path,
    extra_read_paths: &[PathBuf],
    deny_paths: &[PathBuf],
    require_enforcement: bool,
) -> Arc<dyn ExecutionConfiner> {
    Arc::new(LandlockConfiner::new(
        cwd,
        extra_read_paths,
        deny_paths,
        require_enforcement,
    ))
}

#[cfg(not(feature = "sandbox-backend"))]
pub fn default_confiner(
    _cwd: &Path,
    _extra_read_paths: &[PathBuf],
    _deny_paths: &[PathBuf],
    _require_enforcement: bool,
) -> Arc<dyn ExecutionConfiner> {
    Arc::new(NoopConfiner)
}

/// Shared by `ConfirmationGate::is_denied` and (behind `sandbox-backend`)
/// `LandlockConfiner`'s path-grant construction — both need the same
/// "is this path under a denied path" check.
pub(crate) fn path_is_denied(path: &Path, deny_paths: &[PathBuf]) -> bool {
    deny_paths.iter().any(|denied| path.starts_with(denied))
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
