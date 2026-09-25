//! `RoutedBackend`: an `LlmBackend` that picks a model per call with
//! `aivyx-route` and forwards to a lazily-built backend for it. Untagged
//! requests (`ChatRequest::route == None`) go to the configured default
//! backend unchanged. See `aivyx-ecosystem/docs/superpowers/specs/
//! 2026-09-25-model-routing-design.md`, "Parts 2 & 3".

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use aivyx_route::{ModelKey, ModelProfile, Policy, Requirements, TaskKind, TaskOverrides, select};
use async_trait::async_trait;
use futures::stream::BoxStream;

use crate::backend::{ChatRequest, LlmBackend, LlmError, RouteHint, StreamEvent};

/// Builds the backend serving one model. The error is a human sentence.
pub type BackendFactory =
    Box<dyn Fn(&ModelKey) -> Result<Arc<dyn LlmBackend>, String> + Send + Sync>;

/// What a session's most recent routed call went to, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteRecord {
    pub model: ModelKey,
    pub task: TaskKind,
    /// One human sentence: the decision's reason plus any fallback note.
    pub reason: String,
}

pub struct RoutedBackend {
    default_key: ModelKey,
    default_backend: Arc<dyn LlmBackend>,
    factory: BackendFactory,
    tasks: TaskOverrides,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    profiles: Vec<ModelProfile>,
    pool: HashMap<ModelKey, Arc<dyn LlmBackend>>,
    /// Session → the model its main thread is on.
    sticky: HashMap<String, ModelKey>,
    last: HashMap<String, RouteRecord>,
}

/// The ordered models to try for one call, and why.
struct Plan {
    chain: Vec<ModelKey>,
    reason: String,
    /// `Some` only for a sticky task kind with a session.
    sticky_session: Option<String>,
}

impl RoutedBackend {
    pub fn new(
        default_key: ModelKey,
        default_backend: Arc<dyn LlmBackend>,
        profiles: Vec<ModelProfile>,
        tasks: TaskOverrides,
        factory: BackendFactory,
    ) -> Self {
        RoutedBackend {
            default_key,
            default_backend,
            factory,
            tasks,
            state: Mutex::new(State {
                profiles,
                ..State::default()
            }),
        }
    }

    pub fn default_key(&self) -> &ModelKey {
        &self.default_key
    }

    pub fn profiles(&self) -> Vec<ModelProfile> {
        self.state.lock().unwrap().profiles.clone()
    }

    /// The model `session`'s main thread is currently on, if any.
    pub fn current(&self, session: &str) -> Option<ModelKey> {
        self.state.lock().unwrap().sticky.get(session).cloned()
    }

    pub fn last_decision(&self, session: &str) -> Option<RouteRecord> {
        self.state.lock().unwrap().last.get(session).cloned()
    }

    fn plan(&self, request: &ChatRequest, hint: &RouteHint) -> Result<Plan, LlmError> {
        let mut builder = Requirements::builder().task(&hint.task, &self.tasks);
        if !request.tools.is_empty() {
            builder = builder.tools();
        }
        if hint.estimated_prompt_tokens > 0 {
            builder = builder.min_context(hint.estimated_prompt_tokens);
        }
        let req = builder.build();
        let sticky_session = hint.session.clone().filter(|_| hint.task.is_sticky());
        let state = self.state.lock().unwrap();
        let policy = Policy {
            sticky_model: sticky_session
                .as_ref()
                .and_then(|s| state.sticky.get(s).cloned()),
            allow_cloud: false,
            exclude: Vec::new(),
        };
        let decision = select(&req, &state.profiles, &policy).map_err(|e| {
            LlmError::Routing(format!(
                "{e} — add a capable model to [[routing.models]] (see /models)"
            ))
        })?;
        let mut chain = vec![decision.model.key()];
        chain.extend(decision.fallbacks.iter().map(ModelProfile::key));
        Ok(Plan {
            chain,
            reason: decision.to_string(),
            sticky_session,
        })
    }

    fn backend_for(&self, key: &ModelKey) -> Result<Arc<dyn LlmBackend>, String> {
        if *key == self.default_key {
            return Ok(Arc::clone(&self.default_backend));
        }
        let mut state = self.state.lock().unwrap();
        if let Some(backend) = state.pool.get(key) {
            return Ok(Arc::clone(backend));
        }
        let backend = (self.factory)(key)?;
        state.pool.insert(key.clone(), Arc::clone(&backend));
        Ok(backend)
    }

    fn record(&self, plan: &Plan, hint: &RouteHint, model: &ModelKey, reason: String) {
        let mut state = self.state.lock().unwrap();
        if let Some(session) = &plan.sticky_session {
            state.sticky.insert(session.clone(), model.clone());
        }
        if let Some(session) = &hint.session {
            state.last.insert(
                session.clone(),
                RouteRecord {
                    model: model.clone(),
                    task: hint.task.clone(),
                    reason,
                },
            );
        }
    }
}

#[async_trait]
impl LlmBackend for RoutedBackend {
    /// The configured `[backend]` model — the one untagged calls use.
    fn model_id(&self) -> &str {
        self.default_backend.model_id()
    }

    async fn stream_chat(
        &self,
        request: ChatRequest,
    ) -> Result<BoxStream<'static, Result<StreamEvent, LlmError>>, LlmError> {
        let Some(hint) = request.route.clone() else {
            return self.default_backend.stream_chat(request).await;
        };
        let plan = self.plan(&request, &hint)?;
        let key = &plan.chain[0];
        let backend = self.backend_for(key).map_err(LlmError::Routing)?;
        let mut attempt = request;
        if *key != self.default_key {
            // Slot ids/hints describe the default server's KV cache.
            attempt.id_slot = None;
            attempt.slot_hint = None;
        }
        let stream = backend.stream_chat(attempt).await?;
        tracing::info!(model = %key, task = hint.task.name(), reason = %plan.reason, "routed model call");
        self.record(&plan, &hint, key, plan.reason.clone());
        Ok(stream)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use aivyx_route::{Capability, EndpointRef, Tier};
    use aivyx_types::{Message, Role, ToolDefinition};
    use futures::StreamExt;

    use crate::backend::{FinishReason, SlotHint};

    /// Records every request; answers with an empty successful stream, or
    /// with `BackendError { status }` when `fail` is set.
    struct Scripted {
        name: String,
        fail: Option<u16>,
        received: Mutex<Vec<ChatRequest>>,
    }

    impl Scripted {
        fn new(name: &str, fail: Option<u16>) -> Arc<Self> {
            Arc::new(Scripted {
                name: name.to_string(),
                fail,
                received: Mutex::new(Vec::new()),
            })
        }
        fn calls(&self) -> usize {
            self.received.lock().unwrap().len()
        }
        fn last(&self) -> ChatRequest {
            self.received.lock().unwrap().last().cloned().unwrap()
        }
    }

    #[async_trait]
    impl LlmBackend for Scripted {
        fn model_id(&self) -> &str {
            &self.name
        }
        async fn stream_chat(
            &self,
            request: ChatRequest,
        ) -> Result<BoxStream<'static, Result<StreamEvent, LlmError>>, LlmError> {
            self.received.lock().unwrap().push(request);
            if let Some(status) = self.fail {
                return Err(LlmError::BackendError {
                    status,
                    body: "down".into(),
                });
            }
            Ok(futures::stream::iter(vec![Ok(StreamEvent::Done {
                finish_reason: FinishReason::Stop,
            })])
            .boxed())
        }
    }

    fn key(endpoint: &str, id: &str) -> ModelKey {
        ModelKey {
            endpoint: EndpointRef::new(endpoint),
            id: id.into(),
        }
    }

    fn profile(endpoint: &str, id: &str, tier: Tier, caps: &[Capability]) -> ModelProfile {
        let mut p = ModelProfile::new(id, EndpointRef::new(endpoint));
        p.tier = tier;
        p.capabilities.insert(Capability::Completion);
        p.capabilities.extend(caps.iter().copied());
        p
    }

    struct Fixture {
        router: RoutedBackend,
        default: Arc<Scripted>,
        others: HashMap<ModelKey, Arc<Scripted>>,
        built: Arc<AtomicUsize>,
    }

    impl Fixture {
        fn calls(&self, endpoint: &str, id: &str) -> usize {
            self.others[&key(endpoint, id)].calls()
        }
    }

    /// The default model is `default@backend` (Medium, no capabilities
    /// beyond completion). `extra` are the other candidates; `failing`
    /// lists `(endpoint, id, status)` backends that answer with an error.
    fn fixture(extra: Vec<ModelProfile>, failing: &[(&str, &str, u16)]) -> Fixture {
        let default = Scripted::new("default", None);
        let mut profiles = vec![profile("backend", "default", Tier::Medium, &[])];
        let mut others = HashMap::new();
        for p in extra {
            let fail = failing
                .iter()
                .find(|(e, i, _)| *e == p.endpoint.as_str() && *i == p.id)
                .map(|(_, _, s)| *s);
            others.insert(p.key(), Scripted::new(&p.id, fail));
            profiles.push(p);
        }
        let built = Arc::new(AtomicUsize::new(0));
        let pool = others.clone();
        let counter = Arc::clone(&built);
        let factory: BackendFactory = Box::new(move |k: &ModelKey| {
            counter.fetch_add(1, Ordering::SeqCst);
            pool.get(k)
                .map(|b| Arc::clone(b) as Arc<dyn LlmBackend>)
                .ok_or_else(|| format!("no backend for `{k}`"))
        });
        let router = RoutedBackend::new(
            key("backend", "default"),
            Arc::clone(&default) as Arc<dyn LlmBackend>,
            profiles,
            TaskOverrides::default(),
            factory,
        );
        Fixture {
            router,
            default,
            others,
            built,
        }
    }

    fn routed(task: TaskKind, session: Option<&str>) -> ChatRequest {
        let mut r = ChatRequest::new(vec![Message::text(Role::User, "hi")]);
        r.route = Some(RouteHint {
            task,
            session: session.map(str::to_string),
            estimated_prompt_tokens: 0,
        });
        r
    }

    fn a_tool() -> ToolDefinition {
        ToolDefinition {
            name: "read_file".into(),
            description: "read".into(),
            parameters_schema: serde_json::json!({}),
        }
    }

    #[tokio::test]
    async fn untagged_requests_go_to_the_default_backend_unchanged() {
        let f = fixture(vec![profile("gpu", "big", Tier::Large, &[])], &[]);
        let mut r = ChatRequest::new(vec![Message::text(Role::User, "hi")]);
        r.id_slot = Some(3);
        let _ = f.router.stream_chat(r).await.unwrap();
        assert_eq!(f.default.calls(), 1);
        assert_eq!(f.default.last().id_slot, Some(3));
        assert_eq!(f.calls("gpu", "big"), 0);
    }

    #[tokio::test]
    async fn a_code_edit_call_goes_to_the_best_tier_match() {
        let f = fixture(vec![profile("gpu", "big", Tier::Large, &[])], &[]);
        let _ = f
            .router
            .stream_chat(routed(TaskKind::CodeEdit, None))
            .await
            .unwrap();
        assert_eq!(f.calls("gpu", "big"), 1);
        assert_eq!(f.default.calls(), 0);
    }

    #[tokio::test]
    async fn tools_in_the_request_are_a_hard_need() {
        let f = fixture(
            vec![profile("gpu", "caller", Tier::Small, &[Capability::Tools])],
            &[],
        );
        let mut r = routed(TaskKind::Chat, None);
        r.tools = vec![a_tool()];
        let _ = f.router.stream_chat(r).await.unwrap();
        assert_eq!(f.calls("gpu", "caller"), 1);
    }

    #[tokio::test]
    async fn the_prompt_estimate_is_the_minimum_context() {
        let mut long = profile("gpu", "long", Tier::Small, &[]);
        long.context_window = Some(131_072);
        let f = fixture(vec![long], &[]);
        {
            // The default model's window is known and too small.
            let mut state = f.router.state.lock().unwrap();
            state.profiles[0].context_window = Some(8_192);
        }
        let mut r = routed(TaskKind::Chat, None);
        r.route.as_mut().unwrap().estimated_prompt_tokens = 20_000;
        let _ = f.router.stream_chat(r).await.unwrap();
        assert_eq!(f.calls("gpu", "long"), 1);
    }

    #[tokio::test]
    async fn the_main_thread_sticks_and_side_calls_route_freely() {
        let f = fixture(
            vec![
                profile("gpu", "big", Tier::Large, &[]),
                profile("gpu", "tiny", Tier::Small, &[]),
            ],
            &[],
        );
        // CodeEdit wants Large → big, which then sticks for the session.
        let _ = f
            .router
            .stream_chat(routed(TaskKind::CodeEdit, Some("s")))
            .await
            .unwrap();
        // Chat wants Medium (the default model) but the session is on big.
        let _ = f
            .router
            .stream_chat(routed(TaskKind::Chat, Some("s")))
            .await
            .unwrap();
        assert_eq!(f.calls("gpu", "big"), 2);
        // A side call in the same session is routed on its own merits…
        let _ = f
            .router
            .stream_chat(routed(TaskKind::Summarize, Some("s")))
            .await
            .unwrap();
        assert_eq!(f.calls("gpu", "tiny"), 1);
        // …and does not move the session.
        assert_eq!(f.router.current("s"), Some(key("gpu", "big")));
    }

    #[tokio::test]
    async fn only_the_default_model_gets_slot_pinning() {
        let f = fixture(vec![profile("gpu", "big", Tier::Large, &[])], &[]);
        let mut r = routed(TaskKind::CodeEdit, None);
        r.id_slot = Some(2);
        r.slot_hint = Some(SlotHint {
            prefix_hash: "h".into(),
            preferred_slot: Some(2),
        });
        let _ = f.router.stream_chat(r.clone()).await.unwrap();
        let sent = f.others[&key("gpu", "big")].last();
        assert_eq!(sent.id_slot, None);
        assert!(sent.slot_hint.is_none());

        // Chat prefers Medium = the default model: slot pinning survives.
        let mut r = routed(TaskKind::Chat, None);
        r.id_slot = Some(2);
        let _ = f.router.stream_chat(r).await.unwrap();
        assert_eq!(f.default.last().id_slot, Some(2));
    }

    #[tokio::test]
    async fn no_candidate_is_an_actionable_routing_error() {
        let f = fixture(vec![], &[]);
        let mut r = routed(TaskKind::Chat, None);
        r.tools = vec![a_tool()];
        let Err(err) = f.router.stream_chat(r).await else {
            panic!("expected a routing error");
        };
        let text = err.to_string();
        assert!(matches!(err, LlmError::Routing(_)), "{text}");
        assert!(text.contains("tool calling"), "{text}");
        assert!(text.contains("[[routing.models]]"), "{text}");
    }

    #[tokio::test]
    async fn the_last_decision_is_recorded_per_session() {
        let f = fixture(vec![profile("gpu", "big", Tier::Large, &[])], &[]);
        assert!(f.router.last_decision("main").is_none());
        let _ = f
            .router
            .stream_chat(routed(TaskKind::CodeEdit, Some("main")))
            .await
            .unwrap();
        let record = f.router.last_decision("main").unwrap();
        assert_eq!(record.model, key("gpu", "big"));
        assert_eq!(record.task, TaskKind::CodeEdit);
        assert!(
            record.reason.starts_with("chose `big`"),
            "{}",
            record.reason
        );
    }

    #[tokio::test]
    async fn each_pool_backend_is_built_once() {
        let f = fixture(vec![profile("gpu", "big", Tier::Large, &[])], &[]);
        for _ in 0..2 {
            let _ = f
                .router
                .stream_chat(routed(TaskKind::CodeEdit, None))
                .await
                .unwrap();
        }
        assert_eq!(f.built.load(Ordering::SeqCst), 1);
        assert_eq!(f.calls("gpu", "big"), 2);
    }
}
