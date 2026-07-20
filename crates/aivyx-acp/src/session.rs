//! Session lifecycle: `session/new` builds the one-and-only session this
//! process will ever host (see the design's Decision 4 — parallelism is
//! achieved by the editor spawning multiple `aivyx --acp` processes, not
//! by this crate hosting multiple sessions), `session/prompt` drives one
//! turn to completion while streaming `AgentEvent`s out as ACP session
//! updates, `session/set_mode` toggles `PlanMode`.
//!
//! # Deadlock avoidance (read before changing `PromptRequest`'s handler)
//!
//! `agent-client-protocol`'s `Builder` runs every `on_receive_request`
//! handler *inline*, on the single task that also reads incoming JSON-RPC
//! messages off the transport (`agent-client-protocol-1.2.0/src/jsonrpc.rs`,
//! `incoming_protocol_actor` in `jsonrpc/incoming_actor.rs`: one `while let
//! Some(message) = my_rx.next().await { ... dispatch_dispatch(handler).await
//! ... }` loop that both dispatches new requests *and* routes incoming
//! responses to whatever `SentRequest`/`block_task()` is waiting on them).
//! The docs on `on_receive_request` say this explicitly: "This callback
//! runs inside the dispatch loop and blocks further message processing
//! until it completes." `SentRequest::block_task()`'s own docs are more
//! blunt: safe only in a task spawned via `ConnectionTo::spawn`, "using it
//! directly in a handler callback will deadlock the connection."
//!
//! `AcpPrompter::prompt()` (`crate::prompter`) does exactly
//! `.send_request(...).block_task().await` for every `session/request_permission`
//! round trip, and `ConfirmationGate` invokes it synchronously, deep inside
//! `Agent::run_turn()`, for any gated tool call. So if `run_turn` executed
//! inline inside the `PromptRequest` handler closure (as connecting the
//! dots on the brief's literal code would have it), the first gated tool
//! call in any real prompt would try to `block_task()` on the very same
//! task that needs to be free to receive that request's response —
//! deadlock, exactly the failure mode the crate's docs warn about.
//!
//! The fix below follows the crate's own recommended pattern
//! (`ConnectionTo::spawn` — "offload work to a background task"): the
//! `PromptRequest` handler itself does *no* awaiting beyond enqueueing a
//! task and returns immediately, handing the `Responder<PromptResponse>`
//! into the spawned future. `dispatch_dispatch` therefore returns right
//! away, freeing the dispatch loop to keep receiving messages — including
//! the `session/request_permission` responses `AcpPrompter` is waiting on,
//! and including a concurrent `session/set_mode` — while the actual
//! `Agent::run_turn` + event-draining work happens on `task_actor`, a
//! separate concurrently-polled actor that spawned tasks run on
//! (`jsonrpc/task_actor.rs`). The final `responder.respond(...)` call is
//! made from inside that spawned task once the turn completes; `Responder`
//! is a plain `Send` value whose `respond()` just posts to an outgoing
//! channel, so nothing requires it to be called before the handler
//! closure returns.
//!
//! One remaining wrinkle: with the turn moved off the dispatch loop, a
//! `SetSessionModeRequest` can genuinely arrive *while* a prompt is in
//! flight. If its handler awaited the same `Arc<Mutex<Option<Session>>>`
//! the spawned prompt task holds for the whole turn, that would block the
//! dispatch loop on a lock that can only be released by a permission
//! response the dispatch loop itself needs to deliver — the same deadlock
//! shape one level down. `PlanMode` is already a cheap `Clone` handle
//! (`Arc<AtomicBool>`, see `aivyx-sandbox`), so `set_mode` is handled
//! against a top-level clone that never touches the session lock at all —
//! it can only race the flag itself, which `ConfirmationGate` already
//! tolerates per `PlanMode`'s own doc comment ("a toggle racing one
//! in-flight permission check ... is acceptable, since the gate
//! re-checks on every call").

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use agent_client_protocol::schema::v1::{
    AgentCapabilities, ContentBlock, InitializeRequest, InitializeResponse, NewSessionRequest,
    NewSessionResponse, PromptRequest, PromptResponse, SessionId, SessionMode, SessionModeId,
    SessionModeState, SessionNotification, SetSessionModeRequest, SetSessionModeResponse,
    StopReason,
};
use agent_client_protocol::{Agent as AcpAgentBuilder, Dispatch, Error as AcpError, Result, Stdio};
use aivyx_core::{Agent, AgentEvent};
use aivyx_sandbox::PlanMode;
use tokio::sync::{mpsc, Mutex};
use tokio_util::sync::CancellationToken;

use crate::prompter::{AcpPrompter, PrompterInstaller};

const MODE_CODE: &str = "code";
const MODE_PLAN: &str = "plan";

/// Everything the ACP frontend needs, built by `aivyx`'s
/// `agent_builder::build_agent` (Task 1) before this crate's `run` takes
/// over. `prompter_installer` exists because `build_agent` had to hand
/// `ConfirmationGate` a `DeferredPrompter` (Task 4) — there's no real
/// `ConnectionTo<Client>` yet at that point — and this is how the real
/// `AcpPrompter` gets installed behind it, exactly once, from inside the
/// `NewSessionRequest` handler below.
pub struct AcpSessionConfig {
    pub agent: Agent,
    pub events_rx: mpsc::UnboundedReceiver<AgentEvent>,
    pub cwd: PathBuf,
    pub plan_mode: PlanMode,
    pub prompter_installer: PrompterInstaller,
}

struct Session {
    agent: Agent,
    events_rx: mpsc::UnboundedReceiver<AgentEvent>,
    cwd: PathBuf,
    session_id: SessionId,
}

/// Turns `req.prompt`'s content blocks into the plain text `Agent::run_turn`
/// expects. Only `ContentBlock::Text` is forwarded — see the Global
/// Constraints note on v1's text-only scope.
fn extract_prompt_text(blocks: &[ContentBlock]) -> String {
    let mut text = String::new();
    let mut dropped_any = false;
    for block in blocks {
        match block {
            ContentBlock::Text(t) => {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(&t.text);
            }
            _ => dropped_any = true,
        }
    }
    if dropped_any {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str("(non-text content in this message was not forwarded)");
    }
    text
}

pub async fn run(config: AcpSessionConfig) -> Result<()> {
    // `plan_mode` is a cheap `Arc<AtomicBool>` clone, kept outside the
    // session lock entirely — see the module doc comment on why
    // `SetSessionModeRequest` must never contend on the same mutex a
    // spawned `PromptRequest` task can hold for a whole turn.
    let plan_mode = config.plan_mode.clone();
    let session_exists = Arc::new(AtomicBool::new(false));

    let state: Arc<Mutex<Option<Session>>> = Arc::new(Mutex::new(None));
    let mut init_config = Some(config);

    let new_session_state = Arc::clone(&state);
    let new_session_exists = Arc::clone(&session_exists);
    let prompt_state = Arc::clone(&state);
    let mode_exists = Arc::clone(&session_exists);

    AcpAgentBuilder
        .builder()
        .name("aivyx")
        .on_receive_request(
            async move |req: InitializeRequest, responder, _connection| {
                responder.respond(
                    InitializeResponse::new(req.protocol_version)
                        .agent_capabilities(AgentCapabilities::new()),
                )
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |req: NewSessionRequest, responder, connection| {
                // Checked via the atomic *before* ever touching the session
                // mutex: once a session exists, a spawned `PromptRequest`
                // task can hold that mutex for an entire turn (see the
                // module doc comment), and this handler runs inline in the
                // dispatch loop — awaiting a contended lock here would
                // block the very loop that has to deliver the permission
                // responses the in-flight turn is waiting on. A
                // second-session rejection never needs the lock at all;
                // the *first* `NewSessionRequest` (the only case that does
                // lock) is always uncontended, since no `PromptRequest` can
                // exist before a session does.
                if new_session_exists.load(Ordering::Acquire) {
                    return responder.respond_with_error(agent_client_protocol::util::internal_error(
                        "this aivyx --acp process already hosts a session — one session per process",
                    ));
                }
                let mut guard = new_session_state.lock().await;
                if guard.is_some() {
                    return responder.respond_with_error(agent_client_protocol::util::internal_error(
                        "this aivyx --acp process already hosts a session — one session per process",
                    ));
                }
                let Some(built) = init_config.take() else {
                    return responder.respond_with_error(agent_client_protocol::util::internal_error(
                        "session already consumed",
                    ));
                };
                let session_id = SessionId::new(uuid::Uuid::new_v4().to_string());
                // Installs the real prompter behind the `DeferredPrompter`
                // `ConfirmationGate` has been holding since `build_agent`
                // ran — every gated tool call in this session (any Write/
                // Delete/Execute/McpTool action) will now reach the real
                // client via `session/request_permission`.
                built
                    .prompter_installer
                    .install(AcpPrompter::new(session_id.clone(), connection.clone()));
                *guard = Some(Session {
                    agent: built.agent,
                    events_rx: built.events_rx,
                    // The client's declared session `cwd` is authoritative
                    // for the protocol (mandatory on `NewSessionRequest`),
                    // not `AcpSessionConfig.cwd` (the directory `build_agent`
                    // happened to be constructed with before any session
                    // existed) — the two are expected to match under
                    // Decision 4 (editor spawns one `aivyx --acp` process
                    // per session/cwd) but the wire value wins if they ever
                    // diverge.
                    cwd: req.cwd.clone(),
                    session_id: session_id.clone(),
                });
                drop(guard);
                new_session_exists.store(true, Ordering::Release);
                let modes = SessionModeState::new(
                    SessionModeId::new(MODE_CODE),
                    vec![
                        SessionMode::new(SessionModeId::new(MODE_CODE), "Code"),
                        SessionMode::new(SessionModeId::new(MODE_PLAN), "Plan").description(
                            "Read-only: the model can read, search, and build a task list, but cannot edit files or run commands.",
                        ),
                    ],
                );
                responder.respond(NewSessionResponse::new(session_id).modes(modes))
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |req: SetSessionModeRequest, responder, _connection| {
                if !mode_exists.load(Ordering::Acquire) {
                    return responder
                        .respond_with_error(agent_client_protocol::util::internal_error("no session"));
                }
                plan_mode.set_active(req.mode_id.0.as_ref() == MODE_PLAN);
                responder.respond(SetSessionModeResponse::new())
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |req: PromptRequest, responder, connection| {
                // Deliberately *not* awaited here beyond the non-blocking
                // `spawn` call itself — see the module doc comment. The
                // handler must return immediately so the dispatch loop
                // stays free to deliver the `session/request_permission`
                // responses `Agent::run_turn` will block on internally
                // (via `AcpPrompter::prompt`'s `block_task()`), and to keep
                // accepting other requests (e.g. `session/set_mode`) while
                // this turn runs.
                let prompt_state = Arc::clone(&prompt_state);
                let spawn_connection = connection.clone();
                let spawn_result = connection.spawn(async move {
                    let mut guard = prompt_state.lock().await;
                    let Some(session) = guard.as_mut() else {
                        return responder.respond_with_error(agent_client_protocol::util::internal_error(
                            "no session",
                        ));
                    };
                    let text = extract_prompt_text(&req.prompt);
                    let cancellation = CancellationToken::new();
                    // Scoped so the `run` future (which mutably borrows
                    // `session.agent` for `Agent::run_turn`'s duration) is
                    // fully dropped before `session.agent.last_turn_paused()`
                    // needs an immutable borrow of the same field below.
                    let (result, mut stop_reason) = {
                        let run = session.agent.run_turn(text, &session.cwd, cancellation);
                        tokio::pin!(run);
                        let mut stop_reason = None;
                        let result = loop {
                            tokio::select! {
                                result = &mut run => break result,
                                Some(event) = session.events_rx.recv() => {
                                    if let Some(reason) = crate::translate::terminal_stop_reason(&event) {
                                        stop_reason = Some(reason);
                                    }
                                    if let Some(update) = crate::translate::translate_event(&session.session_id, &event) {
                                        let _ = spawn_connection.send_notification(SessionNotification::new(
                                            session.session_id.clone(),
                                            update,
                                        ));
                                    }
                                }
                            }
                        };
                        (result, stop_reason)
                    };
                    // Drain anything buffered right at completion (e.g. the
                    // final TextDelta/TurnComplete pair) before responding.
                    while let Ok(event) = session.events_rx.try_recv() {
                        if let Some(reason) = crate::translate::terminal_stop_reason(&event) {
                            stop_reason = Some(reason);
                        }
                        if let Some(update) = crate::translate::translate_event(&session.session_id, &event) {
                            let _ = spawn_connection.send_notification(SessionNotification::new(
                                session.session_id.clone(),
                                update,
                            ));
                        }
                    }
                    if let Err(err) = result {
                        return responder.respond_with_error(agent_client_protocol::util::internal_error(err.to_string()));
                    }
                    let stop_reason = stop_reason.unwrap_or_else(|| {
                        if session.agent.last_turn_paused() {
                            StopReason::MaxTurnRequests
                        } else {
                            StopReason::EndTurn
                        }
                    });
                    responder.respond(PromptResponse::new(stop_reason))
                });
                // A `spawn` failure means the connection's task channel is
                // already gone (shutting down) — the `Responder` moved into
                // the future is simply dropped unpolled in that case; there
                // is no connection left to send a response over anyway.
                let _ = spawn_result;
                Ok(())
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_dispatch(
            async move |message: Dispatch, cx| {
                message.respond_with_error(AcpError::method_not_found(), cx)
            },
            agent_client_protocol::on_receive_dispatch!(),
        )
        .connect_to(Stdio::new())
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_client_protocol::schema::v1::{ResourceLink, TextContent};

    #[test]
    fn concatenates_only_text_blocks_with_newlines() {
        let blocks = vec![
            ContentBlock::Text(TextContent::new("first line")),
            ContentBlock::Text(TextContent::new("second line")),
        ];
        assert_eq!(extract_prompt_text(&blocks), "first line\nsecond line");
    }

    #[test]
    fn empty_prompt_yields_empty_text() {
        assert_eq!(extract_prompt_text(&[]), "");
    }

    #[test]
    fn non_text_block_is_dropped_with_a_trailing_note() {
        let blocks = vec![
            ContentBlock::Text(TextContent::new("keep me")),
            ContentBlock::ResourceLink(ResourceLink::new("file.rs", "file:///a/file.rs")),
        ];
        assert_eq!(
            extract_prompt_text(&blocks),
            "keep me\n(non-text content in this message was not forwarded)"
        );
    }

    #[test]
    fn only_non_text_blocks_yields_just_the_note() {
        let blocks = vec![ContentBlock::ResourceLink(ResourceLink::new(
            "file.rs",
            "file:///a/file.rs",
        ))];
        assert_eq!(
            extract_prompt_text(&blocks),
            "(non-text content in this message was not forwarded)"
        );
    }
}
