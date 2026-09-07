pub mod backend;
pub mod openai_compat;
pub mod probe;
mod kv_slot_pool;
#[cfg(feature = "provider-mistral-rs")]
pub mod mistral_rs;

pub use backend::{ChatRequest, FinishReason, LlmBackend, LlmError, StreamEvent, ToolChoice};
pub use openai_compat::OpenAiCompatBackend;
pub use probe::{ServedContext, context_warning, probe_served_context};
pub use kv_slot_pool::KvSlotPool;
