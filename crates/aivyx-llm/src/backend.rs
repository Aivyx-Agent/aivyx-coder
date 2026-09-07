use aivyx_types::{Message, ToolCall, ToolDefinition};
use async_trait::async_trait;
use futures::stream::BoxStream;
use thiserror::Error;

/// Abstracts over any local inference server that speaks (a close enough
/// dialect of) the OpenAI chat-completions protocol — Ollama, vLLM, and
/// llama.cpp's server all qualify via one implementation, `OpenAiCompatBackend`.
#[async_trait]
pub trait LlmBackend: Send + Sync {
    fn model_id(&self) -> &str;

    async fn stream_chat(
        &self,
        request: ChatRequest,
    ) -> Result<BoxStream<'static, Result<StreamEvent, LlmError>>, LlmError>;
}

#[derive(Debug, Clone)]
pub struct ChatRequest {
    pub messages: Vec<Message>,
    pub tools: Vec<ToolDefinition>,
    pub tool_choice: ToolChoice,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
    /// llama-server-only: pins this request to a specific `/slots` id
    /// (an extension beyond the OpenAI spec, but honored by llama-server
    /// on `/v1/chat/completions` — verified empirically against a real
    /// server, not documented in llama-server's own API reference). Only
    /// ever set when `[backend] kind = "llama_server"` and a slot has
    /// been checked out (see `aivyx-core::Agent`); `None` for every other
    /// backend and every llama-server request before checkout.
    pub id_slot: Option<u32>,
    /// `aivyx-broker`-only: an additive hint the broker uses for its own
    /// slot admission/restore/warm/save lifecycle -- serialized under the
    /// `aivyx_slot_hint` key, a field a plain OpenAI-compatible server
    /// simply ignores. Only ever set when `[backend] kind =
    /// "llama_server_broker"` (see `aivyx-core::Agent`); `None` for every
    /// other backend. Unlike `id_slot`, this process never picks the
    /// slot itself -- the broker, not this client, owns that decision.
    pub slot_hint: Option<SlotHint>,
}

/// See `ChatRequest::slot_hint`'s doc comment.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SlotHint {
    pub prefix_hash: String,
    pub preferred_slot: Option<u32>,
}

impl ChatRequest {
    pub fn new(messages: Vec<Message>) -> Self {
        Self {
            messages,
            tools: Vec::new(),
            tool_choice: ToolChoice::Auto,
            temperature: None,
            max_tokens: None,
            id_slot: None,
            slot_hint: None,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ToolChoice {
    #[default]
    Auto,
    None,
    Required,
}

#[derive(Debug, Clone)]
pub enum StreamEvent {
    TextDelta(String),
    /// A reasoning-capable model's chain-of-thought, streamed separately
    /// from its final answer — see `docs/superpowers/specs/
    /// 2026-07-19-reasoning-visibility-design.md`. Never accumulated into
    /// anything persisted; display-only.
    ReasoningDelta(String),
    /// Emitted once a native tool call's streamed argument fragments have
    /// been fully reassembled into valid JSON.
    ToolCallComplete(ToolCall),
    Usage {
        prompt_tokens: u32,
        completion_tokens: u32,
    },
    Done {
        finish_reason: FinishReason,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinishReason {
    Stop,
    ToolCalls,
    Length,
    Error,
}

#[derive(Debug, Error)]
pub enum LlmError {
    #[error("request to backend failed: {0}")]
    Request(#[from] reqwest::Error),
    #[error("backend returned an error response ({status}): {body}")]
    BackendError { status: u16, body: String },
    #[error("failed to parse backend response: {0}")]
    Parse(String),
    #[error("backend went silent (no data for too long) — it may be hung or unreachable")]
    Timeout,
    #[error("backend response exceeded the maximum allowed size and was aborted")]
    ResponseTooLarge,
}
