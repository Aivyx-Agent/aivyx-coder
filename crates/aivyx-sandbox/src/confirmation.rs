use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use crate::{
    ActionKind, AutonomousMode, PermissionDecision, PermissionGate, PermissionPrompter,
    PermissionRequest, PermissionTarget, PlanMode, UserResponse, path_is_denied,
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
    pub fn new(
        prompter: Arc<dyn PermissionPrompter>,
        deny_paths: Vec<PathBuf>,
        pre_approved_commands: Vec<(String, Vec<String>)>,
        plan_mode: PlanMode,
        autonomous_mode: AutonomousMode,
        cwd: PathBuf,
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
        }
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

        let key = PermissionKey::from_request(request);
        if self.always_allow.lock().unwrap().contains(&key) {
            tracing::info!(
                tool = %request.tool_name,
                action = ?request.action,
                target = ?request.target,
                "permission allowed (cached Always-Allow)"
            );
            return PermissionDecision::AllowAlways;
        }

        let decision = match self.prompter.prompt(request).await {
            UserResponse::Allow => PermissionDecision::Allow,
            UserResponse::AllowAlways => {
                self.always_allow.lock().unwrap().insert(key);
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
        );

        let decision = gate
            .check(&write_request("/home/user/.ssh/id_ed25519"))
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
        );

        let request = PermissionRequest {
            tool_name: "set_tasks".to_string(),
            action: ActionKind::Internal,
            target: PermissionTarget::Other("session task list".to_string()),
            arguments_preview: serde_json::json!({}),
            preview: None,
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
        );

        let read = gate.check(&read_request("/home/user/project/a.rs")).await;
        assert_eq!(read, PermissionDecision::Allow);

        let internal = PermissionRequest {
            tool_name: "set_tasks".to_string(),
            action: ActionKind::Internal,
            target: PermissionTarget::Other("session task list".to_string()),
            arguments_preview: serde_json::json!({}),
            preview: None,
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
        );

        let request = PermissionRequest {
            tool_name: "write_file".to_string(),
            action: ActionKind::Write,
            target: PermissionTarget::Path(PathBuf::from("/etc/passwd")),
            arguments_preview: serde_json::json!({}),
            preview: None,
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
        };
        assert!(matches!(gate.check(&request).await, PermissionDecision::Deny(_)));
        assert_eq!(prompter.calls.load(Ordering::SeqCst), 0);
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
        );

        let decision = gate
            .check(&write_request("/home/user/project/secret/key"))
            .await;
        let PermissionDecision::Deny(Some(reason)) = decision else {
            panic!("expected a deny_paths denial, got {decision:?}");
        };
        assert!(reason.contains("deny_paths"));
    }
}
