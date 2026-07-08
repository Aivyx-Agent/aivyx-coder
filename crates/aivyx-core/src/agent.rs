use std::path::Path;

use aivyx_llm::{ChatRequest, LlmBackend, LlmError, StreamEvent, ToolChoice};
use aivyx_tools::ToolExecutor;
use aivyx_types::{ContentBlock, Message, Role, ToolCall, ToolResult};
use futures::StreamExt;
use thiserror::Error;
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

const DEFAULT_MAX_TOOL_ITERATIONS: u32 = 25;

#[derive(Debug, Clone)]
pub enum AgentEvent {
    TextDelta(String),
    ToolCallDetected(ToolCall),
    ToolResult(ToolResult),
    TurnComplete,
    Error(String),
}

#[derive(Debug, Error)]
pub enum AgentError {
    #[error("llm backend error: {0}")]
    Llm(#[from] LlmError),
    #[error("maximum tool iterations ({0}) exceeded for this turn")]
    MaxIterationsExceeded(u32),
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
        events_tx: UnboundedSender<AgentEvent>,
    ) -> Self {
        Self {
            llm,
            executor,
            system_prompt: system_prompt.into(),
            history: Vec::new(),
            max_tool_iterations: DEFAULT_MAX_TOOL_ITERATIONS,
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
                    }
                    Ok(StreamEvent::ToolCallComplete(call)) => {
                        self.emit(AgentEvent::ToolCallDetected(call.clone()));
                        tool_calls.push(call);
                    }
                    Ok(StreamEvent::Usage { .. }) => {}
                    Ok(StreamEvent::Done { .. }) => break,
                    Err(err) => {
                        self.emit(AgentEvent::Error(err.to_string()));
                        return Err(AgentError::Llm(err));
                    }
                }
            }

            if cancellation.is_cancelled() {
                break;
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
