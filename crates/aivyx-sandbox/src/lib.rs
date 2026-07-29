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
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;

#[cfg(feature = "sandbox-backend")]
mod confiner;
mod confirmation;
mod editor_approval;
mod injection_scan;
#[cfg(feature = "sandbox-backend")]
pub use confiner::LandlockConfiner;
pub use confirmation::ConfirmationGate;
pub use injection_scan::{InjectionFinding, InjectionTaint, scan_for_injection_markers};

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
    /// Structured before/after content for a file write/edit/delete, for
    /// consumers (e.g. an editor-approval integration) that want to render
    /// their own native diff view rather than a preformatted text blob.
    /// `None` when there's no meaningful structured content (a non-file
    /// action, or a file whose content can't be read as text — see
    /// `write_file`'s/`delete_file`'s own binary-file fallback).
    pub diff: Option<DiffContent>,
}

/// Structured before/after text for a file write/edit/delete.
/// `old_content` is empty for a brand-new file (nothing existed before).
#[derive(Debug, Clone)]
pub struct DiffContent {
    pub old_content: String,
    pub new_content: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ActionKind {
    Read,
    Write,
    Execute,
    Delete,
    /// Mutates only the agent's own in-session state (e.g. the task list) —
    /// no filesystem, process, or network effect. Auto-allowed like `Read`,
    /// but kept distinct so a tool that touches the outside world can't
    /// honestly describe itself this way, and so audit logs don't record an
    /// internal state change as a "Read" of anything.
    Internal,
    /// A tool call dispatched to an external MCP (Model Context Protocol)
    /// server — arbitrary, user-configured third-party code whose actual
    /// behavior this project can't verify, regardless of anything the
    /// server itself claims (e.g. MCP's optional, advisory `readOnlyHint`
    /// annotation). Always confirm-gated, uniformly: there is no case where
    /// this is treated as auto-allowed, unlike every other `ActionKind`.
    /// Kept distinct from `Write`/`Execute` so audit logs and the
    /// confirmation modal can honestly say "this is an MCP call," not
    /// mislabel it as a filesystem write or local command execution.
    McpTool,
    /// The agent proposing an update to its own global, cross-project
    /// preferences file (`remember_preference`). Always confirm-gated —
    /// like `McpTool`, this is uniformly never auto-allowed, and unlike
    /// every other `ActionKind`, a call is never satisfied *or* recorded
    /// by the Always-Allow cache even in interactive mode (see
    /// `ConfirmationGate::check`): the target description is a fixed
    /// constant string regardless of what content is actually being
    /// proposed, so caching it would silently bless every future,
    /// unreviewed rewrite after the first approval. Unconditionally
    /// denied under `--auto` for the same reason `McpTool` is — this
    /// file persists and applies to every future project, unlike an
    /// in-worktree edit `--auto`'s checkpoint/rollback safety net
    /// already covers.
    Memory,
    /// An already-approved interactive process (`repl_send`/`repl_stop`)
    /// continuing to talk to a process `repl_start` already put through
    /// the `Execute` tier. Auto-allowed like `Read`/`Internal` in Act
    /// mode, but — unlike them — checked *after* the plan-mode and
    /// autonomous-mode denial tiers in `ConfirmationGate::check`, not
    /// alongside them: an approval implied by an earlier `Execute` call
    /// must not let a live process keep accepting input once the user
    /// enters plan mode. Distinct from `Internal` (which is documented as
    /// "no filesystem, process, or network effect" — dishonest for a tool
    /// that writes to a live process's stdin) and from `Execute` (which
    /// would mean re-prompting on every send, defeating the point).
    Interact,
    /// Relocating or renaming a file or directory (`move_file`). Neither a
    /// pure `Write` (the old path stops existing) nor a pure `Delete` (the
    /// content survives, just at a new path) — kept distinct so the
    /// confirmation modal, audit log, and autonomous-mode gating can
    /// describe it honestly, the same reasoning `ActionKind::Delete`'s own
    /// doc comment already gives for not folding deletion into `Write`.
    Move,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionTarget {
    Path(PathBuf),
    Command { program: String, args: Vec<String> },
    Other(String),
    /// A move/rename's two endpoints. Kept structured (not folded into
    /// `Other(String)`) because `deny_paths` and the autonomous-mode
    /// worktree-boundary check both need real `PathBuf`s for *both* ends —
    /// approving `move a.rs b.rs` must not bless `move c.rs d.rs`, the same
    /// exact-target discipline `Command`'s cache key already documents.
    Move { from: PathBuf, to: PathBuf },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionDecision {
    Allow,
    AllowAlways,
    /// The optional reason is surfaced to the model in `ToolOutput::Denied`
    /// so it can adapt (plan mode, deny_paths, user refusal) instead of
    /// blindly retrying; `None` falls back to the executor's generic
    /// "permission denied" message.
    Deny(Option<String>),
}

/// Shared plan-mode flag: while active, the permission gate denies every
/// action that could mutate anything outside the agent's own session state,
/// and the agent withholds mutating tools from the model entirely. Toggled
/// only by the user (TUI keybinding / `--plan` startup flag), never by the
/// model — the whole point is that it doesn't depend on model cooperation.
///
/// `Relaxed` ordering throughout: no data is published through this flag; a
/// toggle racing one in-flight permission check by a single tool call is
/// acceptable, since the gate re-checks on every call.
#[derive(Debug, Clone, Default)]
pub struct PlanMode(Arc<AtomicBool>);

impl PlanMode {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn active(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }

    pub fn set_active(&self, active: bool) {
        self.0.store(active, Ordering::Relaxed);
    }

    /// Flips the mode and returns the *new* state.
    pub fn toggle(&self) -> bool {
        !self.0.fetch_xor(true, Ordering::Relaxed)
    }
}

/// Shared autonomous-mode flag: while active, `ConfirmationGate` resolves
/// every decision deterministically (pre-approved commands and in-worktree
/// edits allowed, everything else denied) instead of prompting — there is
/// no human to prompt. Set once at startup from the `--auto` CLI flag; not
/// expected to toggle mid-session (unlike `PlanMode`, no keybinding flips
/// it), but the same shared-flag shape keeps `ConfirmationGate`'s
/// consumption pattern uniform. See docs/superpowers/specs/
/// 2026-07-12-phase-11c-autonomous-loop-design.md.
///
/// `Relaxed` ordering, same rationale as `PlanMode`: no data is published
/// through this flag, so there is nothing for a stricter ordering to
/// synchronize.
#[derive(Debug, Clone, Default)]
pub struct AutonomousMode(Arc<AtomicBool>);

impl AutonomousMode {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn active(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }

    pub fn set_active(&self, active: bool) {
        self.0.store(active, Ordering::Relaxed);
    }
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
///
/// A `deny_paths` entry with a single path component (e.g. `.env`,
/// `*.pem`) is a basename-glob pattern, matched against `path`'s own file
/// name wherever it appears — not just at one fixed location. Every
/// other entry keeps the original exact-prefix `starts_with` check.
/// Classifying by component count (rather than a config-time flag) means
/// this function's signature never has to change: an entry only has a
/// single component in the first place when `aivyx-config`'s
/// `resolved_deny_paths` deliberately left it unresolved for exactly this
/// reason (see that function's own doc comment).
///
/// One documented edge case: a relative, non-`~`-prefixed entry that
/// doesn't exist on disk at resolve time (e.g. a stray trailing-slash
/// entry like `"secrets/"`) can reach here as an unresolved multi- or
/// single-component relative `PathBuf` depending on its shape, which may
/// not classify the way a human reading the raw config string would
/// expect. No default entry or documented usage triggers this — deny_paths
/// entries are meant to be absolute, `~`-prefixed, or bare basename
/// patterns.
pub fn path_is_denied(path: &Path, deny_paths: &[PathBuf]) -> bool {
    deny_paths.iter().any(|denied| {
        if is_bare_pattern(denied) {
            is_basename_glob_match(path, denied)
        } else {
            path.starts_with(denied)
        }
    })
}

/// A `deny_paths` entry with a single path component (e.g. `.env`,
/// `*.pem`) is a basename-glob pattern, not a real filesystem location
/// to resolve — see `path_is_denied`'s own doc comment above for the
/// full rationale. Extracted so `confiner.rs`'s
/// `find_basename_glob_matches` classifies entries identically rather
/// than re-deriving the same check independently.
fn is_bare_pattern(path: &Path) -> bool {
    path.parent() == Some(Path::new(""))
}

fn is_basename_glob_match(path: &Path, pattern: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    let Some(pattern) = pattern.to_str() else {
        return false;
    };
    globset::Glob::new(pattern)
        .map(|glob| glob.compile_matcher().is_match(name))
        .unwrap_or(false)
}

/// Denies every request. A safe stand-in wherever a `PermissionGate` is
/// required but no real one (e.g. `ConfirmationGate`) has been wired up
/// yet: since it fails closed, forgetting to swap it in later breaks a
/// tool call loudly instead of silently allowing it.
pub struct AlwaysDenyGate;

#[async_trait]
impl PermissionGate for AlwaysDenyGate {
    async fn check(&self, _request: &PermissionRequest) -> PermissionDecision {
        PermissionDecision::Deny(Some(
            "no permission gate is configured (fail-closed default)".to_string(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn autonomous_mode_starts_inactive_and_can_be_activated() {
        let mode = AutonomousMode::new();
        assert!(!mode.active());
        mode.set_active(true);
        assert!(mode.active());
    }

    #[test]
    fn autonomous_mode_clones_share_state() {
        let mode = AutonomousMode::new();
        let clone = mode.clone();
        mode.set_active(true);
        assert!(clone.active(), "clones must observe the same underlying flag");
    }

    #[test]
    fn a_bare_basename_matches_the_same_name_under_any_directory() {
        let deny_paths = vec![PathBuf::from(".env")];
        assert!(path_is_denied(Path::new("/project-a/.env"), &deny_paths));
        assert!(path_is_denied(
            Path::new("/project-b/nested/.env"),
            &deny_paths
        ));
        assert!(!path_is_denied(
            Path::new("/project-a/.env.example"),
            &deny_paths
        ));
    }

    #[test]
    fn a_bare_glob_pattern_matches_by_wildcard() {
        let deny_paths = vec![PathBuf::from("*.pem")];
        assert!(path_is_denied(Path::new("/any/dir/server.pem"), &deny_paths));
        assert!(!path_is_denied(Path::new("/any/dir/.env"), &deny_paths));
    }

    #[test]
    fn a_bare_pattern_does_not_match_a_substring_of_a_longer_basename() {
        let deny_paths = vec![PathBuf::from(".env")];
        // A directory or file merely containing the pattern text is not a
        // match — glob matching is exact against the whole basename, not
        // a substring search.
        assert!(!path_is_denied(
            Path::new("/project/.env-backup"),
            &deny_paths
        ));
    }

    #[test]
    fn a_path_separator_entry_keeps_exact_prefix_matching() {
        let deny_paths = vec![PathBuf::from("/home/user/.ssh")];
        assert!(path_is_denied(
            Path::new("/home/user/.ssh/id_rsa"),
            &deny_paths
        ));
        assert!(!path_is_denied(
            Path::new("/home/user/.ssh-backup/id_rsa"),
            &deny_paths
        ));
        assert!(!path_is_denied(Path::new("/home/user/other"), &deny_paths));
    }

    #[test]
    fn is_bare_pattern_is_true_for_a_single_component_entry() {
        assert!(is_bare_pattern(Path::new(".env")));
        assert!(is_bare_pattern(Path::new("*.pem")));
    }

    #[test]
    fn is_bare_pattern_is_false_for_a_path_separator_entry() {
        assert!(!is_bare_pattern(Path::new("/home/user/.ssh")));
        assert!(!is_bare_pattern(Path::new("relative/two/parts")));
    }
}
