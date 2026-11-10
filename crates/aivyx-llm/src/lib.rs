pub mod backend;
mod kv_slot_pool;
pub mod list_models;
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
pub fn model_client_builder() -> reqwest::ClientBuilder {
    reqwest::Client::builder().no_proxy()
}
