use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use crate::{
    ActionKind, PermissionDecision, PermissionGate, PermissionPrompter, PermissionRequest,
    PermissionTarget, UserResponse,
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

/// The real `PermissionGate`: hard-blocks `deny_paths`, auto-allows reads,
/// otherwise prompts (with an in-memory, per-exact-target Always-Allow
/// cache for the rest of the session).
pub struct ConfirmationGate {
    prompter: Arc<dyn PermissionPrompter>,
    deny_paths: Vec<PathBuf>,
    always_allow: Mutex<HashSet<PermissionKey>>,
}

impl ConfirmationGate {
    pub fn new(prompter: Arc<dyn PermissionPrompter>, deny_paths: Vec<PathBuf>) -> Self {
        Self {
            prompter,
            deny_paths,
            always_allow: Mutex::new(HashSet::new()),
        }
    }

    fn is_denied(&self, request: &PermissionRequest) -> bool {
        let PermissionTarget::Path(path) = &request.target else {
            return false;
        };
        self.deny_paths
            .iter()
            .any(|denied| path.starts_with(denied))
    }
}

#[async_trait]
impl PermissionGate for ConfirmationGate {
    async fn check(&self, request: &PermissionRequest) -> PermissionDecision {
        if self.is_denied(request) {
            return PermissionDecision::Deny;
        }

        if request.action == ActionKind::Read {
            return PermissionDecision::Allow;
        }

        let key = PermissionKey::from_request(request);
        if self.always_allow.lock().unwrap().contains(&key) {
            return PermissionDecision::AllowAlways;
        }

        match self.prompter.prompt(request).await {
            UserResponse::Allow => PermissionDecision::Allow,
            UserResponse::AllowAlways => {
                self.always_allow.lock().unwrap().insert(key);
                PermissionDecision::AllowAlways
            }
            UserResponse::Deny => PermissionDecision::Deny,
        }
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
        let gate = ConfirmationGate::new(prompter.clone(), vec![PathBuf::from("/home/user/.ssh")]);

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
        let gate = ConfirmationGate::new(prompter.clone(), vec![]);

        let decision = gate
            .check(&read_request("/home/user/project/src/main.rs"))
            .await;

        assert_eq!(decision, PermissionDecision::Allow);
        assert_eq!(prompter.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn always_allow_caches_per_exact_target_only() {
        let prompter = Arc::new(FakePrompter {
            response: UserResponse::AllowAlways,
            calls: AtomicUsize::new(0),
        });
        let gate = ConfirmationGate::new(prompter.clone(), vec![]);

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
        let gate = ConfirmationGate::new(prompter.clone(), vec![PathBuf::from("/home/user/.ssh")]);

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
        let gate = ConfirmationGate::new(prompter.clone(), vec![]);

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
    async fn plain_deny_is_never_cached() {
        let prompter = Arc::new(FakePrompter {
            response: UserResponse::Deny,
            calls: AtomicUsize::new(0),
        });
        let gate = ConfirmationGate::new(prompter.clone(), vec![]);

        gate.check(&write_request("/home/user/project/a.rs")).await;
        gate.check(&write_request("/home/user/project/a.rs")).await;

        assert_eq!(prompter.calls.load(Ordering::SeqCst), 2);
    }
}
