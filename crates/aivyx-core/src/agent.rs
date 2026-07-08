use std::path::Path;

use aivyx_llm::{ChatRequest, FinishReason, LlmBackend, LlmError, StreamEvent, ToolChoice};
use aivyx_tools::ToolExecutor;
use aivyx_types::{ContentBlock, Message, Role, ToolCall, ToolResult};
use futures::StreamExt;
use thiserror::Error;
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone)]
pub enum AgentEvent {
    TextDelta(String),
    ToolCallDetected(ToolCall),
    ToolResult(ToolResult),
    TurnComplete,
    Error(String),
}

/// Caps unbounded growth of a single turn's accumulated assistant text from
/// a misbehaving backend that never stops streaming.
const MAX_ASSISTANT_TEXT_BYTES: usize = 10 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum AgentError {
    #[error("llm backend error: {0}")]
    Llm(#[from] LlmError),
    #[error("maximum tool iterations ({0}) exceeded for this turn")]
    MaxIterationsExceeded(u32),
    #[error("response exceeded the maximum allowed size and was aborted")]
    ResponseTooLarge,
}

pub struct Agent {
    llm: std::sync::Arc<dyn LlmBackend>,
    executor: ToolExecutor,
    system_prompt: String,
    history: Vec<Message>,
    max_tool_iterations: u32,
    events_tx: UnboundedSender<AgentEvent>,
}

impl Agent {
    pub fn new(
        llm: std::sync::Arc<dyn LlmBackend>,
        executor: ToolExecutor,
        system_prompt: impl Into<String>,
        max_tool_iterations: u32,
        events_tx: UnboundedSender<AgentEvent>,
    ) -> Self {
        Self {
            llm,
            executor,
            system_prompt: system_prompt.into(),
            history: Vec::new(),
            // A misconfigured 0 would otherwise make every turn a silent
            // no-op (the tool-loop range would simply never iterate).
            max_tool_iterations: max_tool_iterations.max(1),
            events_tx,
        }
    }

    fn assemble_messages(&self) -> Vec<Message> {
        let mut messages = Vec::with_capacity(self.history.len() + 1);
        messages.push(Message::text(Role::System, &self.system_prompt));
        messages.extend(self.history.iter().cloned());
        messages
    }

    fn emit(&self, event: AgentEvent) {
        // If the receiver (the TUI) has been dropped there's nothing
        // meaningful left to do with this event.
        let _ = self.events_tx.send(event);
    }

    /// Runs one user turn to completion: sends the request, streams the
    /// response live via `AgentEvent`s, executes any tool calls the model
    /// makes, and repeats until the model produces a final answer with no
    /// further tool calls (or `max_tool_iterations` is hit).
    pub async fn run_turn(
        &mut self,
        user_input: String,
        cwd: &Path,
        cancellation: CancellationToken,
    ) -> Result<(), AgentError> {
        self.history.push(Message::text(Role::User, user_input));

        for iteration in 1..=self.max_tool_iterations {
            if cancellation.is_cancelled() {
                break;
            }

            let request = ChatRequest {
                messages: self.assemble_messages(),
                tools: self.executor.definitions(),
                tool_choice: ToolChoice::Auto,
                temperature: None,
                max_tokens: None,
            };

            let mut stream = match self.llm.stream_chat(request).await {
                Ok(stream) => stream,
                Err(err) => {
                    self.emit(AgentEvent::Error(err.to_string()));
                    return Err(AgentError::Llm(err));
                }
            };

            let mut assistant_text = String::new();
            let mut tool_calls: Vec<ToolCall> = Vec::new();
            let mut finish_reason = None;

            loop {
                let next_event = tokio::select! {
                    _ = cancellation.cancelled() => break,
                    event = stream.next() => event,
                };

                let Some(event) = next_event else { break };

                match event {
                    Ok(StreamEvent::TextDelta(text)) => {
                        assistant_text.push_str(&text);
                        self.emit(AgentEvent::TextDelta(text));
                        if assistant_text.len() > MAX_ASSISTANT_TEXT_BYTES {
                            let err = AgentError::ResponseTooLarge;
                            self.emit(AgentEvent::Error(err.to_string()));
                            return Err(err);
                        }
                    }
                    Ok(StreamEvent::ToolCallComplete(call)) => {
                        self.emit(AgentEvent::ToolCallDetected(call.clone()));
                        tool_calls.push(call);
                    }
                    Ok(StreamEvent::Usage { .. }) => {}
                    Ok(StreamEvent::Done {
                        finish_reason: reason,
                    }) => {
                        finish_reason = Some(reason);
                        break;
                    }
                    Err(err) => {
                        self.emit(AgentEvent::Error(err.to_string()));
                        return Err(AgentError::Llm(err));
                    }
                }
            }

            if cancellation.is_cancelled() {
                break;
            }

            // The model's response was cut off mid-generation (output-length
            // cap or a backend-side error) rather than finishing normally —
            // without this, a truncated turn looks identical to the model
            // simply choosing not to say anything further.
            match finish_reason {
                Some(FinishReason::Length) => {
                    self.emit(AgentEvent::Error(
                        "response was truncated (hit the model's output limit) before it finished"
                            .to_string(),
                    ));
                }
                Some(FinishReason::Error) => {
                    self.emit(AgentEvent::Error(
                        "backend reported an error while finishing the response".to_string(),
                    ));
                }
                _ => {}
            }

            let mut assistant_content = Vec::new();
            if !assistant_text.is_empty() {
                assistant_content.push(ContentBlock::Text(assistant_text));
            }
            for call in &tool_calls {
                assistant_content.push(ContentBlock::ToolCall(call.clone()));
            }
            if !assistant_content.is_empty() {
                self.history.push(Message {
                    role: Role::Assistant,
                    content: assistant_content,
                    tool_call_id: None,
                });
            }

            if tool_calls.is_empty() {
                self.emit(AgentEvent::TurnComplete);
                return Ok(());
            }

            for call in tool_calls {
                if cancellation.is_cancelled() {
                    break;
                }

                let result = self
                    .executor
                    .dispatch(call, cwd, cancellation.clone())
                    .await;
                self.emit(AgentEvent::ToolResult(result.clone()));
                self.history.push(Message {
                    role: Role::Tool,
                    tool_call_id: Some(result.call_id.clone()),
                    content: vec![ContentBlock::ToolResult(result)],
                });
            }

            if cancellation.is_cancelled() {
                break;
            }

            if iteration == self.max_tool_iterations {
                let err = AgentError::MaxIterationsExceeded(self.max_tool_iterations);
                self.emit(AgentEvent::Error(err.to_string()));
                return Err(err);
            }
        }

        self.emit(AgentEvent::TurnComplete);
        Ok(())
    }
}
