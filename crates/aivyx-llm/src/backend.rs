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
}

impl ChatRequest {
    pub fn new(messages: Vec<Message>) -> Self {
        Self {
            messages,
            tools: Vec::new(),
            tool_choice: ToolChoice::Auto,
            temperature: None,
            max_tokens: None,
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
}
