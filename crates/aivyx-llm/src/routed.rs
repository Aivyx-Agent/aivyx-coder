//! `RoutedBackend`: an `LlmBackend` that picks a model per call by
//! delegating to the shared `aivyx_route::Router` — profiles, stickiness,
//! pins, failure cooldowns, residency, and each session's last decision
//! all live there now; see that crate for the full semantics — and
//! forwards to a lazily-built backend for the chosen model. Untagged
//! requests (`ChatRequest::route == None`) go to the configured default
//! backend unchanged. See `aivyx-ecosystem/docs/superpowers/specs/
//! 2026-09-25-model-routing-design.md`, "Parts 2 & 3".

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aivyx_route::{ModelKey, ModelProfile, ResidencySnapshot, RouteQuery, Router, TaskOverrides};
use async_trait::async_trait;
use futures::stream::BoxStream;

use crate::backend::{ChatRequest, LlmBackend, LlmError, RouteHint, StreamEvent};

pub use aivyx_route::{DEFAULT_COOLDOWN, RouteRecord};

/// Re-reads the candidate list (discovery + roster) for `/models refresh`.
#[async_trait]
pub trait ProfileRefresher: Send + Sync {
    async fn refresh(&self) -> Vec<ModelProfile>;
}

/// Builds the backend serving one model. The error is a human sentence.
pub type BackendFactory =
    Box<dyn Fn(&ModelKey) -> Result<Arc<dyn LlmBackend>, String> + Send + Sync>;

/// Delegates every routing decision (profiles, stickiness, pins,
/// cooldowns, residency, last-decision bookkeeping) to a shared
/// `aivyx_route::Router`; owns only the default backend/model and the
/// lazily-built pool of backends for every other candidate.
pub struct RoutedBackend {
    default_key: ModelKey,
    default_backend: Arc<dyn LlmBackend>,
    factory: BackendFactory,
    refresher: Option<Arc<dyn ProfileRefresher>>,
    router: Router,
    pool: Mutex<HashMap<ModelKey, Arc<dyn LlmBackend>>>,
    /// Appended to the error when no candidate can serve a routed call.
    no_route_hint: String,
}

/// The default [`RoutedBackend::with_no_route_hint`].
const DEFAULT_NO_ROUTE_HINT: &str = "add a capable model to [[routing.models]] (see /models)";

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
            refresher: None,
            router: Router::new(profiles, tasks),
            pool: Mutex::new(HashMap::new()),
            no_route_hint: DEFAULT_NO_ROUTE_HINT.to_string(),
        }
    }

    /// Replaces [`DEFAULT_NO_ROUTE_HINT`], the remedy a "no candidate"
    /// routing error names, when the product knows a better one.
    pub fn with_no_route_hint(mut self, hint: impl Into<String>) -> Self {
        self.no_route_hint = hint.into();
        self
    }

    pub fn default_key(&self) -> &ModelKey {
        &self.default_key
    }

    /// See `aivyx_route::Router::profiles`.
    pub fn profiles(&self) -> Vec<ModelProfile> {
        self.router.profiles()
    }

    /// Sets the residency snapshot. Products refresh it on a short TTL,
    /// never per call, so `select()` can prefer already-loaded models.
    /// See `aivyx_route::Router::set_residency`.
    pub fn set_residency(&self, snapshot: ResidencySnapshot) {
        self.router.set_residency(snapshot);
    }

    /// See `aivyx_route::Router::residency`.
    pub fn residency(&self) -> ResidencySnapshot {
        self.router.residency()
    }

    /// See `aivyx_route::Router::current`.
    pub fn current(&self, session: &str) -> Option<ModelKey> {
        self.router.current(session)
    }

    /// See `aivyx_route::Router::last_decision`.
    pub fn last_decision(&self, session: &str) -> Option<RouteRecord> {
        self.router.last_decision(session)
    }

    pub fn with_cooldown(mut self, cooldown: Duration) -> Self {
        self.router = self.router.with_cooldown(cooldown);
        self
    }

    pub fn with_refresher(mut self, refresher: Arc<dyn ProfileRefresher>) -> Self {
        self.refresher = Some(refresher);
        self
    }

    /// Re-runs discovery + merge. Returns the new candidate count. Also
    /// clears every cooldown (`aivyx_route::Router::set_profiles`).
    pub async fn refresh(&self) -> Result<usize, String> {
        let refresher = self
            .refresher
            .as_ref()
            .ok_or_else(|| "this router has no discovery configured".to_string())?;
        let profiles = refresher.refresh().await;
        let count = profiles.len();
        self.router.set_profiles(profiles);
        Ok(count)
    }

    /// See `aivyx_route::Router::pin`.
    pub fn pin(&self, session: &str, key: ModelKey) {
        self.router.pin(session, key);
    }

    /// See `aivyx_route::Router::unpin`.
    pub fn unpin(&self, session: &str) {
        self.router.unpin(session);
    }

    /// See `aivyx_route::Router::pinned`.
    pub fn pinned(&self, session: &str) -> Option<ModelKey> {
        self.router.pinned(session)
    }

    /// See `aivyx_route::Router::forget_session`.
    pub fn forget_session(&self, session: &str) {
        self.router.forget_session(session);
    }

    /// The default model is served by the configured backend; every
    /// other model's backend is built once, on first use.
    fn backend_for(&self, key: &ModelKey) -> Result<Arc<dyn LlmBackend>, String> {
        if *key == self.default_key {
            return Ok(Arc::clone(&self.default_backend));
        }
        let mut pool = self.pool.lock().unwrap();
        if let Some(backend) = pool.get(key) {
            return Ok(Arc::clone(backend));
        }
        let backend = (self.factory)(key)?;
        pool.insert(key.clone(), Arc::clone(&backend));
        Ok(backend)
    }
}

/// `ChatRequest` + `RouteHint` → what the shared Router needs to plan a
/// call. aivyx-coder has no image content blocks, so `vision` is always
/// `false`.
fn query(request: &ChatRequest, hint: &RouteHint) -> RouteQuery {
    RouteQuery {
        task: hint.task.clone(),
        session: hint.session.clone(),
        tools: !request.tools.is_empty(),
        vision: false,
        estimated_prompt_tokens: hint.estimated_prompt_tokens,
        // aivyx-coder has no classifier-driven soft tier of its own yet.
        tier: None,
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
        let query = query(&request, &hint);
        let plan = self
            .router
            .plan(&query, Instant::now())
            .map_err(|e| LlmError::Routing(format!("{e} — {}", self.no_route_hint)))?;
        if plan.reason.contains("; warning: it may lack") {
            tracing::warn!(model = %plan.chain[0], reason = %plan.reason, "pinned model may lack a hard requirement");
        }
        let mut failures: Vec<String> = Vec::new();
        for key in &plan.chain {
            let backend = match self.backend_for(key) {
                Ok(backend) => backend,
                Err(why) => {
                    self.router.failed(key, Instant::now());
                    failures.push(format!("`{key}` ({why})"));
                    continue;
                }
            };
            let mut attempt = request.clone();
            if *key != self.default_key {
                // Slot ids/hints describe the default server's KV cache.
                attempt.id_slot = None;
                attempt.slot_hint = None;
            }
            match backend.stream_chat(attempt).await {
                Ok(stream) => {
                    let record = self.router.succeeded(&plan, key, &failures);
                    tracing::info!(model = %key, task = hint.task.name(), reason = %record.reason, "routed model call");
                    return Ok(stream);
                }
                Err(err) if is_retryable(&err) => {
                    self.router.failed(key, Instant::now());
                    failures.push(format!("`{key}` ({err})"));
                }
                Err(err) => return Err(err),
            }
        }
        Err(LlmError::Routing(format!(
            "every candidate failed: {}",
            failures.join(", ")
        )))
    }
}

/// Connection trouble, a missing/unloadable model, or a server error: try
/// the next candidate. Anything else (a 400, a parse error) would fail the
/// same way on every model.
fn is_retryable(err: &LlmError) -> bool {
    match err {
        LlmError::Request(_) | LlmError::Timeout => true,
        LlmError::BackendError { status, .. } => matches!(status, 404 | 408 | 500..=599),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use aivyx_route::{Capability, EndpointRef, TaskKind, Tier};
    use aivyx_types::{Message, Role, ToolDefinition};
    use futures::StreamExt;

    use crate::backend::{FinishReason, SlotHint};

    /// Records every request; answers with an empty successful stream, or
    /// with `BackendError { status }` when `fail` is set. `succeed_first`
    /// makes the very first call always succeed regardless of `fail`, so a
    /// test can establish real stickiness before the model starts failing.
    struct Scripted {
        name: String,
        fail: Option<u16>,
        succeed_first: bool,
        received: Mutex<Vec<ChatRequest>>,
    }

    impl Scripted {
        fn new(name: &str, fail: Option<u16>) -> Arc<Self> {
            Arc::new(Scripted {
                name: name.to_string(),
                fail,
                succeed_first: false,
                received: Mutex::new(Vec::new()),
            })
        }
        /// The first call always succeeds; every call after that fails
        /// with `status`.
        fn succeed_then_fail(name: &str, status: u16) -> Arc<Self> {
            Arc::new(Scripted {
                name: name.to_string(),
                fail: Some(status),
                succeed_first: true,
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
            let call_no = {
                let mut received = self.received.lock().unwrap();
                received.push(request);
                received.len()
            };
            if let Some(status) = self.fail
                && !(self.succeed_first && call_no == 1)
            {
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
            let mut profiles = f.router.router.profiles();
            profiles[0].context_window = Some(8_192);
            f.router.router.set_profiles(profiles);
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
        // Exact hint suffix (Global Constraints, exception 2).
        assert!(
            text.contains(" — add a capable model to [[routing.models]] (see /models)"),
            "{text}"
        );
    }

    /// Guard test: pins the delegation seam. `last_decision` must return
    /// the shared `aivyx_route::RouteRecord`, not a local look-alike type.
    #[tokio::test]
    async fn last_decision_is_the_shared_routers_record_type() {
        let f = fixture(vec![profile("gpu", "big", Tier::Large, &[])], &[]);
        let _ = f
            .router
            .stream_chat(routed(TaskKind::CodeEdit, Some("s")))
            .await
            .unwrap();
        let record: aivyx_route::RouteRecord = f.router.last_decision("s").unwrap();
        assert_eq!(record.model, key("gpu", "big"));
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

    #[tokio::test]
    async fn a_failing_model_falls_back_and_cools_down() {
        let f = fixture(
            vec![profile("gpu", "big", Tier::Large, &[])],
            &[("gpu", "big", 503)],
        );
        let _ = f
            .router
            .stream_chat(routed(TaskKind::CodeEdit, Some("s")))
            .await
            .unwrap();
        assert_eq!(f.calls("gpu", "big"), 1);
        assert_eq!(f.default.calls(), 1);
        let record = f.router.last_decision("s").unwrap();
        assert_eq!(record.model, key("backend", "default"));
        assert!(
            record.reason.contains("fell back after `big@gpu`"),
            "{}",
            record.reason
        );
        // A fallback is temporary: the session is not moved onto it.
        assert_eq!(f.router.current("s"), None);
        // Cooling down: the next call doesn't retry big.
        let _ = f
            .router
            .stream_chat(routed(TaskKind::CodeEdit, None))
            .await
            .unwrap();
        assert_eq!(f.calls("gpu", "big"), 1);
    }

    #[tokio::test]
    async fn a_zero_cooldown_retries_the_failed_model() {
        let mut f = fixture(
            vec![profile("gpu", "big", Tier::Large, &[])],
            &[("gpu", "big", 503)],
        );
        f.router = f.router.with_cooldown(Duration::ZERO);
        for _ in 0..2 {
            let _ = f
                .router
                .stream_chat(routed(TaskKind::CodeEdit, None))
                .await
                .unwrap();
        }
        assert_eq!(f.calls("gpu", "big"), 2);
    }

    #[tokio::test]
    async fn non_retryable_errors_are_returned_without_fallback() {
        let f = fixture(
            vec![profile("gpu", "big", Tier::Large, &[])],
            &[("gpu", "big", 400)],
        );
        let Err(err) = f.router.stream_chat(routed(TaskKind::CodeEdit, None)).await else {
            panic!("expected the 400 back");
        };
        assert!(matches!(err, LlmError::BackendError { status: 400, .. }));
        assert_eq!(f.default.calls(), 0);
    }

    #[tokio::test]
    async fn exhausting_every_candidate_names_the_chain() {
        let f = fixture(
            vec![
                profile("gpu", "big", Tier::Large, &[]),
                profile("gpu", "mid", Tier::Medium, &[]),
            ],
            &[("gpu", "big", 503), ("gpu", "mid", 404)],
        );
        // Make the default model ineligible so only big and mid remain.
        let mut profiles = f.router.router.profiles();
        profiles[0].availability = aivyx_route::Availability::Unavailable;
        f.router.router.set_profiles(profiles);
        let Err(err) = f.router.stream_chat(routed(TaskKind::CodeEdit, None)).await else {
            panic!("expected every candidate to fail");
        };
        let text = err.to_string();
        assert!(text.contains("every candidate failed"), "{text}");
        assert!(
            text.contains("`big@gpu`") && text.contains("`mid@gpu`"),
            "{text}"
        );
    }

    #[tokio::test]
    async fn a_factory_error_falls_back() {
        let f = fixture(vec![], &[]);
        // In the candidate list but the factory has no backend for it.
        let mut profiles = f.router.router.profiles();
        profiles.push(profile("gpu", "ghost", Tier::Large, &[]));
        f.router.router.set_profiles(profiles);
        let _ = f
            .router
            .stream_chat(routed(TaskKind::CodeEdit, Some("s")))
            .await
            .unwrap();
        assert_eq!(f.default.calls(), 1);
        assert!(
            f.router
                .last_decision("s")
                .unwrap()
                .reason
                .contains("ghost@gpu")
        );
    }

    #[tokio::test]
    async fn a_pin_overrides_selection_and_warns_on_unmet_needs() {
        let f = fixture(
            vec![
                profile("gpu", "big", Tier::Large, &[Capability::Tools]),
                profile("gpu", "tiny", Tier::Small, &[]),
            ],
            &[],
        );
        f.router.pin("s", key("gpu", "tiny"));
        assert_eq!(f.router.pinned("s"), Some(key("gpu", "tiny")));
        let mut r = routed(TaskKind::CodeEdit, Some("s"));
        r.tools = vec![a_tool()];
        let _ = f.router.stream_chat(r.clone()).await.unwrap();
        assert_eq!(f.calls("gpu", "tiny"), 1);
        let reason = f.router.last_decision("s").unwrap().reason;
        // Router wording (Global Constraints, exception 1): `pinned to`,
        // not the old `pinned with /model to`.
        assert!(reason.contains("pinned to `tiny@gpu`"), "{reason}");
        assert!(reason.contains("may lack tool calling"), "{reason}");

        f.router.unpin("s");
        let _ = f.router.stream_chat(r).await.unwrap();
        assert_eq!(f.calls("gpu", "big"), 1);
    }

    #[tokio::test]
    async fn unpinning_returns_the_session_to_routing() {
        let f = fixture(
            vec![
                profile("gpu", "big", Tier::Large, &[]),
                profile("gpu", "tiny", Tier::Small, &[]),
            ],
            &[],
        );
        f.router.pin("s", key("gpu", "tiny"));
        let _ = f
            .router
            .stream_chat(routed(TaskKind::CodeEdit, Some("s")))
            .await
            .unwrap();
        assert_eq!(f.calls("gpu", "tiny"), 1);
        // While pinned, the pin is the session's current model.
        assert_eq!(f.router.current("s"), Some(key("gpu", "tiny")));
        // No forget_session: `/model auto` alone must hand the session back.
        f.router.unpin("s");
        let _ = f
            .router
            .stream_chat(routed(TaskKind::CodeEdit, Some("s")))
            .await
            .unwrap();
        assert_eq!(f.calls("gpu", "big"), 1);
        assert_eq!(f.calls("gpu", "tiny"), 1);
        assert_eq!(f.router.current("s"), Some(key("gpu", "big")));
    }

    #[tokio::test]
    async fn a_pin_does_not_capture_side_calls() {
        let f = fixture(
            vec![
                profile("gpu", "big", Tier::Large, &[]),
                profile("gpu", "tiny", Tier::Small, &[]),
            ],
            &[],
        );
        f.router.pin("s", key("gpu", "big"));
        let _ = f
            .router
            .stream_chat(routed(TaskKind::Summarize, Some("s")))
            .await
            .unwrap();
        assert_eq!(f.calls("gpu", "tiny"), 1);
        assert_eq!(f.calls("gpu", "big"), 0);
    }

    #[tokio::test]
    async fn forget_session_clears_stickiness_but_keeps_the_pin() {
        let f = fixture(vec![profile("gpu", "big", Tier::Large, &[])], &[]);
        let call = || f.router.stream_chat(routed(TaskKind::CodeEdit, Some("s")));
        // Stickiness alone (no pin): forgetting the session clears it.
        let _ = call().await.unwrap();
        assert_eq!(f.router.current("s"), Some(key("gpu", "big")));
        f.router.forget_session("s");
        assert_eq!(f.router.current("s"), None, "stickiness must be cleared");
        assert!(f.router.last_decision("s").is_none());
        // A pin, on the other hand, survives it.
        let _ = call().await.unwrap();
        f.router.pin("s", key("gpu", "big"));
        f.router.forget_session("s");
        assert_eq!(f.router.pinned("s"), Some(key("gpu", "big")));
        assert_eq!(f.router.current("s"), Some(key("gpu", "big")));
    }

    struct FixedRefresher(Vec<ModelProfile>);

    #[async_trait]
    impl ProfileRefresher for FixedRefresher {
        async fn refresh(&self) -> Vec<ModelProfile> {
            self.0.clone()
        }
    }

    #[tokio::test]
    async fn refresh_replaces_the_candidates() {
        let f = fixture(vec![], &[]);
        assert!(f.router.refresh().await.is_err(), "no refresher configured");
        let fresh = vec![
            profile("backend", "default", Tier::Medium, &[]),
            profile("gpu", "new", Tier::Large, &[]),
        ];
        let router = f
            .router
            .with_refresher(Arc::new(FixedRefresher(fresh.clone())));
        assert_eq!(router.refresh().await, Ok(2));
        assert_eq!(router.profiles(), fresh);
    }

    #[tokio::test]
    async fn cooling_models_are_retried_when_nothing_else_qualifies() {
        let f = fixture(
            vec![profile("gpu", "big", Tier::Large, &[])],
            &[("gpu", "big", 503)],
        );
        let mut profiles = f.router.router.profiles();
        profiles[0].availability = aivyx_route::Availability::Unavailable;
        f.router.router.set_profiles(profiles);
        for attempt in 1..=2 {
            let Err(err) = f.router.stream_chat(routed(TaskKind::CodeEdit, None)).await else {
                panic!("big always fails");
            };
            let text = err.to_string();
            assert!(text.contains("every candidate failed"), "{text}");
            assert!(!text.contains("[[routing.models]]"), "{text}");
            assert_eq!(f.calls("gpu", "big"), attempt);
        }
    }

    #[tokio::test]
    async fn residency_snapshot_affects_model_selection() {
        use aivyx_route::ModelResidency;

        // Two same-tier (Small) candidates for Summarize task. Without
        // residency, both are equal so first in list is chosen. With
        // residency marking beta as loaded, beta should rank higher.
        let f = fixture(
            vec![
                profile("gpu", "alpha", Tier::Small, &[]),
                profile("gpu", "beta", Tier::Small, &[]),
            ],
            &[],
        );

        // Summarize task wants Small. Without residency, alpha (first) is chosen.
        let _ = f
            .router
            .stream_chat(routed(TaskKind::Summarize, None))
            .await
            .unwrap();
        assert_eq!(f.calls("gpu", "alpha"), 1);
        assert_eq!(f.calls("gpu", "beta"), 0);

        // Now mark beta as loaded (cost 0) while alpha stays unknown (cost 2).
        let mut snapshot = ResidencySnapshot::default();
        snapshot.models.insert(
            key("gpu", "beta"),
            ModelResidency::Loaded { vram_bytes: None },
        );
        f.router.set_residency(snapshot.clone());

        // The next call should prefer beta (loaded, lower cost).
        let _ = f
            .router
            .stream_chat(routed(TaskKind::Summarize, None))
            .await
            .unwrap();
        assert_eq!(f.calls("gpu", "beta"), 1);
        assert_eq!(f.calls("gpu", "alpha"), 1);

        // residency() returns the snapshot.
        assert_eq!(f.router.residency(), snapshot);
    }

    #[tokio::test]
    async fn a_session_returns_to_its_model_once_the_cooldown_ends() {
        // `big` succeeds once (establishing real stickiness through an
        // actual call, since only a clean first choice may stick) and
        // fails with 503 on every call after that.
        let default = Scripted::new("default", None);
        let big = Scripted::succeed_then_fail("big", 503);
        let profiles = vec![
            profile("backend", "default", Tier::Medium, &[]),
            profile("gpu", "big", Tier::Large, &[]),
        ];
        let big_key = key("gpu", "big");
        let pool: HashMap<ModelKey, Arc<Scripted>> =
            HashMap::from([(big_key.clone(), Arc::clone(&big))]);
        let factory: BackendFactory = Box::new(move |k: &ModelKey| {
            pool.get(k)
                .map(|b| Arc::clone(b) as Arc<dyn LlmBackend>)
                .ok_or_else(|| format!("no backend for `{k}`"))
        });
        let backend = RoutedBackend::new(
            key("backend", "default"),
            Arc::clone(&default) as Arc<dyn LlmBackend>,
            profiles,
            TaskOverrides::default(),
            factory,
        );

        // First call: big succeeds, so the session sticks to it.
        let _ = backend
            .stream_chat(routed(TaskKind::CodeEdit, Some("s")))
            .await
            .unwrap();
        assert_eq!(backend.current("s"), Some(big_key.clone()));
        assert_eq!(big.calls(), 1);

        // Second call: big now fails and cools down; the session falls
        // back to the default model without losing its stickiness to big.
        let _ = backend
            .stream_chat(routed(TaskKind::CodeEdit, Some("s")))
            .await
            .unwrap();
        assert_eq!(default.calls(), 1);
        assert_eq!(big.calls(), 2);
        assert_eq!(backend.current("s"), Some(big_key.clone()));

        // While big cools down, calls keep going to the default without
        // even attempting big.
        let _ = backend
            .stream_chat(routed(TaskKind::CodeEdit, Some("s")))
            .await
            .unwrap();
        assert_eq!(default.calls(), 2);
        assert_eq!(big.calls(), 2);
        assert_eq!(backend.current("s"), Some(big_key.clone()));

        // Cooldown over (the same mechanism `/models refresh` uses to
        // clear it): the session's own model is tried again.
        let fresh = backend.router.profiles();
        backend.router.set_profiles(fresh);
        let _ = backend
            .stream_chat(routed(TaskKind::CodeEdit, Some("s")))
            .await
            .unwrap();
        assert_eq!(big.calls(), 3);
    }
}
