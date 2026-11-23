pub mod backend;
mod kv_slot_pool;
pub mod list_models;
pub mod local;
#[cfg(feature = "provider-mistral-rs")]
pub mod mistral_rs;
pub mod openai_compat;
pub mod probe;
pub mod routed;
mod slot_pool_lock;

pub use backend::{
    ChatRequest, FinishReason, LlmBackend, LlmError, RouteHint, SlotHint, StreamEvent, ToolChoice,
};
pub use kv_slot_pool::KvSlotPool;
pub use openai_compat::OpenAiCompatBackend;
pub use probe::{
    ServedContext, context_warning, effective_context_tokens, probe_served_context,
    unreachable_notice, verify_model_responds,
};
pub use routed::{BackendFactory, DEFAULT_COOLDOWN, ProfileRefresher, RouteRecord, RoutedBackend};
pub use slot_pool_lock::{SlotPoolLock, fnv1a};

/// The client builder for every request to a model server. aivyx-coder only
/// talks to local models, so the client never uses a proxy: reqwest would
/// otherwise send even loopback requests -- prompts and code included -- to
/// `HTTP_PROXY`/`ALL_PROXY` when one is set (aivyx-vision audit 2026-11-11,
/// X1). Internet tools (`web_fetch`, `web_search`) build their own clients
/// and keep honouring a configured proxy.
///
/// It never follows a redirect either: a 307/308 from a local server would
/// otherwise re-send the prompt and code to whatever host it names, past
/// the local-only check (the same policy as aivyx-route's discovery client
/// and aivyx-vision-mold). A 3xx comes back as the response.
///
/// And it resolves host names only to local addresses
/// (`aivyx_route::discovery::LocalOnlyDns`): a name that answers with any
/// public address fails to connect, with the `NotLocal` text. reqwest skips
/// the resolver for an IP-literal URL, which the per-request `require_local`
/// check covers instead; the two together close the gap between checking a
/// URL and connecting to it (a name re-resolving to a public address in
/// between).
pub fn model_client_builder() -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .dns_resolver(std::sync::Arc::new(aivyx_route::discovery::LocalOnlyDns::new()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The full error chain, where reqwest keeps the resolver's text.
    fn chain(err: &dyn std::error::Error) -> String {
        let mut text = err.to_string();
        let mut source = err.source();
        while let Some(e) = source {
            text.push_str(": ");
            text.push_str(&e.to_string());
            source = e.source();
        }
        text
    }

    /// A known cloud host is refused by the connect-time resolver without
    /// any lookup, so this proves `model_client_builder` installs it,
    /// offline. (The request bypasses `require_local`: a raw client.)
    #[tokio::test]
    async fn the_model_client_refuses_a_name_that_is_not_local_at_connect_time() {
        let client = model_client_builder().build().unwrap();
        let err = client.get("http://api.openai.com:9/v1/models").send().await.unwrap_err();
        let text = chain(&err);
        assert!(text.contains("is not local (cloud host)"), "{text}");
    }

    /// What the resolver does with a name's answer: all local passes, any
    /// public address fails with the `NotLocal` text.
    #[test]
    fn the_connect_time_check_refuses_a_name_that_resolves_publicly() {
        struct Answers(Vec<std::net::IpAddr>);
        impl aivyx_route::Resolve for Answers {
            fn resolve(&self, _: &str, _: u16) -> std::io::Result<Vec<std::net::IpAddr>> {
                Ok(self.0.clone())
            }
        }
        let lan = Answers(vec!["192.168.1.5".parse().unwrap()]);
        let addrs = aivyx_route::resolve_local_with("gpu.example.com", 8080, &lan).unwrap();
        assert_eq!(addrs, ["192.168.1.5:8080".parse().unwrap()]);

        let mixed = Answers(vec!["192.168.1.5".parse().unwrap(), "8.8.8.8".parse().unwrap()]);
        let err = aivyx_route::resolve_local_with("gpu.example.com", 8080, &mixed).unwrap_err();
        assert_eq!(
            err.to_string(),
            "gpu.example.com:8080 is not local (resolves to a public address 8.8.8.8) — aivyx \
             is local-only; point it at a model server on this machine or your own network"
        );
    }
}
