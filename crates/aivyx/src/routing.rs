//! Builds the `RoutedBackend` from `[routing]` at startup. With routing
//! off, `wrap_with_routing` hands back the configured backend untouched.

use std::sync::Arc;

use aivyx_config::{BackendKind, Settings};
use aivyx_llm::{BackendFactory, LlmBackend, OpenAiCompatBackend, ProfileRefresher, RoutedBackend};
use aivyx_route::{
    Capability, DefaultEndpoint, EndpointKind, EndpointRef, ModelKey, ModelProfile, RosterEntry,
    RoutingConfig, find, merge,
};

/// The `[backend]` section, as a routing endpoint name.
pub(crate) const DEFAULT_ENDPOINT: &str = "backend";

pub(crate) fn default_endpoint() -> DefaultEndpoint {
    DefaultEndpoint {
        name: EndpointRef::new(DEFAULT_ENDPOINT),
        // Every [backend] kind aivyx-coder supports is local.
        kind: EndpointKind::OpenaiCompat,
    }
}

/// aivyx-coder never calls a cloud API, and `backend` names `[backend]`.
pub(crate) fn check_routing_config(config: &RoutingConfig) -> anyhow::Result<()> {
    for (name, endpoint) in &config.endpoints {
        if name == DEFAULT_ENDPOINT {
            anyhow::bail!(
                "[routing.endpoints.{DEFAULT_ENDPOINT}] is reserved — it means the [backend] \
                 section; give this endpoint another name"
            );
        }
        if matches!(
            endpoint.kind,
            EndpointKind::Anthropic | EndpointKind::Openai
        ) {
            anyhow::bail!(
                "[routing.endpoints.{name}] is a cloud endpoint, but aivyx-coder only talks to \
                 local models — remove it"
            );
        }
    }
    Ok(())
}

/// `settings.routing` plus an implicit roster entry for `[backend] model`
/// on the default endpoint when no entry names it already.
pub(crate) fn effective_routing_config(settings: &Settings) -> RoutingConfig {
    let mut config = settings.routing.clone();
    let named = config.models.iter().any(|m| {
        m.id == settings.backend.model
            && m.endpoint.as_deref().is_none_or(|e| e == DEFAULT_ENDPOINT)
    });
    if !named {
        config.models.push(RosterEntry {
            id: settings.backend.model.clone(),
            endpoint: None,
            locality: None,
            tier: None,
            strengths: None,
            priority: None,
            capabilities: Default::default(),
            capabilities_deny: Default::default(),
            context_window: None,
        });
    }
    config
}

/// Routing endpoints are configured like discovery sees them
/// (`http://host:11434`); chat goes to their OpenAI-compatible `/v1`.
pub(crate) fn chat_base_url(base: &str) -> String {
    let base = base.trim_end_matches('/');
    if base.ends_with("/v1") {
        base.to_string()
    } else {
        format!("{base}/v1")
    }
}

pub(crate) fn backend_factory(settings: &Settings, config: &RoutingConfig) -> BackendFactory {
    let backend = settings.backend.clone();
    let endpoints = config.endpoints.clone();
    Box::new(move |key: &ModelKey| {
        let base =
            if key.endpoint.as_str() == DEFAULT_ENDPOINT {
                match backend.kind {
                    BackendKind::Generic | BackendKind::LlamaServer => backend.base_url.clone(),
                    BackendKind::LlamaServerBroker => backend
                        .broker_base_url
                        .clone()
                        .ok_or_else(|| "backend.broker_base_url is not set".to_string())?,
                    BackendKind::MistralRs => {
                        return Err(format!(
                            "the embedded mistral.rs backend serves only `{}`",
                            backend.model
                        ));
                    }
                }
            } else {
                let endpoint = endpoints.get(key.endpoint.as_str()).ok_or_else(|| {
                    format!("no [routing.endpoints.{}] is configured", key.endpoint)
                })?;
                chat_base_url(endpoint.base_url().ok_or_else(|| {
                    format!("[routing.endpoints.{}] has no base_url", key.endpoint)
                })?)
            };
        let api_key = (key.endpoint.as_str() == DEFAULT_ENDPOINT)
            .then(|| backend.api_key.clone())
            .flatten();
        Ok(
            Arc::new(OpenAiCompatBackend::new(base, key.id.clone(), api_key))
                as Arc<dyn LlmBackend>,
        )
    })
}

/// A warning when the `[backend]` model's tool support is undeclared while
/// another candidate's is known: aivyx-route ranks unknown capabilities and
/// windows below known ones, so it would lose every main-loop call.
pub(crate) fn backend_caps_undeclared_warning(
    profiles: &[ModelProfile],
    default: &ModelKey,
) -> Option<String> {
    let backend = find(profiles, default)?;
    if !backend.unknown_capabilities.contains(&Capability::Tools) {
        return None;
    }
    let others_known = profiles
        .iter()
        .any(|p| p.key() != *default && p.capabilities.contains(&Capability::Tools));
    others_known.then(|| {
        format!(
            "the [backend] model `{}` has no declared capabilities, so routing ranks it below \
             every model known to call tools — add a [[routing.models]] entry for it with \
             capabilities = [\"tools\", ...] and context_window = <your served window>",
            default.id
        )
    })
}

/// Discovery (when `[routing] discover`) + merge, for startup and
/// `/models refresh`.
struct DiscoveryRefresher {
    config: RoutingConfig,
    client: reqwest::Client,
}

#[async_trait::async_trait]
impl ProfileRefresher for DiscoveryRefresher {
    async fn refresh(&self) -> Vec<ModelProfile> {
        let reports = if self.config.discover {
            aivyx_route::discovery::discover_all(&self.config, &self.client).await
        } else {
            Vec::new()
        };
        merge(&self.config, &default_endpoint(), &reports)
    }
}

/// Routing off ⇒ `(llm, None)` with `llm` untouched. Routing on ⇒ a
/// `RoutedBackend` whose default is `llm`.
pub(crate) async fn wrap_with_routing(
    settings: &Settings,
    llm: Arc<dyn LlmBackend>,
) -> anyhow::Result<(Arc<dyn LlmBackend>, Option<Arc<RoutedBackend>>)> {
    if !settings.routing.enabled {
        return Ok((llm, None));
    }
    check_routing_config(&settings.routing)?;
    let config = effective_routing_config(settings);
    for issue in config.validate(&default_endpoint()) {
        tracing::warn!(%issue, "routing config");
    }
    let refresher = Arc::new(DiscoveryRefresher {
        config: config.clone(),
        client: reqwest::Client::new(),
    });
    let profiles = refresher.refresh().await;
    let default_key = ModelKey {
        endpoint: EndpointRef::new(DEFAULT_ENDPOINT),
        id: settings.backend.model.clone(),
    };
    if let Some(warning) = backend_caps_undeclared_warning(&profiles, &default_key) {
        tracing::warn!("{warning}");
    }
    let router = Arc::new(
        RoutedBackend::new(
            default_key,
            llm,
            profiles,
            config.tasks.clone(),
            backend_factory(settings, &config),
        )
        .with_refresher(refresher),
    );
    tracing::info!(
        candidates = router.profiles().len(),
        "model routing enabled"
    );
    Ok((Arc::clone(&router) as Arc<dyn LlmBackend>, Some(router)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use aivyx_config::{BackendKind, Settings};
    use aivyx_route::EndpointConfig;

    fn settings_with(routing_toml: &str) -> Settings {
        let mut s: Settings = toml::from_str(routing_toml).unwrap();
        s.backend.model = "qwen3.5:9b".into();
        s.backend.base_url = "http://localhost:11434/v1".into();
        s
    }

    #[test]
    fn chat_base_url_appends_v1_once() {
        assert_eq!(
            chat_base_url("http://localhost:11434"),
            "http://localhost:11434/v1"
        );
        assert_eq!(
            chat_base_url("http://localhost:11434/"),
            "http://localhost:11434/v1"
        );
        assert_eq!(chat_base_url("http://h:1337/v1"), "http://h:1337/v1");
        assert_eq!(chat_base_url("http://h:1337/v1/"), "http://h:1337/v1");
    }

    #[test]
    fn cloud_endpoints_and_the_reserved_name_are_rejected() {
        let s = settings_with("[routing.endpoints.claude]\nkind = \"anthropic\"\n");
        let err = check_routing_config(&s.routing).unwrap_err().to_string();
        assert!(err.contains("claude") && err.contains("local"), "{err}");

        let s = settings_with("[routing.endpoints.backend]\nkind = \"ollama\"\n");
        let err = check_routing_config(&s.routing).unwrap_err().to_string();
        assert!(err.contains("reserved"), "{err}");

        let s = settings_with("[routing.endpoints.gpu]\nkind = \"ollama\"\n");
        assert!(check_routing_config(&s.routing).is_ok());
    }

    #[test]
    fn the_backend_model_becomes_an_implicit_roster_entry() {
        let s = settings_with("[[routing.models]]\nid = \"other\"\n");
        let c = effective_routing_config(&s);
        let ids: Vec<(&str, Option<&str>)> = c
            .models
            .iter()
            .map(|m| (m.id.as_str(), m.endpoint.as_deref()))
            .collect();
        assert_eq!(ids, vec![("other", None), ("qwen3.5:9b", None)]);
    }

    #[test]
    fn an_existing_entry_for_the_backend_model_is_not_duplicated() {
        for endpoint in ["", "endpoint = \"backend\"\n"] {
            let s = settings_with(&format!(
                "[[routing.models]]\nid = \"qwen3.5:9b\"\n{endpoint}tier = \"small\"\n"
            ));
            assert_eq!(effective_routing_config(&s).models.len(), 1, "{endpoint:?}");
        }
    }

    fn key(endpoint: &str, id: &str) -> aivyx_route::ModelKey {
        aivyx_route::ModelKey {
            endpoint: aivyx_route::EndpointRef::new(endpoint),
            id: id.into(),
        }
    }

    #[test]
    fn the_factory_builds_openai_compatible_backends_per_endpoint() {
        let mut s = settings_with("");
        s.routing.endpoints.insert(
            "gpu".into(),
            EndpointConfig {
                kind: aivyx_route::EndpointKind::Ollama,
                base_url: Some("http://gpu:11434".into()),
            },
        );
        let factory = backend_factory(&s, &s.routing);
        assert_eq!(
            factory(&key("gpu", "llava:13b")).unwrap().model_id(),
            "llava:13b"
        );
        assert_eq!(
            factory(&key("backend", "qwen3:32b")).unwrap().model_id(),
            "qwen3:32b"
        );
        assert!(
            factory(&key("nowhere", "x"))
                .err()
                .unwrap()
                .contains("nowhere")
        );
    }

    #[test]
    fn the_embedded_backend_serves_only_its_own_model() {
        let mut s = settings_with("");
        s.backend.kind = BackendKind::MistralRs;
        let factory = backend_factory(&s, &s.routing);
        assert!(
            factory(&key("backend", "other"))
                .err()
                .unwrap()
                .contains("mistral")
        );
    }

    fn merged(
        profiles: &[(
            &str,
            &str,
            &[aivyx_route::Capability],
            &[aivyx_route::Capability],
        )],
    ) -> Vec<ModelProfile> {
        profiles
            .iter()
            .map(|(ep, id, known, unknown)| {
                let mut p = ModelProfile::new(*id, EndpointRef::new(*ep));
                p.capabilities.extend(known.iter().copied());
                p.unknown_capabilities.extend(unknown.iter().copied());
                p
            })
            .collect()
    }

    #[test]
    fn an_undeclared_backend_model_is_warned_about() {
        use aivyx_route::Capability::{Completion, Tools};
        let default = key("backend", "qwen3.5:9b");
        // The implicit entry (all unknown) vs a discovered tool-caller.
        let ps = merged(&[
            ("backend", "qwen3.5:9b", &[], &[Completion, Tools]),
            ("gpu", "coder", &[Completion, Tools], &[]),
        ]);
        let warning = backend_caps_undeclared_warning(&ps, &default).expect("should warn");
        assert!(warning.contains("qwen3.5:9b"), "{warning}");
        assert!(warning.contains("[[routing.models]]"), "{warning}");
        assert!(warning.contains("capabilities"), "{warning}");

        // Declared tools on the backend model: no warning.
        let ps = merged(&[
            ("backend", "qwen3.5:9b", &[Completion, Tools], &[]),
            ("gpu", "coder", &[Completion, Tools], &[]),
        ]);
        assert_eq!(backend_caps_undeclared_warning(&ps, &default), None);

        // Nobody's tool support is known: nothing to lose to.
        let ps = merged(&[
            ("backend", "qwen3.5:9b", &[], &[Completion, Tools]),
            ("gpu", "coder", &[], &[Tools]),
        ]);
        assert_eq!(backend_caps_undeclared_warning(&ps, &default), None);
    }

    #[tokio::test]
    async fn the_backend_api_key_goes_only_to_the_backend_endpoint() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let backend_server = MockServer::start().await;
        let gpu_server = MockServer::start().await;
        for server in [&backend_server, &gpu_server] {
            Mock::given(method("POST"))
                .respond_with(ResponseTemplate::new(500))
                .mount(server)
                .await;
        }
        let mut s = settings_with("");
        s.backend.base_url = format!("{}/v1", backend_server.uri());
        s.backend.api_key = Some("backend-secret".into());
        s.routing.endpoints.insert(
            "gpu".into(),
            EndpointConfig {
                kind: aivyx_route::EndpointKind::Ollama,
                base_url: Some(gpu_server.uri()),
            },
        );
        let factory = backend_factory(&s, &s.routing);
        let request = || {
            aivyx_llm::ChatRequest::new(vec![aivyx_types::Message::text(
                aivyx_types::Role::User,
                "hi",
            )])
        };
        let _ = factory(&key("backend", "other"))
            .unwrap()
            .stream_chat(request())
            .await;
        let _ = factory(&key("gpu", "coder"))
            .unwrap()
            .stream_chat(request())
            .await;

        let auth = |reqs: Vec<wiremock::Request>| -> Vec<Option<String>> {
            reqs.iter()
                .map(|r| {
                    r.headers
                        .get("authorization")
                        .map(|v| v.to_str().unwrap().to_string())
                })
                .collect()
        };
        assert_eq!(
            auth(backend_server.received_requests().await.unwrap()),
            [Some("Bearer backend-secret".to_string())]
        );
        assert_eq!(auth(gpu_server.received_requests().await.unwrap()), [None]);
    }

    #[tokio::test]
    async fn routing_off_returns_the_same_backend() {
        let s = settings_with("");
        let llm: Arc<dyn LlmBackend> = Arc::new(aivyx_llm::OpenAiCompatBackend::new(
            s.backend.base_url.clone(),
            s.backend.model.clone(),
            None,
        ));
        let (wrapped, router) = wrap_with_routing(&s, Arc::clone(&llm)).await.unwrap();
        assert!(Arc::ptr_eq(&wrapped, &llm));
        assert!(router.is_none());
    }

    #[tokio::test]
    async fn routing_on_without_discovery_offers_the_roster() {
        let s = settings_with(
            "[routing]\nenabled = true\ndiscover = false\n[[routing.models]]\nid = \"qwen3-coder:30b\"\ntier = \"large\"\n",
        );
        let llm: Arc<dyn LlmBackend> = Arc::new(aivyx_llm::OpenAiCompatBackend::new(
            s.backend.base_url.clone(),
            s.backend.model.clone(),
            None,
        ));
        let (wrapped, router) = wrap_with_routing(&s, Arc::clone(&llm)).await.unwrap();
        let router = router.expect("routing is on");
        assert!(!Arc::ptr_eq(&wrapped, &llm));
        assert_eq!(router.default_key(), &key("backend", "qwen3.5:9b"));
        let keys: Vec<String> = router
            .profiles()
            .iter()
            .map(|p| p.key().to_string())
            .collect();
        assert_eq!(keys, vec!["qwen3-coder:30b@backend", "qwen3.5:9b@backend"]);
        assert_eq!(wrapped.model_id(), "qwen3.5:9b");
    }

    #[tokio::test]
    async fn a_cloud_endpoint_fails_startup() {
        let s = settings_with(
            "[routing]\nenabled = true\n[routing.endpoints.claude]\nkind = \"anthropic\"\n",
        );
        let llm: Arc<dyn LlmBackend> = Arc::new(aivyx_llm::OpenAiCompatBackend::new(
            "http://x/v1",
            "m",
            None,
        ));
        assert!(wrap_with_routing(&s, llm).await.is_err());
    }
}
