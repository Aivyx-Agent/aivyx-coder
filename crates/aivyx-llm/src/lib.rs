pub mod backend;
mod kv_slot_pool;
pub mod list_models;
#[cfg(feature = "provider-mistral-rs")]
pub mod mistral_rs;
pub mod openai_compat;
pub mod probe;
mod slot_pool_lock;

pub use backend::{
    ChatRequest, FinishReason, LlmBackend, LlmError, SlotHint, StreamEvent, ToolChoice,
};
pub use kv_slot_pool::KvSlotPool;
pub use openai_compat::OpenAiCompatBackend;
pub use probe::{ServedContext, context_warning, probe_served_context};
pub use slot_pool_lock::{SlotPoolLock, fnv1a};
