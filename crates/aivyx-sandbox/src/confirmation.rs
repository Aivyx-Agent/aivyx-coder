use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use crate::{
    ActionKind, AutonomousMode, InjectionTaint, PermissionDecision, PermissionGate,
    PermissionPrompter, PermissionRequest, PermissionTarget, PlanMode, UserResponse,
    editor_approval, path_is_denied,
};

/// Told to the model on a plan-mode denial. This is a backstop message: in
/// plan mode the agent doesn't offer mutating tools at all, so reaching it
/// means the model invented a call to a tool it wasn't offered.
const PLAN_MODE_DENIAL: &str = "plan mode is active — no file modifications or command execution \
until the user approves the plan and switches to Act mode (Ctrl+P). Explore with the read/search \
tools and record your plan with set_tasks instead.";

/// Told to the model when an autonomous-mode edit/write target resolves
/// outside the worktree it was launched in. Unlike every other denial
/// reason in this gate, this one has no interactive-mode equivalent to
/// point back to (autonomous mode has no confirmation modal for a human to
/// reject it from) — see the "gap found during design" note in the Phase
/// 11c design doc for why this check exists at all.
const AUTONOMOUS_OUTSIDE_CWD_DENIAL: &str =
    "target is outside the autonomous session's worktree boundary — file edits in \
     autonomous mode are confined to the working directory the session was launched in.";

/// Told to the model when an MCP tool call reaches autonomous mode. There is
/// no way to pre-approve an MCP tool the way `[[permissions.allowed_commands]]`
/// pre-approves a shell command, and autonomous mode has no human to prompt —
/// so denial is the only safe behavior, consistent with `ActionKind::McpTool`
/// being "always confirm-gated, uniformly" (see its doc comment in `lib.rs`).
const AUTONOMOUS_MCP_TOOL_DENIAL: &str =
    "MCP tools require interactive confirmation and cannot run in autonomous mode";

/// Told to the model when a `remember_preference` call reaches autonomous
/// mode. Same reasoning as `AUTONOMOUS_MCP_TOOL_DENIAL`: there is no human
/// to review the proposed change, and this file's effect isn't scoped to
/// the current worktree the way `is_outside_autonomous_worktree` already
/// bounds ordinary Write/Delete actions.
const AUTONOMOUS_MEMORY_DENIAL: &str =
    "remembering preferences requires interactive confirmation and cannot happen in autonomous mode";

/// Told to the model when a `repl_send`/`repl_stop` call reaches
/// autonomous mode. `repl_start` (the `Execute`-tier action that would
/// actually spawn the process) is already hidden from the model in
/// autonomous mode (`AUTONOMOUS_HIDDEN_TOOLS` in `aivyx-core`), so this is
/// defense in depth — the same reasoning already applied to
/// `git_commit`'s target being permanently non-cacheable as a backstop
/// even though it's also hidden.
const AUTONOMOUS_INTERACT_DENIAL: &str =
    "interacting with a REPL/process session cannot happen in autonomous mode";

/// Identifies a "class" of requests for the Always-Allow cache. Scoped to
/// the exact target (and action), not the whole tool — approving one write
/// must not silently bless every future write anywhere.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum PermissionKey {
    Path {
        action: ActionKind,
        path: PathBuf,
    },
    Command {
        program: String,
        args: Vec<String>,
    },
    Other {
        action: ActionKind,
        description: String,
    },
}

impl PermissionKey {
    fn from_request(request: &PermissionRequest) -> Self {
        match &request.target {
            PermissionTarget::Path(path) => PermissionKey::Path {
                action: request.action,
                path: path.clone(),
            },
            PermissionTarget::Command { program, args } => PermissionKey::Command {
                program: program.clone(),
                args: args.clone(),
            },
            PermissionTarget::Other(description) => PermissionKey::Other {
                action: request.action,
                description: description.clone(),
            },
        }
    }
}

/// The real `PermissionGate`: hard-blocks `deny_paths`, auto-allows reads
/// and `Internal` (session-state-only) actions, denies everything else
/// outright while plan mode is active, otherwise prompts (with an
/// in-memory, per-exact-target Always-Allow cache for the rest of the
/// session).
pub struct ConfirmationGate {
    prompter: Arc<dyn PermissionPrompter>,
    deny_paths: Vec<PathBuf>,
    plan_mode: PlanMode,
    autonomous_mode: AutonomousMode,
    cwd: PathBuf,
    always_allow: Mutex<HashSet<PermissionKey>>,
    editor_approval_enabled: bool,
    injection_taint: InjectionTaint,
}

impl ConfirmationGate {
    /// `pre_approved_commands` seeds the Always-Allow cache directly (as
    /// `(program, args)` pairs) rather than requiring an interactive
    /// confirmation the first time — these represent commands the user
    /// already trusted by writing them into config, so an additional
    /// "are you sure" click adds friction without a security benefit. This
    /// is the "command-level allowlisting" trust tier: `run_command` only
    /// ever runs entries from this same list, and `run_shell` treats an
    /// exact match against it as pre-approved before falling back to the
    /// normal confirm-then-cache flow for anything else.
    ///
    /// `editor_approval_enabled` gates the editor-side answer race in
    /// `check` below (`docs/superpowers/specs/
    /// 2026-07-19-editor-approval-integration-design.md`) — when `false`,
    /// `check` behaves exactly as it did before this feature existed.
    pub fn new(
        prompter: Arc<dyn PermissionPrompter>,
        deny_paths: Vec<PathBuf>,
        pre_approved_commands: Vec<(String, Vec<String>)>,
        plan_mode: PlanMode,
        autonomous_mode: AutonomousMode,
        cwd: PathBuf,
        editor_approval_enabled: bool,
    ) -> Self {
        let always_allow = pre_approved_commands
            .into_iter()
            .map(|(program, args)| PermissionKey::Command { program, args })
            .collect();
        Self {
            prompter,
            deny_paths,
            plan_mode,
            autonomous_mode,
            cwd,
            always_allow: Mutex::new(always_allow),
            editor_approval_enabled,
            injection_taint: InjectionTaint::new(),
        }
    }

    /// Attaches the shared `InjectionTaint` handle the agent (which
    /// writes to it) and the TUI's autonomous driver (which reads it to
    /// decide when to stop) also hold — must be the *same* instance for
    /// the pause behavior below to fire. Without a call to this, the gate
    /// keeps its own private, never-flagged instance and behaves exactly
    /// as it did before this feature existed. See docs/superpowers/specs/
    /// 2026-07-22-autonomous-mode-injection-guard-design.md.
    pub fn with_injection_taint(mut self, injection_taint: InjectionTaint) -> Self {
        self.injection_taint = injection_taint;
        self
    }

    fn is_denied(&self, request: &PermissionRequest) -> bool {
        let PermissionTarget::Path(path) = &request.target else {
            return false;
        };
        path_is_denied(path, &self.deny_paths)
    }

    /// The autonomous-mode edit boundary: a `Write`/`Delete` action on a
    /// `Path` target is only in-scope if the resolved path is at-or-under
    /// `cwd`. Only meaningful for autonomous mode — interactive mode relies
    /// on a human seeing the target in the confirmation modal instead (see
    /// the Phase 11c design doc's "gap found during design" section).
    fn is_outside_autonomous_worktree(&self, request: &PermissionRequest, cwd: &Path) -> bool {
        if !matches!(request.action, ActionKind::Write | ActionKind::Delete) {
            return false;
        }
        let PermissionTarget::Path(path) = &request.target else {
            return false;
        };
        !path.starts_with(cwd)
    }

    /// Races the terminal's own prompt against a possible editor-side
    /// answer to the same pending request. When editor-approval is
    /// disabled, or there's no structured content to offer the editor for
    /// this particular request (e.g. a binary-file diff gap), falls back
    /// to the terminal path unchanged — exactly as `check` behaved before
    /// this feature existed.
    async fn resolve_via_prompter_or_editor(&self, request: &PermissionRequest) -> UserResponse {
        if !self.editor_approval_enabled {
            return self.prompter.prompt(request).await;
        }

        let request_id = uuid::Uuid::new_v4().to_string();
        let Some(pending) = editor_approval::build_pending_request(request, request_id.clone())
        else {
            return self.prompter.prompt(request).await;
        };
        let (Some(req_path), Some(resp_path)) = (
            editor_approval::request_path(&self.cwd),
            editor_approval::response_path(&self.cwd),
        ) else {
            return self.prompter.prompt(request).await;
        };

        if editor_approval::write_pending_request(&req_path, &pending)
            .await
            .is_err()
        {
            return self.prompter.prompt(request).await;
        }

        let response: UserResponse = tokio::select! {
            response = self.prompter.prompt(request) => response,
            response = editor_approval::poll_for_response(&resp_path, &request_id) => response,
        };

        let _ = tokio::fs::remove_file(&req_path).await;
        let _ = tokio::fs::remove_file(&resp_path).await;

        response
    }
}

#[async_trait]
impl PermissionGate for ConfirmationGate {
    async fn check(&self, request: &PermissionRequest) -> PermissionDecision {
        if self.is_denied(request) {
            tracing::warn!(
                tool = %request.tool_name,
                action = ?request.action,
                target = ?request.target,
                "permission denied: target is under a deny_paths entry"
            );
            return PermissionDecision::Deny(Some(
                "target is under a configured deny_paths entry (hard-blocked)".to_string(),
            ));
        }

        if matches!(request.action, ActionKind::Read | ActionKind::Internal) {
            return PermissionDecision::Allow;
        }

        // Everything past this point mutates or executes. The plan-mode
        // check MUST sit before the Always-Allow cache and the pre-approved
        // `allowed_commands` lookup below — an approval granted before plan
        // mode was entered must not leak through it.
        if self.plan_mode.active() {
            tracing::warn!(
                tool = %request.tool_name,
                action = ?request.action,
                target = ?request.target,
                "permission denied: plan mode is active"
            );
            return PermissionDecision::Deny(Some(PLAN_MODE_DENIAL.to_string()));
        }

        // Autonomous mode: resolve deterministically, never prompt (there is
        // no human to prompt). Checked before the Always-Allow cache lookup
        // below because the cwd-boundary check applies to Write/Delete
        // targets that the cache path doesn't otherwise examine.
        if self.autonomous_mode.active() {
            // Gate on the action, not the target: every MCP tool call
            // currently uses `PermissionTarget::Other`, but this check must
            // not depend on that implementation detail staying true. There
            // is no pre-approval tier for MCP tools, so denial is
            // unconditional here, unlike the Command branch below.
            if request.action == ActionKind::McpTool {
                tracing::warn!(
                    tool = %request.tool_name,
                    action = ?request.action,
                    target = ?request.target,
                    "permission denied: MCP tool call in autonomous mode"
                );
                return PermissionDecision::Deny(Some(AUTONOMOUS_MCP_TOOL_DENIAL.to_string()));
            }
            if request.action == ActionKind::Memory {
                tracing::warn!(
                    tool = %request.tool_name,
                    action = ?request.action,
                    target = ?request.target,
                    "permission denied: remember_preference call in autonomous mode"
                );
                return PermissionDecision::Deny(Some(AUTONOMOUS_MEMORY_DENIAL.to_string()));
            }
            if request.action == ActionKind::Interact {
                tracing::warn!(
                    tool = %request.tool_name,
                    action = ?request.action,
                    target = ?request.target,
                    "permission denied: Interact call in autonomous mode"
                );
                return PermissionDecision::Deny(Some(AUTONOMOUS_INTERACT_DENIAL.to_string()));
            }
            if (matches!(request.action, ActionKind::Write | ActionKind::Delete)
                || matches!(request.target, PermissionTarget::Command { .. }))
                && let Some(finding) = self.injection_taint.current()
            {
                tracing::warn!(
                    tool = %request.tool_name,
                    action = ?request.action,
                    target = ?request.target,
                    source = %finding.source,
                    "permission denied: injection-flagged content ingested this session"
                );
                return PermissionDecision::Deny(Some(format!(
                    "permission denied: flagged content was ingested this session \
                     (possible prompt injection from {}) — autonomous mode is \
                     pausing for human review",
                    finding.source
                )));
            }
            if self.is_outside_autonomous_worktree(request, &self.cwd) {
                tracing::warn!(
                    tool = %request.tool_name,
                    action = ?request.action,
                    target = ?request.target,
                    "permission denied: outside the autonomous worktree boundary"
                );
                return PermissionDecision::Deny(Some(AUTONOMOUS_OUTSIDE_CWD_DENIAL.to_string()));
            }
            return match &request.target {
                // Already passed the cwd-boundary check above (or wasn't a
                // Write/Delete-on-Path at all, e.g. a Read/Internal target
                // reaching here would be unusual since tier 2 already
                // caught those — but Other targets like set_tasks's aren't
                // Path/Command, so they fall here too and must be allowed).
                PermissionTarget::Path(_) | PermissionTarget::Other(_) => {
                    tracing::info!(
                        tool = %request.tool_name,
                        action = ?request.action,
                        target = ?request.target,
                        "permission allowed (autonomous mode, within worktree)"
                    );
                    PermissionDecision::Allow
                }
                PermissionTarget::Command { .. } => {
                    let key = PermissionKey::from_request(request);
                    if self.always_allow.lock().unwrap().contains(&key) {
                        tracing::info!(
                            tool = %request.tool_name,
                            action = ?request.action,
                            target = ?request.target,
                            "permission allowed (autonomous mode, pre-approved)"
                        );
                        PermissionDecision::AllowAlways
                    } else {
                        tracing::warn!(
                            tool = %request.tool_name,
                            action = ?request.action,
                            target = ?request.target,
                            "permission denied: not pre-approved and autonomous mode never prompts"
                        );
                        PermissionDecision::Deny(Some(
                            "not pre-approved, and autonomous mode has no one to prompt — add \
                             this to [[permissions.allowed_commands]] if it should be allowed"
                                .to_string(),
                        ))
                    }
                }
            };
        }

        // An already-approved REPL/process session (repl_send/repl_stop)
        // continuing to interact with a process repl_start already put
        // through the Execute tier above. Checked here — after plan-mode
        // and autonomous-mode denial, both of which already returned
        // above if active — not alongside the Read/Internal auto-allow
        // near the top of this function, which sits BEFORE plan-mode
        // specifically so a pre-plan-mode approval can't leak through it
        // (see that check's own comment). If Interact auto-allowed in
        // that same early tier, a session still running when the user
        // enters plan mode would keep silently accepting input during
        // it — exactly the leak-through failure mode that comment guards
        // against, just via a live process instead of a cached decision.
        if request.action == ActionKind::Interact {
            return PermissionDecision::Allow;
        }

        // `Memory` actions never participate in the Always-Allow cache,
        // in either direction — see ActionKind::Memory's doc comment for
        // why (the target description is fixed regardless of proposed
        // content, so caching would silently bless every future rewrite).
        let never_cached = request.action == ActionKind::Memory;
        let key = PermissionKey::from_request(request);
        if !never_cached && self.always_allow.lock().unwrap().contains(&key) {
            tracing::info!(
                tool = %request.tool_name,
                action = ?request.action,
                target = ?request.target,
                "permission allowed (cached Always-Allow)"
            );
            return PermissionDecision::AllowAlways;
        }

        let decision = match self.resolve_via_prompter_or_editor(request).await {
            UserResponse::Allow => PermissionDecision::Allow,
            UserResponse::AllowAlways => {
                if !never_cached {
                    self.always_allow.lock().unwrap().insert(key);
                }
                PermissionDecision::AllowAlways
            }
            UserResponse::Deny => {
                PermissionDecision::Deny(Some("the user denied this action".to_string()))
            }
        };
        match &decision {
            PermissionDecision::Deny(_) => tracing::warn!(
                tool = %request.tool_name,
                action = ?request.action,
                target = ?request.target,
                "permission denied by user"
            ),
            _ => tracing::info!(
                tool = %request.tool_name,
                action = ?request.action,
                target = ?request.target,
                always = %matches!(decision, PermissionDecision::AllowAlways),
                "permission allowed by user"
            ),
        }
        decision
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::InjectionFinding;
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

    fn write_request(path: &str) -> PermissionRequest {
        PermissionRequest {
            tool_name: "write_file".to_string(),
            action: ActionKind::Write,
            target: PermissionTarget::Path(PathBuf::from(path)),
            arguments_preview: serde_json::json!({}),
            preview: None,
            diff: None,
        }
    }

    fn read_request(path: &str) -> PermissionRequest {
        PermissionRequest {
            action: ActionKind::Read,
            ..write_request(path)
        }
    }

    #[tokio::test]
    async fn deny_paths_short_circuits_without_prompting() {
        let prompter = Arc::new(FakePrompter {
            response: UserResponse::Allow,
            calls: AtomicUsize::new(0),
        });
        let gate = ConfirmationGate::new(
            prompter.clone(),
            vec![PathBuf::from("/home/user/.ssh")],
            vec![],
            PlanMode::new(),
            AutonomousMode::new(),
            PathBuf::from("/home/user/project"),
            false,
        );

        let decision = gate
            .check(&write_request("/home/user/.ssh/id_ed25519"))
            .await;

        assert!(matches!(decision, PermissionDecision::Deny(_)));
        assert_eq!(prompter.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn deny_paths_blocks_a_generic_write_under_the_config_directory() {
        let prompter = Arc::new(FakePrompter {
            response: UserResponse::Allow,
            calls: AtomicUsize::new(0),
        });
        let gate = ConfirmationGate::new(
            prompter.clone(),
            vec![PathBuf::from("/home/user/.config/aivyx-coder")],
            vec![],
            PlanMode::new(),
            AutonomousMode::new(),
            PathBuf::from("/home/user/project"),
            false,
        );

        let decision = gate
            .check(&write_request(
                "/home/user/.config/aivyx-coder/config.toml",
            ))
            .await;

        assert!(matches!(decision, PermissionDecision::Deny(_)));
        assert_eq!(prompter.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn reads_auto_allow_without_prompting() {
        let prompter = Arc::new(FakePrompter {
            response: UserResponse::Deny,
            calls: AtomicUsize::new(0),
        });
        let gate = ConfirmationGate::new(
            prompter.clone(),
            vec![],
            vec![],
            PlanMode::new(),
            AutonomousMode::new(),
            PathBuf::from("/home/user/project"),
            false,
        );

        let decision = gate
            .check(&read_request("/home/user/project/src/main.rs"))
            .await;

        assert_eq!(decision, PermissionDecision::Allow);
        assert_eq!(prompter.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn internal_actions_auto_allow_without_prompting() {
        let prompter = Arc::new(FakePrompter {
            response: UserResponse::Deny,
            calls: AtomicUsize::new(0),
        });
        let gate = ConfirmationGate::new(
            prompter.clone(),
            vec![],
            vec![],
            PlanMode::new(),
            AutonomousMode::new(),
            PathBuf::from("/home/user/project"),
            false,
        );

        let request = PermissionRequest {
            tool_name: "set_tasks".to_string(),
            action: ActionKind::Internal,
            target: PermissionTarget::Other("session task list".to_string()),
            arguments_preview: serde_json::json!({}),
            preview: None,
            diff: None,
        };

        let decision = gate.check(&request).await;
        assert_eq!(decision, PermissionDecision::Allow);
        assert_eq!(prompter.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn always_allow_caches_per_exact_target_only() {
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
            PathBuf::from("/home/user/project"),
            false,
        );

        let first = gate.check(&write_request("/home/user/project/a.rs")).await;
        assert_eq!(first, PermissionDecision::AllowAlways);
        assert_eq!(prompter.calls.load(Ordering::SeqCst), 1);

        // Same exact target again: cached, no second prompt.
        let second = gate.check(&write_request("/home/user/project/a.rs")).await;
        assert_eq!(second, PermissionDecision::AllowAlways);
        assert_eq!(prompter.calls.load(Ordering::SeqCst), 1);

        // A different target still prompts.
        let third = gate.check(&write_request("/home/user/project/b.rs")).await;
        assert_eq!(third, PermissionDecision::AllowAlways);
        assert_eq!(prompter.calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn deny_paths_wins_over_read_auto_allow() {
        // Locks in the order inside `check`: the deny-list is consulted
        // before the read-auto-allow shortcut, not after. A refactor that
        // swapped the order (e.g. to skip the deny check for reads as an
        // "optimization") would silently start auto-allowing reads of
        // denied paths — and every other test in this file uses either a
        // Write request or a non-denied Read, so none of them would catch
        // that regression.
        let prompter = Arc::new(FakePrompter {
            response: UserResponse::Allow,
            calls: AtomicUsize::new(0),
        });
        let gate = ConfirmationGate::new(
            prompter.clone(),
            vec![PathBuf::from("/home/user/.ssh")],
            vec![],
            PlanMode::new(),
            AutonomousMode::new(),
            PathBuf::from("/home/user/project"),
            false,
        );

        let decision = gate
            .check(&read_request("/home/user/.ssh/id_ed25519"))
            .await;

        assert!(matches!(decision, PermissionDecision::Deny(_)));
        assert_eq!(prompter.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn always_allow_for_a_command_does_not_cover_a_different_argv_with_the_same_program() {
        // Regression test for audit finding #3: PermissionKey::Command used
        // to key the Always-Allow cache by `program` alone, dropping `args`
        // — approving `cargo test` would have silently also auto-approved
        // `cargo build` or any other argv sharing the same program name.
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
            PathBuf::from("/home/user/project"),
            false,
        );

        let test_request = PermissionRequest {
            tool_name: "run_command".to_string(),
            action: ActionKind::Execute,
            target: PermissionTarget::Command {
                program: "cargo".to_string(),
                args: vec!["test".to_string()],
            },
            arguments_preview: serde_json::json!({}),
            preview: None,
            diff: None,
        };
        let build_request = PermissionRequest {
            target: PermissionTarget::Command {
                program: "cargo".to_string(),
                args: vec!["build".to_string()],
            },
            ..test_request.clone()
        };

        let first = gate.check(&test_request).await;
        assert_eq!(first, PermissionDecision::AllowAlways);
        assert_eq!(prompter.calls.load(Ordering::SeqCst), 1);

        // Same program, different args: must prompt again, not hit the
        // `cargo test` cache entry.
        let second = gate.check(&build_request).await;
        assert_eq!(second, PermissionDecision::AllowAlways);
        assert_eq!(prompter.calls.load(Ordering::SeqCst), 2);

        // The original exact command is still cached on its own.
        let third = gate.check(&test_request).await;
        assert_eq!(third, PermissionDecision::AllowAlways);
        assert_eq!(prompter.calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn pre_approved_commands_skip_the_prompt_entirely() {
        let prompter = Arc::new(FakePrompter {
            response: UserResponse::Deny,
            calls: AtomicUsize::new(0),
        });
        let gate = ConfirmationGate::new(
            prompter.clone(),
            vec![],
            vec![("cargo".to_string(), vec!["test".to_string()])],
            PlanMode::new(),
            AutonomousMode::new(),
            PathBuf::from("/home/user/project"),
            false,
        );

        let request = PermissionRequest {
            tool_name: "run_shell".to_string(),
            action: ActionKind::Execute,
            target: PermissionTarget::Command {
                program: "cargo".to_string(),
                args: vec!["test".to_string()],
            },
            arguments_preview: serde_json::json!({}),
            preview: None,
            diff: None,
        };

        let decision = gate.check(&request).await;
        assert_eq!(decision, PermissionDecision::AllowAlways);
        assert_eq!(prompter.calls.load(Ordering::SeqCst), 0);

        // A different argv sharing the same program is NOT pre-approved —
        // the exact-match scoping applies here too.
        let different_args = PermissionRequest {
            target: PermissionTarget::Command {
                program: "cargo".to_string(),
                args: vec!["build".to_string()],
            },
            ..request
        };
        let decision = gate.check(&different_args).await;
        assert!(matches!(decision, PermissionDecision::Deny(_)));
        assert_eq!(prompter.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn plan_mode_denies_mutations_even_when_cached_or_pre_approved() {
        // Locks in the check order: the plan-mode deny sits BEFORE the
        // Always-Allow cache and the pre-approved `allowed_commands` tier.
        // A refactor that consulted the cache first would let any approval
        // granted before plan mode was entered execute during planning —
        // exactly what plan mode promises can't happen.
        let prompter = Arc::new(FakePrompter {
            response: UserResponse::AllowAlways,
            calls: AtomicUsize::new(0),
        });
        let plan_mode = PlanMode::new();
        let gate = ConfirmationGate::new(
            prompter.clone(),
            vec![],
            vec![("cargo".to_string(), vec!["test".to_string()])],
            plan_mode.clone(),
            AutonomousMode::new(),
            PathBuf::from("/home/user/project"),
            false,
        );

        // Cache a write approval while still in Act mode.
        let cached = gate.check(&write_request("/home/user/project/a.rs")).await;
        assert_eq!(cached, PermissionDecision::AllowAlways);
        assert_eq!(prompter.calls.load(Ordering::SeqCst), 1);

        plan_mode.set_active(true);

        // The cached write: denied, no prompt, reason names plan mode.
        let decision = gate.check(&write_request("/home/user/project/a.rs")).await;
        let PermissionDecision::Deny(Some(reason)) = decision else {
            panic!("expected a plan-mode denial with a reason, got {decision:?}");
        };
        assert!(reason.contains("plan mode"));
        assert_eq!(prompter.calls.load(Ordering::SeqCst), 1);

        // The pre-approved command: also denied, also without prompting.
        let pre_approved = PermissionRequest {
            tool_name: "run_command".to_string(),
            action: ActionKind::Execute,
            target: PermissionTarget::Command {
                program: "cargo".to_string(),
                args: vec!["test".to_string()],
            },
            arguments_preview: serde_json::json!({}),
            preview: None,
            diff: None,
        };
        assert!(matches!(
            gate.check(&pre_approved).await,
            PermissionDecision::Deny(_)
        ));
        assert_eq!(prompter.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn plan_mode_leaves_reads_and_internal_actions_untouched() {
        let prompter = Arc::new(FakePrompter {
            response: UserResponse::Deny,
            calls: AtomicUsize::new(0),
        });
        let plan_mode = PlanMode::new();
        plan_mode.set_active(true);
        let gate = ConfirmationGate::new(
            prompter.clone(),
            vec![],
            vec![],
            plan_mode,
            AutonomousMode::new(),
            PathBuf::from("/home/user/project"),
            false,
        );

        let read = gate.check(&read_request("/home/user/project/a.rs")).await;
        assert_eq!(read, PermissionDecision::Allow);

        let internal = PermissionRequest {
            tool_name: "set_tasks".to_string(),
            action: ActionKind::Internal,
            target: PermissionTarget::Other("session task list".to_string()),
            arguments_preview: serde_json::json!({}),
            preview: None,
            diff: None,
        };
        assert_eq!(gate.check(&internal).await, PermissionDecision::Allow);
        assert_eq!(prompter.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn deny_paths_still_win_inside_plan_mode() {
        // Both tiers deny here, but the deny_paths reason must be the one
        // reported — a hard block is stronger information than "wait for
        // plan approval", and this locks the deny_paths check first.
        let prompter = Arc::new(FakePrompter {
            response: UserResponse::Allow,
            calls: AtomicUsize::new(0),
        });
        let plan_mode = PlanMode::new();
        plan_mode.set_active(true);
        let gate = ConfirmationGate::new(
            prompter.clone(),
            vec![PathBuf::from("/home/user/.ssh")],
            vec![],
            plan_mode,
            AutonomousMode::new(),
            PathBuf::from("/home/user/project"),
            false,
        );

        let decision = gate.check(&write_request("/home/user/.ssh/config")).await;
        let PermissionDecision::Deny(Some(reason)) = decision else {
            panic!("expected a denial with a reason");
        };
        assert!(reason.contains("deny_paths"));
    }

    #[tokio::test]
    async fn toggling_plan_mode_off_restores_cached_approvals_without_reprompting() {
        let prompter = Arc::new(FakePrompter {
            response: UserResponse::AllowAlways,
            calls: AtomicUsize::new(0),
        });
        let plan_mode = PlanMode::new();
        let gate = ConfirmationGate::new(
            prompter.clone(),
            vec![],
            vec![],
            plan_mode.clone(),
            AutonomousMode::new(),
            PathBuf::from("/home/user/project"),
            false,
        );

        gate.check(&write_request("/home/user/project/a.rs")).await;
        assert_eq!(prompter.calls.load(Ordering::SeqCst), 1);

        plan_mode.set_active(true);
        assert!(matches!(
            gate.check(&write_request("/home/user/project/a.rs")).await,
            PermissionDecision::Deny(_)
        ));

        plan_mode.set_active(false);
        let restored = gate.check(&write_request("/home/user/project/a.rs")).await;
        assert_eq!(restored, PermissionDecision::AllowAlways);
        assert_eq!(
            prompter.calls.load(Ordering::SeqCst),
            1,
            "cache must survive plan mode"
        );
    }

    #[tokio::test]
    async fn plain_deny_is_never_cached() {
        let prompter = Arc::new(FakePrompter {
            response: UserResponse::Deny,
            calls: AtomicUsize::new(0),
        });
        let gate = ConfirmationGate::new(
            prompter.clone(),
            vec![],
            vec![],
            PlanMode::new(),
            AutonomousMode::new(),
            PathBuf::from("/home/user/project"),
            false,
        );

        gate.check(&write_request("/home/user/project/a.rs")).await;
        gate.check(&write_request("/home/user/project/a.rs")).await;

        assert_eq!(prompter.calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn autonomous_mode_allows_edits_inside_cwd() {
        let prompter = Arc::new(FakePrompter {
            response: UserResponse::Deny,
            calls: AtomicUsize::new(0),
        });
        let autonomous_mode = AutonomousMode::new();
        autonomous_mode.set_active(true);
        let gate = ConfirmationGate::new(
            prompter.clone(),
            vec![],
            vec![],
            PlanMode::new(),
            autonomous_mode,
            PathBuf::from("/home/user/project"),
            false,
        );

        let decision = gate
            .check(&write_request("/home/user/project/src/a.rs"))
            .await;

        assert_eq!(decision, PermissionDecision::Allow);
        assert_eq!(
            prompter.calls.load(Ordering::SeqCst),
            0,
            "autonomous mode must never prompt"
        );
    }

    #[tokio::test]
    async fn autonomous_mode_denies_edits_outside_cwd() {
        let prompter = Arc::new(FakePrompter {
            response: UserResponse::Allow,
            calls: AtomicUsize::new(0),
        });
        let autonomous_mode = AutonomousMode::new();
        autonomous_mode.set_active(true);
        let gate = ConfirmationGate::new(
            prompter.clone(),
            vec![],
            vec![],
            PlanMode::new(),
            autonomous_mode,
            PathBuf::from("/home/user/project"),
            false,
        );

        let request = PermissionRequest {
            tool_name: "write_file".to_string(),
            action: ActionKind::Write,
            target: PermissionTarget::Path(PathBuf::from("/etc/passwd")),
            arguments_preview: serde_json::json!({}),
            preview: None,
            diff: None,
        };
        let decision = gate.check(&request).await;

        let PermissionDecision::Deny(Some(reason)) = decision else {
            panic!("expected a denial with a reason, got {decision:?}");
        };
        assert!(reason.contains("cwd") || reason.contains("worktree"), "reason: {reason}");
        assert_eq!(prompter.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn autonomous_mode_allows_pre_approved_commands_only() {
        let prompter = Arc::new(FakePrompter {
            response: UserResponse::Allow,
            calls: AtomicUsize::new(0),
        });
        let autonomous_mode = AutonomousMode::new();
        autonomous_mode.set_active(true);
        let gate = ConfirmationGate::new(
            prompter.clone(),
            vec![],
            vec![("cargo".to_string(), vec!["test".to_string()])],
            PlanMode::new(),
            autonomous_mode,
            PathBuf::from("/home/user/project"),
            false,
        );

        let approved = PermissionRequest {
            tool_name: "run_command".to_string(),
            action: ActionKind::Execute,
            target: PermissionTarget::Command {
                program: "cargo".to_string(),
                args: vec!["test".to_string()],
            },
            arguments_preview: serde_json::json!({}),
            preview: None,
            diff: None,
        };
        assert_eq!(gate.check(&approved).await, PermissionDecision::AllowAlways);

        let not_approved = PermissionRequest {
            target: PermissionTarget::Command {
                program: "cargo".to_string(),
                args: vec!["publish".to_string()],
            },
            ..approved
        };
        assert!(matches!(
            gate.check(&not_approved).await,
            PermissionDecision::Deny(_)
        ));
        assert_eq!(
            prompter.calls.load(Ordering::SeqCst),
            0,
            "autonomous mode must never fall back to prompting for an unapproved command"
        );
    }

    #[tokio::test]
    async fn autonomous_mode_denies_git_commit_with_no_special_casing() {
        // git_commit's target is a unique commit message every time (Phase 7's
        // design specifically to prevent blanket-approval), so it can never be
        // in the Always-Allow cache — this is a regression test proving the
        // denial falls out of that existing property, not code that could
        // later be "simplified" away.
        let prompter = Arc::new(FakePrompter {
            response: UserResponse::Allow,
            calls: AtomicUsize::new(0),
        });
        let autonomous_mode = AutonomousMode::new();
        autonomous_mode.set_active(true);
        let gate = ConfirmationGate::new(
            prompter.clone(),
            vec![],
            vec![],
            PlanMode::new(),
            autonomous_mode,
            PathBuf::from("/home/user/project"),
            false,
        );

        let request = PermissionRequest {
            tool_name: "git_commit".to_string(),
            action: ActionKind::Execute,
            target: PermissionTarget::Command {
                program: "git".to_string(),
                args: vec!["commit".to_string(), "-m".to_string(), "anything".to_string()],
            },
            arguments_preview: serde_json::json!({}),
            preview: None,
            diff: None,
        };
        assert!(matches!(gate.check(&request).await, PermissionDecision::Deny(_)));
        assert_eq!(prompter.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn autonomous_mode_denies_writes_once_injection_taint_is_flagged() {
        let prompter = Arc::new(FakePrompter {
            response: UserResponse::Allow,
            calls: AtomicUsize::new(0),
        });
        let autonomous_mode = AutonomousMode::new();
        autonomous_mode.set_active(true);
        let injection_taint = InjectionTaint::new();
        injection_taint.flag(InjectionFinding {
            source: "read_file: notes.txt".to_string(),
            matched_pattern: "ignore previous instructions".to_string(),
            excerpt: "...".to_string(),
        });
        let gate = ConfirmationGate::new(
            prompter.clone(),
            vec![],
            vec![],
            PlanMode::new(),
            autonomous_mode,
            PathBuf::from("/home/user/project"),
            false,
        )
        .with_injection_taint(injection_taint);

        let decision = gate
            .check(&write_request("/home/user/project/src/a.rs"))
            .await;

        let PermissionDecision::Deny(Some(reason)) = decision else {
            panic!("expected a denial with a reason, got {decision:?}");
        };
        assert!(reason.contains("notes.txt"), "reason: {reason}");
        assert_eq!(
            prompter.calls.load(Ordering::SeqCst),
            0,
            "autonomous mode must never prompt"
        );
    }

    #[tokio::test]
    async fn autonomous_mode_denies_pre_approved_commands_once_injection_taint_is_flagged() {
        let prompter = Arc::new(FakePrompter {
            response: UserResponse::Allow,
            calls: AtomicUsize::new(0),
        });
        let autonomous_mode = AutonomousMode::new();
        autonomous_mode.set_active(true);
        let injection_taint = InjectionTaint::new();
        injection_taint.flag(InjectionFinding {
            source: "web_fetch: https://example.com".to_string(),
            matched_pattern: "new system prompt".to_string(),
            excerpt: "...".to_string(),
        });
        let gate = ConfirmationGate::new(
            prompter.clone(),
            vec![],
            vec![("cargo".to_string(), vec!["build".to_string()])],
            PlanMode::new(),
            autonomous_mode,
            PathBuf::from("/home/user/project"),
            false,
        )
        .with_injection_taint(injection_taint);

        let request = PermissionRequest {
            tool_name: "run_command".to_string(),
            action: ActionKind::Execute,
            target: PermissionTarget::Command {
                program: "cargo".to_string(),
                args: vec!["build".to_string()],
            },
            arguments_preview: serde_json::json!({}),
            preview: None,
            diff: None,
        };
        let decision = gate.check(&request).await;

        assert!(matches!(decision, PermissionDecision::Deny(_)));
    }

    #[tokio::test]
    async fn autonomous_mode_is_unaffected_when_injection_taint_is_never_flagged() {
        // Regression: the new taint check must not change any existing
        // autonomous-mode behavior when nothing has been flagged.
        let prompter = Arc::new(FakePrompter {
            response: UserResponse::Deny,
            calls: AtomicUsize::new(0),
        });
        let autonomous_mode = AutonomousMode::new();
        autonomous_mode.set_active(true);
        let gate = ConfirmationGate::new(
            prompter.clone(),
            vec![],
            vec![],
            PlanMode::new(),
            autonomous_mode,
            PathBuf::from("/home/user/project"),
            false,
        );

        let decision = gate
            .check(&write_request("/home/user/project/src/a.rs"))
            .await;

        assert_eq!(decision, PermissionDecision::Allow);
    }

    #[tokio::test]
    async fn interactive_mode_still_prompts_normally_when_injection_taint_is_flagged() {
        // Explicitly out of scope (design doc): the interactive
        // confirmation modal is unaffected by the taint flag — a human
        // already reviews the raw diff before approving.
        let prompter = Arc::new(FakePrompter {
            response: UserResponse::Allow,
            calls: AtomicUsize::new(0),
        });
        let injection_taint = InjectionTaint::new();
        injection_taint.flag(InjectionFinding {
            source: "read_file: notes.txt".to_string(),
            matched_pattern: "ignore previous instructions".to_string(),
            excerpt: "...".to_string(),
        });
        let gate = ConfirmationGate::new(
            prompter.clone(),
            vec![],
            vec![],
            PlanMode::new(),
            AutonomousMode::new(),
            PathBuf::from("/home/user/project"),
            false,
        )
        .with_injection_taint(injection_taint);

        let decision = gate
            .check(&write_request("/home/user/project/src/a.rs"))
            .await;

        assert_eq!(decision, PermissionDecision::Allow);
        assert_eq!(
            prompter.calls.load(Ordering::SeqCst),
            1,
            "interactive mode must still prompt"
        );
    }

    #[tokio::test]
    async fn plan_mode_still_wins_over_autonomous_mode_if_both_are_active() {
        // Defense in depth: this is not an expected state (autonomous and
        // plan mode are mutually exclusive at the CLI level, enforced in
        // main.rs), but if it ever happened, the stricter mode must win.
        let prompter = Arc::new(FakePrompter {
            response: UserResponse::Allow,
            calls: AtomicUsize::new(0),
        });
        let plan_mode = PlanMode::new();
        plan_mode.set_active(true);
        let autonomous_mode = AutonomousMode::new();
        autonomous_mode.set_active(true);
        let gate = ConfirmationGate::new(
            prompter.clone(),
            vec![],
            vec![],
            plan_mode,
            autonomous_mode,
            PathBuf::from("/home/user/project"),
            false,
        );

        let decision = gate
            .check(&write_request("/home/user/project/src/a.rs"))
            .await;
        let PermissionDecision::Deny(Some(reason)) = decision else {
            panic!("expected a plan-mode denial, got {decision:?}");
        };
        assert!(reason.contains("plan mode"));
    }

    #[tokio::test]
    async fn mcp_tool_actions_are_confirm_gated_not_auto_allowed() {
        let prompter = Arc::new(FakePrompter {
            response: UserResponse::Allow,
            calls: AtomicUsize::new(0),
        });
        let gate = ConfirmationGate::new(
            prompter.clone(),
            vec![],
            vec![],
            PlanMode::new(),
            AutonomousMode::new(),
            PathBuf::from("/home/user/project"),
            false,
        );

        let request = PermissionRequest {
            tool_name: "mcp__filesystem__search_docs".to_string(),
            action: ActionKind::McpTool,
            target: PermissionTarget::Other("search_docs (server: filesystem)".to_string()),
            arguments_preview: serde_json::json!({}),
            preview: None,
            diff: None,
        };

        let decision = gate.check(&request).await;
        assert_eq!(decision, PermissionDecision::Allow);
        // The point of this test: unlike Read/Internal, the prompter was
        // actually invoked — this call did NOT short-circuit to auto-allow.
        assert_eq!(prompter.calls.load(Ordering::SeqCst), 1);
    }

    fn interact_request() -> PermissionRequest {
        PermissionRequest {
            tool_name: "repl_send".to_string(),
            action: ActionKind::Interact,
            target: PermissionTarget::Other("repl session".to_string()),
            arguments_preview: serde_json::json!({}),
            preview: None,
            diff: None,
        }
    }

    #[tokio::test]
    async fn interact_actions_auto_allow_in_act_mode_without_prompting() {
        let prompter = Arc::new(FakePrompter {
            response: UserResponse::Deny,
            calls: AtomicUsize::new(0),
        });
        let gate = ConfirmationGate::new(
            prompter.clone(),
            vec![],
            vec![],
            PlanMode::new(),
            AutonomousMode::new(),
            PathBuf::from("/home/user/project"),
            false,
        );

        let decision = gate.check(&interact_request()).await;
        assert_eq!(decision, PermissionDecision::Allow);
        assert_eq!(
            prompter.calls.load(Ordering::SeqCst),
            0,
            "Interact must auto-allow without prompting"
        );
    }

    #[tokio::test]
    async fn interact_actions_are_denied_during_plan_mode() {
        // Regression test for the leak-through failure mode this tier's
        // placement specifically guards against: a session still running
        // when the user enters plan mode must stop accepting input
        // immediately, not keep auto-allowing because Interact "looks
        // like" Read/Internal.
        let prompter = Arc::new(FakePrompter {
            response: UserResponse::Allow,
            calls: AtomicUsize::new(0),
        });
        let plan_mode = PlanMode::new();
        plan_mode.set_active(true);
        let gate = ConfirmationGate::new(
            prompter.clone(),
            vec![],
            vec![],
            plan_mode,
            AutonomousMode::new(),
            PathBuf::from("/home/user/project"),
            false,
        );

        let decision = gate.check(&interact_request()).await;
        let PermissionDecision::Deny(Some(reason)) = decision else {
            panic!("expected a plan-mode denial, got {decision:?}");
        };
        assert!(reason.contains("plan mode"));
        assert_eq!(prompter.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn interact_actions_are_denied_in_autonomous_mode() {
        // FakePrompter is set to Allow to prove the denial isn't
        // accidentally coming from the prompter path — autonomous mode
        // must never reach it for an Interact action, mirroring
        // autonomous_mode_denies_mcp_tool_calls_unconditionally.
        let prompter = Arc::new(FakePrompter {
            response: UserResponse::Allow,
            calls: AtomicUsize::new(0),
        });
        let autonomous_mode = AutonomousMode::new();
        autonomous_mode.set_active(true);
        let gate = ConfirmationGate::new(
            prompter.clone(),
            vec![],
            vec![],
            PlanMode::new(),
            autonomous_mode,
            PathBuf::from("/home/user/project"),
            false,
        );

        let decision = gate.check(&interact_request()).await;
        let PermissionDecision::Deny(Some(reason)) = decision else {
            panic!("expected an autonomous-mode denial, got {decision:?}");
        };
        assert!(reason.contains("autonomous"), "reason: {reason}");
        assert_eq!(prompter.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn autonomous_mode_denies_mcp_tool_calls_unconditionally() {
        // Regression test for the final-review finding: in autonomous mode,
        // every MCP tool call must be denied outright — there is no
        // pre-approval tier for MCP tools (unlike Command), and autonomous
        // mode never prompts a human. The FakePrompter is configured to
        // Allow to prove the denial isn't accidentally coming from the
        // prompter path (autonomous mode must never reach it at all).
        let prompter = Arc::new(FakePrompter {
            response: UserResponse::Allow,
            calls: AtomicUsize::new(0),
        });
        let autonomous_mode = AutonomousMode::new();
        autonomous_mode.set_active(true);
        let gate = ConfirmationGate::new(
            prompter.clone(),
            vec![],
            vec![],
            PlanMode::new(),
            autonomous_mode,
            PathBuf::from("/home/user/project"),
            false,
        );

        let request = PermissionRequest {
            tool_name: "mcp__filesystem__search_docs".to_string(),
            action: ActionKind::McpTool,
            target: PermissionTarget::Other("search_docs (server: filesystem)".to_string()),
            arguments_preview: serde_json::json!({}),
            preview: None,
            diff: None,
        };

        let decision = gate.check(&request).await;
        let PermissionDecision::Deny(Some(reason)) = decision else {
            panic!("expected an MCP-tool denial with a reason, got {decision:?}");
        };
        assert!(reason.contains("autonomous"), "reason: {reason}");
        assert_eq!(
            prompter.calls.load(Ordering::SeqCst),
            0,
            "autonomous mode must never prompt, even for MCP tool calls"
        );
    }

    fn memory_request() -> PermissionRequest {
        PermissionRequest {
            tool_name: "remember_preference".to_string(),
            action: ActionKind::Memory,
            target: PermissionTarget::Other("your global preferences (AGENTS.md)".to_string()),
            arguments_preview: serde_json::json!({}),
            preview: None,
            diff: None,
        }
    }

    #[tokio::test]
    async fn autonomous_mode_denies_memory_actions_unconditionally() {
        // Mirrors autonomous_mode_denies_mcp_tool_calls_unconditionally
        // exactly: the FakePrompter is configured to Allow, to prove the
        // denial isn't accidentally coming from the prompter path —
        // autonomous mode must never reach it at all for a Memory action.
        let prompter = Arc::new(FakePrompter {
            response: UserResponse::Allow,
            calls: AtomicUsize::new(0),
        });
        let autonomous_mode = AutonomousMode::new();
        autonomous_mode.set_active(true);
        let gate = ConfirmationGate::new(
            prompter.clone(),
            vec![],
            vec![],
            PlanMode::new(),
            autonomous_mode,
            PathBuf::from("/home/user/project"),
            false,
        );

        let decision = gate.check(&memory_request()).await;
        assert!(matches!(decision, PermissionDecision::Deny(_)));
        assert_eq!(prompter.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn memory_actions_are_never_cached_even_after_always_allow() {
        // Mirrors always_allow_caches_per_exact_target_only's structure,
        // but proves the OPPOSITE property for ActionKind::Memory: two
        // calls with the identical (fixed) target must both reach the
        // prompter — `calls` must be 2, not 1 — since a fixed target
        // description means "same target" would otherwise wrongly imply
        // "same proposed content" the way it correctly does for
        // PermissionTarget::Path.
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
            PathBuf::from("/home/user/project"),
            false,
        );

        let first = gate.check(&memory_request()).await;
        assert_eq!(first, PermissionDecision::AllowAlways);
        assert_eq!(prompter.calls.load(Ordering::SeqCst), 1);

        let second = gate.check(&memory_request()).await;
        assert_eq!(second, PermissionDecision::AllowAlways);
        assert_eq!(prompter.calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn memory_actions_are_not_affected_by_deny_paths() {
        let gate = ConfirmationGate::new(
            Arc::new(FakePrompter {
                response: UserResponse::Allow,
                calls: AtomicUsize::new(0),
            }),
            vec![PathBuf::from("/home/user/.config/aivyx-coder")],
            vec![],
            PlanMode::new(),
            AutonomousMode::new(),
            PathBuf::from("/home/user/project"),
            false,
        );

        // A Memory action's target is PermissionTarget::Other, never
        // Path — is_denied (confirmation.rs:130-134) only matches Path,
        // so this must be false regardless of deny_paths content.
        assert!(!gate.is_denied(&memory_request()));
    }

    #[tokio::test]
    async fn deny_paths_still_wins_over_autonomous_mode() {
        let prompter = Arc::new(FakePrompter {
            response: UserResponse::Allow,
            calls: AtomicUsize::new(0),
        });
        let autonomous_mode = AutonomousMode::new();
        autonomous_mode.set_active(true);
        let gate = ConfirmationGate::new(
            prompter.clone(),
            vec![PathBuf::from("/home/user/project/secret")],
            vec![],
            PlanMode::new(),
            autonomous_mode,
            PathBuf::from("/home/user/project"),
            false,
        );

        let decision = gate
            .check(&write_request("/home/user/project/secret/key"))
            .await;
        let PermissionDecision::Deny(Some(reason)) = decision else {
            panic!("expected a deny_paths denial, got {decision:?}");
        };
        assert!(reason.contains("deny_paths"));
    }

    /// A prompter that never resolves — used to prove the editor-response
    /// branch of the race can win deterministically, without any timing
    /// dependency on how fast a "normal" prompter would answer.
    struct NeverPrompter;

    #[async_trait]
    impl PermissionPrompter for NeverPrompter {
        async fn prompt(&self, _request: &PermissionRequest) -> UserResponse {
            std::future::pending::<()>().await;
            unreachable!("NeverPrompter must never resolve")
        }
    }

    #[tokio::test]
    async fn editor_response_wins_when_terminal_never_answers() {
        let cwd_dir = tempfile::tempdir().unwrap();
        let gate = ConfirmationGate::new(
            Arc::new(NeverPrompter),
            vec![],
            vec![],
            PlanMode::new(),
            AutonomousMode::new(),
            cwd_dir.path().to_path_buf(),
            true,
        );

        let request = PermissionRequest {
            tool_name: "write_file".to_string(),
            action: ActionKind::Write,
            target: PermissionTarget::Path(cwd_dir.path().join("a.rs")),
            arguments_preview: serde_json::json!({}),
            preview: None,
            diff: Some(crate::DiffContent {
                old_content: "old\n".to_string(),
                new_content: "new\n".to_string(),
            }),
        };

        // Poll the request file until it appears, extract the request_id
        // the gate generated, then answer it — mirroring what a real
        // editor plugin would do.
        let req_path = editor_approval::request_path(cwd_dir.path()).unwrap();
        let resp_path = editor_approval::response_path(cwd_dir.path()).unwrap();
        let answer_task = tokio::spawn(async move {
            let request_id = loop {
                if let Ok(content) = tokio::fs::read_to_string(&req_path).await {
                    let value: serde_json::Value = serde_json::from_str(&content).unwrap();
                    break value["request_id"].as_str().unwrap().to_string();
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            };
            let response_json = format!(
                r#"{{ "schema_version": 1, "request_id": "{request_id}", "decision": "allow" }}"#
            );
            tokio::fs::write(&resp_path, response_json).await.unwrap();
        });

        let decision = gate.check(&request).await;
        answer_task.await.unwrap();

        assert_eq!(decision, PermissionDecision::Allow);
    }

    #[tokio::test]
    async fn terminal_still_answers_when_editor_approval_is_disabled() {
        let cwd_dir = tempfile::tempdir().unwrap();
        let prompter = Arc::new(FakePrompter {
            response: UserResponse::Allow,
            calls: AtomicUsize::new(0),
        });
        let gate = ConfirmationGate::new(
            prompter.clone(),
            vec![],
            vec![],
            PlanMode::new(),
            AutonomousMode::new(),
            cwd_dir.path().to_path_buf(),
            false,
        );

        let request = PermissionRequest {
            tool_name: "write_file".to_string(),
            action: ActionKind::Write,
            target: PermissionTarget::Path(cwd_dir.path().join("a.rs")),
            arguments_preview: serde_json::json!({}),
            preview: None,
            diff: Some(crate::DiffContent {
                old_content: "old\n".to_string(),
                new_content: "new\n".to_string(),
            }),
        };

        let decision = gate.check(&request).await;
        assert_eq!(decision, PermissionDecision::Allow);
        assert_eq!(prompter.calls.load(Ordering::SeqCst), 1);

        // No pending-request file should ever have been written.
        let req_path = editor_approval::request_path(cwd_dir.path()).unwrap();
        assert!(!req_path.exists());
    }

    #[tokio::test]
    async fn no_structured_diff_falls_back_to_the_terminal_even_when_enabled() {
        let cwd_dir = tempfile::tempdir().unwrap();
        let prompter = Arc::new(FakePrompter {
            response: UserResponse::Deny,
            calls: AtomicUsize::new(0),
        });
        let gate = ConfirmationGate::new(
            prompter.clone(),
            vec![],
            vec![],
            PlanMode::new(),
            AutonomousMode::new(),
            cwd_dir.path().to_path_buf(),
            true,
        );

        // A Write action with diff: None (the binary-file gap) has no
        // pending content to offer the editor at all.
        let request = PermissionRequest {
            tool_name: "write_file".to_string(),
            action: ActionKind::Write,
            target: PermissionTarget::Path(cwd_dir.path().join("a.bin")),
            arguments_preview: serde_json::json!({}),
            preview: None,
            diff: None,
        };

        let decision = gate.check(&request).await;
        assert!(matches!(decision, PermissionDecision::Deny(_)));
        assert_eq!(prompter.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn always_allow_from_the_editor_populates_the_same_cache() {
        let cwd_dir = tempfile::tempdir().unwrap();
        let gate = ConfirmationGate::new(
            Arc::new(NeverPrompter),
            vec![],
            vec![],
            PlanMode::new(),
            AutonomousMode::new(),
            cwd_dir.path().to_path_buf(),
            true,
        );

        let target_path = cwd_dir.path().join("a.rs");
        let request = PermissionRequest {
            tool_name: "write_file".to_string(),
            action: ActionKind::Write,
            target: PermissionTarget::Path(target_path.clone()),
            arguments_preview: serde_json::json!({}),
            preview: None,
            diff: Some(crate::DiffContent {
                old_content: "old\n".to_string(),
                new_content: "new\n".to_string(),
            }),
        };

        let req_path = editor_approval::request_path(cwd_dir.path()).unwrap();
        let resp_path = editor_approval::response_path(cwd_dir.path()).unwrap();
        let answer_task = tokio::spawn(async move {
            let request_id = loop {
                if let Ok(content) = tokio::fs::read_to_string(&req_path).await {
                    let value: serde_json::Value = serde_json::from_str(&content).unwrap();
                    break value["request_id"].as_str().unwrap().to_string();
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            };
            let response_json = format!(
                r#"{{ "schema_version": 1, "request_id": "{request_id}", "decision": "always_allow" }}"#
            );
            tokio::fs::write(&resp_path, response_json).await.unwrap();
        });

        let decision = gate.check(&request).await;
        answer_task.await.unwrap();
        assert_eq!(decision, PermissionDecision::AllowAlways);

        // Second identical request must now hit the Always-Allow cache
        // without writing a new pending-request file at all.
        let decision2 = gate.check(&request).await;
        assert_eq!(decision2, PermissionDecision::AllowAlways);
        assert!(
            !editor_approval::request_path(cwd_dir.path()).unwrap().exists(),
            "cached Always-Allow must short-circuit before ever reaching the editor race"
        );
    }

    #[tokio::test]
    async fn both_files_are_deleted_after_resolution() {
        let cwd_dir = tempfile::tempdir().unwrap();
        let gate = ConfirmationGate::new(
            Arc::new(NeverPrompter),
            vec![],
            vec![],
            PlanMode::new(),
            AutonomousMode::new(),
            cwd_dir.path().to_path_buf(),
            true,
        );

        let request = PermissionRequest {
            tool_name: "write_file".to_string(),
            action: ActionKind::Write,
            target: PermissionTarget::Path(cwd_dir.path().join("a.rs")),
            arguments_preview: serde_json::json!({}),
            preview: None,
            diff: Some(crate::DiffContent {
                old_content: "old\n".to_string(),
                new_content: "new\n".to_string(),
            }),
        };

        let req_path = editor_approval::request_path(cwd_dir.path()).unwrap();
        let resp_path = editor_approval::response_path(cwd_dir.path()).unwrap();
        let resp_path_for_task = resp_path.clone();
        let req_path_for_task = req_path.clone();
        let answer_task = tokio::spawn(async move {
            let request_id = loop {
                if let Ok(content) = tokio::fs::read_to_string(&req_path_for_task).await {
                    let value: serde_json::Value = serde_json::from_str(&content).unwrap();
                    break value["request_id"].as_str().unwrap().to_string();
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            };
            let response_json = format!(
                r#"{{ "schema_version": 1, "request_id": "{request_id}", "decision": "deny" }}"#
            );
            tokio::fs::write(&resp_path_for_task, response_json).await.unwrap();
        });

        gate.check(&request).await;
        answer_task.await.unwrap();

        assert!(!req_path.exists(), "request file must be deleted after resolution");
        assert!(!resp_path.exists(), "response file must be deleted after resolution");
    }
}
