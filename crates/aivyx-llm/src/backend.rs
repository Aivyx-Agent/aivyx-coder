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
    /// Routing metadata for `RoutedBackend` (`routed.rs`); every other
    /// backend ignores it. `None` (an untagged call site) means a
    /// `RoutedBackend` forwards the request to the configured `[backend]`
    /// model unchanged.
    pub route: Option<RouteHint>,
}

/// See `ChatRequest::slot_hint`'s doc comment.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SlotHint {
    pub prefix_hash: String,
    pub preferred_slot: Option<u32>,
}

/// See `ChatRequest::route`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteHint {
    pub task: aivyx_route::TaskKind,
    /// Stickiness key: calls sharing a session with a sticky task kind
    /// (`Chat`/`CodeEdit`) stay on one model. `None` for side calls.
    pub session: Option<String>,
    /// The caller's prompt-size estimate; becomes the minimum context
    /// window. `0` = no requirement.
    pub estimated_prompt_tokens: u32,
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
            route: None,
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
    #[error("{}", describe_request_error(.0))]
    Request(#[from] reqwest::Error),
    #[error("backend returned an error response ({status}): {body}")]
    BackendError { status: u16, body: String },
    #[error("failed to parse backend response: {0}")]
    Parse(String),
    #[error("backend went silent (no data for too long) — it may be hung or unreachable")]
    Timeout,
    #[error("backend response exceeded the maximum allowed size and was aborted")]
    ResponseTooLarge,
    #[error("model routing: {0}")]
    Routing(String),
}

/// A request failure in plain words: a refused connection is the common
/// first-run case (the server isn't running, or the config points
/// elsewhere), so it names the server and what to check.
fn describe_request_error(err: &reqwest::Error) -> String {
    if err.is_connect() {
        let server = err
            .url()
            .map(|u| format!("{}://{}", u.scheme(), u.authority()))
            .unwrap_or_else(|| "the configured base_url".to_string());
        return format!(
            "couldn't connect to the model server at {server} -- is it running? \
             Check [backend] base_url in config.toml, or run `aivyx-coder --setup`"
        );
    }
    format!("request to backend failed: {err}")
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn an_unreachable_server_says_what_to_check() {
        // Port 1 on loopback: nothing listens there, so the connect fails.
        let err = reqwest::Client::new()
            .get("http://127.0.0.1:1/v1/chat/completions")
            .send()
            .await
            .unwrap_err();
        let text = LlmError::Request(err).to_string();
        assert!(text.contains("couldn't connect to the model server"), "{text}");
        assert!(text.contains("127.0.0.1:1"), "{text}");
        assert!(text.contains("is it running?"), "{text}");
        assert!(text.contains("aivyx-coder --setup"), "{text}");
    }

    use super::*;

    #[test]
    fn new_requests_are_untagged() {
        assert!(ChatRequest::new(Vec::new()).route.is_none());
    }

    #[test]
    fn routing_errors_explain_themselves() {
        assert_eq!(
            LlmError::Routing("no model has tool calling".into()).to_string(),
            "model routing: no model has tool calling"
        );
    }
}
