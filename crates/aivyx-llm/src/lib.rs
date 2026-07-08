pub mod backend;
pub mod openai_compat;

pub use backend::{ChatRequest, FinishReason, LlmBackend, LlmError, StreamEvent, ToolChoice};
pub use openai_compat::OpenAiCompatBackend;
