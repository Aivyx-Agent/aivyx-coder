//! Builds the `RoutedBackend` from `[routing]` at startup. With routing
//! off, `wrap_with_routing` hands back the configured backend untouched.

use std::sync::{Arc, Weak};
use std::time::Duration;

use aivyx_config::{BackendKind, BackendSettings, Settings};
use aivyx_llm::{BackendFactory, LlmBackend, OpenAiCompatBackend, ProfileRefresher, RoutedBackend};
use aivyx_route::discovery::residency::{BrokerSource, collect, endpoints_of};
use aivyx_route::{
    Capability, ConfigIssue, DefaultEndpoint, EndpointConfig, EndpointKind, EndpointRef, Locality,
    ModelKey, ModelProfile, ResidencySnapshot, RosterEntry, RoutingConfig, find, merge,
};

/// The `[backend]` section, as a routing endpoint name.
pub(crate) const DEFAULT_ENDPOINT: &str = "backend";

/// `[backend]` as aivyx-route's default endpoint, with the address chat
/// actually connects to, so its locality follows the same rule as any
/// routing endpoint's (`DefaultEndpoint::effective_locality`).
pub(crate) fn default_endpoint(backend: &BackendSettings) -> DefaultEndpoint {
    let (base_url, implied) = match backend.kind {
        BackendKind::Generic | BackendKind::LlamaServer => (Some(backend.base_url.clone()), None),
        BackendKind::LlamaServerBroker => (backend.broker_base_url.clone(), None),
        // In-process: nothing leaves the machine, whatever base_url says.
        BackendKind::MistralRs => (None, Some(Locality::Local)),
    };
    DefaultEndpoint {
        base_url,
        locality: backend.locality.or(implied),
        ..DefaultEndpoint::new(DEFAULT_ENDPOINT, EndpointKind::OpenaiCompat)
    }
}

/// aivyx-coder never routes to a cloud endpoint — by kind, by address, or
/// marked `locality = "cloud"` (`EndpointConfig::effective_locality`) —
/// and `backend` names `[backend]`.
pub(crate) fn check_routing_config(config: &RoutingConfig) -> anyhow::Result<()> {
    for (name, endpoint) in &config.endpoints {
        if name == DEFAULT_ENDPOINT {
            anyhow::bail!(
                "[routing.endpoints.{DEFAULT_ENDPOINT}] is reserved — it means the [backend] \
                 section; give this endpoint another name"
            );
        }
        if endpoint.effective_locality() == Locality::Cloud {
            if matches!(
                endpoint.kind,
                EndpointKind::Anthropic | EndpointKind::Openai
            ) {
                anyhow::bail!(
                    "[routing.endpoints.{name}] is a cloud endpoint, but aivyx-coder only talks \
                     to local models — remove it"
                );
            }
            if endpoint.locality == Some(Locality::Cloud) {
                anyhow::bail!(
                    "[routing.endpoints.{name}] is marked `locality = \"cloud\"`, but aivyx-coder \
                     only talks to local models — remove it"
                );
            }
            let address = endpoint.base_url().unwrap_or("no base_url");
            anyhow::bail!(
                "[routing.endpoints.{name}] ({address}) is not a local address, so it counts as \
                 cloud, and aivyx-coder only talks to local models — if it is on your own \
                 network, set `locality = \"local\"` on it; otherwise remove it"
            );
        }
    }
    Ok(())
}

/// aivyx-route's config warnings, with the ones about the default endpoint
/// pointed at `[backend]` (there is no `[routing.endpoints.backend]` to
/// set `locality` on).
pub(crate) fn routing_config_warnings(
    config: &RoutingConfig,
    backend: &BackendSettings,
) -> Vec<String> {
    let default = default_endpoint(backend);
    let default_is_local = default.effective_locality() == Locality::Local;
    config
        .validate(&default)
        .into_iter()
        .filter(|issue| {
            // Local with no address is the embedded backend (or a marked-
            // local broker, which fails at backend construction anyway):
            // nothing for routing to check.
            !(default_is_local
                && matches!(issue, ConfigIssue::MissingBaseUrl { endpoint } if endpoint == DEFAULT_ENDPOINT))
        })
        .map(|issue| match &issue {
            ConfigIssue::NonLocalAddress { endpoint, host } if endpoint == DEFAULT_ENDPOINT => {
                format!(
                    "the [backend] server `{host}` is not a local address, so routing counts its \
                     model as cloud and never picks it — if it is on your own network, set \
                     `locality = \"local\"` in [backend]"
                )
            }
            ConfigIssue::MissingBaseUrl { endpoint } if endpoint == DEFAULT_ENDPOINT => {
                "[backend] has no address routing can check (llama_server_broker needs \
                 broker_base_url), so routing counts its model as cloud and never picks it"
                    .to_string()
            }
            _ => issue.to_string(),
        })
        .collect()
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

/// The inverse of [`chat_base_url`]: a residency probe hits the server's
/// own root, not its OpenAI-compatible `/v1` path.
fn strip_v1(url: &str) -> String {
    let url = url.trim_end_matches('/');
    url.strip_suffix("/v1").unwrap_or(url).to_string()
}

pub(crate) fn backend_factory(settings: &Settings, config: &RoutingConfig) -> BackendFactory {
    let backend = settings.backend.clone();
    let endpoints = config.endpoints.clone();
    Box::new(move |key: &ModelKey| {
        let base =
            if key.endpoint.as_str() == DEFAULT_ENDPOINT {
                match backend.kind {
                    BackendKind::Generic | BackendKind::LlamaServer => backend.base_url.clone(),
                    // aivyx-broker serves `/v1/chat/completions`.
                    BackendKind::LlamaServerBroker => chat_base_url(
                        backend
                            .broker_base_url
                            .as_deref()
                            .ok_or_else(|| "backend.broker_base_url is not set".to_string())?,
                    ),
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
    default: DefaultEndpoint,
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
        merge(&self.config, &self.default, &reports)
    }
}

/// Model routing Part 4 — how often the router's residency snapshot is
/// refreshed. Cheap endpoints; never polled per call.
pub(crate) const RESIDENCY_REFRESH: Duration = Duration::from_secs(5);

/// What the `backend` endpoint can tell residency, by `[backend] kind`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DefaultResidency {
    /// No signal: `Generic` may be Ollama's OpenAI API (many models,
    /// loaded on demand) or any other multi-model OpenAI-compatible
    /// server.
    None,
    /// `llama-server`: router-mode `/models` (loaded/loading/unloaded/
    /// sleeping), and resident — a single-model server has its model
    /// loaded whatever it calls it.
    LlamaServer(String),
    /// The embedded mistral.rs backend: a single-model server with
    /// nothing to ask.
    Resident,
    /// `aivyx-broker` at this base URL, fronting a single llama-server.
    Broker(String),
}

/// [`DefaultResidency`] for `[backend]`, per the exact `BackendKind`
/// mapping (see the Model Routing Part 4b spec). A `[backend]` that
/// routing counts as cloud is never polled, as aivyx-route never polls a cloud
/// endpoint.
pub(crate) fn default_residency(backend: &BackendSettings) -> DefaultResidency {
    if default_endpoint(backend).effective_locality() == Locality::Cloud {
        return DefaultResidency::None;
    }
    match backend.kind {
        BackendKind::LlamaServer => DefaultResidency::LlamaServer(strip_v1(&backend.base_url)),
        BackendKind::LlamaServerBroker => backend
            .broker_base_url
            .as_deref()
            .map_or(DefaultResidency::None, |url| {
                DefaultResidency::Broker(strip_v1(url))
            }),
        BackendKind::MistralRs => DefaultResidency::Resident,
        BackendKind::Generic => DefaultResidency::None,
    }
}

/// Everything one residency poll reads.
pub(crate) struct ResidencySources {
    endpoints: Vec<(EndpointRef, EndpointConfig)>,
    default: DefaultResidency,
    vram_bytes: Option<u64>,
}

impl ResidencySources {
    pub(crate) fn new(config: &RoutingConfig, default: DefaultResidency) -> Self {
        let mut endpoints = endpoints_of(config);
        if let DefaultResidency::LlamaServer(url) = &default {
            endpoints.push((
                EndpointRef::new(DEFAULT_ENDPOINT),
                EndpointConfig {
                    kind: EndpointKind::LlamaRouter,
                    base_url: Some(url.clone()),
                    // `default_residency` only names a local `[backend]`,
                    // possibly local by `[backend] locality` alone.
                    locality: Some(Locality::Local),
                },
            ));
        }
        ResidencySources {
            endpoints,
            default,
            vram_bytes: config.vram_bytes,
        }
    }

    /// Anything to read at all? Without a source no refresh task runs.
    pub(crate) fn is_active(&self) -> bool {
        self.default != DefaultResidency::None
            || self.vram_bytes.is_some()
            || self.endpoints.iter().any(|(_, c)| {
                matches!(
                    c.kind,
                    EndpointKind::Ollama | EndpointKind::LlamaRouter | EndpointKind::Lemonade
                )
            })
    }

    pub(crate) async fn poll(&self, client: &reqwest::Client) -> ResidencySnapshot {
        let default = EndpointRef::new(DEFAULT_ENDPOINT);
        let broker = match &self.default {
            DefaultResidency::Broker(url) => Some(BrokerSource {
                endpoint: default.clone(),
                base_url: url.clone(),
            }),
            _ => None,
        };
        let mut snap = collect(&self.endpoints, broker.as_ref(), self.vram_bytes, client).await;
        if matches!(
            self.default,
            DefaultResidency::LlamaServer(_)
                | DefaultResidency::Resident
                | DefaultResidency::Broker(_)
        ) {
            snap.resident_endpoints.insert(default);
        }
        snap
    }
}

/// Polls `sources` every [`RESIDENCY_REFRESH`] (first poll immediately)
/// and hands each snapshot to `routed`'s router; exits once `routed` is
/// gone.
pub(crate) fn spawn_residency_refresh(routed: Weak<RoutedBackend>, sources: ResidencySources) {
    tokio::spawn(async move {
        let client = reqwest::Client::new();
        let mut tick = tokio::time::interval(RESIDENCY_REFRESH);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            let snapshot = sources.poll(&client).await;
            let Some(routed) = routed.upgrade() else {
                break;
            };
            routed.set_residency(snapshot);
        }
    });
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
    for issue in routing_config_warnings(&config, &settings.backend) {
        tracing::warn!(%issue, "routing config");
    }
    let refresher = Arc::new(DiscoveryRefresher {
        config: config.clone(),
        default: default_endpoint(&settings.backend),
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
    let residency = ResidencySources::new(&config, default_residency(&settings.backend));
    if residency.is_active() {
        spawn_residency_refresh(Arc::downgrade(&router), residency);
    }
    Ok((Arc::clone(&router) as Arc<dyn LlmBackend>, Some(router)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use aivyx_config::{BackendKind, Settings};
    use aivyx_route::{EndpointConfig, Locality};

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
    fn a_lemonade_endpoint_is_accepted() {
        let s = settings_with("[routing.endpoints.lemon]\nkind = \"lemonade\"\n");
        assert!(check_routing_config(&s.routing).is_ok());
    }

    /// The kind is not the whole story: a hosted OpenAI-compatible API
    /// (or a remote Ollama) is cloud by its address.
    #[test]
    fn an_endpoint_whose_address_is_not_local_is_rejected() {
        for (kind, url) in [
            ("openai_compat", "https://api.groq.com/openai/v1"),
            ("ollama", "http://ollama.example.com:11434"),
            ("openai_compat", "http://api%2Egroq%2Ecom/v1"),
        ] {
            let s = settings_with(&format!(
                "[routing.endpoints.hosted]\nkind = \"{kind}\"\nbase_url = \"{url}\"\n"
            ));
            let err = check_routing_config(&s.routing).unwrap_err().to_string();
            assert!(
                err.contains("hosted") && err.contains("locality = \"local\""),
                "{kind} {url}: {err}"
            );
        }
        // No address at all is cloud too (nothing says where it is).
        let s = settings_with("[routing.endpoints.c]\nkind = \"openai_compat\"\n");
        assert!(check_routing_config(&s.routing).is_err());
        // locality = "cloud" forces cloud.
        let s = settings_with(
            "[routing.endpoints.c]\nkind = \"ollama\"\nbase_url = \"http://localhost:11434\"\n\
             locality = \"cloud\"\n",
        );
        let err = check_routing_config(&s.routing).unwrap_err().to_string();
        assert!(err.contains("marked `locality = \"cloud\"`"), "{err}");
    }

    #[test]
    fn a_lan_endpoint_is_accepted_by_address_or_override() {
        for extra in [
            "base_url = \"http://192.168.1.5:11434\"\n",
            "base_url = \"http://gpu.lan:11434\"\n",
            "base_url = \"http://gpu.example.com:11434\"\nlocality = \"local\"\n",
        ] {
            let s = settings_with(&format!(
                "[routing.endpoints.gpu]\nkind = \"ollama\"\n{extra}"
            ));
            assert!(check_routing_config(&s.routing).is_ok(), "{extra}");
        }
    }

    fn backend(kind: BackendKind, base_url: &str, broker: Option<&str>) -> BackendSettings {
        BackendSettings {
            kind,
            base_url: base_url.to_string(),
            broker_base_url: broker.map(String::from),
            ..BackendSettings::default()
        }
    }

    #[test]
    fn the_default_endpoint_carries_the_address_the_backend_connects_to() {
        let d = default_endpoint(&backend(
            BackendKind::Generic,
            "http://localhost:11434/v1",
            None,
        ));
        assert_eq!(d.name.as_str(), DEFAULT_ENDPOINT);
        assert_eq!(d.base_url.as_deref(), Some("http://localhost:11434/v1"));
        assert_eq!(d.locality, None);

        let d = default_endpoint(&backend(
            BackendKind::LlamaServer,
            "http://127.0.0.1:8080/v1",
            None,
        ));
        assert_eq!(d.base_url.as_deref(), Some("http://127.0.0.1:8080/v1"));

        // The broker, not base_url, is what chat connects to.
        let d = default_endpoint(&backend(
            BackendKind::LlamaServerBroker,
            "http://127.0.0.1:8080/v1",
            Some("http://broker.example.com:8899"),
        ));
        assert_eq!(
            d.base_url.as_deref(),
            Some("http://broker.example.com:8899")
        );
        assert_eq!(d.effective_locality(), Locality::Cloud);
    }

    #[test]
    fn the_default_endpoints_locality_follows_its_address() {
        use Locality::{Cloud, Local};
        let loc = |b: &BackendSettings| default_endpoint(b).effective_locality();
        assert_eq!(
            loc(&backend(
                BackendKind::Generic,
                "http://localhost:11434/v1",
                None
            )),
            Local
        );
        assert_eq!(
            loc(&backend(
                BackendKind::Generic,
                "http://10.0.0.7:8080/v1",
                None
            )),
            Local
        );
        assert_eq!(
            loc(&backend(
                BackendKind::Generic,
                "https://api.groq.com/openai/v1",
                None
            )),
            Cloud
        );
        // A broker kind with no broker address: nothing says where it is.
        assert_eq!(
            loc(&backend(
                BackendKind::LlamaServerBroker,
                "http://localhost:8080/v1",
                None
            )),
            Cloud
        );
        // The embedded backend runs in-process, whatever base_url says.
        assert_eq!(
            loc(&backend(
                BackendKind::MistralRs,
                "https://api.groq.com/v1",
                None
            )),
            Local
        );
        // `[backend] locality` overrides the address, either way.
        let mut lan = backend(BackendKind::Generic, "http://gpu.example.com:8080/v1", None);
        lan.locality = Some(Local);
        assert_eq!(loc(&lan), Local);
        let mut forced = backend(BackendKind::Generic, "http://localhost:11434/v1", None);
        forced.locality = Some(Cloud);
        assert_eq!(loc(&forced), Cloud);
    }

    /// A remote `[backend]` makes its model cloud, so local-only routing
    /// never picks it; the startup warning says how to mark it local.
    #[test]
    fn a_remote_backend_model_is_cloud_and_the_warning_names_backend_locality() {
        let mut s = settings_with("[routing]\nenabled = true\n");
        s.backend.base_url = "https://api.groq.com/openai/v1".into();
        let config = effective_routing_config(&s);
        let profiles = merge(&config, &default_endpoint(&s.backend), &[]);
        let backend_model = find(&profiles, &key(DEFAULT_ENDPOINT, "qwen3.5:9b")).unwrap();
        assert_eq!(backend_model.locality, Locality::Cloud);

        let warnings = routing_config_warnings(&config, &s.backend);
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("[backend]") && w.contains("locality = \"local\"")),
            "{warnings:?}"
        );

        s.backend.locality = Some(Locality::Local);
        let profiles = merge(&config, &default_endpoint(&s.backend), &[]);
        let backend_model = find(&profiles, &key(DEFAULT_ENDPOINT, "qwen3.5:9b")).unwrap();
        assert_eq!(backend_model.locality, Locality::Local);
        assert!(routing_config_warnings(&config, &s.backend).is_empty());
    }

    /// The embedded backend has no address and needs none; a broker with
    /// no broker_base_url has nothing to check, so it is cloud.
    #[test]
    fn a_default_without_an_address_is_warned_about_only_when_it_counts_as_cloud() {
        let config = RoutingConfig::default();
        let embedded = backend(BackendKind::MistralRs, "http://localhost:11434/v1", None);
        assert!(routing_config_warnings(&config, &embedded).is_empty());

        let broker = backend(
            BackendKind::LlamaServerBroker,
            "http://localhost:8080/v1",
            None,
        );
        let warnings = routing_config_warnings(&config, &broker);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("broker_base_url"), "{warnings:?}");
    }

    /// aivyx-route never probes an endpoint it calls cloud; neither does
    /// aivyx-coder's own poll of the `[backend]` server.
    #[test]
    fn a_cloud_default_is_never_polled_for_residency() {
        assert_eq!(
            default_residency(&backend(
                BackendKind::LlamaServer,
                "http://llama.example.com:8080/v1",
                None
            )),
            DefaultResidency::None
        );
        assert_eq!(
            default_residency(&backend(
                BackendKind::LlamaServerBroker,
                "http://127.0.0.1:8080/v1",
                Some("http://broker.example.com:8899")
            )),
            DefaultResidency::None
        );
        // Marked local, it is polled as before.
        let mut lan = backend(
            BackendKind::LlamaServer,
            "http://llama.example.com:8080/v1",
            None,
        );
        lan.locality = Some(Locality::Local);
        assert_eq!(
            default_residency(&lan),
            DefaultResidency::LlamaServer("http://llama.example.com:8080".into())
        );
    }

    /// The `[backend]` llama-server added as a residency endpoint keeps
    /// the locality aivyx-coder already decided, so aivyx-route doesn't
    /// skip a `[backend] locality = "local"` server with a public name.
    #[test]
    fn the_default_residency_endpoint_is_local() {
        let s = ResidencySources::new(
            &RoutingConfig::default(),
            DefaultResidency::LlamaServer("http://llama.example.com:8080".into()),
        );
        let (_, c) = s.endpoints.last().unwrap();
        assert_eq!(c.effective_locality(), Locality::Local);
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
                locality: None,
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
                locality: None,
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

    /// aivyx-broker serves `/v1/chat/completions`; the documented
    /// `broker_base_url` has no `/v1`.
    #[tokio::test]
    async fn the_broker_default_endpoint_posts_to_v1_chat_completions() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let broker = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&broker)
            .await;
        let mut s = settings_with("");
        s.backend.kind = BackendKind::LlamaServerBroker;
        s.backend.broker_base_url = Some(broker.uri());
        let request = aivyx_llm::ChatRequest::new(vec![aivyx_types::Message::text(
            aivyx_types::Role::User,
            "hi",
        )]);
        let _ = backend_factory(&s, &s.routing)(&key("backend", "other"))
            .unwrap()
            .stream_chat(request)
            .await;
        let paths: Vec<String> = broker
            .received_requests()
            .await
            .unwrap()
            .iter()
            .map(|r| r.url.path().to_string())
            .collect();
        assert_eq!(paths, ["/v1/chat/completions"]);
    }

    /// Lemonade's base is the `.../api` form; the OpenAI-compatible backend
    /// needs `.../api/v1`.
    #[tokio::test]
    async fn the_factory_builds_a_lemonade_backend_on_its_api_v1_base() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let lemonade = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&lemonade)
            .await;
        let mut s = settings_with("");
        s.routing.endpoints.insert(
            "lemon".into(),
            EndpointConfig {
                kind: aivyx_route::EndpointKind::Lemonade,
                base_url: Some(format!("{}/api", lemonade.uri())),
                locality: None,
            },
        );
        let factory = backend_factory(&s, &s.routing);
        let request = aivyx_llm::ChatRequest::new(vec![aivyx_types::Message::text(
            aivyx_types::Role::User,
            "hi",
        )]);
        let _ = factory(&key("lemon", "some-model"))
            .unwrap()
            .stream_chat(request)
            .await;
        let paths: Vec<String> = lemonade
            .received_requests()
            .await
            .unwrap()
            .iter()
            .map(|r| r.url.path().to_string())
            .collect();
        assert_eq!(paths, ["/api/v1/chat/completions"]);
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

    #[test]
    fn strip_v1_drops_v1_and_trailing_slashes() {
        assert_eq!(strip_v1("http://127.0.0.1:8080"), "http://127.0.0.1:8080");
        assert_eq!(strip_v1("http://127.0.0.1:8080/"), "http://127.0.0.1:8080");
        assert_eq!(
            strip_v1("http://127.0.0.1:8080/v1"),
            "http://127.0.0.1:8080"
        );
        assert_eq!(
            strip_v1("http://127.0.0.1:8080/v1/"),
            "http://127.0.0.1:8080"
        );
    }

    #[test]
    fn default_residency_follows_the_backend_kind() {
        use DefaultResidency as D;
        let backend = |kind, base_url: &str, broker: Option<&str>| BackendSettings {
            kind,
            base_url: base_url.to_string(),
            broker_base_url: broker.map(String::from),
            ..BackendSettings::default()
        };
        assert_eq!(
            default_residency(&backend(
                BackendKind::LlamaServer,
                "http://127.0.0.1:8080/v1",
                None
            )),
            D::LlamaServer("http://127.0.0.1:8080".into())
        );
        assert_eq!(
            default_residency(&backend(
                BackendKind::LlamaServerBroker,
                "http://x/v1",
                Some("http://127.0.0.1:8899")
            )),
            D::Broker("http://127.0.0.1:8899".into())
        );
        assert_eq!(
            default_residency(&backend(
                BackendKind::LlamaServerBroker,
                "http://x/v1",
                None
            )),
            D::None
        );
        assert_eq!(
            default_residency(&backend(BackendKind::MistralRs, "http://x/v1", None)),
            D::Resident
        );
        assert_eq!(
            default_residency(&backend(BackendKind::Generic, "http://x/v1", None)),
            D::None
        );
    }

    #[test]
    fn residency_sources_add_the_default_endpoints_own_signal() {
        let mut config = RoutingConfig::default();
        config.endpoints.insert(
            "gpu".into(),
            EndpointConfig {
                kind: aivyx_route::EndpointKind::Ollama,
                base_url: Some("http://gpu:11434".into()),
                locality: None,
            },
        );
        let s = ResidencySources::new(
            &config,
            DefaultResidency::LlamaServer("http://x:8080".into()),
        );
        let names: Vec<(String, EndpointKind)> = s
            .endpoints
            .iter()
            .map(|(e, c)| (e.to_string(), c.kind))
            .collect();
        assert_eq!(
            names,
            [
                ("gpu".to_string(), EndpointKind::Ollama),
                (DEFAULT_ENDPOINT.to_string(), EndpointKind::LlamaRouter),
            ]
        );
        assert!(s.is_active());

        // Nothing to read ⇒ inactive: no task is spawned.
        let idle = ResidencySources::new(&RoutingConfig::default(), DefaultResidency::None);
        assert!(!idle.is_active());

        // An OpenAI-compatible routing endpoint is not a residency source.
        let mut compat = RoutingConfig::default();
        compat.endpoints.insert(
            "c".into(),
            EndpointConfig {
                kind: aivyx_route::EndpointKind::OpenaiCompat,
                base_url: Some("http://c".into()),
                locality: None,
            },
        );
        assert!(!ResidencySources::new(&compat, DefaultResidency::None).is_active());

        // vram_bytes alone is a source.
        let vram = RoutingConfig {
            vram_bytes: Some(24 << 30),
            ..RoutingConfig::default()
        };
        assert!(ResidencySources::new(&vram, DefaultResidency::None).is_active());
    }

    /// Lemonade holds one LLM at a time and reports its own residency
    /// (`/v1/health` + `/v1/models`), same as Ollama and llama-router.
    #[test]
    fn a_lemonade_endpoint_counts_as_a_residency_source() {
        let mut config = RoutingConfig::default();
        config.endpoints.insert(
            "lemon".into(),
            EndpointConfig {
                kind: aivyx_route::EndpointKind::Lemonade,
                base_url: Some("http://127.0.0.1:13305/api".into()),
                locality: None,
            },
        );
        assert!(ResidencySources::new(&config, DefaultResidency::None).is_active());
    }

    #[tokio::test]
    async fn a_single_model_default_is_marked_resident_without_any_network() {
        let s = ResidencySources::new(&RoutingConfig::default(), DefaultResidency::Resident);
        let snap = s.poll(&reqwest::Client::new()).await;
        assert!(
            snap.resident_endpoints
                .contains(&EndpointRef::new(DEFAULT_ENDPOINT))
        );
        assert!(snap.models.is_empty());
    }

    #[tokio::test]
    async fn residency_refresh_reports_a_loaded_ollama_model() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let ollama = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/ps"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                r#"{"models":[{"name":"qwen3:8b","model":"qwen3:8b","size":5000000000,"size_vram":5000000000}]}"#,
                "application/json",
            ))
            .mount(&ollama)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/tags"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                r#"{"models":[{"name":"qwen3:8b","size":5000000000},{"name":"phi4:14b","size":9000000000}]}"#,
                "application/json",
            ))
            .mount(&ollama)
            .await;

        let mut s = settings_with("[routing]\nenabled = true\ndiscover = false\n");
        s.routing.endpoints.insert(
            "ollama".into(),
            EndpointConfig {
                kind: aivyx_route::EndpointKind::Ollama,
                base_url: Some(ollama.uri()),
                locality: None,
            },
        );
        let llm: Arc<dyn LlmBackend> = Arc::new(aivyx_llm::OpenAiCompatBackend::new(
            s.backend.base_url.clone(),
            s.backend.model.clone(),
            None,
        ));
        let (_, router) = wrap_with_routing(&s, llm).await.unwrap();
        let router = router.expect("routing is on");

        let target = key("ollama", "qwen3:8b");
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        let mut loaded = None;
        while std::time::Instant::now() < deadline {
            loaded = router.residency().models.get(&target).copied();
            if loaded.is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(
            matches!(loaded, Some(aivyx_route::ModelResidency::Loaded { .. })),
            "{loaded:?}"
        );
    }
}
