//! Three tools for a resumable, multi-exchange conversation with a team
//! specialist -- `spawn_specialist`, `query_specialist`,
//! `close_specialist` -- additive alongside (not a replacement for) the
//! existing one-shot `delegate_to_specialist`. See
//! `docs/superpowers/specs/2026-09-20-nonagon-team-specialist-sessions-design.md`.
//!
//! The load-bearing fact this whole module rests on: `Agent::run_turn`
//! is already safely callable multiple times on the same instance --
//! `delegate_to_specialist::execute`'s own "continue" loop already does
//! exactly that for a single exchange. Parking a specialist `Agent`
//! between tool calls (instead of dropping it after one exchange, as
//! `delegate_to_specialist` does) needs no new capability in `Agent`
//! itself.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aivyx_llm::LlmBackend;
use aivyx_repomap::RepoMap;
use aivyx_sandbox::{
    ActionKind, AutonomousMode, InjectionTaint, PermissionRequest, PermissionTarget, PlanMode,
};
use aivyx_team::TeamConfig;
use aivyx_tools::{
    GitCheckpointer, Tool, ToolError, ToolExecutionContext, ToolExecutor, ToolRegistry,
};
use aivyx_types::{ToolDefinition, ToolOutput};
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use tokio::sync::mpsc;
use tokio::sync::mpsc::UnboundedSender;

use crate::agent::{Agent, AgentConfig, AgentEvent, EditFormat};
use crate::delegate_to_specialist::{
    compute_specialist_registry, specialist_names, specialist_roster_description,
};

/// Mirrors `delegate_to_specialist.rs`'s own identical constants -- same
/// wording, kept consistent across both sibling modules.
const CUTOFF_NOTICE: &str = "\n\n(sub-agent stopped: reached its iteration budget before \
finishing — the above is its best-effort partial result.)";
const INJECTION_CUTOFF_NOTICE: &str = "\n\n(sub-agent stopped: a tool result was flagged as a \
possible prompt injection — the above is its best-effort partial result.)";
const NO_TEXT_RESPONSE: &str = "(the specialist produced no text response)";

/// The barrier-sync channel `run_bounded_exchange` and `forward_task` use
/// to agree the accumulator has caught up -- see `run_bounded_exchange`'s
/// doc comment for the full rationale.
type BarrierSender = mpsc::UnboundedSender<tokio::sync::oneshot::Sender<()>>;

struct ParkedSpecialistSession {
    agent: Agent,
    member: String,
    forward_task: tokio::task::JoinHandle<()>,
    accumulated: Arc<Mutex<String>>,
    /// See `run_bounded_exchange`'s barrier-sync comment: send a oneshot
    /// reply channel here and await it to be sure `forward_task` has
    /// drained every event sent by the exchange that just finished before
    /// `accumulated` is read.
    barrier_tx: BarrierSender,
    last_active: Instant,
}

/// A minimal snapshot of one open specialist session -- interpolated into
/// cap-exceeded error messages (`open_sessions_description`) and exposed
/// to observability consumers (the TUI's mission panel) via
/// `AgentEvent::SpecialistSessionsUpdated`. Deliberately just
/// `(session_id, member)` -- no status/last-active/exchange-count, per
/// this phase's own explicit scope decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpecialistSessionSummary {
    pub session_id: String,
    pub member: String,
}

struct SessionPoolState {
    sessions: HashMap<String, ParkedSpecialistSession>,
    max_concurrent: usize,
    idle_timeout: Duration,
}

impl SessionPoolState {
    /// Removes every session idle longer than `idle_timeout`. Called at
    /// the top of every `take`/`insert_new`/`has_room` so staleness is
    /// judged relative to "now", not to whenever the pool was last
    /// touched -- mirrors `aivyx-mcp-server/src/server.rs`'s
    /// `SessionMap::evict_stale`.
    fn evict_stale(&mut self) {
        let idle_timeout = self.idle_timeout;
        self.sessions
            .retain(|_, s| s.last_active.elapsed() < idle_timeout);
    }
}

/// Shared, keyed pool of parked specialist sessions -- `Arc`-wrapped so
/// it can be cloned into all three tool structs below. A plain
/// `std::sync::Mutex`, not an async one: every critical section here is
/// a short, synchronous `HashMap` operation, never held across an
/// `.await` -- mirrors `aivyx-tools`' own `repl.rs` output-buffer field
/// for the identical reason.
#[derive(Clone)]
pub struct SpecialistSessionPool {
    inner: Arc<Mutex<SessionPoolState>>,
}

impl SpecialistSessionPool {
    pub fn new(max_concurrent: usize, idle_timeout: Duration) -> Self {
        Self {
            inner: Arc::new(Mutex::new(SessionPoolState {
                sessions: HashMap::new(),
                max_concurrent,
                idle_timeout,
            })),
        }
    }

    fn has_room(&self) -> bool {
        let mut state = self.inner.lock().unwrap();
        state.evict_stale();
        state.sessions.len() < state.max_concurrent
    }

    pub fn max_concurrent(&self) -> usize {
        self.inner.lock().unwrap().max_concurrent
    }

    /// A snapshot of every currently-open session's id and member, evicting
    /// stale (idle-timed-out) entries first -- mirroring `take`'s/
    /// `insert_new`'s own pattern, so this really is "every currently-open
    /// session" rather than including entries that have already timed out
    /// but not yet been touched. Sorted by `session_id` for deterministic
    /// output -- backs both `open_sessions_description()`'s error-message
    /// listing and `AgentEvent::SpecialistSessionsUpdated`, both of which
    /// would otherwise reorder arbitrarily between calls (`HashMap`
    /// iteration order is not stable).
    pub fn open_sessions(&self) -> Vec<SpecialistSessionSummary> {
        let mut state = self.inner.lock().unwrap();
        state.evict_stale();
        let mut sessions: Vec<SpecialistSessionSummary> = state
            .sessions
            .iter()
            .map(|(id, session)| SpecialistSessionSummary {
                session_id: id.clone(),
                member: session.member.clone(),
            })
            .collect();
        sessions.sort_by(|a, b| a.session_id.cmp(&b.session_id));
        sessions
    }

    /// A short `"<session_id> (<member>)"` listing of every currently-open
    /// session, comma-separated -- interpolated into `spawn_specialist`'s
    /// cap-exceeded error messages so a model that hits the cap can see
    /// which sessions it could close, mirroring
    /// `delegate_to_specialist.rs`'s own `specialist_names` convention of
    /// giving a model that guessed wrong a recovery path in the same tool
    /// result.
    fn open_sessions_description(&self) -> String {
        self.open_sessions()
            .iter()
            .map(|s| format!("{} ({})", s.session_id, s.member))
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// Removes and returns a session so its turn can run WITHOUT holding
    /// the pool's lock -- the whole point of this method existing
    /// instead of a borrow-returning accessor. `None` if the id doesn't
    /// exist or has gone stale.
    fn take(&self, id: &str) -> Option<ParkedSpecialistSession> {
        let mut state = self.inner.lock().unwrap();
        state.evict_stale();
        state.sessions.remove(id)
    }

    /// Re-inserts a session after its turn completes, refreshing
    /// `last_active` to now.
    fn put_back(&self, id: String, mut session: ParkedSpecialistSession) {
        session.last_active = Instant::now();
        self.inner.lock().unwrap().sessions.insert(id, session);
    }

    /// Inserts a brand-new session, enforcing the concurrent-session cap.
    /// `Err(max_concurrent)` if the pool is already full at the moment of
    /// insertion -- callers should prefer checking `has_room()` first to
    /// avoid running a wasted specialist turn, but this is still checked
    /// here too as the authoritative guard.
    fn insert_new(&self, id: String, session: ParkedSpecialistSession) -> Result<(), usize> {
        let mut state = self.inner.lock().unwrap();
        state.evict_stale();
        if state.sessions.len() >= state.max_concurrent {
            return Err(state.max_concurrent);
        }
        state.sessions.insert(id, session);
        Ok(())
    }
}

/// Shared by all three tools below -- the same fields
/// `DelegateToSpecialistConfig` carries (this phase's tools reuse the
/// identical specialist-construction mechanism), plus the shared pool.
#[derive(Clone)]
pub struct SpecialistSessionsConfig {
    pub llm: Arc<dyn LlmBackend>,
    pub enforcement: crate::specialist_enforcement::SpecialistEnforcementIngredients,
    pub checkpointer: Option<Arc<GitCheckpointer>>,
    pub repo_map: Option<(Arc<RepoMap>, u32)>,
    pub events_tx: UnboundedSender<AgentEvent>,
    pub parent_registry: ToolRegistry,
    pub team: TeamConfig,
    pub plan_mode: PlanMode,
    pub autonomous_mode: AutonomousMode,
    pub injection_taint: InjectionTaint,
    pub context_tokens: u32,
    pub edit_format: EditFormat,
    pub verification: Option<(String, u32)>,
    pub max_iterations: u32,
    pub broker_mode: bool,
    pub pool: SpecialistSessionPool,
}

/// Runs one bounded exchange against `agent` (a `run_turn` call, then the
/// same "continue" loop `delegate_to_specialist::execute` uses for a
/// single exchange, bounded by `config.max_iterations`), draining
/// `accumulated` into the returned `ToolOutput` and leaving it empty for
/// the next exchange (`std::mem::take`, not `Arc::try_unwrap` --
/// `accumulated` must survive for a possible future exchange on the same
/// session, unlike `delegate_to_specialist`'s one-shot teardown). Used by
/// both `spawn_specialist` (the first exchange, right after constructing
/// `agent`) and `query_specialist` (every later exchange on an
/// already-parked `agent`) -- the only difference between the two call
/// sites is whether `agent` was just constructed or was already sitting
/// in the pool.
///
/// `barrier_tx` exists to close a real race: `agent.run_turn` sends every
/// `AgentEvent` for the exchange synchronously into the `sub_tx`/`sub_rx`
/// channel before it returns (confirmed against `Agent::emit`'s plain,
/// non-async `self.events_tx.send(event)`), but `forward_task` (spawned in
/// `build_specialist_agent`) drains that channel and updates `accumulated`
/// on its own schedule as a separately-spawned task -- there is no
/// guarantee it has caught up by the time this function would otherwise
/// read `accumulated` right after `run_turn` resolves.
/// `delegate_to_specialist::execute` sidesteps this by dropping its
/// one-shot specialist (closing the channel) and awaiting `forward_task`
/// to completion before reading its own `accumulated`; that isn't
/// available here since the whole point of this module is to keep the
/// specialist (and its event channel) alive across exchanges. Sending a
/// oneshot reply channel through `barrier_tx` and awaiting the reply
/// instead gives a deterministic synchronization point: `forward_task`'s
/// `biased` `select!` always prefers draining `sub_rx` over servicing a
/// barrier request, so replying to a barrier request guarantees every
/// event sent before that request was already forwarded/accumulated.
async fn run_bounded_exchange(
    agent: &mut Agent,
    input: String,
    ctx: &ToolExecutionContext,
    config: &SpecialistSessionsConfig,
    accumulated: &Arc<Mutex<String>>,
    barrier_tx: &BarrierSender,
) -> ToolOutput {
    let max_iterations = config.max_iterations.max(1);
    let mut result = agent
        .run_turn(input, &ctx.cwd, ctx.cancellation.clone())
        .await;
    let mut iterations_used = 1u32;
    let is_injection_tainted =
        || config.autonomous_mode.active() && config.injection_taint.current().is_some();
    while result.is_ok()
        && agent.last_turn_paused()
        && iterations_used < max_iterations
        && !ctx.cancellation.is_cancelled()
        && !is_injection_tainted()
    {
        iterations_used += 1;
        result = agent
            .run_turn("continue".to_string(), &ctx.cwd, ctx.cancellation.clone())
            .await;
    }
    let paused = result.is_ok() && agent.last_turn_paused();
    let cap_hit = paused && !is_injection_tainted();
    let injection_hit = paused && is_injection_tainted();

    // See the doc comment above: block until `forward_task` confirms it
    // has drained every event this exchange sent, so the read below never
    // races it. If `forward_task` has already exited (the channel is
    // closed), `send` fails and there is nothing to wait for.
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    if barrier_tx.send(reply_tx).is_ok() {
        let _ = reply_rx.await;
    }

    // Drained unconditionally, before branching on `result`: a failed turn
    // can still have emitted `TextDelta` text before it errored (e.g. a
    // stream that yields partial text then an error event), and the
    // session is put back into the pool on both success AND failure --
    // leaving `accumulated` undrained on the `Err` arm would leak this
    // exchange's partial text into the next exchange's output the next
    // time this function runs on the same session.
    let drained = std::mem::take(&mut *accumulated.lock().unwrap());
    match result {
        Err(err) => ToolOutput::Error(format!("specialist turn failed: {err}")),
        Ok(()) => {
            let mut text = drained;
            if injection_hit {
                text.push_str(INJECTION_CUTOFF_NOTICE);
            } else if cap_hit {
                text.push_str(CUTOFF_NOTICE);
            }
            if text.trim().is_empty() {
                text = NO_TEXT_RESPONSE.to_string();
            }
            ToolOutput::Ok(text)
        }
    }
}

/// Builds a fresh specialist `Agent` for `member`, identical construction
/// to `delegate_to_specialist::execute`'s own (same attenuated registry,
/// same shared gate/confiner/checkpointer/injection-taint/plan-mode/
/// autonomous-mode, same event-forwarding task). Returns the agent, its
/// forward-task handle, the shared accumulator, and the barrier-sync
/// sender `run_bounded_exchange` uses to know `forward_task` has caught
/// up (see that function's doc comment) -- the caller
/// (`SpawnSpecialistTool::execute`) still owns running the first exchange
/// and deciding whether to park the result.
fn build_specialist_agent(
    member: &aivyx_team::TeamMember,
    config: &SpecialistSessionsConfig,
    cwd: &std::path::Path,
) -> (
    Agent,
    tokio::task::JoinHandle<()>,
    Arc<Mutex<String>>,
    BarrierSender,
) {
    let specialist_registry = compute_specialist_registry(member, &config.parent_registry);
    let (gate, confiner) =
        crate::specialist_enforcement::scoped_gate_and_confiner(&config.enforcement, member, cwd);
    let mut sub_executor = ToolExecutor::new(specialist_registry, gate, confiner);
    if let Some(checkpointer) = &config.checkpointer {
        sub_executor.set_checkpointer(Arc::clone(checkpointer));
    }

    let (sub_tx, mut sub_rx) = mpsc::unbounded_channel();
    let (barrier_tx, mut barrier_rx) =
        mpsc::unbounded_channel::<tokio::sync::oneshot::Sender<()>>();
    let accumulated = Arc::new(Mutex::new(String::new()));
    let accumulated_for_task = Arc::clone(&accumulated);
    let parent_tx = config.events_tx.clone();
    let forward_task = tokio::spawn(async move {
        // `biased` always checks `sub_rx` first: as long as it has a
        // buffered event, this loop keeps draining it before ever
        // servicing a pending barrier request -- see
        // `run_bounded_exchange`'s doc comment for why that ordering is
        // what makes the barrier reply a valid synchronization point.
        loop {
            tokio::select! {
                biased;
                maybe_event = sub_rx.recv() => {
                    match maybe_event {
                        Some(event) => {
                            if let AgentEvent::TextDelta(text) = &event {
                                accumulated_for_task.lock().unwrap().push_str(text);
                            }
                            let _ = parent_tx.send(AgentEvent::SubAgentActivity(Box::new(event)));
                        }
                        None => break,
                    }
                }
                maybe_barrier = barrier_rx.recv() => {
                    match maybe_barrier {
                        Some(reply) => {
                            // Defensive: with `biased` this should already
                            // be empty, but drain unconditionally so the
                            // reply's guarantee never depends on `select!`
                            // internals.
                            while let Ok(event) = sub_rx.try_recv() {
                                if let AgentEvent::TextDelta(text) = &event {
                                    accumulated_for_task.lock().unwrap().push_str(text);
                                }
                                let _ = parent_tx.send(AgentEvent::SubAgentActivity(Box::new(event)));
                            }
                            let _ = reply.send(());
                        }
                        None => break,
                    }
                }
            }
        }
    });

    let mut agent = Agent::new(
        Arc::clone(&config.llm),
        sub_executor,
        member.persona.clone(),
        AgentConfig {
            max_tool_iterations: 1,
            context_tokens: config.context_tokens,
            edit_format: config.edit_format,
        },
        Arc::default(),
        config.plan_mode.clone(),
        config.autonomous_mode.clone(),
        sub_tx,
    );
    if let Some((map, budget)) = &config.repo_map {
        agent.set_repo_map(Arc::clone(map), *budget);
    }
    if let Some((command, max_retries)) = &config.verification {
        agent.set_verification(command.clone(), *max_retries);
    }
    agent.set_injection_taint(config.injection_taint.clone());
    agent.set_broker_mode(config.broker_mode);

    (agent, forward_task, accumulated, barrier_tx)
}

fn internal_permission_request(tool_name: &str) -> Result<PermissionRequest, ToolError> {
    Ok(PermissionRequest {
        tool_name: tool_name.to_string(),
        action: ActionKind::Internal,
        target: PermissionTarget::Other(tool_name.to_string()),
        arguments_preview: serde_json::json!({}),
        preview: None,
        diff: None,
    })
}

#[derive(Deserialize, JsonSchema)]
struct SpawnSpecialistArgs {
    /// The name of a `TeamConfig` member to spawn a session with -- must
    /// match one of the specialists listed in this tool's own
    /// description, excluding the team's own `lead`.
    member: String,
    /// A complete, self-contained description of the first task for the
    /// specialist -- it starts with no context beyond this text and the
    /// specialist's own persona.
    task: String,
}

pub struct SpawnSpecialistTool {
    config: SpecialistSessionsConfig,
}

impl SpawnSpecialistTool {
    pub fn new(config: SpecialistSessionsConfig) -> Self {
        Self { config }
    }
}

#[async_trait]
impl Tool for SpawnSpecialistTool {
    fn name(&self) -> &str {
        "spawn_specialist"
    }

    fn mutates_outside_session(&self) -> bool {
        false
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name().to_string(),
            description: format!(
                "Start a resumable session with one team specialist -- unlike \
                delegate_to_specialist (a single exchange), the specialist stays alive so \
                you can send it follow-ups with query_specialist, then end it with \
                close_specialist when done. Returns a session_id plus the specialist's \
                response to the first task. Available specialists: {}. At most {} sessions \
                may be open at once.",
                specialist_roster_description(&self.config.team),
                self.config.pool.max_concurrent(),
            ),
            parameters_schema: serde_json::Value::from(schemars::schema_for!(SpawnSpecialistArgs)),
        }
    }

    fn permission_request(
        &self,
        _arguments: &serde_json::Value,
        _cwd: &Path,
    ) -> Result<PermissionRequest, ToolError> {
        internal_permission_request(self.name())
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        ctx: &ToolExecutionContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: SpawnSpecialistArgs = serde_json::from_value(arguments)
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;

        if args.member == self.config.team.lead {
            return Ok(ToolOutput::Error(format!(
                "cannot spawn a session with the team's own lead ({:?}) -- spawn one of the \
                other specialists instead: {}",
                args.member,
                specialist_names(&self.config.team)
            )));
        }
        let Some(member) = self
            .config
            .team
            .members
            .iter()
            .find(|m| m.name == args.member)
        else {
            return Ok(ToolOutput::Error(format!(
                "unknown team member: {:?} -- valid specialists: {}",
                args.member,
                specialist_names(&self.config.team)
            )));
        };

        if !self.config.pool.has_room() {
            return Ok(ToolOutput::Error(format!(
                "cannot open a new specialist session: {} are already open ({}) -- close one \
                with close_specialist first",
                self.config.pool.max_concurrent(),
                self.config.pool.open_sessions_description()
            )));
        }

        let (mut agent, forward_task, accumulated, barrier_tx) =
            build_specialist_agent(member, &self.config, &ctx.cwd);
        let output = run_bounded_exchange(
            &mut agent,
            args.task,
            ctx,
            &self.config,
            &accumulated,
            &barrier_tx,
        )
        .await;

        let ToolOutput::Ok(text) = output else {
            drop(agent);
            let _ = forward_task.await;
            return Ok(output);
        };

        let session_id = uuid::Uuid::new_v4().to_string();
        let session = ParkedSpecialistSession {
            agent,
            member: member.name.clone(),
            forward_task,
            accumulated,
            barrier_tx,
            last_active: Instant::now(),
        };
        if let Err(max) = self.config.pool.insert_new(session_id.clone(), session) {
            return Ok(ToolOutput::Error(format!(
                "cannot open a new specialist session: {max} are already open ({}) -- close \
                one with close_specialist first",
                self.config.pool.open_sessions_description()
            )));
        }

        let _ = self
            .config
            .events_tx
            .send(AgentEvent::SpecialistSessionsUpdated(
                self.config.pool.open_sessions(),
            ));

        Ok(ToolOutput::Ok(format!(
            "session_id: {session_id}\n\n{text}"
        )))
    }
}

#[derive(Deserialize, JsonSchema)]
struct QuerySpecialistArgs {
    /// The session_id returned by a prior spawn_specialist call.
    session_id: String,
    /// The follow-up message to send to the specialist -- it sees this
    /// in addition to everything from its earlier exchanges in this
    /// session.
    message: String,
}

pub struct QuerySpecialistTool {
    config: SpecialistSessionsConfig,
}

impl QuerySpecialistTool {
    pub fn new(config: SpecialistSessionsConfig) -> Self {
        Self { config }
    }
}

#[async_trait]
impl Tool for QuerySpecialistTool {
    fn name(&self) -> &str {
        "query_specialist"
    }

    fn mutates_outside_session(&self) -> bool {
        false
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name().to_string(),
            description: "Send a follow-up message to a specialist session opened with \
                spawn_specialist -- the specialist remembers everything from earlier \
                exchanges in this same session. Returns the specialist's response. Errors if \
                session_id is unknown, already closed, or has expired from inactivity."
                .to_string(),
            parameters_schema: serde_json::Value::from(schemars::schema_for!(QuerySpecialistArgs)),
        }
    }

    fn permission_request(
        &self,
        _arguments: &serde_json::Value,
        _cwd: &Path,
    ) -> Result<PermissionRequest, ToolError> {
        internal_permission_request(self.name())
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        ctx: &ToolExecutionContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: QuerySpecialistArgs = serde_json::from_value(arguments)
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;

        let Some(mut session) = self.config.pool.take(&args.session_id) else {
            return Ok(ToolOutput::Error(format!(
                "unknown session_id: {:?} -- it may not exist, may already be closed, or may \
                have expired from inactivity; call spawn_specialist to start a new one",
                args.session_id
            )));
        };

        let output = run_bounded_exchange(
            &mut session.agent,
            args.message,
            ctx,
            &self.config,
            &session.accumulated,
            &session.barrier_tx,
        )
        .await;

        self.config.pool.put_back(args.session_id, session);
        Ok(output)
    }
}

#[derive(Deserialize, JsonSchema)]
struct CloseSpecialistArgs {
    /// The session_id returned by a prior spawn_specialist call.
    session_id: String,
}

pub struct CloseSpecialistTool {
    config: SpecialistSessionsConfig,
}

impl CloseSpecialistTool {
    pub fn new(config: SpecialistSessionsConfig) -> Self {
        Self { config }
    }
}

#[async_trait]
impl Tool for CloseSpecialistTool {
    fn name(&self) -> &str {
        "close_specialist"
    }

    fn mutates_outside_session(&self) -> bool {
        false
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name().to_string(),
            description: "End a specialist session opened with spawn_specialist, freeing it \
                up so a new session can be opened within the concurrent-session limit. Errors \
                if session_id is unknown or already closed."
                .to_string(),
            parameters_schema: serde_json::Value::from(schemars::schema_for!(CloseSpecialistArgs)),
        }
    }

    fn permission_request(
        &self,
        _arguments: &serde_json::Value,
        _cwd: &Path,
    ) -> Result<PermissionRequest, ToolError> {
        internal_permission_request(self.name())
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        _ctx: &ToolExecutionContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: CloseSpecialistArgs = serde_json::from_value(arguments)
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;

        let Some(session) = self.config.pool.take(&args.session_id) else {
            return Ok(ToolOutput::Error(format!(
                "unknown session_id: {:?} -- it may not exist or may already be closed",
                args.session_id
            )));
        };
        let member = session.member.clone();
        drop(session.agent);
        let _ = session.forward_task.await;

        let _ = self
            .config
            .events_tx
            .send(AgentEvent::SpecialistSessionsUpdated(
                self.config.pool.open_sessions(),
            ));

        Ok(ToolOutput::Ok(format!(
            "specialist session {:?} closed ({member})",
            args.session_id
        )))
    }
}

#[cfg(test)]
mod specialist_session_tests {
    use super::*;
    use aivyx_llm::{ChatRequest, FinishReason, LlmError, StreamEvent};
    use aivyx_sandbox::NoopConfiner;
    use aivyx_team::TeamMember;
    use futures::StreamExt;
    use futures::stream::BoxStream;
    use std::time::Duration;
    use tokio_util::sync::CancellationToken;

    struct MockBackend {
        // Each queued response is itself a sequence of results, not just
        // events: `a_failed_exchange_does_not_leak_text_into_the_next_successful_one`
        // needs a stream that yields a real `TextDelta` and THEN an error,
        // to reproduce a turn that partially streams text before failing
        // (the exact shape `run_turn_inner` hits when a stream's `Err`
        // event arrives after some `Ok(StreamEvent::TextDelta(_))`s).
        responses: Mutex<std::collections::VecDeque<Vec<Result<StreamEvent, String>>>>,
        received: Mutex<Vec<ChatRequest>>,
    }

    impl MockBackend {
        fn new(responses: Vec<Vec<StreamEvent>>) -> Self {
            Self {
                responses: Mutex::new(
                    responses
                        .into_iter()
                        .map(|events| events.into_iter().map(Ok).collect())
                        .collect(),
                ),
                received: Mutex::new(Vec::new()),
            }
        }

        /// Like `new`, but each queued response is a pre-built sequence of
        /// `Ok`/`Err` results, so a response can yield partial text before
        /// failing mid-stream.
        fn new_with_mixed_results(responses: Vec<Vec<Result<StreamEvent, String>>>) -> Self {
            Self {
                responses: Mutex::new(responses.into()),
                received: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl LlmBackend for MockBackend {
        fn model_id(&self) -> &str {
            "mock"
        }

        async fn stream_chat(
            &self,
            request: ChatRequest,
        ) -> Result<BoxStream<'static, Result<StreamEvent, LlmError>>, LlmError> {
            self.received.lock().unwrap().push(request);
            let events = self
                .responses
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_default();
            Ok(
                futures::stream::iter(events.into_iter().map(|r| r.map_err(LlmError::Parse)))
                    .boxed(),
            )
        }
    }

    struct AlwaysAllowPrompter;
    #[async_trait]
    impl aivyx_sandbox::PermissionPrompter for AlwaysAllowPrompter {
        async fn prompt(&self, _request: &PermissionRequest) -> aivyx_sandbox::UserResponse {
            aivyx_sandbox::UserResponse::Allow
        }
    }

    fn text_response(text: &str) -> Vec<StreamEvent> {
        vec![
            StreamEvent::TextDelta(text.to_string()),
            StreamEvent::Done {
                finish_reason: FinishReason::Stop,
            },
        ]
    }

    fn exec_ctx(cwd: &std::path::Path) -> ToolExecutionContext {
        ToolExecutionContext {
            cwd: cwd.to_path_buf(),
            confiner: Arc::new(NoopConfiner),
            cancellation: CancellationToken::new(),
        }
    }

    fn simple_team() -> TeamConfig {
        TeamConfig {
            lead: "coordinator".to_string(),
            members: vec![
                TeamMember {
                    name: "coordinator".to_string(),
                    role: "Lead".to_string(),
                    persona: "You delegate.".to_string(),
                    tool_allowlist: vec![],
                    extra_deny_paths: vec![],
                },
                TeamMember {
                    name: "implementer".to_string(),
                    role: "Implementer".to_string(),
                    persona: "You are the implementer specialist. You write code.".to_string(),
                    tool_allowlist: vec![],
                    extra_deny_paths: vec![],
                },
            ],
        }
    }

    fn config(
        llm: Arc<dyn LlmBackend>,
        events_tx: UnboundedSender<AgentEvent>,
        team: TeamConfig,
        pool: SpecialistSessionPool,
    ) -> SpecialistSessionsConfig {
        SpecialistSessionsConfig {
            llm,
            enforcement: crate::specialist_enforcement::SpecialistEnforcementIngredients {
                prompter: Arc::new(AlwaysAllowPrompter),
                base_deny_paths: vec![],
                pre_approved_commands: vec![],
                plan_mode: PlanMode::new(),
                autonomous_mode: AutonomousMode::new(),
                editor_approval_enabled: false,
                injection_taint: InjectionTaint::new(),
                extra_read_paths: vec![],
                require_enforcement: false,
            },
            checkpointer: None,
            repo_map: None,
            events_tx,
            parent_registry: ToolRegistry::new(),
            team,
            plan_mode: PlanMode::new(),
            autonomous_mode: AutonomousMode::new(),
            injection_taint: InjectionTaint::new(),
            context_tokens: 8192,
            edit_format: EditFormat::Native,
            verification: None,
            max_iterations: 3,
            broker_mode: false,
            pool,
        }
    }

    #[tokio::test]
    async fn spawn_specialist_rejects_the_lead_as_a_target() {
        let llm = Arc::new(MockBackend::new(vec![]));
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let pool = SpecialistSessionPool::new(3, Duration::from_secs(600));
        let cfg = config(llm.clone(), tx, simple_team(), pool);
        let tool = SpawnSpecialistTool::new(cfg);
        let ctx = exec_ctx(std::path::Path::new("."));
        let args = serde_json::json!({ "member": "coordinator", "task": "do something" });
        let result = tool.execute(args, &ctx).await.unwrap();
        match result {
            ToolOutput::Error(msg) => assert!(msg.contains("coordinator")),
            other => panic!("expected Error, got {other:?}"),
        }
        assert!(llm.received.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn spawn_specialist_rejects_an_unknown_member() {
        let llm = Arc::new(MockBackend::new(vec![]));
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let pool = SpecialistSessionPool::new(3, Duration::from_secs(600));
        let cfg = config(llm.clone(), tx, simple_team(), pool);
        let tool = SpawnSpecialistTool::new(cfg);
        let ctx = exec_ctx(std::path::Path::new("."));
        let args = serde_json::json!({ "member": "nonexistent", "task": "do something" });
        let result = tool.execute(args, &ctx).await.unwrap();
        match result {
            ToolOutput::Error(msg) => {
                assert!(msg.contains("nonexistent"));
                assert!(msg.contains("implementer"));
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn spawn_and_query_specialist_preserves_conversation_history_across_calls() {
        let llm = Arc::new(MockBackend::new(vec![
            text_response("first: got the task"),
            text_response("second: remembered the first"),
        ]));
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let pool = SpecialistSessionPool::new(3, Duration::from_secs(600));
        let cfg = config(llm.clone(), tx, simple_team(), pool);
        let spawn_tool = SpawnSpecialistTool::new(cfg.clone());
        let query_tool = QuerySpecialistTool::new(cfg);
        let ctx = exec_ctx(std::path::Path::new("."));

        let spawn_args = serde_json::json!({ "member": "implementer", "task": "fix the bug" });
        let spawn_result = spawn_tool.execute(spawn_args, &ctx).await.unwrap();
        let ToolOutput::Ok(spawn_text) = spawn_result else {
            panic!("expected Ok from spawn_specialist");
        };
        assert!(spawn_text.contains("first: got the task"));
        let session_id = spawn_text
            .lines()
            .next()
            .unwrap()
            .strip_prefix("session_id: ")
            .unwrap()
            .to_string();

        let query_args =
            serde_json::json!({ "session_id": session_id, "message": "what did I ask first?" });
        let query_result = query_tool.execute(query_args, &ctx).await.unwrap();
        let ToolOutput::Ok(query_text) = query_result else {
            panic!("expected Ok from query_specialist");
        };
        assert!(query_text.contains("second: remembered the first"));

        // The second request sent to the backend must carry the first
        // exchange's content in its message history -- proof this is a
        // real continuation, not two disconnected one-shot calls.
        let received = llm.received.lock().unwrap();
        assert_eq!(received.len(), 2);
        let second_request_text = format!("{:?}", received[1].messages);
        assert!(
            second_request_text.contains("fix the bug")
                && second_request_text.contains("first: got the task"),
            "expected the second request to carry the first exchange's content, got: {second_request_text}"
        );
    }

    #[tokio::test]
    async fn spawn_specialist_errors_once_the_concurrent_session_cap_is_reached() {
        let llm = Arc::new(MockBackend::new(vec![
            text_response("session one"),
            text_response("session two"),
        ]));
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let pool = SpecialistSessionPool::new(1, Duration::from_secs(600));
        let cfg = config(llm.clone(), tx, simple_team(), pool);
        let tool = SpawnSpecialistTool::new(cfg);
        let ctx = exec_ctx(std::path::Path::new("."));

        let first = tool
            .execute(
                serde_json::json!({ "member": "implementer", "task": "first task" }),
                &ctx,
            )
            .await
            .unwrap();
        assert!(matches!(first, ToolOutput::Ok(_)));

        let second = tool
            .execute(
                serde_json::json!({ "member": "implementer", "task": "second task" }),
                &ctx,
            )
            .await
            .unwrap();
        match second {
            ToolOutput::Error(msg) => assert!(msg.contains('1')),
            other => panic!("expected Error, got {other:?}"),
        }
        // The cap must be checked BEFORE running a wasted turn -- only
        // one backend request should have been sent.
        assert_eq!(llm.received.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn query_specialist_rejects_an_unknown_session_id() {
        let llm = Arc::new(MockBackend::new(vec![]));
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let pool = SpecialistSessionPool::new(3, Duration::from_secs(600));
        let cfg = config(llm, tx, simple_team(), pool);
        let tool = QuerySpecialistTool::new(cfg);
        let ctx = exec_ctx(std::path::Path::new("."));
        let args = serde_json::json!({ "session_id": "nonexistent", "message": "hi" });
        let result = tool.execute(args, &ctx).await.unwrap();
        assert!(matches!(result, ToolOutput::Error(_)));
    }

    #[tokio::test]
    async fn close_specialist_then_query_specialist_reports_unknown_session() {
        let llm = Arc::new(MockBackend::new(vec![text_response("hello")]));
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let pool = SpecialistSessionPool::new(3, Duration::from_secs(600));
        let cfg = config(llm, tx, simple_team(), pool);
        let spawn_tool = SpawnSpecialistTool::new(cfg.clone());
        let close_tool = CloseSpecialistTool::new(cfg.clone());
        let query_tool = QuerySpecialistTool::new(cfg);
        let ctx = exec_ctx(std::path::Path::new("."));

        let spawn_result = spawn_tool
            .execute(
                serde_json::json!({ "member": "implementer", "task": "task" }),
                &ctx,
            )
            .await
            .unwrap();
        let ToolOutput::Ok(spawn_text) = spawn_result else {
            panic!("expected Ok");
        };
        let session_id = spawn_text
            .lines()
            .next()
            .unwrap()
            .strip_prefix("session_id: ")
            .unwrap()
            .to_string();

        let close_result = close_tool
            .execute(
                serde_json::json!({ "session_id": session_id.clone() }),
                &ctx,
            )
            .await
            .unwrap();
        assert!(matches!(close_result, ToolOutput::Ok(_)));

        let query_result = query_tool
            .execute(
                serde_json::json!({ "session_id": session_id, "message": "still there?" }),
                &ctx,
            )
            .await
            .unwrap();
        assert!(matches!(query_result, ToolOutput::Error(_)));
    }

    #[tokio::test]
    async fn close_specialist_frees_a_slot_for_a_new_spawn() {
        let llm = Arc::new(MockBackend::new(vec![
            text_response("session one"),
            text_response("session two"),
        ]));
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let pool = SpecialistSessionPool::new(1, Duration::from_secs(600));
        let cfg = config(llm, tx, simple_team(), pool);
        let spawn_tool = SpawnSpecialistTool::new(cfg.clone());
        let close_tool = CloseSpecialistTool::new(cfg);
        let ctx = exec_ctx(std::path::Path::new("."));

        let first = spawn_tool
            .execute(
                serde_json::json!({ "member": "implementer", "task": "first" }),
                &ctx,
            )
            .await
            .unwrap();
        let ToolOutput::Ok(first_text) = first else {
            panic!("expected Ok");
        };
        let first_id = first_text
            .lines()
            .next()
            .unwrap()
            .strip_prefix("session_id: ")
            .unwrap()
            .to_string();

        close_tool
            .execute(serde_json::json!({ "session_id": first_id }), &ctx)
            .await
            .unwrap();

        let second = spawn_tool
            .execute(
                serde_json::json!({ "member": "implementer", "task": "second" }),
                &ctx,
            )
            .await
            .unwrap();
        assert!(matches!(second, ToolOutput::Ok(_)));
    }

    #[tokio::test]
    async fn sessions_past_the_idle_timeout_are_evicted_on_the_next_pool_touch() {
        let llm = Arc::new(MockBackend::new(vec![text_response("hello")]));
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let pool = SpecialistSessionPool::new(3, Duration::from_millis(10));
        let cfg = config(llm, tx, simple_team(), pool);
        let spawn_tool = SpawnSpecialistTool::new(cfg.clone());
        let query_tool = QuerySpecialistTool::new(cfg);
        let ctx = exec_ctx(std::path::Path::new("."));

        let spawn_result = spawn_tool
            .execute(
                serde_json::json!({ "member": "implementer", "task": "task" }),
                &ctx,
            )
            .await
            .unwrap();
        let ToolOutput::Ok(spawn_text) = spawn_result else {
            panic!("expected Ok");
        };
        let session_id = spawn_text
            .lines()
            .next()
            .unwrap()
            .strip_prefix("session_id: ")
            .unwrap()
            .to_string();

        tokio::time::sleep(Duration::from_millis(30)).await;

        let query_result = query_tool
            .execute(
                serde_json::json!({ "session_id": session_id, "message": "still there?" }),
                &ctx,
            )
            .await
            .unwrap();
        assert!(matches!(query_result, ToolOutput::Error(_)));
    }

    #[tokio::test]
    async fn a_failed_exchange_does_not_leak_text_into_the_next_successful_one() {
        // Regression test for the "shared text accumulator isn't drained
        // on a failed turn" bug: `run_bounded_exchange`'s `Err` arm used to
        // return without draining `accumulated`, so any `TextDelta` text a
        // specialist streamed before a turn failed stayed in the buffer --
        // and since `QuerySpecialistTool::execute` puts the session back
        // into the pool unconditionally even after an error, the NEXT
        // exchange on that session would silently get the failed turn's
        // leftover text concatenated in front of its own real response.
        let llm = Arc::new(MockBackend::new_with_mixed_results(vec![
            // Exchange 1 (via spawn_specialist): succeeds cleanly.
            vec![
                Ok(StreamEvent::TextDelta("first: got the task".to_string())),
                Ok(StreamEvent::Done {
                    finish_reason: FinishReason::Stop,
                }),
            ],
            // Exchange 2 (via query_specialist): streams partial text,
            // then the stream itself errors -- the exact shape that used
            // to leak into exchange 3 below.
            vec![
                Ok(StreamEvent::TextDelta(
                    "second: PARTIAL TEXT THAT MUST NOT LEAK".to_string(),
                )),
                Err("simulated backend failure".to_string()),
            ],
            // Exchange 3 (via query_specialist): succeeds cleanly and must
            // NOT carry any trace of exchange 2's partial text.
            vec![
                Ok(StreamEvent::TextDelta("third: clean response".to_string())),
                Ok(StreamEvent::Done {
                    finish_reason: FinishReason::Stop,
                }),
            ],
        ]));
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let pool = SpecialistSessionPool::new(3, Duration::from_secs(600));
        let cfg = config(llm.clone(), tx, simple_team(), pool);
        let spawn_tool = SpawnSpecialistTool::new(cfg.clone());
        let query_tool = QuerySpecialistTool::new(cfg);
        let ctx = exec_ctx(std::path::Path::new("."));

        let spawn_result = spawn_tool
            .execute(
                serde_json::json!({ "member": "implementer", "task": "first task" }),
                &ctx,
            )
            .await
            .unwrap();
        let ToolOutput::Ok(spawn_text) = spawn_result else {
            panic!("expected Ok from spawn_specialist");
        };
        let session_id = spawn_text
            .lines()
            .next()
            .unwrap()
            .strip_prefix("session_id: ")
            .unwrap()
            .to_string();

        let second_result = query_tool
            .execute(
                serde_json::json!({ "session_id": session_id, "message": "second message" }),
                &ctx,
            )
            .await
            .unwrap();
        assert!(
            matches!(second_result, ToolOutput::Error(_)),
            "expected the second exchange to fail, got: {second_result:?}"
        );

        let third_result = query_tool
            .execute(
                serde_json::json!({ "session_id": session_id, "message": "third message" }),
                &ctx,
            )
            .await
            .unwrap();
        let ToolOutput::Ok(third_text) = third_result else {
            panic!("expected Ok from the third exchange, got an Error");
        };
        assert!(
            third_text.contains("third: clean response"),
            "expected the third exchange's own text, got: {third_text}"
        );
        assert!(
            !third_text.contains("PARTIAL TEXT THAT MUST NOT LEAK"),
            "the second (failed) exchange's partial text leaked into the third exchange's \
             output: {third_text}"
        );
    }

    #[tokio::test]
    async fn spawn_specialist_emits_specialist_sessions_updated() {
        let llm = Arc::new(MockBackend::new(vec![text_response("hello")]));
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let pool = SpecialistSessionPool::new(3, Duration::from_secs(600));
        let cfg = config(llm, tx, simple_team(), pool);
        let tool = SpawnSpecialistTool::new(cfg);
        let ctx = exec_ctx(std::path::Path::new("."));

        tool.execute(
            serde_json::json!({ "member": "implementer", "task": "task" }),
            &ctx,
        )
        .await
        .unwrap();

        let mut found = false;
        while let Ok(event) = rx.try_recv() {
            if let AgentEvent::SpecialistSessionsUpdated(sessions) = event {
                assert_eq!(sessions.len(), 1);
                assert_eq!(sessions[0].member, "implementer");
                found = true;
            }
        }
        assert!(found, "expected a SpecialistSessionsUpdated event");
    }

    #[tokio::test]
    async fn close_specialist_emits_specialist_sessions_updated_with_the_session_removed() {
        let llm = Arc::new(MockBackend::new(vec![text_response("hello")]));
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let pool = SpecialistSessionPool::new(3, Duration::from_secs(600));
        let cfg = config(llm, tx, simple_team(), pool);
        let spawn_tool = SpawnSpecialistTool::new(cfg.clone());
        let close_tool = CloseSpecialistTool::new(cfg);
        let ctx = exec_ctx(std::path::Path::new("."));

        let spawn_result = spawn_tool
            .execute(
                serde_json::json!({ "member": "implementer", "task": "task" }),
                &ctx,
            )
            .await
            .unwrap();
        let ToolOutput::Ok(spawn_text) = spawn_result else {
            panic!("expected Ok");
        };
        let session_id = spawn_text
            .lines()
            .next()
            .unwrap()
            .strip_prefix("session_id: ")
            .unwrap()
            .to_string();
        // Drain spawn's own event so only close's event remains below.
        while rx.try_recv().is_ok() {}

        close_tool
            .execute(serde_json::json!({ "session_id": session_id }), &ctx)
            .await
            .unwrap();

        let mut found = false;
        while let Ok(event) = rx.try_recv() {
            if let AgentEvent::SpecialistSessionsUpdated(sessions) = event {
                assert!(
                    sessions.is_empty(),
                    "expected the closed session to be gone, got: {sessions:?}"
                );
                found = true;
            }
        }
        assert!(found, "expected a SpecialistSessionsUpdated event");
    }

    #[tokio::test]
    async fn query_specialist_does_not_emit_specialist_sessions_updated() {
        let llm = Arc::new(MockBackend::new(vec![
            text_response("first"),
            text_response("second"),
        ]));
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let pool = SpecialistSessionPool::new(3, Duration::from_secs(600));
        let cfg = config(llm, tx, simple_team(), pool);
        let spawn_tool = SpawnSpecialistTool::new(cfg.clone());
        let query_tool = QuerySpecialistTool::new(cfg);
        let ctx = exec_ctx(std::path::Path::new("."));

        let spawn_result = spawn_tool
            .execute(
                serde_json::json!({ "member": "implementer", "task": "task" }),
                &ctx,
            )
            .await
            .unwrap();
        let ToolOutput::Ok(spawn_text) = spawn_result else {
            panic!("expected Ok");
        };
        let session_id = spawn_text
            .lines()
            .next()
            .unwrap()
            .strip_prefix("session_id: ")
            .unwrap()
            .to_string();
        while rx.try_recv().is_ok() {}

        query_tool
            .execute(
                serde_json::json!({ "session_id": session_id, "message": "follow up" }),
                &ctx,
            )
            .await
            .unwrap();

        // query_specialist's own turn still forwards its sub-agent's normal
        // conversation events (TextDelta/ToolResult/TurnComplete) as
        // `SubAgentActivity` -- that's pre-existing, correct behavior
        // unrelated to this task, so the channel is NOT expected to be
        // empty. What must never appear is a `SpecialistSessionsUpdated`
        // event, since the open-session set is unchanged by query_specialist.
        while let Ok(event) = rx.try_recv() {
            assert!(
                !matches!(event, AgentEvent::SpecialistSessionsUpdated(_)),
                "query_specialist must not emit SpecialistSessionsUpdated"
            );
        }
    }
}
