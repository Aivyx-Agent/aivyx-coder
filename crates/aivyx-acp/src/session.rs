//! Session lifecycle: `session/new` builds the one-and-only session this
//! process will ever host (see the design's Decision 4 — parallelism is
//! achieved by the editor spawning multiple `aivyx --acp` processes, not
//! by this crate hosting multiple sessions), `session/prompt` drives one
//! turn to completion while streaming `AgentEvent`s out as ACP session
//! updates, `session/set_mode` toggles `PlanMode`, and `session/cancel`
//! cancels the running prompt (see `TurnCancellation`).
//!
//! # Deadlock avoidance (read before changing `PromptRequest`'s handler)
//!
//! `agent-client-protocol`'s `Builder` runs every `on_receive_request`
//! handler *inline*, on the single task that also reads incoming JSON-RPC
//! messages off the transport (`agent-client-protocol-2.0.0/src/jsonrpc.rs`,
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
//! This spawned-task requirement isn't just about deadlock avoidance —
//! `agent-client-protocol` 1.2.0 (used through the ACP integration's
//! first live Zed session) had a real bug where response-dispatch
//! ordering wasn't actually enforced despite being documented as if it
//! were, so `AcpPrompter::prompt`'s `block_task()` could receive a
//! spurious `-32601 Method not found` instead of a client's genuine
//! answer — every edit was denied no matter what the user clicked in
//! Zed. Root-caused via a from-scratch, in-process reproduction using
//! only the crate's own public API (see `prompter.rs`'s
//! `prompt_resolves_to_allow_over_a_real_connection` test); fixed by
//! upgrading to 2.0.0, which the migration guide confirms: "the
//! implementation did not enforce that ordering" in 1.x, and 2.0 "routes
//! response-handler failures to the pending local request" instead of
//! surfacing "the misleading generic failure that previously appeared
//! when the real interceptor error was lost." See
//! `docs/HISTORY.md` for the full account.
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
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use agent_client_protocol::schema::v1::{
    AgentCapabilities, CancelNotification, ContentBlock, InitializeRequest, InitializeResponse,
    NewSessionRequest, NewSessionResponse, PromptRequest, PromptResponse, SessionId, SessionMode,
    SessionModeId, SessionModeState, SessionNotification, SessionUpdate, SetSessionModeRequest,
    SetSessionModeResponse, StopReason,
};
use agent_client_protocol::{
    Agent as AcpAgentBuilder, Client, ConnectionTo, Responder, Result, Stdio,
};
use aivyx_core::{Agent, AgentEvent, SpecialistSessionSummary};
use aivyx_sandbox::{InjectionFinding, PlanMode};
use aivyx_types::{MissionPlan, Task};
use tokio::sync::{Mutex, mpsc};
use tokio_util::sync::CancellationToken;

use crate::commands::{
    LocalCommand, acp_local_command, available_commands_update, clear_update, help_update,
};
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
    /// Last-known state from each of the three sources
    /// `translate::build_merged_plan` unions into one ACP `Plan` --
    /// updated by `translate_and_merge` below (and, during an active
    /// turn, by the same underlying free function called directly at the
    /// `select!` loop's call site -- see that call site's own comment for
    /// why). Not persisted across process restart (in-memory only, same
    /// as the TUI's own equivalent fields from Phase 6a).
    tasks: Vec<Task>,
    mission_plan: Option<MissionPlan>,
    open_specialist_sessions: Vec<SpecialistSessionSummary>,
}

impl Session {
    /// Thin wrapper over `translate::translate_event_with_state`,
    /// threading this session's own tracked-state fields into it. See
    /// that function's doc comment for why the real merge logic lives
    /// there (testable with plain values) rather than here.
    fn translate_and_merge(&mut self, event: &AgentEvent) -> Option<SessionUpdate> {
        crate::translate::translate_event_with_state(
            &self.session_id,
            &mut self.tasks,
            &mut self.mission_plan,
            &mut self.open_specialist_sessions,
            event,
        )
    }
}

/// The running prompts' `CancellationToken`s, reachable by the
/// `session/cancel` handler without the session mutex: that mutex is held
/// by the spawned prompt task for the whole turn, and the cancel handler
/// runs inline in the dispatch loop (see the module doc comment), so
/// waiting on it there would only return once the turn it was meant to
/// stop had finished. A plain `std::sync::Mutex` is fine here: it's held
/// only for an insert, a removal or a `cancel()`, never across an
/// `.await`.
///
/// Each `begin` gets a generation number, and every unfinished turn's
/// token is kept under it: prompts can overlap (a second `session/prompt`
/// arriving while the first still runs), and `session/cancel` must stop
/// all of them, while a turn that finishes only ever drops its own token.
#[derive(Clone, Default)]
struct TurnCancellation(Arc<std::sync::Mutex<TurnSlot>>);

#[derive(Default)]
struct TurnSlot {
    generation: u64,
    running: std::collections::HashMap<u64, CancellationToken>,
}

impl TurnCancellation {
    fn slot(&self) -> std::sync::MutexGuard<'_, TurnSlot> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// A fresh token for the prompt about to run, which `cancel` reaches
    /// until `finish` is called with the returned generation.
    fn begin(&self) -> (u64, CancellationToken) {
        let mut slot = self.slot();
        slot.generation += 1;
        let generation = slot.generation;
        let token = CancellationToken::new();
        slot.running.insert(generation, token.clone());
        (generation, token)
    }

    /// Forgets the token `begin` returned `generation` for.
    fn finish(&self, generation: u64) {
        self.slot().running.remove(&generation);
    }

    /// Cancels every running prompt; whether there was one. A cancel with
    /// nothing running is a no-op and never carries over to the next
    /// prompt.
    fn cancel(&self) -> bool {
        let slot = self.slot();
        for token in slot.running.values() {
            token.cancel();
        }
        !slot.running.is_empty()
    }
}

/// The `StopReason` a finished prompt reports. ACP requires `Cancelled`
/// once the client has cancelled the prompt, whatever the turn emitted on
/// the way out (`run_turn` breaks out of its loop on cancellation and may
/// still emit `TurnComplete`). Otherwise the stop reason observed from the
/// turn's own terminal event wins, falling back to whether the turn
/// paused at its iteration cap.
fn final_stop_reason(cancelled: bool, observed: Option<StopReason>, paused: bool) -> StopReason {
    if cancelled {
        return StopReason::Cancelled;
    }
    observed.unwrap_or(if paused {
        StopReason::MaxTurnRequests
    } else {
        StopReason::EndTurn
    })
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

/// Same fix as `aivyx-tui/src/app.rs`'s own `interactive_injection_notice`
/// (identical wording, kept as a separate copy since the two frontends
/// don't share a text-formatting crate): `Agent::record_tool_result` scans
/// and flags shared injection taint regardless of mode, but an ACP session
/// is always the "a human is watching, at the editor" case (`--acp` and
/// `--auto` are mutually exclusive — see the CLI validation in `aivyx`'s
/// `main.rs` — so `AutonomousMode` is never active here, and neither the
/// gate's autonomous branch nor `run_turn`'s own mid-turn taint check ever
/// fire for this frontend). Before this fix, nothing here ever surfaced a
/// flagged finding to the editor. No iteration count, no "stopped" framing
/// — this frontend never halts on the flag, it only names the source.
fn interactive_injection_notice(finding: &InjectionFinding) -> String {
    format!(
        "note: content from {} was flagged as a likely prompt injection this session \
         (matched \"{}\"): \"{}\"",
        finding.source, finding.matched_pattern, finding.excerpt
    )
}

/// The one `terminal` auth method this agent advertises, shared by both
/// `run()` (a real session is possible) and `run_unconfigured()` (no
/// config exists yet, so `session/new` will always fail with
/// `auth_required` until the client runs this method and reconnects).
fn terminal_auth_method() -> agent_client_protocol::schema::v1::AuthMethod {
    // `AuthMethodTerminal` is `#[non_exhaustive]`, so it's built via its
    // own builder methods rather than a struct literal (a struct literal
    // naming every field still doesn't compile outside the defining crate
    // once a struct carries that attribute).
    let terminal_auth = agent_client_protocol::schema::v1::AuthMethodTerminal::new(
        agent_client_protocol::schema::v1::AuthMethodId::new("setup"),
        "Run first-run setup",
    )
    .description("Pick a backend and model, and write config.toml, before this agent can start.")
    .args(vec!["--setup".to_string()])
    .env(std::collections::HashMap::from([(
        "AIVYX_CODER_ACP_TERMINAL_AUTH".to_string(),
        "1".to_string(),
    )]));
    agent_client_protocol::schema::v1::AuthMethod::Terminal(terminal_auth)
}

/// The `code`/`plan` modes `session/new` offers.
fn session_modes() -> SessionModeState {
    SessionModeState::new(
        SessionModeId::new(MODE_CODE),
        vec![
            SessionMode::new(SessionModeId::new(MODE_CODE), "Code"),
            SessionMode::new(SessionModeId::new(MODE_PLAN), "Plan").description(
                "Read-only: the model can read, search, and build a task list, but cannot edit files or run commands.",
            ),
        ],
    )
}

/// Answers `session/new`, then tells the editor's command picker what this
/// agent supports (see `commands.rs`'s `available_commands_update` doc
/// comment for which commands and why). The response goes first: an
/// editor that registers the session when this response arrives (Zed
/// does) drops a `session/update` for a session id it hasn't seen yet.
/// Both calls post to the same outgoing channel, so call order is wire
/// order.
fn respond_then_advertise(
    responder: Responder<NewSessionResponse>,
    connection: &ConnectionTo<Client>,
    session_id: SessionId,
) -> Result<()> {
    let responded =
        responder.respond(NewSessionResponse::new(session_id.clone()).modes(session_modes()));
    let _ = connection.send_notification(SessionNotification::new(
        session_id,
        available_commands_update(),
    ));
    responded
}

/// A minimal ACP server for when no `config.toml` exists yet:
/// `initialize` advertises the same `terminal` auth method `run()` does,
/// but `session/new` always fails with `auth_required` -- there is no
/// configured backend to build a real `Agent` from. The client is
/// expected to launch this process's own `--setup` (per the advertised
/// method's `args`/`env`), then reconnect to a *fresh* `aivyx --acp`
/// process -- which by then finds `Settings::load_existing()` returning
/// `Some` and runs `run()` normally instead of this function.
pub async fn run_unconfigured() -> Result<()> {
    AcpAgentBuilder
        .builder()
        .name("aivyx-coder")
        .on_receive_request(
            async move |req: InitializeRequest, responder, _connection| {
                responder.respond(
                    InitializeResponse::new(req.protocol_version)
                        .agent_capabilities(AgentCapabilities::new())
                        .auth_methods(vec![terminal_auth_method()]),
                )
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |_req: NewSessionRequest, responder, _connection| {
                responder.respond_with_error(agent_client_protocol::Error::auth_required())
            },
            agent_client_protocol::on_receive_request!(),
        )
        .connect_to(Stdio::new())
        .await
}

/// One `session/prompt`, run inside the task `connection.spawn` starts
/// (see the module doc comment for why it can't run in the handler):
/// handles `/help` and `/clear` locally, otherwise drives
/// `Agent::run_turn` with `cancellation` (the token `session/cancel`
/// reaches) while streaming its events out as session updates, then
/// answers the prompt.
async fn run_prompt(
    prompt_state: Arc<Mutex<Option<Session>>>,
    spawn_connection: ConnectionTo<Client>,
    req: PromptRequest,
    responder: Responder<PromptResponse>,
    cancellation: CancellationToken,
) -> Result<()> {
    let mut guard = prompt_state.lock().await;
    let Some(session) = guard.as_mut() else {
        return responder
            .respond_with_error(agent_client_protocol::util::internal_error("no session"));
    };
    let text = extract_prompt_text(&req.prompt);
    // `/help` and `/clear` are handled entirely here -- neither ever
    // reaches `Agent::run_turn` or the model. See `commands.rs`'s own doc
    // comments for why these two are the ones this frontend intercepts
    // rather than just advertises.
    if let Some(local) = acp_local_command(&text) {
        let mut updates = Vec::new();
        match local {
            LocalCommand::Help => updates.push(help_update()),
            LocalCommand::Clear => {
                session.agent.clear_conversation();
                // `clear_conversation` emits `ConversationCleared`, which
                // `translate_and_merge` turns into resetting the tracked
                // tasks/mission/specialist sessions and an empty Plan, so
                // the editor's Plan panel clears too. Drained here, not
                // left for the next turn.
                while let Ok(event) = session.events_rx.try_recv() {
                    updates.extend(session.translate_and_merge(&event));
                }
                updates.push(clear_update());
            }
        }
        for update in updates {
            let _ = spawn_connection
                .send_notification(SessionNotification::new(session.session_id.clone(), update));
        }
        return responder.respond(PromptResponse::new(StopReason::EndTurn));
    }
    // Scoped so the `run` future (which mutably borrows
    // `session.agent` for `Agent::run_turn`'s duration) is
    // fully dropped before `session.agent.last_turn_paused()`
    // needs an immutable borrow of the same field below.
    let (result, mut stop_reason) = {
        let run = session
            .agent
            .run_turn(text, &session.cwd, cancellation.clone());
        tokio::pin!(run);
        let mut stop_reason = None;
        let result = loop {
            tokio::select! {
                result = &mut run => break result,
                Some(event) = session.events_rx.recv() => {
                    if let Some(reason) = crate::translate::terminal_stop_reason(&event) {
                        stop_reason = Some(reason);
                    }
                    // Calls the free function with disjoint
                    // field-path borrows directly, rather
                    // than `session.translate_and_merge(...)`
                    // -- `run` (above) holds `session.agent`
                    // mutably borrowed for this whole loop,
                    // and a `&mut self` method call borrows
                    // the entire `session` value, which the
                    // borrow checker rejects as overlapping.
                    // Field-path arguments to a free
                    // function get disjoint-borrow treatment
                    // instead. The second call site below
                    // (after `run` is dropped) has no such
                    // conflict and uses the method as usual.
                    if let Some(update) = crate::translate::translate_event_with_state(
                        &session.session_id,
                        &mut session.tasks,
                        &mut session.mission_plan,
                        &mut session.open_specialist_sessions,
                        &event,
                    ) {
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
        if let Some(update) = session.translate_and_merge(&event) {
            let _ = spawn_connection
                .send_notification(SessionNotification::new(session.session_id.clone(), update));
        }
    }
    // Peek — never take() — the shared taint flag after
    // every turn, regardless of `result`/`stop_reason`. An
    // ACP session is always the interactive-equivalent
    // case (see `interactive_injection_notice`'s own doc
    // comment), and has no pause-and-resume flow to clear
    // this the way autonomous mode's `.take()` in
    // `aivyx-tui`'s driver loop does — the editor's user
    // already saw every action this turn took, so naming
    // the flagged source is a notice, not a gate.
    if let Some(finding) = session.agent.injection_taint().current() {
        let notice = AgentEvent::Error(interactive_injection_notice(&finding));
        if let Some(update) = crate::translate::translate_event(&session.session_id, &notice) {
            let _ = spawn_connection
                .send_notification(SessionNotification::new(session.session_id.clone(), update));
        }
    }
    // A cancelled prompt reports `Cancelled` even when cancelling made
    // the turn fail (ACP requires it, so the editor doesn't show an error
    // for a stop the user asked for).
    let cancelled = cancellation.is_cancelled();
    if let Err(err) = result
        && !cancelled
    {
        return responder
            .respond_with_error(agent_client_protocol::util::internal_error(err.to_string()));
    }
    let stop_reason = final_stop_reason(cancelled, stop_reason, session.agent.last_turn_paused());
    responder.respond(PromptResponse::new(stop_reason))
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
    // Outside the session lock for the same reason as `plan_mode` — see
    // `TurnCancellation`'s doc comment.
    let turn_cancellation = TurnCancellation::default();
    let cancel_turn = turn_cancellation.clone();

    AcpAgentBuilder
        .builder()
        .name("aivyx-coder")
        .on_receive_request(
            async move |req: InitializeRequest, responder, _connection| {
                responder.respond(
                    InitializeResponse::new(req.protocol_version)
                        .agent_capabilities(AgentCapabilities::new())
                        .auth_methods(vec![terminal_auth_method()]),
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
                    tasks: Vec::new(),
                    mission_plan: None,
                    open_specialist_sessions: Vec::new(),
                });
                drop(guard);
                new_session_exists.store(true, Ordering::Release);
                respond_then_advertise(responder, &connection, session_id)
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
        .on_receive_notification(
            async move |_notification: CancelNotification, _connection| {
                // This process hosts exactly one session (see the module
                // doc comment), so there's only ever one prompt this can
                // be about. Pending `session/request_permission` calls
                // are the client's to answer with `Cancelled` (which
                // `AcpPrompter` maps to Deny); the turn then stops at its
                // next cancellation check, and a running `/test` is killed.
                cancel_turn.cancel();
                Ok(())
            },
            agent_client_protocol::on_receive_notification!(),
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
                // Started here, in the dispatch loop, rather than inside the
                // spawned task: a `session/cancel` sent right after this
                // prompt must find its token even if the task hasn't run
                // yet.
                let turn_cancellation = turn_cancellation.clone();
                let (turn_generation, cancellation) = turn_cancellation.begin();
                let spawn_result = connection.spawn(async move {
                    let responded = run_prompt(
                        prompt_state,
                        spawn_connection,
                        req,
                        responder,
                        cancellation,
                    )
                    .await;
                    turn_cancellation.finish(turn_generation);
                    responded
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
        .connect_to(Stdio::new())
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_client_protocol::schema::v1::{ResourceLink, TextContent};

    /// Zed registers a session when the `session/new` response arrives and
    /// drops updates for a session it doesn't know yet, so the advertised
    /// command list has to follow the response on the wire. Reads the
    /// agent's raw output, so the order checked is the order sent.
    #[tokio::test]
    async fn new_session_response_goes_out_before_the_command_list() {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

        let (agent_writer, client_reader) = tokio::io::duplex(65536);
        let (mut client_writer, agent_reader) = tokio::io::duplex(65536);
        let transport = agent_client_protocol::ByteStreams::new(
            agent_writer.compat_write(),
            agent_reader.compat(),
        );
        let agent_task = tokio::spawn(async move {
            AcpAgentBuilder
                .builder()
                .on_receive_request(
                    async move |_req: NewSessionRequest, responder, connection| {
                        respond_then_advertise(responder, &connection, SessionId::new("s-1"))
                    },
                    agent_client_protocol::on_receive_request!(),
                )
                .connect_to(transport)
                .await
        });

        client_writer
            .write_all(
                b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"session/new\",\
                  \"params\":{\"cwd\":\"/\",\"mcpServers\":[]}}\n",
            )
            .await
            .unwrap();
        let mut lines = BufReader::new(client_reader).lines();
        let mut next = async || -> serde_json::Value {
            let line = tokio::time::timeout(std::time::Duration::from_secs(5), lines.next_line())
                .await
                .expect("the agent should answer within 5s")
                .unwrap()
                .expect("the agent closed its output early");
            serde_json::from_str(&line).unwrap()
        };
        let first = next().await;
        let second = next().await;
        agent_task.abort();

        assert_eq!(
            first["id"], 1,
            "the response must come first: {first} then {second}"
        );
        assert_eq!(first["result"]["sessionId"], "s-1", "{first}");
        assert_eq!(second["method"], "session/update", "{second}");
        assert_eq!(
            second["params"]["update"]["sessionUpdate"], "available_commands_update",
            "{second}"
        );
    }

    #[test]
    fn cancel_cancels_the_running_turns_token() {
        let slot = TurnCancellation::default();
        let (generation, token) = slot.begin();
        assert!(slot.cancel(), "a turn is running");
        assert!(token.is_cancelled());
        slot.finish(generation);
        assert!(!slot.cancel(), "nothing is running once the turn finished");
    }

    #[test]
    fn cancel_with_no_turn_running_is_a_no_op() {
        let slot = TurnCancellation::default();
        assert!(!slot.cancel());
        // ...and doesn't pre-cancel the next turn.
        let (_, token) = slot.begin();
        assert!(!token.is_cancelled());
    }

    #[test]
    fn a_finished_turn_never_clears_a_newer_turns_token() {
        let slot = TurnCancellation::default();
        let (old, old_token) = slot.begin();
        let (_, new_token) = slot.begin();
        slot.finish(old);
        assert!(slot.cancel(), "the newer turn is still running");
        assert!(new_token.is_cancelled());
        assert!(!old_token.is_cancelled());
    }

    #[test]
    fn cancel_reaches_every_overlapping_turn() {
        let slot = TurnCancellation::default();
        let (first, first_token) = slot.begin();
        let (second, second_token) = slot.begin();
        assert!(slot.cancel());
        assert!(first_token.is_cancelled() && second_token.is_cancelled());

        // `finish` drops only its own turn's token.
        let (third, third_token) = slot.begin();
        slot.finish(first);
        slot.finish(second);
        assert!(slot.cancel(), "the third turn is still running");
        assert!(third_token.is_cancelled());
        slot.finish(third);
        assert!(!slot.cancel(), "nothing is running");
    }

    #[test]
    fn a_cancelled_turn_stops_with_cancelled_whatever_else_was_seen() {
        assert_eq!(final_stop_reason(true, None, false), StopReason::Cancelled);
        assert_eq!(
            final_stop_reason(true, Some(StopReason::EndTurn), false),
            StopReason::Cancelled
        );
        assert_eq!(final_stop_reason(true, None, true), StopReason::Cancelled);
    }

    #[test]
    fn an_uncancelled_turn_keeps_its_observed_or_fallback_stop_reason() {
        assert_eq!(
            final_stop_reason(false, Some(StopReason::MaxTurnRequests), false),
            StopReason::MaxTurnRequests
        );
        assert_eq!(final_stop_reason(false, None, false), StopReason::EndTurn);
        assert_eq!(
            final_stop_reason(false, None, true),
            StopReason::MaxTurnRequests
        );
    }

    /// `session/cancel` arrives as a notification carrying the session id;
    /// checks that the protocol type this frontend registers for decodes
    /// the editor's message and reaches the turn's token.
    #[tokio::test]
    async fn a_session_cancel_notification_reaches_the_turn_token() {
        use agent_client_protocol::schema::v1::CancelNotification;
        use tokio::io::AsyncWriteExt;
        use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

        let (agent_writer, _client_reader) = tokio::io::duplex(65536);
        let (mut client_writer, agent_reader) = tokio::io::duplex(65536);
        let transport = agent_client_protocol::ByteStreams::new(
            agent_writer.compat_write(),
            agent_reader.compat(),
        );
        let slot = TurnCancellation::default();
        let (_, token) = slot.begin();
        let handler_slot = slot.clone();
        let agent_task = tokio::spawn(async move {
            AcpAgentBuilder
                .builder()
                .on_receive_notification(
                    async move |_n: CancelNotification, _connection| {
                        handler_slot.cancel();
                        Ok(())
                    },
                    agent_client_protocol::on_receive_notification!(),
                )
                .connect_to(transport)
                .await
        });

        client_writer
            .write_all(
                b"{\"jsonrpc\":\"2.0\",\"method\":\"session/cancel\",\
                  \"params\":{\"sessionId\":\"s-1\"}}\n",
            )
            .await
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), token.cancelled())
            .await
            .expect("session/cancel should cancel the running turn within 5s");
        agent_task.abort();
    }

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

    #[test]
    fn interactive_injection_notice_names_the_flagged_source_and_pattern() {
        let finding = InjectionFinding {
            source: "read_file: notes.txt".to_string(),
            matched_pattern: "ignore previous instructions".to_string(),
            excerpt: "...IGNORE PREVIOUS INSTRUCTIONS...".to_string(),
        };
        let message = interactive_injection_notice(&finding);
        assert!(message.contains("flagged as a likely prompt injection"));
        assert!(message.contains("read_file: notes.txt"));
        assert!(message.contains("ignore previous instructions"));
        // Unlike an autonomous "stopped" notice, this frontend never halts
        // on the flag -- it only names the source.
        assert!(!message.contains("stopped"));
    }

    #[test]
    fn interactive_injection_notice_translates_to_a_visible_agent_message() {
        // Regression coverage for the actual wiring: the notice is emitted
        // as `AgentEvent::Error`, and `translate::translate_event` must
        // turn that into a real `SessionUpdate` the editor renders (not a
        // silently-dropped variant, the way `ContextUsage` deliberately
        // is) -- see `translate.rs`'s own `AgentEvent::Error` arm.
        let finding = InjectionFinding {
            source: "grep: vendor/README.md".to_string(),
            matched_pattern: "disregard all prior".to_string(),
            excerpt: "...disregard all prior instructions...".to_string(),
        };
        let notice = AgentEvent::Error(interactive_injection_notice(&finding));
        let session_id = SessionId::new("test-session");
        let update = crate::translate::translate_event(&session_id, &notice)
            .expect("an Error event must always translate to a visible session update");
        let agent_client_protocol::schema::v1::SessionUpdate::AgentMessageChunk(chunk) = update
        else {
            panic!("expected an AgentMessageChunk carrying the notice text");
        };
        let agent_client_protocol::schema::v1::ContentBlock::Text(text) = chunk.content else {
            panic!("expected a text content block");
        };
        assert!(text.text.contains("grep: vendor/README.md"));
        assert!(text.text.contains("flagged as a likely prompt injection"));
    }
}
