use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use aivyx_llm::{ChatRequest, FinishReason, LlmBackend, LlmError, StreamEvent, ToolChoice};
use aivyx_tools::ToolExecutor;
use aivyx_types::{ContentBlock, Message, Role, ToolCall, ToolOutput, ToolResult};
use futures::StreamExt;
use thiserror::Error;
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

use crate::session::{self, SessionState, Task};

#[derive(Debug, Clone)]
pub enum AgentEvent {
    TextDelta(String),
    ToolCallDetected(ToolCall),
    ToolResult(ToolResult),
    TurnComplete,
    Error(String),
    /// The backend's reported prompt-token count for the most recent
    /// request, against the configured context window — drives the TUI's
    /// live budget indicator.
    ContextUsage {
        used: u32,
        limit: u32,
    },
    /// The task list changed during this turn (the model called
    /// `set_tasks`) — carries the full new list for the TUI's task panel.
    TasksUpdated(Vec<Task>),
}

/// Caps unbounded growth of a single turn's accumulated assistant text from
/// a misbehaving backend that never stops streaming.
const MAX_ASSISTANT_TEXT_BYTES: usize = 10 * 1024 * 1024;

/// A single LLM response containing more tool calls than this has the
/// excess skipped rather than dispatched. `max_tool_iterations_per_turn`
/// counts LLM round-trips, not calls within one round, so without this a
/// single response could commit an unbounded number of actions — each one
/// individually already-approved via the Always-Allow cache or a
/// pre-approved `allowed_commands` entry — with no existing safeguard
/// noticing until well after the fact.
const MAX_TOOL_CALLS_PER_RESPONSE: usize = 20;

/// Compaction fires when the estimated prompt exceeds this fraction of the
/// context window, and reduces it back below `COMPACT_LOW_WATER` — a gap so
/// it doesn't re-trigger every single turn once near the ceiling.
const COMPACT_HIGH_WATER: f64 = 0.80;
const COMPACT_LOW_WATER: f64 = 0.60;

/// A single tool result longer than this (characters) is elided to a
/// head+tail excerpt during compaction — one huge `read_file`/command
/// output is usually the dominant consumer, and eliding it is far less
/// lossy than dropping whole earlier turns.
const ELIDE_TOOL_RESULT_CHARS: usize = 4000;

/// Starting characters-per-token ratio for the size estimator, before the
/// backend's real `prompt_tokens` calibrate it to the running model. ~4 is
/// the standard rough approximation for English/code.
const DEFAULT_CHARS_PER_TOKEN: f64 = 4.0;

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
    context_limit: u32,
    /// Char-count of the most recently sent request, paired with the
    /// backend's returned `prompt_tokens` to calibrate `chars_per_token`.
    last_request_chars: Option<usize>,
    /// Self-calibrating estimator ratio (see `DEFAULT_CHARS_PER_TOKEN`).
    chars_per_token: f64,
    /// Set once compaction has ever dropped earlier turns, so the assembled
    /// system prompt can tell the model its history was truncated.
    history_truncated: bool,
    /// The task list — the same `Arc` handed to the `set_tasks` tool (which
    /// mutates it); the agent reads it to emit `TasksUpdated` events and to
    /// persist it with the session.
    tasks: Arc<Mutex<Vec<Task>>>,
    /// Where the session is persisted after each turn; `None` disables it.
    session_path: Option<PathBuf>,
    events_tx: UnboundedSender<AgentEvent>,
}

impl Agent {
    pub fn new(
        llm: std::sync::Arc<dyn LlmBackend>,
        executor: ToolExecutor,
        system_prompt: impl Into<String>,
        max_tool_iterations: u32,
        context_limit: u32,
        tasks: Arc<Mutex<Vec<Task>>>,
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
            // A 0 here would make the budget indicator meaningless and
            // trigger compaction constantly; clamp to a floor.
            context_limit: context_limit.max(1),
            last_request_chars: None,
            chars_per_token: DEFAULT_CHARS_PER_TOKEN,
            history_truncated: false,
            tasks,
            session_path: None,
            events_tx,
        }
    }

    /// Enables session persistence: after each turn the full session is
    /// written to `path` (best-effort).
    pub fn set_session_path(&mut self, path: PathBuf) {
        self.session_path = Some(path);
    }

    /// Seeds history and tasks from a resumed session, replacing whatever
    /// the agent currently holds. Call before the first turn.
    pub fn restore(&mut self, state: SessionState) {
        self.history = state.history;
        *self.tasks.lock().unwrap() = state.tasks;
    }

    /// Best-effort snapshot to disk. A persistence failure is logged, never
    /// propagated — losing a save must not fail the user's turn.
    fn persist(&self) {
        let Some(path) = &self.session_path else {
            return;
        };
        let tasks = self.tasks.lock().unwrap().clone();
        let state = SessionState::new(self.history.clone(), tasks);
        if let Err(err) = session::save(path, &state) {
            tracing::warn!(error = %err, "failed to persist session");
        }
    }

    fn assemble_messages(&self) -> Vec<Message> {
        let system = if self.history_truncated {
            format!(
                "{}\n\n(Note: earlier parts of this conversation were truncated to fit the \
                 model's context window. Ask the user to restate anything you're missing.)",
                self.system_prompt
            )
        } else {
            self.system_prompt.clone()
        };
        let mut messages = Vec::with_capacity(self.history.len() + 1);
        messages.push(Message::text(Role::System, system));
        messages.extend(self.history.iter().cloned());
        messages
    }

    /// Rough token estimate for the current system prompt + history, using
    /// the self-calibrated `chars_per_token`. Deliberately consistent with
    /// the char-count used for calibration (both ignore tool-definition
    /// tokens), so the systematic omission cancels out in the ratio.
    fn estimate_prompt_tokens(&self) -> u32 {
        let chars = message_chars(&self.system_prompt, &self.history);
        (chars as f64 / self.chars_per_token).ceil() as u32
    }

    /// Keeps the prompt within the model's window: when the estimate crosses
    /// the high-water mark, first elide oversized tool results, then drop the
    /// oldest whole turn-groups until back under the low-water mark. Turn-
    /// groups are cut only at `Role::User` boundaries, so a `tool_call` is
    /// never separated from its `Role::Tool` result (the loop's invariant),
    /// and the most recent turn is always kept. Truncation is surfaced to the
    /// user (an event) and to the model (a system-prompt note) — never silent.
    fn compact_if_needed(&mut self) {
        let high = (self.context_limit as f64 * COMPACT_HIGH_WATER) as u32;
        if self.estimate_prompt_tokens() <= high {
            return;
        }

        elide_oversized_tool_results(&mut self.history, ELIDE_TOOL_RESULT_CHARS);

        let low = (self.context_limit as f64 * COMPACT_LOW_WATER) as u32;
        let mut dropped = false;
        while self.estimate_prompt_tokens() > low {
            if !drop_oldest_group(&mut self.history) {
                break;
            }
            dropped = true;
        }

        if dropped {
            self.history_truncated = true;
            self.emit(AgentEvent::Error(
                "earlier conversation was truncated to fit the model's context window".to_string(),
            ));
        }
    }

    fn emit(&self, event: AgentEvent) {
        // If the receiver (the TUI) has been dropped there's nothing
        // meaningful left to do with this event.
        let _ = self.events_tx.send(event);
    }

    /// Records a synthetic tool result for a call that was never dispatched
    /// — either cancellation fired first, or the response exceeded
    /// `MAX_TOOL_CALLS_PER_RESPONSE`. The assistant message already pushed
    /// to history recorded this call as a `ContentBlock::ToolCall`; every
    /// such call needs a matching `Role::Tool` result or the next turn's
    /// request will contain an assistant message with unanswered
    /// tool_calls, which most OpenAI-compatible backends reject outright.
    fn record_skipped_tool_result(&mut self, call: ToolCall, reason: &str) {
        let result = ToolResult {
            call_id: call.id,
            output: ToolOutput::Denied(reason.to_string()),
        };
        self.emit(AgentEvent::ToolResult(result.clone()));
        self.history.push(Message {
            role: Role::Tool,
            tool_call_id: Some(result.call_id.clone()),
            content: vec![ContentBlock::ToolResult(result)],
        });
    }

    /// Runs one user turn to completion, then persists the session — the
    /// wrapper ensures *every* exit path of the inner loop (normal, error,
    /// iteration-cap, cancellation) saves, without threading a save into
    /// each `return`.
    pub async fn run_turn(
        &mut self,
        user_input: String,
        cwd: &Path,
        cancellation: CancellationToken,
    ) -> Result<(), AgentError> {
        let result = self.run_turn_inner(user_input, cwd, cancellation).await;
        self.persist();
        result
    }

    /// Runs one user turn: sends the request, streams the response live via
    /// `AgentEvent`s, executes any tool calls the model makes, and repeats
    /// until the model produces a final answer with no further tool calls
    /// (or `max_tool_iterations` is hit).
    async fn run_turn_inner(
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

            self.compact_if_needed();
            // Recorded here (not from `assemble_messages`) so it pairs with
            // the estimator's own char-count for a consistent calibration.
            self.last_request_chars = Some(message_chars(&self.system_prompt, &self.history));

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
                    Ok(StreamEvent::Usage { prompt_tokens, .. }) => {
                        // Calibrate the estimator to this model: how many
                        // chars actually mapped to one prompt token this time.
                        if let Some(chars) = self.last_request_chars
                            && prompt_tokens > 0
                        {
                            self.chars_per_token =
                                (chars as f64 / prompt_tokens as f64).clamp(1.0, 12.0);
                        }
                        self.emit(AgentEvent::ContextUsage {
                            used: prompt_tokens,
                            limit: self.context_limit,
                        });
                    }
                    Ok(StreamEvent::Done {
                        finish_reason: reason,
                    }) => {
                        // Record but keep draining: with `include_usage`,
                        // OpenAI-compatible servers send the usage-bearing
                        // chunk *after* the one carrying `finish_reason`, so
                        // breaking here would lose the token counts that
                        // drive the context indicator and the estimator's
                        // calibration. The stream ends on its own at [DONE].
                        finish_reason = Some(reason);
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

            // Snapshot-compare rather than matching on the tool's name, so
            // the agent loop stays ignorant of which registered tool (if
            // any) mutates the list.
            let tasks_before = self.tasks.lock().unwrap().clone();

            for (index, call) in tool_calls.into_iter().enumerate() {
                // Every one of these calls is already recorded as a
                // ContentBlock::ToolCall in the assistant message just
                // pushed to history — each one needs a matching Role::Tool
                // result or the *next* turn's request will contain an
                // assistant message with unanswered tool_calls, which most
                // OpenAI-compatible backends reject outright. So once
                // cancelled (or over the per-response cap), record the
                // remaining calls as skipped rather than silently dropping
                // them.
                if index >= MAX_TOOL_CALLS_PER_RESPONSE {
                    self.record_skipped_tool_result(
                        call,
                        "skipped — too many tool calls in a single response",
                    );
                    continue;
                }
                if cancellation.is_cancelled() {
                    self.record_skipped_tool_result(
                        call,
                        "cancelled before this tool call was executed",
                    );
                    continue;
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

            let tasks_after = self.tasks.lock().unwrap().clone();
            if tasks_after != tasks_before {
                self.emit(AgentEvent::TasksUpdated(tasks_after));
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

/// Total character count of the system prompt plus all message content —
/// the estimator's proxy for prompt size. Tool-call arguments and result
/// text count; this ignores tool-*definition* tokens, which is fine because
/// calibration uses the same measure (the omission cancels in the ratio).
fn message_chars(system_prompt: &str, history: &[Message]) -> usize {
    let mut total = system_prompt.chars().count();
    for message in history {
        for block in &message.content {
            total += match block {
                ContentBlock::Text(text) => text.chars().count(),
                ContentBlock::ToolCall(call) => {
                    call.name.chars().count() + call.arguments.to_string().chars().count()
                }
                ContentBlock::ToolResult(result) => match &result.output {
                    ToolOutput::Ok(s) | ToolOutput::Error(s) | ToolOutput::Denied(s) => {
                        s.chars().count()
                    }
                },
            };
        }
    }
    total
}

/// Drops the oldest complete turn-group — everything from the start up to
/// (but not including) the *second* `Role::User` message. Cutting only at
/// user boundaries keeps every `tool_call`/`Role::Tool`-result pairing
/// intact and always preserves the most recent turn. Returns whether
/// anything was dropped (false when one group or fewer remains).
fn drop_oldest_group(history: &mut Vec<Message>) -> bool {
    let mut users_seen = 0;
    let mut cut = None;
    for (i, message) in history.iter().enumerate() {
        if message.role == Role::User {
            users_seen += 1;
            if users_seen == 2 {
                cut = Some(i);
                break;
            }
        }
    }
    match cut {
        Some(cut) => {
            history.drain(0..cut);
            true
        }
        None => false,
    }
}

/// Elides the body of any `ToolOutput::Ok` result longer than `cap` chars to
/// a head+tail excerpt — a huge file read or command output is usually the
/// single dominant consumer, and this is far less lossy than dropping turns.
fn elide_oversized_tool_results(history: &mut [Message], cap: usize) {
    for message in history.iter_mut() {
        for block in message.content.iter_mut() {
            if let ContentBlock::ToolResult(result) = block
                && let ToolOutput::Ok(text) = &mut result.output
                && text.chars().count() > cap
            {
                *text = elide(text, cap);
            }
        }
    }
}

fn elide(text: &str, cap: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= cap {
        return text.to_string();
    }
    let half = cap / 2;
    let head: String = chars[..half].iter().collect();
    let tail: String = chars[chars.len() - half..].iter().collect();
    format!(
        "{head}\n[... {} characters elided to fit the context window ...]\n{tail}",
        chars.len() - 2 * half
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    use aivyx_sandbox::{
        ActionKind, ExecutionConfiner, NoopConfiner, PermissionDecision, PermissionGate,
        PermissionRequest, PermissionTarget,
    };
    use aivyx_tools::{Tool, ToolError, ToolExecutionContext, ToolRegistry};
    use aivyx_types::{ToolCallId, ToolCallSource, ToolDefinition};
    use futures::stream::BoxStream;
    use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

    /// Scriptable `LlmBackend`: each `stream_chat` pops the next scripted
    /// response (a whole `Vec<StreamEvent>`) and streams it, and records the
    /// request it received so tests can assert on what history was actually
    /// sent (used heavily once compaction lands). An exhausted queue streams
    /// nothing — the loop then sees a response with no tool calls and ends.
    struct MockBackend {
        responses: Mutex<VecDeque<Vec<StreamEvent>>>,
        received: Mutex<Vec<ChatRequest>>,
    }

    impl MockBackend {
        fn new(responses: Vec<Vec<StreamEvent>>) -> Self {
            Self {
                responses: Mutex::new(responses.into()),
                received: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait::async_trait]
    impl LlmBackend for MockBackend {
        fn model_id(&self) -> &str {
            "mock"
        }

        async fn stream_chat(
            &self,
            request: ChatRequest,
        ) -> Result<BoxStream<'static, Result<StreamEvent, LlmError>>, LlmError> {
            self.received.lock().unwrap().push(request);
            let events = self
                .responses
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_default();
            Ok(futures::stream::iter(events.into_iter().map(Ok::<StreamEvent, LlmError>)).boxed())
        }
    }

    /// Gate that allows everything — the loop tests are about loop mechanics,
    /// not permission decisions (those have their own tests in aivyx-sandbox).
    struct AllowAllGate;

    #[async_trait::async_trait]
    impl PermissionGate for AllowAllGate {
        async fn check(&self, _request: &PermissionRequest) -> PermissionDecision {
            PermissionDecision::Allow
        }
    }

    /// A tool whose only effect is to cancel the run's cancellation token —
    /// lets a test deterministically trigger the mid-dispatch cancellation
    /// checkpoint (the token becomes cancelled *during* the dispatch loop, so
    /// any remaining calls in the same response must be recorded as skipped).
    struct CancelTool;

    #[async_trait::async_trait]
    impl Tool for CancelTool {
        fn name(&self) -> &str {
            "cancel_tool"
        }

        fn definition(&self) -> ToolDefinition {
            ToolDefinition {
                name: "cancel_tool".to_string(),
                description: "test".to_string(),
                parameters_schema: serde_json::json!({}),
            }
        }

        fn permission_request(
            &self,
            _arguments: &serde_json::Value,
            _cwd: &Path,
        ) -> Result<PermissionRequest, ToolError> {
            Ok(PermissionRequest {
                tool_name: "cancel_tool".to_string(),
                action: ActionKind::Execute,
                target: PermissionTarget::Other("cancel".to_string()),
                arguments_preview: serde_json::json!({}),
                preview: None,
            })
        }

        async fn execute(
            &self,
            _arguments: serde_json::Value,
            ctx: &ToolExecutionContext,
        ) -> Result<ToolOutput, ToolError> {
            ctx.cancellation.cancel();
            Ok(ToolOutput::Ok("cancelled the token".to_string()))
        }
    }

    fn tool_call(id: &str, name: &str) -> ToolCall {
        ToolCall {
            id: ToolCallId(id.to_string()),
            name: name.to_string(),
            arguments: serde_json::json!({}),
            source: ToolCallSource::Native,
        }
    }

    fn user_msg(text: &str) -> Message {
        Message::text(Role::User, text)
    }

    fn assistant_msg(text: &str) -> Message {
        Message::text(Role::Assistant, text)
    }

    fn ok_tool_result_msg(id: &str, text: &str) -> Message {
        Message {
            role: Role::Tool,
            tool_call_id: Some(ToolCallId(id.to_string())),
            content: vec![ContentBlock::ToolResult(ToolResult {
                call_id: ToolCallId(id.to_string()),
                output: ToolOutput::Ok(text.to_string()),
            })],
        }
    }

    fn build_agent(
        responses: Vec<Vec<StreamEvent>>,
        registry: ToolRegistry,
        max_iters: u32,
    ) -> (Agent, UnboundedReceiver<AgentEvent>, Arc<MockBackend>) {
        let (tx, rx) = unbounded_channel();
        let mock = Arc::new(MockBackend::new(responses));
        let llm: std::sync::Arc<dyn LlmBackend> = mock.clone();
        let gate: Arc<dyn PermissionGate> = Arc::new(AllowAllGate);
        let confiner: Arc<dyn ExecutionConfiner> = Arc::new(NoopConfiner);
        let executor = ToolExecutor::new(registry, gate, confiner);
        let agent = Agent::new(llm, executor, "system", max_iters, 8192, Arc::default(), tx);
        (agent, rx, mock)
    }

    fn drain(rx: &mut UnboundedReceiver<AgentEvent>) -> Vec<AgentEvent> {
        let mut out = Vec::new();
        while let Ok(event) = rx.try_recv() {
            out.push(event);
        }
        out
    }

    fn count_tool_calls(history: &[Message]) -> usize {
        history
            .iter()
            .flat_map(|m| &m.content)
            .filter(|b| matches!(b, ContentBlock::ToolCall(_)))
            .count()
    }

    fn count_tool_results(history: &[Message]) -> usize {
        history.iter().filter(|m| m.role == Role::Tool).count()
    }

    fn count_denied_containing(history: &[Message], needle: &str) -> usize {
        history
            .iter()
            .flat_map(|m| &m.content)
            .filter(|b| {
                matches!(
                    b,
                    ContentBlock::ToolResult(ToolResult { output: ToolOutput::Denied(msg), .. })
                        if msg.contains(needle)
                )
            })
            .count()
    }

    #[tokio::test]
    async fn plain_text_response_completes_the_turn() {
        let (mut agent, mut rx, _) = build_agent(
            vec![vec![
                StreamEvent::TextDelta("hi there".to_string()),
                StreamEvent::Done {
                    finish_reason: FinishReason::Stop,
                },
            ]],
            ToolRegistry::new(),
            10,
        );

        agent
            .run_turn(
                "hello".to_string(),
                Path::new("."),
                CancellationToken::new(),
            )
            .await
            .unwrap();

        assert_eq!(agent.history.len(), 2);
        assert_eq!(agent.history[0].role, Role::User);
        assert_eq!(agent.history[1].role, Role::Assistant);
        assert_eq!(agent.history[1].text_content(), "hi there");
        assert!(
            drain(&mut rx)
                .iter()
                .any(|e| matches!(e, AgentEvent::TurnComplete))
        );
    }

    #[tokio::test]
    async fn tool_call_then_final_answer_produces_balanced_history() {
        // The tool is unregistered, so dispatch returns a NotFound error
        // result — enough to exercise the full assistant-toolcall -> dispatch
        // -> tool-result -> next-iteration mechanic and its balance invariant.
        let (mut agent, _rx, _) = build_agent(
            vec![
                vec![
                    StreamEvent::ToolCallComplete(tool_call("c1", "read_file")),
                    StreamEvent::Done {
                        finish_reason: FinishReason::ToolCalls,
                    },
                ],
                vec![
                    StreamEvent::TextDelta("done".to_string()),
                    StreamEvent::Done {
                        finish_reason: FinishReason::Stop,
                    },
                ],
            ],
            ToolRegistry::new(),
            10,
        );

        agent
            .run_turn("go".to_string(), Path::new("."), CancellationToken::new())
            .await
            .unwrap();

        assert_eq!(count_tool_calls(&agent.history), 1);
        assert_eq!(count_tool_results(&agent.history), 1);
        assert_eq!(agent.history.last().unwrap().text_content(), "done");
    }

    #[tokio::test]
    async fn too_many_tool_calls_in_one_response_are_capped_but_all_recorded() {
        let mut first: Vec<StreamEvent> = (0..25)
            .map(|i| StreamEvent::ToolCallComplete(tool_call(&format!("c{i}"), "read_file")))
            .collect();
        first.push(StreamEvent::Done {
            finish_reason: FinishReason::ToolCalls,
        });
        let (mut agent, _rx, _) = build_agent(
            vec![
                first,
                vec![StreamEvent::Done {
                    finish_reason: FinishReason::Stop,
                }],
            ],
            ToolRegistry::new(),
            10,
        );

        agent
            .run_turn("go".to_string(), Path::new("."), CancellationToken::new())
            .await
            .unwrap();

        // Every call still gets a matching result (invariant preserved)...
        assert_eq!(count_tool_results(&agent.history), 25);
        // ...but only the first MAX_TOOL_CALLS_PER_RESPONSE actually ran; the
        // remaining 5 are recorded as skipped.
        assert_eq!(
            count_denied_containing(&agent.history, "too many tool calls"),
            25 - MAX_TOOL_CALLS_PER_RESPONSE
        );
    }

    #[tokio::test]
    async fn truncated_response_surfaces_an_error_event() {
        let (mut agent, mut rx, _) = build_agent(
            vec![vec![
                StreamEvent::TextDelta("partial".to_string()),
                StreamEvent::Done {
                    finish_reason: FinishReason::Length,
                },
            ]],
            ToolRegistry::new(),
            10,
        );

        agent
            .run_turn("go".to_string(), Path::new("."), CancellationToken::new())
            .await
            .unwrap();

        assert!(
            drain(&mut rx)
                .iter()
                .any(|e| matches!(e, AgentEvent::Error(msg) if msg.contains("truncated")))
        );
    }

    #[tokio::test]
    async fn usage_is_surfaced_as_a_context_usage_event() {
        let (mut agent, mut rx, _) = build_agent(
            vec![vec![
                StreamEvent::Usage {
                    prompt_tokens: 1234,
                    completion_tokens: 56,
                },
                StreamEvent::TextDelta("ok".to_string()),
                StreamEvent::Done {
                    finish_reason: FinishReason::Stop,
                },
            ]],
            ToolRegistry::new(),
            10,
        );

        agent
            .run_turn("go".to_string(), Path::new("."), CancellationToken::new())
            .await
            .unwrap();

        assert!(drain(&mut rx).iter().any(|e| matches!(
            e,
            AgentEvent::ContextUsage {
                used: 1234,
                limit: 8192
            }
        )));
    }

    #[tokio::test]
    async fn usage_arriving_after_done_is_still_surfaced() {
        // The shape real OpenAI-compatible servers (incl. Ollama) produce
        // with `stream_options.include_usage`: the usage chunk trails the
        // finish_reason chunk. Regression test for the loop breaking on
        // `Done` and losing the token counts — caught live, not by the
        // original tests, which all put Usage before Done.
        let (mut agent, mut rx, _) = build_agent(
            vec![vec![
                StreamEvent::TextDelta("ok".to_string()),
                StreamEvent::Done {
                    finish_reason: FinishReason::Stop,
                },
                StreamEvent::Usage {
                    prompt_tokens: 777,
                    completion_tokens: 5,
                },
            ]],
            ToolRegistry::new(),
            10,
        );

        agent
            .run_turn("go".to_string(), Path::new("."), CancellationToken::new())
            .await
            .unwrap();

        assert!(
            drain(&mut rx)
                .iter()
                .any(|e| matches!(e, AgentEvent::ContextUsage { used: 777, .. }))
        );
    }

    #[tokio::test]
    async fn a_set_tasks_call_surfaces_tasks_updated_and_persists_the_session() {
        // The real `set_tasks` tool, wired the same way `main.rs` wires it:
        // one shared handle given to both the tool and the agent — this test
        // covers the whole loop (dispatch mutates the list, the agent
        // notices, emits, and persists it with the turn).
        let tasks: Arc<Mutex<Vec<Task>>> = Arc::default();
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(aivyx_tools::SetTasksTool::new(Arc::clone(&tasks))));

        let (tx, mut rx) = unbounded_channel();
        let mock = Arc::new(MockBackend::new(vec![
            vec![
                StreamEvent::ToolCallComplete(ToolCall {
                    id: ToolCallId("c1".to_string()),
                    name: "set_tasks".to_string(),
                    arguments: serde_json::json!({ "tasks": [
                        { "text": "step one", "status": "in_progress" },
                    ]}),
                    source: ToolCallSource::Native,
                }),
                StreamEvent::Done {
                    finish_reason: FinishReason::ToolCalls,
                },
            ],
            vec![
                StreamEvent::TextDelta("done".to_string()),
                StreamEvent::Done {
                    finish_reason: FinishReason::Stop,
                },
            ],
        ]));
        let llm: Arc<dyn LlmBackend> = mock.clone();
        let gate: Arc<dyn PermissionGate> = Arc::new(AllowAllGate);
        let confiner: Arc<dyn ExecutionConfiner> = Arc::new(NoopConfiner);
        let executor = ToolExecutor::new(registry, gate, confiner);
        let mut agent = Agent::new(llm, executor, "system", 10, 8192, Arc::clone(&tasks), tx);

        let dir = tempfile::tempdir().unwrap();
        let session_path = dir.path().join("session.json");
        agent.set_session_path(session_path.clone());

        agent
            .run_turn("go".to_string(), Path::new("."), CancellationToken::new())
            .await
            .unwrap();

        assert!(drain(&mut rx).iter().any(|e| matches!(
            e,
            AgentEvent::TasksUpdated(list) if list.len() == 1 && list[0].text == "step one"
        )));

        let saved = crate::session::load(&session_path).expect("session should have been saved");
        assert_eq!(saved.tasks.len(), 1);
        assert_eq!(
            saved.tasks[0].status,
            crate::session::TaskStatus::InProgress
        );
        // user, assistant (tool call), tool result, final assistant text.
        assert_eq!(saved.history.len(), 4);
    }

    #[tokio::test]
    async fn max_tool_iterations_is_enforced() {
        // Both responses request a tool and never give a final answer, so the
        // loop must terminate on the iteration cap.
        let looping = || {
            vec![
                StreamEvent::ToolCallComplete(tool_call("c", "read_file")),
                StreamEvent::Done {
                    finish_reason: FinishReason::ToolCalls,
                },
            ]
        };
        let (mut agent, _rx, _) = build_agent(vec![looping(), looping()], ToolRegistry::new(), 2);

        let result = agent
            .run_turn("go".to_string(), Path::new("."), CancellationToken::new())
            .await;

        assert!(matches!(result, Err(AgentError::MaxIterationsExceeded(2))));
    }

    #[tokio::test]
    async fn a_precancelled_turn_is_a_noop_with_only_the_user_message() {
        let (mut agent, _rx, mock) = build_agent(
            vec![vec![StreamEvent::Done {
                finish_reason: FinishReason::Stop,
            }]],
            ToolRegistry::new(),
            10,
        );
        let cancellation = CancellationToken::new();
        cancellation.cancel();

        agent
            .run_turn("go".to_string(), Path::new("."), cancellation)
            .await
            .unwrap();

        // The user message is recorded, but no request is ever sent and no
        // assistant/tool messages are appended.
        assert_eq!(agent.history.len(), 1);
        assert_eq!(agent.history[0].role, Role::User);
        assert!(mock.received.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn cancellation_mid_dispatch_records_remaining_calls_as_skipped() {
        // Two calls to a tool that cancels the run token on its first
        // execution: call 1 runs (and cancels), call 2 must then be recorded
        // as cancelled rather than dispatched — and both still get results.
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(CancelTool));
        let (mut agent, _rx, _) = build_agent(
            vec![vec![
                StreamEvent::ToolCallComplete(tool_call("c1", "cancel_tool")),
                StreamEvent::ToolCallComplete(tool_call("c2", "cancel_tool")),
                StreamEvent::Done {
                    finish_reason: FinishReason::ToolCalls,
                },
            ]],
            registry,
            10,
        );

        agent
            .run_turn("go".to_string(), Path::new("."), CancellationToken::new())
            .await
            .unwrap();

        assert_eq!(count_tool_results(&agent.history), 2);
        assert_eq!(
            count_denied_containing(&agent.history, "cancelled before"),
            1
        );
    }

    #[test]
    fn drop_oldest_group_removes_the_first_turn_and_keeps_the_rest() {
        let mut history = vec![
            user_msg("u1"),
            assistant_msg("a1"),
            ok_tool_result_msg("c1", "r1"),
            user_msg("u2"),
            assistant_msg("a2"),
        ];
        assert!(drop_oldest_group(&mut history));
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].role, Role::User);
        assert_eq!(history[0].text_content(), "u2");
    }

    #[test]
    fn drop_oldest_group_keeps_the_only_group() {
        let mut history = vec![user_msg("u1"), assistant_msg("a1")];
        assert!(!drop_oldest_group(&mut history));
        assert_eq!(history.len(), 2);
    }

    #[test]
    fn elide_shrinks_only_oversized_ok_results() {
        let big = "x".repeat(10_000);
        let mut history = vec![
            ok_tool_result_msg("c1", &big),
            ok_tool_result_msg("c2", "small"),
        ];
        elide_oversized_tool_results(&mut history, 100);

        let ContentBlock::ToolResult(r1) = &history[0].content[0] else {
            panic!("expected a tool result")
        };
        let ToolOutput::Ok(s1) = &r1.output else {
            panic!("expected Ok")
        };
        assert!(s1.chars().count() < 10_000);
        assert!(s1.contains("elided"));

        let ContentBlock::ToolResult(r2) = &history[1].content[0] else {
            panic!("expected a tool result")
        };
        let ToolOutput::Ok(s2) = &r2.output else {
            panic!("expected Ok")
        };
        assert_eq!(s2, "small");
    }

    #[test]
    fn compaction_drops_oldest_turns_and_surfaces_a_notice() {
        let (mut agent, mut rx, _) = build_agent(vec![], ToolRegistry::new(), 10);
        // Small window so a handful of padded turns overflows it. With the
        // default 4 chars/token: high-water 80 tok = 320 chars, low 60 = 240.
        agent.context_limit = 100;
        for i in 0..5 {
            agent
                .history
                .push(user_msg(&format!("u{i} {}", "x".repeat(150))));
            agent
                .history
                .push(assistant_msg(&format!("a{i} {}", "y".repeat(150))));
        }
        let before = agent.history.len();

        agent.compact_if_needed();

        assert!(agent.history.len() < before, "history should have shrunk");
        assert!(agent.history_truncated);
        // The most recent turn is always preserved.
        assert!(
            agent
                .history
                .last()
                .unwrap()
                .text_content()
                .starts_with("a4")
        );
        assert!(
            drain(&mut rx)
                .iter()
                .any(|e| matches!(e, AgentEvent::Error(m) if m.contains("truncated")))
        );
    }

    #[test]
    fn no_compaction_when_well_under_the_window() {
        let (mut agent, mut rx, _) = build_agent(vec![], ToolRegistry::new(), 10);
        agent.context_limit = 100_000;
        agent.history.push(user_msg("hello"));
        agent.history.push(assistant_msg("hi"));

        agent.compact_if_needed();

        assert_eq!(agent.history.len(), 2);
        assert!(!agent.history_truncated);
        assert!(drain(&mut rx).is_empty());
    }
}
