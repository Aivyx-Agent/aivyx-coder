use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use crate::{
    ActionKind, PermissionDecision, PermissionGate, PermissionPrompter, PermissionRequest,
    PermissionTarget, UserResponse, path_is_denied,
};

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
/// and `Internal` (session-state-only) actions, otherwise prompts (with an
/// in-memory, per-exact-target Always-Allow cache for the rest of the
/// session).
pub struct ConfirmationGate {
    prompter: Arc<dyn PermissionPrompter>,
    deny_paths: Vec<PathBuf>,
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
    ) -> Self {
        let always_allow = pre_approved_commands
            .into_iter()
            .map(|(program, args)| PermissionKey::Command { program, args })
            .collect();
        Self {
            prompter,
            deny_paths,
            always_allow: Mutex::new(always_allow),
        }
    }

    fn is_denied(&self, request: &PermissionRequest) -> bool {
        let PermissionTarget::Path(path) = &request.target else {
            return false;
        };
        path_is_denied(path, &self.deny_paths)
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
            return PermissionDecision::Deny;
        }

        if matches!(request.action, ActionKind::Read | ActionKind::Internal) {
            return PermissionDecision::Allow;
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
            UserResponse::Deny => PermissionDecision::Deny,
        };
        match decision {
            PermissionDecision::Deny => tracing::warn!(
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
        );

        let decision = gate
            .check(&write_request("/home/user/.ssh/id_ed25519"))
            .await;

        assert_eq!(decision, PermissionDecision::Deny);
        assert_eq!(prompter.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn reads_auto_allow_without_prompting() {
        let prompter = Arc::new(FakePrompter {
            response: UserResponse::Deny,
            calls: AtomicUsize::new(0),
        });
        let gate = ConfirmationGate::new(prompter.clone(), vec![], vec![]);

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
        let gate = ConfirmationGate::new(prompter.clone(), vec![], vec![]);

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
        let gate = ConfirmationGate::new(prompter.clone(), vec![], vec![]);

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
        );

        let decision = gate
            .check(&read_request("/home/user/.ssh/id_ed25519"))
            .await;

        assert_eq!(decision, PermissionDecision::Deny);
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
        let gate = ConfirmationGate::new(prompter.clone(), vec![], vec![]);

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
        assert_eq!(decision, PermissionDecision::Deny);
        assert_eq!(prompter.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn plain_deny_is_never_cached() {
        let prompter = Arc::new(FakePrompter {
            response: UserResponse::Deny,
            calls: AtomicUsize::new(0),
        });
        let gate = ConfirmationGate::new(prompter.clone(), vec![], vec![]);

        gate.check(&write_request("/home/user/project/a.rs")).await;
        gate.check(&write_request("/home/user/project/a.rs")).await;

        assert_eq!(prompter.calls.load(Ordering::SeqCst), 2);
    }
}
