//! Maps `aivyx-sandbox`'s `PermissionRequest`/`UserResponse` to and from
//! ACP's `session/request_permission`. See
//! `crates/aivyx-sandbox/src/editor_approval.rs` for the sibling mapping
//! this one is modeled on (same `ActionKind` match, different target
//! shape — ACP's `Diff`/`ToolCallUpdate` instead of the bespoke
//! `ApprovalContent` enum).

use agent_client_protocol::schema::v1::{
    Diff, PermissionOption, PermissionOptionKind, RequestPermissionOutcome, RequestPermissionRequest,
    RequestPermissionResponse, SelectedPermissionOutcome, SessionId, ToolCallContent, ToolCallStatus,
    ToolCallUpdate, ToolCallUpdateFields, ToolKind,
};
use agent_client_protocol::{Client, ConnectionTo};
use aivyx_sandbox::{ActionKind, PermissionPrompter, PermissionRequest, PermissionTarget, UserResponse};
use async_trait::async_trait;

const ALLOW_ONCE: &str = "allow_once";
const ALLOW_ALWAYS: &str = "allow_always";
const REJECT_ONCE: &str = "reject_once";
const REJECT_ALWAYS: &str = "reject_always";

/// Every `PermissionOption` field is set through its constructor/builder
/// rather than a struct literal — `PermissionOption` is `#[non_exhaustive]`
/// in `agent-client-protocol-schema` 1.4.0, so a plain struct literal
/// doesn't compile from outside that crate.
///
/// Omits `ALLOW_ALWAYS` when `request`'s target is one
/// `aivyx_sandbox::runs_code_later_for_request` flags (a shell startup
/// file, an XDG autostart entry, a systemd user unit) — same reasoning as
/// `aivyx-tui`'s permission modal hiding its own "Always Allow" button for
/// the same targets (audit finding M1, 2026-10-02): one approval must not
/// silently bless every future edit to a file that runs code outside
/// Landlock's confinement scope.
fn options_for(request: &PermissionRequest) -> Vec<PermissionOption> {
    let mut options = vec![PermissionOption::new(
        ALLOW_ONCE,
        "Allow",
        PermissionOptionKind::AllowOnce,
    )];
    let one_off = aivyx_sandbox::is_one_off_command(request);
    if !one_off && aivyx_sandbox::runs_code_later_for_request(request).is_none() {
        options.push(PermissionOption::new(
            ALLOW_ALWAYS,
            "Always Allow",
            PermissionOptionKind::AllowAlways,
        ));
    }
    options.push(PermissionOption::new(
        REJECT_ONCE,
        "Deny",
        PermissionOptionKind::RejectOnce,
    ));
    if !one_off {
        options.push(PermissionOption::new(
            REJECT_ALWAYS,
            "Always Deny",
            PermissionOptionKind::RejectAlways,
        ));
    }
    options
}

fn target_string(target: &PermissionTarget) -> String {
    match target {
        PermissionTarget::Path(path) => aivyx_sandbox::escape_invisible(&path.display().to_string()),
        PermissionTarget::Command { program, args } => aivyx_sandbox::display_command(program, args),
        PermissionTarget::Other(description) => aivyx_sandbox::escape_invisible(description),
        PermissionTarget::Move { from, to } => aivyx_sandbox::escape_invisible(&format!(
            "{} -> {}",
            from.display(),
            to.display()
        )),
    }
}

/// Builds the `ToolCallUpdate` describing what's pending, reusing
/// `request.diff` for `Write`/`Delete` exactly like
/// `editor_approval::build_pending_request` does — this is the *only*
/// place in `aivyx-acp` where a real diff reaches the client, per the
/// Global Constraints note in the plan.
///
/// `call_id` must be unique per pending permission request within the
/// session — `ToolCallId` is documented in the ACP schema as unique
/// *within the session*, and `PermissionRequest` carries no call-level id
/// of its own (only `tool_name`, e.g. `"write_file"`, which repeats across
/// every call to the same tool). Mirrors `ConfirmationGate::check`'s
/// `request_id` (a fresh `uuid::Uuid::new_v4()` per pending decision) in
/// `crates/aivyx-sandbox/src/confirmation.rs`.
/// The call's raw input for the editor. A command whose tool gives no
/// arguments preview (`run_shell` leaves it `{}`: the TUI already shows the
/// command line) gets its program and arguments, so the editor sees what
/// will run without parsing the title.
fn raw_input(request: &PermissionRequest) -> serde_json::Value {
    match &request.target {
        PermissionTarget::Command { program, args }
            if request.arguments_preview == serde_json::json!({}) =>
        {
            serde_json::json!({ "program": program, "args": args })
        }
        _ => request.arguments_preview.clone(),
    }
}

fn pending_tool_call(
    request: &PermissionRequest,
    call_id: &str,
    project: Option<&std::path::Path>,
) -> ToolCallUpdate {
    let title = target_string(&request.target);
    let mut content = match (request.action, &request.diff) {
        (ActionKind::Write | ActionKind::Delete, Some(diff)) => {
            let path = match &request.target {
                PermissionTarget::Path(p) => p.clone(),
                _ => std::path::PathBuf::from(&title),
            };
            let old_text = if diff.old_content.is_empty() {
                None
            } else {
                Some(diff.old_content.clone())
            };
            vec![ToolCallContent::Diff(
                Diff::new(path, diff.new_content.clone()).old_text(old_text),
            )]
        }
        _ => match &request.preview {
            Some(preview) => {
                // Routes through `ToolCallContent`'s blanket
                // `From<T: Into<ContentBlock>>` impl — `Content` itself has
                // no `From<String>`, so this must target the enum level.
                let content: ToolCallContent = aivyx_sandbox::escape_invisible(preview).into();
                vec![content]
            }
            None => Vec::new(),
        },
    };
    // Same warning aivyx-tui's permission modal shows for the same targets
    // (audit finding M1, 2026-10-02) — appended regardless of which branch
    // above populated `content`, since a Write/Delete with a diff never
    // otherwise gets a chance to say anything beyond the diff itself.
    if let Some(reason) = aivyx_sandbox::runs_code_later_for_request(request) {
        let warning: ToolCallContent = format!(
            "⚠ This file {reason} — approving lets it run code outside the sandbox."
        )
        .into();
        content.push(warning);
    }
    if let Some(note) = project.and_then(|p| aivyx_sandbox::outside_project_for_request(request, p)) {
        let note: ToolCallContent = format!("⚠ {note}").into();
        content.push(note);
    }
    let kind = match request.action {
        ActionKind::Write => ToolKind::Edit,
        ActionKind::Delete => ToolKind::Delete,
        ActionKind::Execute => ToolKind::Execute,
        ActionKind::Move => ToolKind::Move,
        ActionKind::McpTool
        | ActionKind::Memory
        | ActionKind::PersistentMemory
        | ActionKind::Interact => ToolKind::Other,
        ActionKind::Read | ActionKind::Internal | ActionKind::Network => ToolKind::Other,
    };
    let fields = ToolCallUpdateFields::new()
        .kind(kind)
        .status(ToolCallStatus::Pending)
        .title(title)
        .content(content)
        .raw_input(raw_input(request));
    ToolCallUpdate::new(call_id.to_string(), fields)
}

#[cfg(test)]
pub(crate) fn permission_request_to_acp(
    session_id: SessionId,
    request: &PermissionRequest,
    call_id: &str,
) -> RequestPermissionRequest {
    permission_request_to_acp_in(session_id, request, call_id, None)
}

/// With the "outside the project" note when `project` is known.
pub(crate) fn permission_request_to_acp_in(
    session_id: SessionId,
    request: &PermissionRequest,
    call_id: &str,
    project: Option<&std::path::Path>,
) -> RequestPermissionRequest {
    RequestPermissionRequest::new(
        session_id,
        pending_tool_call(request, call_id, project),
        options_for(request),
    )
}

/// `RejectOnce`/`RejectAlways` both deny this one call — aivyx-coder's
/// `UserResponse` has no "always deny" cache concept
/// (`crates/aivyx-sandbox/src/lib.rs:156-160`), so both collapse to
/// `Deny`. A `Cancelled` outcome (the client cancelled the whole prompt
/// turn) also fails closed to `Deny`, as does any future
/// `RequestPermissionOutcome` variant this build doesn't know about yet
/// (the enum is `#[non_exhaustive]`, so the match needs a catch-all arm).
fn acp_response_to_user_response(response: RequestPermissionResponse) -> UserResponse {
    match response.outcome {
        RequestPermissionOutcome::Cancelled => UserResponse::Deny,
        RequestPermissionOutcome::Selected(SelectedPermissionOutcome { option_id, .. }) => {
            match option_id.0.as_ref() {
                ALLOW_ONCE => UserResponse::Allow,
                ALLOW_ALWAYS => UserResponse::AllowAlways,
                _ => UserResponse::Deny,
            }
        }
        _ => UserResponse::Deny,
    }
}

/// The `PermissionPrompter` side of the ACP bridge — structurally
/// parallel to `aivyx-tui`'s `TuiPrompter`
/// (`crates/aivyx-tui/src/permission.rs:24-45`), swapping the
/// `oneshot`-channel-to-render-loop bridge for a real
/// `session/request_permission` round trip.
pub struct AcpPrompter {
    session_id: SessionId,
    connection: ConnectionTo<Client>,
    /// The session's project folder, for the "outside the project" note.
    project: Option<std::path::PathBuf>,
    /// Woken by `session/cancel`: a permission request still unanswered then
    /// is denied instead of waiting for a client that never replies.
    cancel_signal: Option<std::sync::Arc<tokio::sync::Notify>>,
}

impl AcpPrompter {
    pub fn new(session_id: SessionId, connection: ConnectionTo<Client>) -> Self {
        Self { session_id, connection, project: None, cancel_signal: None }
    }

    /// Makes an unanswered permission request end (as a denial) when
    /// `session/cancel` arrives.
    pub fn with_cancel_signal(mut self, signal: std::sync::Arc<tokio::sync::Notify>) -> Self {
        self.cancel_signal = Some(signal);
        self
    }

    /// Notes approvals for changes outside `project`.
    pub fn with_project(mut self, project: std::path::PathBuf) -> Self {
        self.project = Some(project);
        self
    }
}

#[async_trait]
impl PermissionPrompter for AcpPrompter {
    async fn prompt(&self, request: &PermissionRequest) -> UserResponse {
        let call_id = uuid::Uuid::new_v4().to_string();
        let acp_request = permission_request_to_acp_in(
            self.session_id.clone(),
            request,
            &call_id,
            self.project.as_deref(),
        );
        let cancelled = async {
            match &self.cancel_signal {
                Some(signal) => signal.notified().await,
                None => std::future::pending().await,
            }
        };
        let answer = tokio::select! {
            answer = self.connection.send_request(acp_request).block_task() => answer,
            _ = cancelled => return UserResponse::Deny,
        };
        match answer {
            Ok(response) => acp_response_to_user_response(response),
            // Connection gone / request failed — fail closed, matching
            // `TuiPrompter`'s "render loop is gone" fallback.
            Err(_) => UserResponse::Deny,
        }
    }
}

/// A `PermissionPrompter` that blocks until the real `AcpPrompter` is
/// installed, then delegates every call to it. Needed because
/// `ConfirmationGate` (and therefore its prompter) is constructed inside
/// `build_agent`, before `session/new` — and therefore before any
/// `ConnectionTo<Client>` — exists. `NewSessionRequest`'s handler
/// (Task 5) calls `PrompterInstaller::install` exactly once, as soon as
/// it has a real connection and session id.
pub struct DeferredPrompter {
    inner: tokio::sync::Mutex<Option<AcpPrompter>>,
    installed: tokio::sync::Mutex<Option<tokio::sync::oneshot::Receiver<AcpPrompter>>>,
}

pub struct PrompterInstaller(tokio::sync::oneshot::Sender<AcpPrompter>);

impl PrompterInstaller {
    pub fn install(self, prompter: AcpPrompter) {
        let _ = self.0.send(prompter);
    }
}

pub fn deferred_prompter() -> (DeferredPrompter, PrompterInstaller) {
    let (tx, rx) = tokio::sync::oneshot::channel();
    (
        DeferredPrompter {
            inner: tokio::sync::Mutex::new(None),
            installed: tokio::sync::Mutex::new(Some(rx)),
        },
        PrompterInstaller(tx),
    )
}

#[async_trait]
impl PermissionPrompter for DeferredPrompter {
    async fn prompt(&self, request: &PermissionRequest) -> UserResponse {
        let mut inner = self.inner.lock().await;
        if inner.is_none() {
            let mut installed = self.installed.lock().await;
            if let Some(rx) = installed.take()
                && let Ok(prompter) = rx.await
            {
                *inner = Some(prompter);
            }
        }
        match inner.as_ref() {
            Some(prompter) => prompter.prompt(request).await,
            // Should not happen in practice — `session/new` always runs
            // before the first `session/prompt` that could trigger a
            // gated tool call. Fail closed if it somehow does.
            None => UserResponse::Deny,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aivyx_sandbox::DiffContent;
    use std::path::PathBuf;

    fn sid() -> SessionId {
        SessionId::new("sess-1")
    }

    fn json_of(acp: &RequestPermissionRequest) -> String {
        serde_json::to_string(acp).unwrap()
    }

    #[test]
    fn a_command_title_is_quoted_like_the_terminal_shows_it() {
        let request = PermissionRequest {
            tool_name: "run_shell".to_string(),
            action: ActionKind::Execute,
            target: PermissionTarget::Command {
                program: "sh".to_string(),
                args: vec!["-c".to_string(), "python3 -m unittest 2>&1".to_string()],
            },
            arguments_preview: serde_json::json!({}),
            preview: None,
            diff: None,
        };
        let json = json_of(&permission_request_to_acp(sid(), &request, "call-1"));
        assert!(json.contains("sh -c 'python3 -m unittest 2>&1'"), "{json}");
    }

    #[test]
    fn a_write_outside_the_project_is_noted() {
        let request = write_request(None);
        let outside = json_of(&permission_request_to_acp_in(
            sid(),
            &request,
            "call-1",
            Some(std::path::Path::new("/elsewhere")),
        ));
        assert!(outside.contains("outside the project"), "{outside}");
        let inside = json_of(&permission_request_to_acp_in(
            sid(),
            &request,
            "call-1",
            Some(std::path::Path::new("/project")),
        ));
        assert!(!inside.contains("outside the project"), "{inside}");
    }

    fn write_request(diff: Option<DiffContent>) -> PermissionRequest {
        PermissionRequest {
            tool_name: "write_file".to_string(),
            action: ActionKind::Write,
            target: PermissionTarget::Path(PathBuf::from("/project/a.rs")),
            arguments_preview: serde_json::json!({}),
            preview: None,
            diff,
        }
    }

    #[test]
    fn write_with_diff_produces_a_diff_tool_call_content() {
        let request = write_request(Some(DiffContent {
            old_content: "old\n".to_string(),
            new_content: "new\n".to_string(),
        }));
        let acp = permission_request_to_acp(sid(), &request, "call-1");
        assert_eq!(acp.options.len(), 4);
        assert_eq!(acp.tool_call.tool_call_id.0.as_ref(), "call-1");
        let ToolCallContent::Diff(diff) = &acp.tool_call.fields.content.as_ref().unwrap()[0] else {
            panic!("expected a Diff content block");
        };
        assert_eq!(diff.old_text.as_deref(), Some("old\n"));
        assert_eq!(diff.new_text, "new\n");
    }

    #[test]
    fn execute_request_carries_no_diff() {
        let request = PermissionRequest {
            tool_name: "run_command".to_string(),
            action: ActionKind::Execute,
            target: PermissionTarget::Command { program: "cargo".to_string(), args: vec!["test".to_string()] },
            arguments_preview: serde_json::json!({}),
            preview: None,
            diff: None,
        };
        let acp = permission_request_to_acp(sid(), &request, "call-1");
        assert!(acp.tool_call.fields.content.as_ref().unwrap().is_empty());
        assert_eq!(acp.tool_call.fields.kind, Some(ToolKind::Execute));
        assert_eq!(acp.tool_call.tool_call_id.0.as_ref(), "call-1");
    }

    #[test]
    fn move_request_maps_to_move_tool_kind_and_arrow_title() {
        let request = PermissionRequest {
            tool_name: "move_file".to_string(),
            action: ActionKind::Move,
            target: PermissionTarget::Move {
                from: PathBuf::from("/project/old.rs"),
                to: PathBuf::from("/project/new.rs"),
            },
            arguments_preview: serde_json::json!({}),
            preview: None,
            diff: None,
        };
        let acp = permission_request_to_acp(sid(), &request, "call-1");
        assert_eq!(acp.tool_call.fields.kind, Some(ToolKind::Move));
        assert_eq!(
            acp.tool_call.fields.title.as_deref(),
            Some("/project/old.rs -> /project/new.rs")
        );
    }

    #[test]
    fn a_shell_command_with_no_arguments_preview_sends_its_command_as_raw_input() {
        // Live test (2026-10-19): run_shell approvals reached the editor
        // with rawInput {}.
        let request = PermissionRequest {
            tool_name: "run_shell".to_string(),
            action: ActionKind::Execute,
            target: PermissionTarget::Command {
                program: "sh".to_string(),
                args: vec!["-c".to_string(), "ls -la".to_string()],
            },
            arguments_preview: serde_json::json!({}),
            preview: None,
            diff: None,
        };
        let acp = permission_request_to_acp(sid(), &request, "call-1");
        assert_eq!(
            acp.tool_call.fields.raw_input,
            Some(serde_json::json!({ "program": "sh", "args": ["-c", "ls -la"] }))
        );
        // A tool that does give a preview keeps it.
        let named = PermissionRequest {
            arguments_preview: serde_json::json!({ "name": "test" }),
            ..request
        };
        let acp = permission_request_to_acp(sid(), &named, "call-1");
        assert_eq!(acp.tool_call.fields.raw_input, Some(serde_json::json!({ "name": "test" })));
    }

    // --- M1: warn + hide Always Allow for files that run code later ---

    fn shell_startup_write_request() -> PermissionRequest {
        let home = PathBuf::from(std::env::var_os("HOME").expect("HOME must be set"));
        PermissionRequest {
            tool_name: "write_file".to_string(),
            action: ActionKind::Write,
            target: PermissionTarget::Path(home.join(".bashrc")),
            arguments_preview: serde_json::json!({}),
            preview: Some("echo hi".to_string()),
            diff: None,
        }
    }

    #[test]
    fn a_shell_startup_file_write_omits_the_always_allow_option() {
        let request = shell_startup_write_request();
        let acp = permission_request_to_acp(sid(), &request, "call-1");
        assert!(
            acp.options.iter().all(|o| o.option_id.0.as_ref() != ALLOW_ALWAYS),
            "Always Allow must not be offered for a file that runs code later: {:?}",
            acp.options
        );
        // Allow-once and both deny options are still offered.
        assert_eq!(acp.options.len(), 3);
    }

    #[test]
    fn an_ordinary_write_still_offers_always_allow() {
        let request = write_request(None);
        let acp = permission_request_to_acp(sid(), &request, "call-1");
        assert!(acp.options.iter().any(|o| o.option_id.0.as_ref() == ALLOW_ALWAYS));
        assert_eq!(acp.options.len(), 4);
    }

    #[test]
    fn a_shell_startup_file_write_carries_the_sandbox_escape_warning() {
        let request = shell_startup_write_request();
        let acp = permission_request_to_acp(sid(), &request, "call-1");
        let content = acp.tool_call.fields.content.as_ref().unwrap();
        let has_warning = content.iter().any(|c| {
            matches!(c, ToolCallContent::Content(block) if format!("{block:?}").contains("outside the sandbox"))
        });
        assert!(has_warning, "expected a sandbox-escape warning in content: {content:?}");
    }

    #[test]
    fn allow_once_option_maps_to_allow() {
        let response = RequestPermissionResponse::new(RequestPermissionOutcome::Selected(
            SelectedPermissionOutcome::new(ALLOW_ONCE),
        ));
        assert_eq!(acp_response_to_user_response(response), UserResponse::Allow);
    }

    #[test]
    fn allow_always_option_maps_to_allow_always() {
        let response = RequestPermissionResponse::new(RequestPermissionOutcome::Selected(
            SelectedPermissionOutcome::new(ALLOW_ALWAYS),
        ));
        assert_eq!(acp_response_to_user_response(response), UserResponse::AllowAlways);
    }

    #[test]
    fn user_commands_offer_only_allow_and_deny() {
        for name in ["commit", "undo", "redo"] {
            let request = PermissionRequest {
                tool_name: name.to_string(),
                action: aivyx_sandbox::ActionKind::Write,
                target: PermissionTarget::Other(name.to_string()),
                arguments_preview: serde_json::json!({}),
                preview: None,
                diff: None,
            };
            let kinds: Vec<PermissionOptionKind> =
                options_for(&request).into_iter().map(|o| o.kind).collect();
            assert_eq!(
                kinds,
                vec![PermissionOptionKind::AllowOnce, PermissionOptionKind::RejectOnce],
                "{name}"
            );
        }
    }

    #[test]
    fn both_reject_options_and_cancelled_map_to_deny() {
        let reject_once = RequestPermissionResponse::new(RequestPermissionOutcome::Selected(
            SelectedPermissionOutcome::new(REJECT_ONCE),
        ));
        let reject_always = RequestPermissionResponse::new(RequestPermissionOutcome::Selected(
            SelectedPermissionOutcome::new(REJECT_ALWAYS),
        ));
        let cancelled = RequestPermissionResponse::new(RequestPermissionOutcome::Cancelled);
        assert_eq!(acp_response_to_user_response(reject_once), UserResponse::Deny);
        assert_eq!(acp_response_to_user_response(reject_always), UserResponse::Deny);
        assert_eq!(acp_response_to_user_response(cancelled), UserResponse::Deny);
    }

    /// End-to-end regression test for a real bug: `agent-client-protocol`
    /// 1.2.0's response-dispatch ordering wasn't actually enforced despite
    /// being documented as if it were (confirmed by the 2.0 migration
    /// guide's own changelog: "the implementation did not enforce that
    /// ordering" / fixes "the misleading generic failure that previously
    /// appeared when the real interceptor error was lost"). In practice,
    /// this meant `AcpPrompter::prompt`'s `send_request(...).block_task()`
    /// — called from inside a `connection.spawn`'d task, exactly as this
    /// crate's own deadlock-avoidance design requires (see session.rs's
    /// module doc comment) — could receive a spurious `-32601 Method not
    /// found` instead of the real answer a client had genuinely already
    /// sent. A real Zed session hit this directly: every edit was denied
    /// no matter what the user clicked. Unlike the other tests in this
    /// file, this one exercises `AcpPrompter` against a *real*
    /// `agent-client-protocol` connection (in-process duplex streams, no
    /// subprocess, no LLM needed) rather than just the pure mapping
    /// functions — those alone couldn't have caught this, since the bug
    /// was in response routing before `acp_response_to_user_response` ever
    /// runs.
    #[tokio::test]
    async fn prompt_resolves_to_allow_over_a_real_connection() {
        use agent_client_protocol::schema::v1::{InitializeRequest, InitializeResponse};
        use agent_client_protocol::{Agent as AgentRole, Client as ClientRole};
        use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

        let (agent_writer, client_reader) = tokio::io::duplex(65536);
        let (client_writer, agent_reader) = tokio::io::duplex(65536);
        let agent_transport = agent_client_protocol::ByteStreams::new(
            agent_writer.compat_write(),
            agent_reader.compat(),
        );
        let client_transport = agent_client_protocol::ByteStreams::new(
            client_writer.compat_write(),
            client_reader.compat(),
        );

        // Drives the client role: initiates the handshake (which is what
        // triggers the agent's `InitializeRequest` handler below to start
        // the permission round trip) *and* answers the resulting
        // `RequestPermissionRequest` exactly like a human clicking "Allow"
        // in Zed would — both responsibilities on one connection, since a
        // transport can only be claimed by one builder.
        let client_task = tokio::spawn(async move {
            ClientRole
                .builder()
                .on_receive_request(
                    async move |_req: RequestPermissionRequest, responder, _connection| {
                        responder.respond(RequestPermissionResponse::new(
                            RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(
                                ALLOW_ONCE,
                            )),
                        ))
                    },
                    agent_client_protocol::on_receive_request!(),
                )
                .connect_with(client_transport, async |cx| {
                    let _ = cx
                        .send_request(InitializeRequest::new(
                            agent_client_protocol::schema::ProtocolVersion::V1,
                        ))
                        .block_task()
                        .await;
                    // Stay connected long enough for the permission round
                    // trip triggered by the handler above to complete —
                    // it's near-instant in-process, this just needs to
                    // outlast it, well inside the 5s timeout below.
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                    Ok::<(), agent_client_protocol::Error>(())
                })
                .await
        });

        // The result of `AcpPrompter::prompt` travels out of the spawned
        // task via this channel — `block_task()` can only run inside a
        // task spawned via `ConnectionTo::spawn`, exactly mirroring how
        // `session.rs`'s `PromptRequest` handler reaches `AcpPrompter`.
        let (result_tx, result_rx) = tokio::sync::oneshot::channel();
        let result_tx = std::sync::Mutex::new(Some(result_tx));

        // `.connect_to` runs the connection's full receive loop until the
        // transport closes, not just until one request is handled — so it
        // must run in the background alongside the client, not be awaited
        // directly here (this test only needs the one response, not a
        // graceful protocol shutdown).
        let agent_task = tokio::spawn(async move {
            AgentRole
                .builder()
                .on_receive_request(
                    async move |req: InitializeRequest, responder, connection| {
                        let session_id = SessionId::new("test-session");
                        // `AcpPrompter::new` takes the connection by value,
                        // but `connection.spawn(...)` below also needs a
                        // handle — `ConnectionTo` is a cheap `Clone`, same
                        // pattern `session.rs` uses (e.g. `spawn_connection
                        // = connection.clone()`).
                        let prompter = AcpPrompter::new(session_id, connection.clone());
                        let request = write_request(None);
                        let tx = result_tx.lock().unwrap().take();
                        let _ = connection.spawn(async move {
                            let response = prompter.prompt(&request).await;
                            if let Some(tx) = tx {
                                let _ = tx.send(response);
                            }
                            Ok(())
                        });
                        responder.respond(
                            InitializeResponse::new(req.protocol_version).agent_capabilities(
                                agent_client_protocol::schema::v1::AgentCapabilities::new(),
                            ),
                        )
                    },
                    agent_client_protocol::on_receive_request!(),
                )
                .connect_to(agent_transport)
                .await
        });

        let response = tokio::time::timeout(std::time::Duration::from_secs(5), result_rx)
            .await
            .expect("prompt() should resolve within 5s, not hang")
            .expect("result channel should not be dropped without sending");

        assert_eq!(
            response,
            UserResponse::Allow,
            "a client answering allow_once must resolve to UserResponse::Allow, \
             not a spurious denial from a lost/misrouted response"
        );

        client_task.abort();
        agent_task.abort();
    }
}
