use std::collections::BTreeMap;
use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aivyx_types::{
    ContentBlock, Message, Role, ToolCall, ToolCallId, ToolCallSource, ToolDefinition, ToolOutput,
};
use async_trait::async_trait;
use eventsource_stream::Eventsource;
use futures::stream::{self, BoxStream, StreamExt};
use serde::{Deserialize, Serialize};

use crate::backend::{
    ChatRequest, FinishReason, LlmBackend, LlmError, SlotHint, StreamEvent, ToolChoice,
};

/// How long to wait for a TCP+TLS handshake before giving up — this is
/// deliberately NOT a whole-request timeout, since a legitimately long
/// local-model generation can run far longer than any reasonable connect
/// window without anything being wrong.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// How long to tolerate silence between SSE chunks before treating the
/// backend as hung. Generous on purpose — large local models can be slow
/// between tokens — but bounded, since nothing else in this codebase can
/// currently interrupt a `stream_chat` call that just never produces
/// another byte.
const IDLE_TIMEOUT: Duration = Duration::from_secs(60);

/// Caps unbounded growth from a misbehaving or malicious backend that
/// never stops streaming a single field.
const MAX_ACCUMULATED_BYTES: usize = 10 * 1024 * 1024;

/// Talks to any server exposing an OpenAI-compatible `/chat/completions`
/// endpoint with SSE streaming — this covers Ollama (`/v1`), vLLM, and
/// llama.cpp's `--server` mode without backend-specific code.
pub struct OpenAiCompatBackend {
    base_url: String,
    model: String,
    api_key: Option<String>,
    http: reqwest::Client,
    idle_timeout: Duration,
    debug_log: Option<Arc<Mutex<std::fs::File>>>,
}

impl OpenAiCompatBackend {
    pub fn new(
        base_url: impl Into<String>,
        model: impl Into<String>,
        api_key: Option<String>,
    ) -> Self {
        Self::with_idle_timeout(base_url, model, api_key, IDLE_TIMEOUT)
    }

    /// Like `new`, but with a caller-chosen silence tolerance. Council
    /// seats use this: a swapped-in Ollama model that has to cold-load
    /// tens of GB (possibly partially into CPU RAM) can legitimately take
    /// past the interactive default before its first token.
    pub fn with_idle_timeout(
        base_url: impl Into<String>,
        model: impl Into<String>,
        api_key: Option<String>,
        idle_timeout: Duration,
    ) -> Self {
        let http = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            // Bounds the gap `connect_timeout` and the SSE idle-timeout
            // wrapper both miss: a backend that accepts the TCP connection
            // but never sends so much as a status line. Resets on every
            // successful read, so it never caps a legitimately long
            // generation — only true silence.
            .read_timeout(idle_timeout)
            .build()
            .expect("failed to build the HTTP client");

        Self {
            base_url: base_url.into(),
            model: model.into(),
            api_key,
            http,
            idle_timeout,
            debug_log: debug_log_from_env(),
        }
    }
}

/// Opt-in raw request/response capture for diagnosing local-model quirks —
/// e.g. malformed history assembly, or any wire field a future backend
/// sends that `WireDelta` doesn't yet model and so silently drops via
/// serde's default behavior during normal parsing (a reasoning-capable
/// model's `delta.reasoning_content` used to be exactly this case, until
/// `WireDelta` started modeling it — see `docs/superpowers/specs/
/// 2026-07-19-reasoning-visibility-design.md`). Set `AIVYX_DEBUG_LOG=<path>`
/// to capture raw wire traffic there; unset by default, so this has zero
/// cost for normal use.
fn debug_log_from_env() -> Option<Arc<Mutex<std::fs::File>>> {
    let path = std::env::var_os("AIVYX_DEBUG_LOG")?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .ok()?;

    // This file can accumulate whatever the agent reads or runs — file
    // contents, command output, potentially secrets — across the whole
    // session, indefinitely. Restrict it to owner-only, same as
    // `config.toml`. Set unconditionally (not just on fresh creation) in
    // case a file from before this existed with looser permissions.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }

    Some(Arc::new(Mutex::new(file)))
}

fn log_line(log: &Arc<Mutex<std::fs::File>>, line: &str) {
    // Best-effort: a logging failure (disk full, lock poisoned by a panic
    // elsewhere) must never take down the actual chat turn.
    if let Ok(mut file) = log.lock() {
        let _ = writeln!(file, "{line}\n");
    }
}

#[async_trait]
impl LlmBackend for OpenAiCompatBackend {
    fn model_id(&self) -> &str {
        &self.model
    }

    async fn stream_chat(
        &self,
        request: ChatRequest,
    ) -> Result<BoxStream<'static, Result<StreamEvent, LlmError>>, LlmError> {
        let wire_request = WireRequest::from_chat_request(&self.model, &request);
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));

        if let Some(log) = &self.debug_log {
            let body = serde_json::to_string_pretty(&wire_request)
                .unwrap_or_else(|err| format!("<failed to serialize wire_request: {err}>"));
            log_line(log, &format!("=== request ===\n{body}"));
        }

        let mut http_request = self.http.post(url).json(&wire_request);
        if let Some(key) = &self.api_key {
            http_request = http_request.bearer_auth(key);
        }

        let response = http_request.send().await?;
        if !response.status().is_success() {
            let status = response.status().as_u16();
            let body = response.text().await.unwrap_or_default();
            return Err(LlmError::BackendError { status, body });
        }

        // Wrapping with an idle timeout here (rather than a blanket
        // `reqwest::Client::timeout`) means a slow-but-alive local model
        // can stream for as long as it needs to, while a truly hung
        // connection still gets caught.
        let sse_stream = tokio_stream::StreamExt::timeout(
            response.bytes_stream().eventsource(),
            self.idle_timeout,
        );

        let debug_log = self.debug_log.clone();
        let stream = sse_stream
            .scan(ToolCallAccumulator::default(), move |accumulator, item| {
                let events = match item {
                    Err(_elapsed) => vec![Err(LlmError::Timeout)],
                    Ok(Err(err)) => vec![Err(LlmError::Parse(err.to_string()))],
                    Ok(Ok(event)) => {
                        let data = event.data.trim();
                        // Logged before typed parsing so fields `WireChunk`
                        // doesn't model still show up in the capture, even
                        // though `delta.reasoning_content` (a reasoning
                        // model's chain-of-thought) is one such field
                        // `WireChunk` now models — see `docs/superpowers/
                        // specs/2026-07-19-reasoning-visibility-design.md`.
                        if let Some(log) = &debug_log {
                            log_line(log, &format!("=== event ===\n{data}"));
                        }
                        if data == "[DONE]" {
                            Vec::new()
                        } else {
                            match serde_json::from_str::<WireChunk>(data) {
                                Ok(chunk) => accumulator.consume(chunk),
                                Err(err) => vec![Err(LlmError::Parse(format!(
                                    "invalid stream chunk: {err} (data: {data})"
                                )))],
                            }
                        }
                    }
                };
                futures::future::ready(Some(events))
            })
            .flat_map(stream::iter);

        Ok(stream.boxed())
    }
}

/// Reassembles the argument-fragment stream OpenAI-style servers send for
/// native tool calls (`delta.tool_calls[i].function.arguments` arrives as
/// partial JSON, keyed by index, across multiple chunks).
#[derive(Default)]
struct ToolCallAccumulator {
    pending: BTreeMap<u32, PendingToolCall>,
}

#[derive(Default)]
struct PendingToolCall {
    id: Option<String>,
    name: Option<String>,
    arguments: String,
}

impl ToolCallAccumulator {
    fn consume(&mut self, chunk: WireChunk) -> Vec<Result<StreamEvent, LlmError>> {
        let mut events = Vec::new();

        if let Some(usage) = chunk.usage {
            events.push(Ok(StreamEvent::Usage {
                prompt_tokens: usage.prompt_tokens,
                completion_tokens: usage.completion_tokens,
            }));
        }

        let Some(choice) = chunk.choices.into_iter().next() else {
            return events;
        };

        if let Some(content) = choice.delta.content
            && !content.is_empty()
        {
            events.push(Ok(StreamEvent::TextDelta(content)));
        }

        if let Some(reasoning) = choice.delta.reasoning_content
            && !reasoning.is_empty()
        {
            events.push(Ok(StreamEvent::ReasoningDelta(reasoning)));
        }

        for tool_call_delta in choice.delta.tool_calls.unwrap_or_default() {
            let entry = self.pending.entry(tool_call_delta.index).or_default();
            if let Some(id) = tool_call_delta.id {
                entry.id = Some(id);
            }
            if let Some(function) = tool_call_delta.function {
                if let Some(name) = function.name {
                    entry.name = Some(name);
                }
                if let Some(arguments) = function.arguments {
                    entry.arguments.push_str(&arguments);
                    if entry.arguments.len() > MAX_ACCUMULATED_BYTES {
                        return vec![Err(LlmError::ResponseTooLarge)];
                    }
                }
            }
        }

        if let Some(finish_reason) = choice.finish_reason {
            let reason = match finish_reason.as_str() {
                "stop" => FinishReason::Stop,
                "tool_calls" => FinishReason::ToolCalls,
                "length" => FinishReason::Length,
                _ => FinishReason::Error,
            };

            if reason == FinishReason::ToolCalls {
                for (_, pending) in std::mem::take(&mut self.pending) {
                    events.push(Self::finalize(pending).map(StreamEvent::ToolCallComplete));
                }
            }

            events.push(Ok(StreamEvent::Done {
                finish_reason: reason,
            }));
        }

        events
    }

    fn finalize(pending: PendingToolCall) -> Result<ToolCall, LlmError> {
        let id = pending
            .id
            .ok_or_else(|| LlmError::Parse("tool call missing id".to_string()))?;
        let name = pending
            .name
            .ok_or_else(|| LlmError::Parse("tool call missing name".to_string()))?;
        let arguments = if pending.arguments.trim().is_empty() {
            serde_json::Value::Object(Default::default())
        } else {
            serde_json::from_str(&pending.arguments)
                .map_err(|err| LlmError::Parse(format!("invalid tool call arguments: {err}")))?
        };

        Ok(ToolCall {
            id: ToolCallId(id),
            name,
            arguments,
            source: ToolCallSource::Native,
        })
    }
}

// ---- wire format (request) ----

#[derive(Serialize)]
struct WireRequest {
    model: String,
    messages: Vec<WireMessage>,
    stream: bool,
    stream_options: WireStreamOptions,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<WireToolDefinition>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    id_slot: Option<u32>,
    #[serde(rename = "aivyx_slot_hint", skip_serializing_if = "Option::is_none")]
    slot_hint: Option<WireSlotHint>,
}

#[derive(Serialize)]
struct WireStreamOptions {
    include_usage: bool,
}

#[derive(Serialize)]
struct WireSlotHint {
    prefix_hash: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    preferred_slot: Option<u32>,
}

impl From<&SlotHint> for WireSlotHint {
    fn from(hint: &SlotHint) -> Self {
        Self { prefix_hash: hint.prefix_hash.clone(), preferred_slot: hint.preferred_slot }
    }
}

impl WireRequest {
    fn from_chat_request(model: &str, request: &ChatRequest) -> Self {
        let tool_choice = if request.tools.is_empty() {
            None
        } else {
            Some(match request.tool_choice {
                ToolChoice::Auto => "auto",
                ToolChoice::None => "none",
                ToolChoice::Required => "required",
            })
        };

        Self {
            model: model.to_string(),
            messages: request.messages.iter().map(WireMessage::from).collect(),
            stream: true,
            stream_options: WireStreamOptions {
                include_usage: true,
            },
            tools: request.tools.iter().map(WireToolDefinition::from).collect(),
            tool_choice,
            temperature: request.temperature,
            max_tokens: request.max_tokens,
            id_slot: request.id_slot,
            slot_hint: request.slot_hint.as_ref().map(WireSlotHint::from),
        }
    }
}

#[derive(Serialize)]
struct WireMessage {
    role: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tool_calls: Vec<WireToolCall>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
}

impl From<&Message> for WireMessage {
    fn from(message: &Message) -> Self {
        let role = match message.role {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
        };

        let mut text = String::new();
        let mut tool_calls = Vec::new();
        let mut tool_result_text: Option<String> = None;

        for block in &message.content {
            match block {
                ContentBlock::Text(t) => text.push_str(t),
                ContentBlock::ToolCall(call) => tool_calls.push(WireToolCall::from(call)),
                ContentBlock::ToolResult(result) => {
                    tool_result_text = Some(match &result.output {
                        ToolOutput::Ok(s) => s.clone(),
                        ToolOutput::Error(s) => format!("Error: {s}"),
                        ToolOutput::Denied(s) => format!("Denied: {s}"),
                    });
                }
            }
        }

        let content = tool_result_text.or(if text.is_empty() && !tool_calls.is_empty() {
            None
        } else {
            Some(text)
        });

        Self {
            role,
            content,
            tool_calls,
            tool_call_id: message.tool_call_id.as_ref().map(|id| id.0.clone()),
        }
    }
}

#[derive(Serialize)]
struct WireToolCall {
    id: String,
    #[serde(rename = "type")]
    call_type: &'static str,
    function: WireFunctionCall,
}

#[derive(Serialize)]
struct WireFunctionCall {
    name: String,
    arguments: String,
}

impl From<&ToolCall> for WireToolCall {
    fn from(call: &ToolCall) -> Self {
        Self {
            id: call.id.0.clone(),
            call_type: "function",
            function: WireFunctionCall {
                name: call.name.clone(),
                arguments: call.arguments.to_string(),
            },
        }
    }
}

#[derive(Serialize)]
struct WireToolDefinition {
    #[serde(rename = "type")]
    tool_type: &'static str,
    function: WireFunctionDefinition,
}

#[derive(Serialize)]
struct WireFunctionDefinition {
    name: String,
    description: String,
    parameters: serde_json::Value,
}

impl From<&ToolDefinition> for WireToolDefinition {
    fn from(def: &ToolDefinition) -> Self {
        Self {
            tool_type: "function",
            function: WireFunctionDefinition {
                name: def.name.clone(),
                description: def.description.clone(),
                parameters: def.parameters_schema.clone(),
            },
        }
    }
}

// ---- wire format (streamed response) ----

#[derive(Deserialize)]
struct WireChunk {
    #[serde(default)]
    choices: Vec<WireChoice>,
    #[serde(default)]
    usage: Option<WireUsage>,
}

#[derive(Deserialize)]
struct WireChoice {
    #[serde(default)]
    delta: WireDelta,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Deserialize, Default)]
struct WireDelta {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    reasoning_content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<WireToolCallDelta>>,
}

#[derive(Deserialize)]
struct WireToolCallDelta {
    index: u32,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: Option<WireFunctionCallDelta>,
}

#[derive(Deserialize)]
struct WireFunctionCallDelta {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

#[derive(Deserialize)]
struct WireUsage {
    prompt_tokens: u32,
    completion_tokens: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_message_omits_content_for_tool_call_only_assistant_message() {
        let message = Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolCall(ToolCall {
                id: ToolCallId("call_1".to_string()),
                name: "read_file".to_string(),
                arguments: serde_json::json!({"path": "src/main.rs"}),
                source: ToolCallSource::Native,
            })],
            tool_call_id: None,
        };

        let wire = WireMessage::from(&message);
        assert_eq!(wire.role, "assistant");
        assert!(wire.content.is_none());
        assert_eq!(wire.tool_calls.len(), 1);
        assert_eq!(wire.tool_calls[0].function.name, "read_file");
    }

    #[test]
    fn wire_message_carries_tool_result_text_and_call_id() {
        let message = Message {
            role: Role::Tool,
            content: vec![ContentBlock::ToolResult(aivyx_types::ToolResult {
                call_id: ToolCallId("call_1".to_string()),
                output: ToolOutput::Ok("file contents".to_string()),
            })],
            tool_call_id: Some(ToolCallId("call_1".to_string())),
        };

        let wire = WireMessage::from(&message);
        assert_eq!(wire.role, "tool");
        assert_eq!(wire.content.as_deref(), Some("file contents"));
        assert_eq!(wire.tool_call_id.as_deref(), Some("call_1"));
    }

    #[test]
    fn accumulator_reassembles_streamed_tool_call_arguments() {
        let mut accumulator = ToolCallAccumulator::default();

        let chunk1: WireChunk = serde_json::from_value(serde_json::json!({
            "choices": [{
                "delta": {"tool_calls": [{"index": 0, "id": "call_1", "function": {"name": "read_file", "arguments": "{\"path\":"}}]},
                "finish_reason": null
            }]
        }))
        .unwrap();
        assert!(accumulator.consume(chunk1).is_empty());

        let chunk2: WireChunk = serde_json::from_value(serde_json::json!({
            "choices": [{
                "delta": {"tool_calls": [{"index": 0, "function": {"arguments": "\"src/main.rs\"}"}}]},
                "finish_reason": "tool_calls"
            }]
        }))
        .unwrap();
        let events = accumulator.consume(chunk2);

        let mut saw_tool_call = false;
        for event in events {
            if let Ok(StreamEvent::ToolCallComplete(call)) = event {
                saw_tool_call = true;
                assert_eq!(call.name, "read_file");
                assert_eq!(call.arguments, serde_json::json!({"path": "src/main.rs"}));
            }
        }
        assert!(saw_tool_call, "expected a completed tool call event");
    }

    #[test]
    fn oversized_tool_call_arguments_abort_instead_of_growing_unbounded() {
        let mut accumulator = ToolCallAccumulator::default();

        let huge_fragment = "a".repeat(MAX_ACCUMULATED_BYTES + 1);
        let chunk: WireChunk = serde_json::from_value(serde_json::json!({
            "choices": [{
                "delta": {"tool_calls": [{"index": 0, "id": "call_1", "function": {"name": "write_file", "arguments": huge_fragment}}]},
                "finish_reason": null
            }]
        }))
        .unwrap();

        let events = accumulator.consume(chunk);
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], Err(LlmError::ResponseTooLarge)));
    }

    #[test]
    fn reasoning_content_delta_produces_a_reasoning_delta_event() {
        let mut accumulator = ToolCallAccumulator::default();

        let chunk: WireChunk = serde_json::from_value(serde_json::json!({
            "choices": [{
                "delta": {"reasoning_content": "Thinking about the problem"},
                "finish_reason": null
            }]
        }))
        .unwrap();

        let events = accumulator.consume(chunk);
        assert_eq!(events.len(), 1);
        assert!(matches!(
            &events[0],
            Ok(StreamEvent::ReasoningDelta(text)) if text == "Thinking about the problem"
        ));
    }

    #[test]
    fn content_only_delta_produces_no_reasoning_delta_event() {
        let mut accumulator = ToolCallAccumulator::default();

        let chunk: WireChunk = serde_json::from_value(serde_json::json!({
            "choices": [{
                "delta": {"content": "4"},
                "finish_reason": null
            }]
        }))
        .unwrap();

        let events = accumulator.consume(chunk);
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], Ok(StreamEvent::TextDelta(text)) if text == "4"));
    }

    #[test]
    fn a_delta_carrying_both_fields_produces_both_events_content_first() {
        let mut accumulator = ToolCallAccumulator::default();

        let chunk: WireChunk = serde_json::from_value(serde_json::json!({
            "choices": [{
                "delta": {"content": "answer", "reasoning_content": "thought"},
                "finish_reason": null
            }]
        }))
        .unwrap();

        let events = accumulator.consume(chunk);
        assert_eq!(events.len(), 2);
        assert!(matches!(&events[0], Ok(StreamEvent::TextDelta(text)) if text == "answer"));
        assert!(matches!(&events[1], Ok(StreamEvent::ReasoningDelta(text)) if text == "thought"));
    }

    #[test]
    fn wire_request_omits_id_slot_when_none() {
        let mut request = ChatRequest::new(vec![]);
        request.id_slot = None;
        let wire = WireRequest::from_chat_request("test-model", &request);
        let json = serde_json::to_value(&wire).unwrap();
        assert!(json.get("id_slot").is_none(), "id_slot must be omitted entirely when None");
    }

    #[test]
    fn wire_request_includes_id_slot_when_set() {
        let mut request = ChatRequest::new(vec![]);
        request.id_slot = Some(2);
        let wire = WireRequest::from_chat_request("test-model", &request);
        let json = serde_json::to_value(&wire).unwrap();
        assert_eq!(json.get("id_slot"), Some(&serde_json::json!(2)));
    }

    #[test]
    fn wire_request_omits_aivyx_slot_hint_when_none() {
        let mut request = ChatRequest::new(vec![]);
        request.slot_hint = None;
        let wire = WireRequest::from_chat_request("test-model", &request);
        let json = serde_json::to_value(&wire).unwrap();
        assert!(
            json.get("aivyx_slot_hint").is_none(),
            "aivyx_slot_hint must be omitted entirely when None"
        );
    }

    #[test]
    fn wire_request_includes_aivyx_slot_hint_when_set() {
        let mut request = ChatRequest::new(vec![]);
        request.slot_hint =
            Some(SlotHint { prefix_hash: "abc123".to_string(), preferred_slot: Some(2) });
        let wire = WireRequest::from_chat_request("test-model", &request);
        let json = serde_json::to_value(&wire).unwrap();
        assert_eq!(
            json.get("aivyx_slot_hint"),
            Some(&serde_json::json!({"prefix_hash": "abc123", "preferred_slot": 2}))
        );
    }

    #[test]
    fn wire_request_aivyx_slot_hint_omits_preferred_slot_when_none() {
        // A session's first request has no preferred slot yet -- the
        // broker's own occupancy tracking is what makes subsequent
        // same-session requests fast, not this client remembering a slot
        // id (see BackendKind::LlamaServerBroker's doc comment).
        let mut request = ChatRequest::new(vec![]);
        request.slot_hint =
            Some(SlotHint { prefix_hash: "abc123".to_string(), preferred_slot: None });
        let wire = WireRequest::from_chat_request("test-model", &request);
        let json = serde_json::to_value(&wire).unwrap();
        assert_eq!(
            json.get("aivyx_slot_hint"),
            Some(&serde_json::json!({"prefix_hash": "abc123"}))
        );
    }
}
