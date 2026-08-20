//! The rmcp-facing layer: `code`/`code_reply` tool definitions, an
//! in-memory TTL-evicted session map, and the startup ceiling check.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use aivyx_core::Agent;
use rmcp::handler::server::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolResult, Content, Implementation, ProtocolVersion, ServerCapabilities, ServerInfo,
};
use rmcp::{tool, tool_handler, tool_router, ErrorData as McpError, ServerHandler, ServiceExt};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::session::{build_session_agent, run_bounded_turn, SessionConfig};
use crate::tiers::AccessLevel;

pub struct McpServerRunConfig {
    pub session_config: SessionConfig,
    pub max_access_level: AccessLevel,
    pub session_ttl: Duration,
    pub max_concurrent_sessions: usize,
    pub max_iterations: u32,
}

/// `events_rx` travels with its `Agent`: `Agent::run_turn` sends every
/// `AgentEvent` to whichever channel it was constructed with, baked in
/// once at `build_session_agent` (Task 3) time -- a `code_reply` call
/// must reuse the SAME receiver `code` first created, not a fresh,
/// disconnected one, or its accumulated text would always come back
/// empty. Storing them together is what makes that automatic.
struct StoredSession {
    agent: Agent,
    events_rx: tokio::sync::mpsc::UnboundedReceiver<aivyx_core::AgentEvent>,
    last_active: Instant,
}

/// Bounded, TTL-evicted in-memory session map -- the whole of "cleanup"
/// for v1 (Global Constraints: no `close_session` tool). `evict_stale`
/// and `evict_oldest_if_full` are separated from insertion so each is
/// independently testable.
struct SessionMap {
    sessions: HashMap<String, StoredSession>,
    ttl: Duration,
    max_concurrent: usize,
}

impl SessionMap {
    fn new(ttl: Duration, max_concurrent: usize) -> Self {
        Self { sessions: HashMap::new(), ttl, max_concurrent }
    }

    /// Removes every session idle longer than `ttl`. Call before every
    /// insert/lookup so staleness is judged relative to "now", not to
    /// whenever the map was last touched.
    fn evict_stale(&mut self) {
        let ttl = self.ttl;
        self.sessions.retain(|_, s| s.last_active.elapsed() < ttl);
    }

    /// If inserting one more session would exceed `max_concurrent`,
    /// evicts whichever current session has been idle longest. No-op if
    /// there's already room.
    fn evict_oldest_if_full(&mut self) {
        if self.sessions.len() < self.max_concurrent {
            return;
        }
        if let Some(oldest_id) = self
            .sessions
            .iter()
            .min_by_key(|(_, s)| s.last_active)
            .map(|(id, _)| id.clone())
        {
            self.sessions.remove(&oldest_id);
        }
    }

    fn insert(
        &mut self,
        id: String,
        agent: Agent,
        events_rx: tokio::sync::mpsc::UnboundedReceiver<aivyx_core::AgentEvent>,
    ) {
        self.sessions.insert(id, StoredSession { agent, events_rx, last_active: Instant::now() });
    }

    /// Removes and returns the session, so its turn can run WITHOUT
    /// holding the map's lock -- the whole point of this method existing
    /// instead of a borrow-returning accessor. Returns None if the id
    /// doesn't exist, has expired, OR is already checked out by another
    /// in-flight code_reply call (this last case is intentional: two
    /// concurrent code_reply calls for the SAME session_id must not both
    /// get a copy of the same Agent).
    fn take(&mut self, id: &str) -> Option<StoredSession> {
        self.sessions.remove(id)
    }

    /// Re-inserts a session after its turn completes, refreshing
    /// last_active to now (not whenever it was originally checked out) --
    /// called regardless of whether the turn itself succeeded or errored,
    /// since a turn-level error (e.g. a backend network failure) does not
    /// mean the Agent's own internal state is unusable for a future retry.
    fn put_back(&mut self, id: String, mut session: StoredSession) {
        session.last_active = Instant::now();
        self.sessions.insert(id, session);
    }
}

#[cfg(test)]
mod session_map_tests {
    use super::*;
    use crate::session::SessionConfig;
    use aivyx_core::AgentEvent;
    use aivyx_llm::{ChatRequest, FinishReason, LlmBackend, LlmError, StreamEvent};
    use aivyx_sandbox::NoopConfiner;
    use aivyx_tools::ToolRegistry;
    use futures::stream::BoxStream;
    use futures::StreamExt;

    struct MockBackend;
    #[async_trait::async_trait]
    impl LlmBackend for MockBackend {
        fn model_id(&self) -> &str {
            "mock"
        }
        async fn stream_chat(
            &self,
            _r: ChatRequest,
        ) -> Result<BoxStream<'static, Result<StreamEvent, LlmError>>, LlmError> {
            Ok(futures::stream::iter([Ok(StreamEvent::Done {
                finish_reason: FinishReason::Stop,
            })])
            .boxed())
        }
    }

    /// A real, minimal `Agent` built via `build_session_agent` (Task 3's
    /// own function, already proven in `session.rs`'s own tests) plus its
    /// matching `events_rx` -- this module's tests only exercise map
    /// bookkeeping (eviction order, TTL, touch-refresh), never a real turn.
    async fn fake_session() -> (Agent, tokio::sync::mpsc::UnboundedReceiver<AgentEvent>) {
        let config = SessionConfig {
            llm: Arc::new(MockBackend),
            confiner: Arc::new(NoopConfiner),
            checkpointer: None,
            repo_map: None,
            base_registry: ToolRegistry::new(),
            deny_paths: Vec::new(),
            cwd: std::env::temp_dir(),
            context_tokens: 8192,
            edit_format: aivyx_core::EditFormat::Native,
        };
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<AgentEvent>();
        let agent = build_session_agent(&config, AccessLevel::Plan, tx).await;
        (agent, rx)
    }

    #[tokio::test]
    async fn evict_oldest_if_full_removes_the_least_recently_active_session() {
        let mut map = SessionMap::new(Duration::from_secs(3600), 2);
        let (a, a_rx) = fake_session().await;
        map.insert("a".to_string(), a, a_rx);
        tokio::time::sleep(Duration::from_millis(5)).await;
        let (b, b_rx) = fake_session().await;
        map.insert("b".to_string(), b, b_rx);

        map.evict_oldest_if_full(); // len == max_concurrent (2) -- evicts "a" before the 3rd insert
        let (c, c_rx) = fake_session().await;
        map.insert("c".to_string(), c, c_rx);

        assert!(map.take("a").is_none(), "a was the oldest, must be evicted");
        assert!(map.take("b").is_some());
        assert!(map.take("c").is_some());
    }

    #[tokio::test]
    async fn evict_stale_removes_only_sessions_past_the_ttl() {
        let mut map = SessionMap::new(Duration::from_millis(10), 8);
        let (stale, stale_rx) = fake_session().await;
        map.insert("stale".to_string(), stale, stale_rx);
        tokio::time::sleep(Duration::from_millis(20)).await;
        let (fresh, fresh_rx) = fake_session().await;
        map.insert("fresh".to_string(), fresh, fresh_rx);

        map.evict_stale();

        assert!(map.take("stale").is_none());
        assert!(map.take("fresh").is_some());
    }

    #[tokio::test]
    async fn take_then_put_back_refreshes_last_active_so_evict_stale_spares_it() {
        let mut map = SessionMap::new(Duration::from_millis(15), 8);
        let (s, s_rx) = fake_session().await;
        map.insert("s".to_string(), s, s_rx);
        tokio::time::sleep(Duration::from_millis(10)).await;
        // "touch" by taking it out and immediately putting it back --
        // put_back refreshes last_active, exactly like the old
        // touch_and_borrow did.
        let session = map.take("s").expect("touched before its TTL expires");
        map.put_back("s".to_string(), session);
        tokio::time::sleep(Duration::from_millis(10)).await;
        // 10ms since the touch above -- still under the 15ms TTL relative
        // to that touch, even though 20ms have passed since insertion.
        map.evict_stale();
        assert!(map.take("s").is_some(), "the touch above must have reset the TTL clock");
    }

    #[tokio::test]
    async fn take_makes_a_session_unavailable_until_put_back() {
        let mut map = SessionMap::new(Duration::from_secs(3600), 8);
        let (s, s_rx) = fake_session().await;
        map.insert("s".to_string(), s, s_rx);

        let checked_out = map.take("s").expect("session exists");
        assert!(
            map.take("s").is_none(),
            "a session already checked out must not be handed to a second concurrent caller"
        );

        map.put_back("s".to_string(), checked_out);
        assert!(map.take("s").is_some(), "put_back must make the session available again");
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct CodeParams {
    /// A complete, self-contained description of the task -- the session
    /// starts with no context beyond this text.
    pub task: String,
    /// "plan" | "edit" | "execute". Rejected if it exceeds this server's
    /// configured ceiling.
    pub access_level: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct CodeReplyParams {
    pub session_id: String,
    pub message: String,
}

#[derive(Debug, Serialize)]
struct CodeOutcome {
    session_id: String,
    result: String,
}

fn mcp_error(detail: impl Into<String>) -> McpError {
    McpError::internal_error(detail.into(), None)
}

#[derive(Clone)]
pub struct AivyxCoderMcpServer {
    tool_router: ToolRouter<Self>,
    sessions: Arc<Mutex<SessionMap>>,
    session_config: Arc<SessionConfig>,
    max_access_level: AccessLevel,
    max_iterations: u32,
}

#[tool_router]
impl AivyxCoderMcpServer {
    fn new(config: McpServerRunConfig) -> Self {
        Self {
            tool_router: Self::tool_router(),
            sessions: Arc::new(Mutex::new(SessionMap::new(config.session_ttl, config.max_concurrent_sessions))),
            session_config: Arc::new(config.session_config),
            max_access_level: config.max_access_level,
            max_iterations: config.max_iterations,
        }
    }

    #[tool(description = "Delegate a bounded coding task to aivyx-coder. access_level is \
        \"plan\" (read-only), \"edit\" (file writes, no shell), or \"execute\" (full tool \
        access, still sandboxed) -- rejected if it exceeds this server's configured ceiling. \
        Returns a session_id for use with code_reply, plus the session's final answer.")]
    async fn code(
        &self,
        Parameters(params): Parameters<CodeParams>,
    ) -> Result<CallToolResult, McpError> {
        let level = AccessLevel::parse(&params.access_level).map_err(mcp_error)?;
        if !level.at_most(&self.max_access_level) {
            return Err(mcp_error(format!(
                "access_level {:?} exceeds this server's configured ceiling",
                params.access_level
            )));
        }

        let (events_tx, mut events_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut agent = build_session_agent(&self.session_config, level, events_tx).await;
        let (result, text) = run_bounded_turn(
            &mut agent,
            &mut events_rx,
            params.task,
            &self.session_config.cwd,
            self.max_iterations,
            CancellationToken::new(),
        )
        .await;
        result.map_err(|e| mcp_error(e.to_string()))?;

        let session_id = uuid::Uuid::new_v4().to_string();
        {
            let mut sessions = self.sessions.lock().await;
            sessions.evict_stale();
            sessions.evict_oldest_if_full();
            sessions.insert(session_id.clone(), agent, events_rx);
        }

        let content = Content::json(CodeOutcome { session_id, result: text })
            .map_err(|e| mcp_error(format!("failed to encode result: {e}")))?;
        Ok(CallToolResult::success(vec![content]))
    }

    #[tool(description = "Continue a session started by code, with a follow-up message. The \
        access level chosen at session start is not renegotiable here.")]
    async fn code_reply(
        &self,
        Parameters(params): Parameters<CodeReplyParams>,
    ) -> Result<CallToolResult, McpError> {
        let mut session = {
            let mut sessions = self.sessions.lock().await;
            sessions.evict_stale();
            sessions.take(&params.session_id).ok_or_else(|| {
                mcp_error(format!(
                    "no session {:?} -- it may have expired (idle past the configured TTL) or \
                     is already processing another request",
                    params.session_id
                ))
            })?
        }; // lock released here -- the turn below runs with no lock held

        // Reuses the SAME events_rx `code` first created (stored alongside
        // the Agent in StoredSession) -- Agent::run_turn sends to whatever
        // channel it was constructed with, baked in once at
        // build_session_agent time, so a fresh, disconnected channel here
        // would drain nothing and always return empty text.
        let (result, text) = run_bounded_turn(
            &mut session.agent,
            &mut session.events_rx,
            params.message,
            &self.session_config.cwd,
            self.max_iterations,
            CancellationToken::new(),
        )
        .await;

        {
            let mut sessions = self.sessions.lock().await;
            sessions.put_back(params.session_id.clone(), session);
        }

        result.map_err(|e| mcp_error(e.to_string()))?;
        let content = Content::json(CodeOutcome { session_id: params.session_id, result: text })
            .map_err(|e| mcp_error(format!("failed to encode result: {e}")))?;
        Ok(CallToolResult::success(vec![content]))
    }
}

#[tool_handler]
impl ServerHandler for AivyxCoderMcpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            protocol_version: ProtocolVersion::V_2025_06_18,
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            server_info: Implementation::from_build_env(),
            instructions: Some(
                "Delegate bounded coding tasks to aivyx-coder via code/code_reply.".to_string(),
            ),
        }
    }
}

pub async fn run(config: McpServerRunConfig) -> anyhow::Result<()> {
    let server = AivyxCoderMcpServer::new(config);
    let service = server
        .serve(rmcp::transport::stdio())
        .await
        .inspect_err(|e| tracing::error!("aivyx-mcp-server: serving error: {e:?}"))?;
    service.waiting().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::SessionConfig;
    use aivyx_llm::{ChatRequest, FinishReason, LlmBackend, LlmError, StreamEvent};
    use aivyx_sandbox::NoopConfiner;
    use aivyx_tools::ToolRegistry;
    use futures::stream::BoxStream;
    use futures::StreamExt;
    use std::sync::Mutex as StdMutex;

    /// Scripted responses, one Vec<StreamEvent> per call -- proves
    /// code_reply's turn is a SECOND real call to the backend (not, e.g.,
    /// replaying the first response) and that its own TextDelta content
    /// makes it all the way back out, discriminating this test from one
    /// that would pass even with the events_rx bug the plan's fix note
    /// above describes (that bug makes code_reply's text always empty --
    /// this test's second assertion fails under it).
    struct ScriptedBackend {
        responses: StdMutex<std::collections::VecDeque<&'static str>>,
    }
    #[async_trait::async_trait]
    impl LlmBackend for ScriptedBackend {
        fn model_id(&self) -> &str {
            "scripted"
        }
        async fn stream_chat(
            &self,
            _r: ChatRequest,
        ) -> Result<BoxStream<'static, Result<StreamEvent, LlmError>>, LlmError> {
            let text = self.responses.lock().unwrap().pop_front().unwrap_or("");
            Ok(futures::stream::iter([
                Ok(StreamEvent::TextDelta(text.to_string())),
                Ok(StreamEvent::Done { finish_reason: FinishReason::Stop }),
            ])
            .boxed())
        }
    }

    fn server_with_ceiling(ceiling: AccessLevel) -> AivyxCoderMcpServer {
        let session_config = SessionConfig {
            llm: Arc::new(ScriptedBackend {
                responses: StdMutex::new(std::collections::VecDeque::from(vec!["first answer", "second answer"])),
            }),
            confiner: Arc::new(NoopConfiner),
            checkpointer: None,
            repo_map: None,
            base_registry: ToolRegistry::new(),
            deny_paths: Vec::new(),
            cwd: std::env::temp_dir(),
            context_tokens: 8192,
            edit_format: aivyx_core::EditFormat::Native,
        };
        AivyxCoderMcpServer::new(McpServerRunConfig {
            session_config,
            max_access_level: ceiling,
            session_ttl: Duration::from_secs(3600),
            max_concurrent_sessions: 8,
            max_iterations: 10,
        })
    }

    #[tokio::test]
    async fn code_then_code_reply_round_trips_real_conversation_state() {
        let server = server_with_ceiling(AccessLevel::Execute);
        let first = server
            .code(Parameters(CodeParams { task: "start".to_string(), access_level: "plan".to_string() }))
            .await
            .expect("code call should succeed");
        let CallToolResult { content, .. } = first;
        let first_json: serde_json::Value = content[0].raw.as_text().unwrap().text.parse().unwrap_or_else(|_| {
            serde_json::from_str(&content[0].raw.as_text().unwrap().text).unwrap()
        });
        let session_id = first_json["session_id"].as_str().unwrap().to_string();
        assert_eq!(first_json["result"], "first answer");

        let second = server
            .code_reply(Parameters(CodeReplyParams { session_id: session_id.clone(), message: "continue".to_string() }))
            .await
            .expect("code_reply should succeed");
        let second_json: serde_json::Value =
            serde_json::from_str(&second.content[0].raw.as_text().unwrap().text).unwrap();
        assert_eq!(
            second_json["result"], "second answer",
            "code_reply must return its OWN turn's real text, not an empty string from a \
             disconnected events channel"
        );
        assert_eq!(second_json["session_id"], session_id);
    }

    #[tokio::test]
    async fn code_call_above_the_ceiling_is_rejected_before_any_agent_is_built() {
        let server = server_with_ceiling(AccessLevel::Plan);
        let outcome = server
            .code(Parameters(CodeParams { task: "do something".to_string(), access_level: "execute".to_string() }))
            .await;
        assert!(outcome.is_err(), "execute must be rejected when the ceiling is plan");
    }

    #[tokio::test]
    async fn code_reply_against_an_unknown_session_id_fails_clearly() {
        let server = server_with_ceiling(AccessLevel::Execute);
        let outcome = server
            .code_reply(Parameters(CodeReplyParams {
                session_id: "does-not-exist".to_string(),
                message: "hi".to_string(),
            }))
            .await;
        assert!(outcome.is_err());
    }
}
