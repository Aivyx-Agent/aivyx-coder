//! Editor-side approval of a pending `ConfirmationGate` decision: a small,
//! versioned JSON contract that lets an editor integration answer the same
//! decision the terminal's own confirmation modal is waiting on. See
//! `docs/superpowers/specs/2026-07-19-editor-approval-integration-design.md`
//! for the full design.
//!
//! Lives in `aivyx-sandbox` (not `aivyx-core`, unlike the sibling
//! `editor_context` module in `aivyx-core`) because `ConfirmationGate`,
//! the only consumer, lives here — `aivyx-sandbox` has no dependency on
//! `aivyx-core`, so the module can't live on the other side of that edge.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::{ActionKind, PermissionRequest, PermissionTarget, UserResponse};

/// Bumped if the on-disk shape changes incompatibly. A response file
/// reporting any other value is treated as absent — this is a
/// machine-to-machine contract, not a human-edited config file.
const SCHEMA_VERSION: u32 = 1;

/// How often `poll_for_response` re-reads the response file while waiting.
/// No existing polling-interval constant to reuse in this codebase —
/// `editor_context`'s own refresh is a once-per-turn check, not a
/// sleep-loop. Fast enough to feel responsive, slow enough not to busy-loop.
pub(crate) const POLL_INTERVAL: Duration = Duration::from_millis(250);

/// The action-kind-specific content of a pending request — a tagged union
/// serializing to exactly the shape described in the design spec's Change 3
/// (`action_kind` field plus whichever content fields that kind carries).
#[derive(Debug, Serialize)]
#[serde(tag = "action_kind", rename_all = "snake_case")]
pub(crate) enum ApprovalContent {
    Write {
        old_content: String,
        new_content: String,
    },
    Delete {
        old_content: String,
        will_delete: bool,
    },
    Execute {
        command: String,
        args: Vec<String>,
    },
    McpTool {
        description: String,
    },
}

#[derive(Debug, Serialize)]
pub(crate) struct PendingApprovalRequest {
    pub(crate) schema_version: u32,
    pub(crate) request_id: String,
    pub(crate) target: String,
    #[serde(flatten)]
    pub(crate) content: ApprovalContent,
}

#[derive(Debug, Deserialize)]
struct ApprovalResponse {
    schema_version: u32,
    request_id: String,
    decision: Decision,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Decision {
    Allow,
    Deny,
    AlwaysAllow,
}

/// Where an editor integration writes/reads pending-approval files for this
/// project: `~/.local/state/aivyx-coder/editor-approval/` on Linux, keyed
/// by a stable hash of the canonicalized `cwd` — identical construction to
/// `editor_context_file_path` in `aivyx-core` (own inlined FNV-1a copy,
/// same rationale: the key must be stable across program versions, and
/// `std`'s `DefaultHasher` doesn't guarantee that; this crate can't import
/// `aivyx-core`'s copy since the dependency points the other way).
fn state_subdir_path(cwd: &Path, filename_suffix: &str) -> Option<PathBuf> {
    let dirs = directories::ProjectDirs::from("", "", "aivyx-coder")?;
    let state_dir = dirs
        .state_dir()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| dirs.data_local_dir().to_path_buf());

    let canonical = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
    let key = format!("{:016x}", fnv1a(canonical.to_string_lossy().as_bytes()));
    Some(
        state_dir
            .join("editor-approval")
            .join(format!("{key}-{filename_suffix}.json")),
    )
}

pub(crate) fn request_path(cwd: &Path) -> Option<PathBuf> {
    state_subdir_path(cwd, "request")
}

pub(crate) fn response_path(cwd: &Path) -> Option<PathBuf> {
    state_subdir_path(cwd, "response")
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Builds the content to write for a pending request, or `None` if this
/// request either can't reach the interactive-prompt tier (`Read`/
/// `Internal` — `ConfirmationGate` never calls this for those) or has no
/// structured content to offer the editor at all (a `Write`/`Delete`
/// action whose `diff` is `None` — e.g. `write_file`'s/`delete_file`'s own
/// binary-file fallback, which has no text content to hand a diff viewer).
/// `None` here means the editor-approval channel simply doesn't
/// participate for this one request — the terminal remains the sole
/// surface, exactly like "no editor connected" behaves.
pub(crate) fn build_pending_request(
    request: &PermissionRequest,
    request_id: String,
) -> Option<PendingApprovalRequest> {
    let target = match &request.target {
        PermissionTarget::Path(path) => path.display().to_string(),
        PermissionTarget::Command { program, args } => format!("{program} {}", args.join(" ")),
        PermissionTarget::Other(description) => description.clone(),
    };

    let content = match request.action {
        ActionKind::Write => {
            let diff = request.diff.as_ref()?;
            ApprovalContent::Write {
                old_content: diff.old_content.clone(),
                new_content: diff.new_content.clone(),
            }
        }
        ActionKind::Delete => {
            let diff = request.diff.as_ref()?;
            ApprovalContent::Delete {
                old_content: diff.old_content.clone(),
                will_delete: true,
            }
        }
        ActionKind::Execute => {
            let PermissionTarget::Command { program, args } = &request.target else {
                return None;
            };
            ApprovalContent::Execute {
                command: program.clone(),
                args: args.clone(),
            }
        }
        ActionKind::McpTool => ApprovalContent::McpTool {
            description: request.preview.clone().unwrap_or_else(|| target.clone()),
        },
        // `Memory` and `Interact` fall back to the terminal-only path like Read/Internal:
        // there's no `ApprovalContent` shape defined for them yet, and the
        // editor-approval channel simply not participating for these
        // requests is the documented "no editor connected" fallback above,
        // not a functional regression — the terminal prompt still runs.
        ActionKind::Read | ActionKind::Internal | ActionKind::Memory | ActionKind::Interact => return None,
    };

    Some(PendingApprovalRequest {
        schema_version: SCHEMA_VERSION,
        request_id,
        target,
        content,
    })
}

/// Writes the pending-request file, 0600 (explicit, not relying on the
/// process umask — this file can carry real file content or command text,
/// unlike `editor_context`'s deliberately metadata-only file).
pub(crate) async fn write_pending_request(
    path: &Path,
    pending: &PendingApprovalRequest,
) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let json = serde_json::to_string(pending)
        .map_err(std::io::Error::other)?;
    tokio::fs::write(path, json).await?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).await?;
    }
    Ok(())
}

/// Reads and parses the response file at `path`. `None` on any I/O error,
/// malformed JSON, or a `schema_version` this build doesn't recognize — a
/// missing/leftover/incompatible file is a normal, silent state.
async fn read_response(path: &Path) -> Option<ApprovalResponse> {
    let content = tokio::fs::read_to_string(path).await.ok()?;
    let response: ApprovalResponse = serde_json::from_str(&content).ok()?;
    if response.schema_version != SCHEMA_VERSION {
        return None;
    }
    Some(response)
}

/// Polls `path` every `POLL_INTERVAL` until a response whose `request_id`
/// matches `expected_request_id` appears, then returns the corresponding
/// `UserResponse`. Never returns for a non-matching or absent response —
/// intended to be raced against the terminal's own prompt via
/// `tokio::select!`, which drops whichever branch loses.
pub(crate) async fn poll_for_response(path: &Path, expected_request_id: &str) -> UserResponse {
    loop {
        tokio::time::sleep(POLL_INTERVAL).await;
        if let Some(response) = read_response(path).await
            && response.request_id == expected_request_id
        {
            return match response.decision {
                Decision::Allow => UserResponse::Allow,
                Decision::Deny => UserResponse::Deny,
                Decision::AlwaysAllow => UserResponse::AllowAlways,
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DiffContent;

    #[test]
    fn same_directory_keys_to_the_same_request_and_response_paths() {
        let dir_a = tempfile::tempdir().unwrap();
        let dir_b = tempfile::tempdir().unwrap();

        let a1 = request_path(dir_a.path()).expect("state dir should exist in tests");
        let a2 = request_path(dir_a.path()).unwrap();
        let b = request_path(dir_b.path()).unwrap();

        assert_eq!(a1, a2, "same directory must key to the same request file");
        assert_ne!(a1, b, "different directories must not collide");
        assert_ne!(
            request_path(dir_a.path()).unwrap(),
            response_path(dir_a.path()).unwrap(),
            "request and response paths must differ"
        );
        assert!(a1.to_string_lossy().contains("editor-approval"));
    }

    fn write_request(path: &str, diff: Option<DiffContent>) -> PermissionRequest {
        PermissionRequest {
            tool_name: "write_file".to_string(),
            action: ActionKind::Write,
            target: PermissionTarget::Path(PathBuf::from(path)),
            arguments_preview: serde_json::json!({}),
            preview: None,
            diff,
        }
    }

    #[test]
    fn builds_write_content_from_diff() {
        let request = write_request(
            "/project/a.rs",
            Some(DiffContent {
                old_content: "old\n".to_string(),
                new_content: "new\n".to_string(),
            }),
        );

        let pending = build_pending_request(&request, "req-1".to_string())
            .expect("expected pending content for a write with diff");
        assert_eq!(pending.target, "/project/a.rs");
        let ApprovalContent::Write { old_content, new_content } = pending.content else {
            panic!("expected Write content");
        };
        assert_eq!(old_content, "old\n");
        assert_eq!(new_content, "new\n");
    }

    #[test]
    fn write_with_no_diff_produces_no_pending_content() {
        let request = write_request("/project/a.bin", None);
        assert!(
            build_pending_request(&request, "req-1".to_string()).is_none(),
            "no structured diff means no editor-approval participation for this request"
        );
    }

    #[test]
    fn builds_delete_content_with_will_delete_true() {
        let request = PermissionRequest {
            tool_name: "delete_file".to_string(),
            action: ActionKind::Delete,
            target: PermissionTarget::Path(PathBuf::from("/project/gone.txt")),
            arguments_preview: serde_json::json!({}),
            preview: None,
            diff: Some(DiffContent {
                old_content: "bye\n".to_string(),
                new_content: String::new(),
            }),
        };

        let pending = build_pending_request(&request, "req-1".to_string()).unwrap();
        let ApprovalContent::Delete { old_content, will_delete } = pending.content else {
            panic!("expected Delete content");
        };
        assert_eq!(old_content, "bye\n");
        assert!(will_delete);
    }

    #[test]
    fn builds_execute_content_from_command_target() {
        let request = PermissionRequest {
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

        let pending = build_pending_request(&request, "req-1".to_string()).unwrap();
        assert_eq!(pending.target, "cargo test");
        let ApprovalContent::Execute { command, args } = pending.content else {
            panic!("expected Execute content");
        };
        assert_eq!(command, "cargo");
        assert_eq!(args, vec!["test".to_string()]);
    }

    #[test]
    fn builds_mcp_tool_content_from_preview() {
        let request = PermissionRequest {
            tool_name: "mcp__filesystem__search".to_string(),
            action: ActionKind::McpTool,
            target: PermissionTarget::Other("search (server: filesystem)".to_string()),
            arguments_preview: serde_json::json!({}),
            preview: Some("search_docs(query=\"foo\")".to_string()),
            diff: None,
        };

        let pending = build_pending_request(&request, "req-1".to_string()).unwrap();
        let ApprovalContent::McpTool { description } = pending.content else {
            panic!("expected McpTool content");
        };
        assert_eq!(description, "search_docs(query=\"foo\")");
    }

    #[test]
    fn read_and_internal_actions_produce_no_pending_content() {
        let read_request = PermissionRequest {
            tool_name: "read_file".to_string(),
            action: ActionKind::Read,
            target: PermissionTarget::Path(PathBuf::from("/project/a.rs")),
            arguments_preview: serde_json::json!({}),
            preview: None,
            diff: None,
        };
        assert!(build_pending_request(&read_request, "req-1".to_string()).is_none());
    }

    #[tokio::test]
    async fn write_then_read_round_trips_the_response() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("response.json");
        tokio::fs::write(
            &path,
            r#"{ "schema_version": 1, "request_id": "abc", "decision": "allow" }"#,
        )
        .await
        .unwrap();

        let response = read_response(&path).await.expect("should parse");
        assert_eq!(response.request_id, "abc");
        assert_eq!(response.decision, Decision::Allow);
    }

    #[tokio::test]
    async fn wrong_schema_version_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("response.json");
        tokio::fs::write(
            &path,
            r#"{ "schema_version": 99, "request_id": "abc", "decision": "allow" }"#,
        )
        .await
        .unwrap();

        assert!(read_response(&path).await.is_none());
    }

    #[tokio::test]
    async fn poll_for_response_ignores_a_mismatched_request_id() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("response.json");
        tokio::fs::write(
            &path,
            r#"{ "schema_version": 1, "request_id": "other-request", "decision": "allow" }"#,
        )
        .await
        .unwrap();

        let result = tokio::time::timeout(
            Duration::from_millis(600),
            poll_for_response(&path, "expected-request"),
        )
        .await;

        assert!(
            result.is_err(),
            "a mismatched request_id must never resolve the poll"
        );
    }

    #[tokio::test]
    async fn poll_for_response_returns_deny_and_always_allow_correctly() {
        let dir = tempfile::tempdir().unwrap();

        let deny_path = dir.path().join("deny.json");
        tokio::fs::write(
            &deny_path,
            r#"{ "schema_version": 1, "request_id": "r1", "decision": "deny" }"#,
        )
        .await
        .unwrap();
        assert_eq!(
            poll_for_response(&deny_path, "r1").await,
            UserResponse::Deny
        );

        let always_path = dir.path().join("always.json");
        tokio::fs::write(
            &always_path,
            r#"{ "schema_version": 1, "request_id": "r2", "decision": "always_allow" }"#,
        )
        .await
        .unwrap();
        assert_eq!(
            poll_for_response(&always_path, "r2").await,
            UserResponse::AllowAlways
        );
    }

    #[tokio::test]
    async fn write_pending_request_creates_a_readable_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("request.json");
        let pending = PendingApprovalRequest {
            schema_version: SCHEMA_VERSION,
            request_id: "req-1".to_string(),
            target: "/project/a.rs".to_string(),
            content: ApprovalContent::Write {
                old_content: "old\n".to_string(),
                new_content: "new\n".to_string(),
            },
        };

        write_pending_request(&path, &pending).await.unwrap();

        let written = tokio::fs::read_to_string(&path).await.unwrap();
        assert!(written.contains("\"action_kind\":\"write\""));
        assert!(written.contains("req-1"));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = tokio::fs::metadata(&path).await.unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "request file must be 0600");
        }
    }
}
