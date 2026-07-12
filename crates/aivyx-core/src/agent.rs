use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use aivyx_llm::{ChatRequest, FinishReason, LlmBackend, LlmError, StreamEvent, ToolChoice};
use aivyx_repomap::RepoMap;
use aivyx_sandbox::{AutonomousMode, PlanMode};
use aivyx_tools::ToolExecutor;
use aivyx_types::{
    ContentBlock, Message, Role, ToolCall, ToolCallId, ToolCallSource, ToolOutput, ToolResult,
};
use futures::StreamExt;
use thiserror::Error;
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

use crate::edit_blocks::{self, BlockParse};
use crate::session::{self, SessionState, Task};

#[derive(Debug, Clone)]
pub enum AgentEvent {
    TextDelta(String),
    ToolCallDetected(ToolCall),
    ToolResult(ToolResult),
    TurnComplete,
    Error(String),
    /// The per-round-trip iteration cap (`max_tool_iterations`) was hit
    /// while the model was still actively issuing tool calls — not a
    /// natural no-more-tool-calls stop. Deliberately distinct from both
    /// `TurnComplete` and `Error`: nothing was lost (every dispatched tool
    /// call already has a matching result in history, and the session
    /// persists as usual), so this is a pause the user can resume from by
    /// sending another message, not a failure. See ROADMAP.md Phase 12
    /// Part A — `AgentError::MaxIterationsExceeded` is reserved for a
    /// future, coarser autonomous-session budget (Phase 11c) instead.
    TurnPaused(String),
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
    /// One block of council-mode output (a stage banner, a member's answer
    /// or ranking, the chairman's synthesis, or a failure note) — the whole
    /// deliberation streams through these; see `council::convene`.
    CouncilNote(String),
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

/// Appended to the system prompt while plan mode is active. The withheld
/// tools (see `ToolExecutor::plan_definitions`) are the enforcement; this
/// note is what makes the model *plan* instead of flailing at their absence.
const PLAN_MODE_PROMPT: &str = "PLAN MODE is active: tools that modify files or run commands are \
withheld. Explore with the read and search tools, record a step-by-step plan with set_tasks, \
then summarize the plan and ask the user to press Ctrl+P to approve it and switch to Act mode. \
Do not claim to have made any changes — you cannot make any in this mode.";

/// How edit content travels to and from the model. See ROADMAP.md Phase 2
/// for the A/B evidence behind the configured default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditFormat {
    /// Edits are edit_file/write_file tool calls with JSON arguments.
    Native,
    /// Edits are SEARCH/REPLACE blocks in the assistant's plain text,
    /// parsed by `edit_blocks` and applied through the same tools (and
    /// therefore the same permission gate) as synthesized calls; the edit
    /// tools themselves are withheld from the model's tool list.
    Prompted,
}

/// The tools hidden from the model (but kept registered — the synthesized
/// calls dispatch to them) while `EditFormat::Prompted` is active.
const PROMPTED_EDIT_HIDDEN_TOOLS: &[&str] = &["edit_file", "write_file"];

/// Appended to the system prompt in prompted edit mode (outside plan mode).
const EDIT_FORMAT_PROMPT: &str = "To modify or create files, do NOT call tools. Write \
SEARCH/REPLACE blocks directly in your reply, formatted exactly like this:\n\
\n\
path/to/file.rs\n\
<<<<<<< SEARCH\n\
exact existing lines to find\n\
=======\n\
replacement lines\n\
>>>>>>> REPLACE\n\
\n\
Rules: the SEARCH text must match the current file content exactly — copy it verbatim, \
including indentation. Keep each block small and focused; use several blocks for several \
changes. To create a new file, leave the SEARCH section empty. Each block is applied only \
after the user approves it, and you will receive a result for each block.";

/// Appended to the system prompt whenever `[verification]` is configured, so
/// the model isn't confused seeing a `run_command` call in its own history
/// that it doesn't remember making — see ROADMAP.md Phase 12 Part B.
const VERIFICATION_PROMPT: &str = "After you finish making file edits, an automatic run_command \
verification call may appear in your history — this is the agent enforcing your project's \
configured verification command, not something you called yourself. If it fails, fix the \
issue based on its output; it will run again automatically once you stop making further edits.";

/// The tools hidden from the model while autonomous mode is active — belt-
/// and-braces with `ConfirmationGate`'s independent denial of both (Phase
/// 11c): `run_shell` would almost always be denied anyway (only an exact
/// `allowed_commands` match could pass), and `git_commit` is *always*
/// denied in autonomous mode (checkpoints are the record; a human reviews
/// and commits afterward) — offering either would just invite the small-
/// model retry-loop-on-unavailable-action failure mode Phase 8 already
/// found and designed plan mode around.
const AUTONOMOUS_HIDDEN_TOOLS: &[&str] = &["run_shell", "git_commit"];

/// Appended to the system prompt while autonomous mode is active.
const AUTONOMOUS_PROMPT: &str = "You are running unattended (autonomous mode): no human will \
approve your actions. Edits inside the project directory are automatically approved — do not \
wait for confirmation, it will never come. Verification runs automatically after your edits; if \
it fails, fix the issue based on the output fed back to you. Use set_tasks to track your plan: \
marking every task done is how you signal the goal is achieved and this session should stop. \
Leaving tasks incomplete means you will be prompted to continue working toward the goal.";

/// Configures the enforced verification loop (ROADMAP.md Phase 12 Part B):
/// after file edits, before a turn is allowed to end, the named
/// `allowed_commands` entry is auto-run via `run_command`.
#[derive(Debug, Clone)]
struct VerificationConfig {
    command_name: String,
    /// Clamped to a minimum of 1 by `Agent::set_verification` — a 0 here
    /// would report "still failing" without ever actually attempting a
    /// verification run.
    max_retries: u32,
}

/// Scalar knobs for an `Agent`, grouped so `Agent::new`'s arity stays sane
/// as configuration accumulates (it has grown every phase so far).
#[derive(Debug, Clone, Copy)]
pub struct AgentConfig {
    /// Max LLM round-trips per user turn; clamped to a minimum of 1.
    pub max_tool_iterations: u32,
    /// The model's context window, in tokens; clamped to a minimum of 1.
    pub context_tokens: u32,
    /// How edit content travels (see `EditFormat`).
    pub edit_format: EditFormat,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            max_tool_iterations: 25,
            context_tokens: 8192,
            edit_format: EditFormat::Native,
        }
    }
}

#[derive(Debug, Error)]
pub enum AgentError {
    #[error("llm backend error: {0}")]
    Llm(#[from] LlmError),
    /// Reserved for a future, coarser autonomous-session budget (Phase
    /// 11c) — the per-round-trip `max_tool_iterations` cap no longer
    /// constructs this (see `AgentEvent::TurnPaused`, ROADMAP.md Phase 12
    /// Part A): hitting that cap mid-work is a resumable pause, not a
    /// failure. Kept as a real hard ceiling for whatever unattended-budget
    /// check Phase 11c adds, so an unbounded autonomous loop still has
    /// something able to say no.
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
    /// Read at every request assembly (tool list + system-prompt note); the
    /// gate holds its own clone for enforcement, and the TUI toggles it.
    plan_mode: PlanMode,
    /// Read at every request assembly (tool list + system-prompt note) and
    /// consulted by the discard/rewind path (Task 6); the gate holds its
    /// own clone for enforcement. See ROADMAP.md Phase 11c.
    autonomous_mode: AutonomousMode,
    /// Set to `true` right before emitting `AgentEvent::TurnPaused`, `false`
    /// at the start of every `run_turn`/`run_council_turn` call and right
    /// before emitting `AgentEvent::TurnComplete`. Exists so a caller that
    /// owns `agent: Agent` directly (the autonomous driver, Task 8) can
    /// synchronously tell "did the turn I just ran pause or complete"
    /// without racing the `AgentEvent` stream across tasks — see the Phase
    /// 11c design doc / plan for why event-stream inference is unreliable
    /// here.
    last_turn_paused: bool,
    /// `(map, budget_tokens)` when the repo map is enabled; re-rendered
    /// once per turn (cheap after the first pass — only changed files
    /// re-parse) into `repo_map_text`.
    repo_map: Option<(Arc<RepoMap>, u32)>,
    /// The rendered slice appended to the system prompt; also counted by
    /// the context estimator — a ~1k-token block compaction can't see would
    /// silently eat the window's headroom.
    repo_map_text: Option<String>,
    edit_format: EditFormat,
    /// Monotonic id source for tool calls synthesized from SEARCH/REPLACE
    /// blocks — they need ids that can't collide with the backend's.
    synthetic_seq: u64,
    /// `/council` support when configured; `None` makes the command explain
    /// how to enable itself instead of running.
    council: Option<crate::council::Council>,
    /// `[verification]` support (ROADMAP.md Phase 12 Part B); `None`
    /// disables the feature entirely.
    verification: Option<VerificationConfig>,
    /// Set whenever a `write_file`/`edit_file` call succeeds; cleared only
    /// by a passing verification run. Deliberately spans turn boundaries —
    /// a turn pausing mid-edit (Phase 12 Part A) must not let unverified
    /// edits be silently forgotten by whichever turn continues it.
    unverified_edits: bool,
    /// How many (edit, re-verify) cycles have failed since edits last
    /// became unverified; reset to 0 both on a pass and on exhaustion (see
    /// `run_turn_inner`'s completion check) so the feature never silently
    /// disables itself for the rest of the session.
    verify_retries: u32,
    /// The checkpoint ref taken right before the first unverified edit of
    /// the current batch — the rewind target if verification exhausts its
    /// retries in autonomous mode. `None` when there's no unverified batch
    /// in flight, or once it's resolved (pass or rewind). Interactive mode
    /// never reads this field. See ROADMAP.md Phase 11c.
    pre_experiment_ref: Option<String>,
    events_tx: UnboundedSender<AgentEvent>,
}

impl Agent {
    // `AgentConfig` already groups the scalar knobs (see its doc comment);
    // the remaining params are distinct collaborators (backend, executor,
    // shared state handles, event sink) that don't share a natural group,
    // and `autonomous_mode` (Phase 11c) tips this over clippy's default
    // threshold of 7.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        llm: std::sync::Arc<dyn LlmBackend>,
        executor: ToolExecutor,
        system_prompt: impl Into<String>,
        config: AgentConfig,
        tasks: Arc<Mutex<Vec<Task>>>,
        plan_mode: PlanMode,
        autonomous_mode: AutonomousMode,
        events_tx: UnboundedSender<AgentEvent>,
    ) -> Self {
        Self {
            llm,
            executor,
            system_prompt: system_prompt.into(),
            history: Vec::new(),
            // A misconfigured 0 would otherwise make every turn a silent
            // no-op (the tool-loop range would simply never iterate).
            max_tool_iterations: config.max_tool_iterations.max(1),
            // A 0 here would make the budget indicator meaningless and
            // trigger compaction constantly; clamp to a floor.
            context_limit: config.context_tokens.max(1),
            last_request_chars: None,
            chars_per_token: DEFAULT_CHARS_PER_TOKEN,
            history_truncated: false,
            tasks,
            session_path: None,
            plan_mode,
            autonomous_mode,
            last_turn_paused: false,
            repo_map: None,
            repo_map_text: None,
            edit_format: config.edit_format,
            synthetic_seq: 0,
            council: None,
            verification: None,
            unverified_edits: false,
            verify_retries: 0,
            pre_experiment_ref: None,
            events_tx,
        }
    }

    /// Whether the most recent `run_turn` call paused (Phase 12A's
    /// `AgentEvent::TurnPaused`) rather than completing normally. See the
    /// `last_turn_paused` field's doc comment for why this exists.
    pub fn last_turn_paused(&self) -> bool {
        self.last_turn_paused
    }

    /// Enables `/council` (Phase 11a). The caller builds the seats — each
    /// is any `LlmBackend`, typically Ollama-swapped models alongside the
    /// resident daily driver.
    pub fn set_council(&mut self, council: crate::council::Council) {
        self.council = Some(council);
    }

    /// Enables enforced verification (Phase 12 Part B): `command_name` must
    /// name an entry the `run_command` tool was built with (the caller's
    /// responsibility to keep in sync with `[[permissions.allowed_commands]]`
    /// — see `main.rs`'s startup validation warning). `max_retries` is
    /// clamped to a minimum of 1, matching the same "a misconfigured 0 would
    /// otherwise be silently wrong" reasoning as `max_tool_iterations`.
    pub fn set_verification(&mut self, command_name: String, max_retries: u32) {
        self.verification = Some(VerificationConfig {
            command_name,
            max_retries: max_retries.max(1),
        });
    }

    /// Enables the repository map: rendered per turn, appended to the
    /// system prompt within `budget_tokens`.
    pub fn set_repo_map(&mut self, map: Arc<RepoMap>, budget_tokens: u32) {
        self.repo_map = Some((map, budget_tokens));
    }

    /// Re-renders the map off the async runtime. Best-effort: a failure
    /// just means this turn goes without a map.
    async fn refresh_repo_map(&mut self) {
        let Some((map, budget)) = &self.repo_map else {
            return;
        };
        let map = Arc::clone(map);
        let budget = *budget;
        self.repo_map_text = match tokio::task::spawn_blocking(move || map.render(budget)).await {
            Ok(text) => text,
            Err(err) => {
                tracing::warn!(error = %err, "repo map rendering panicked; continuing without it");
                None
            }
        };
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
        let mut system = self.system_prompt.clone();
        if self.history_truncated {
            system.push_str(
                "\n\n(Note: earlier parts of this conversation were truncated to fit the \
                 model's context window. Ask the user to restate anything you're missing.)",
            );
        }
        if self.plan_mode.active() {
            system.push_str("\n\n");
            system.push_str(PLAN_MODE_PROMPT);
        } else if self.edit_format == EditFormat::Prompted {
            // Not taught during plan mode — the plan note already forbids
            // modifications, and teaching an edit syntax at the same time
            // would just invite blocks the gate then has to bounce.
            system.push_str("\n\n");
            system.push_str(EDIT_FORMAT_PROMPT);
        }
        // Not taught during plan mode — no edits happen there, so
        // verification can never actually fire (see `run_turn_inner`).
        if self.verification.is_some() && !self.plan_mode.active() {
            system.push_str("\n\n");
            system.push_str(VERIFICATION_PROMPT);
        }
        if self.autonomous_mode.active() {
            system.push_str("\n\n");
            system.push_str(AUTONOMOUS_PROMPT);
        }
        if let Some(map) = &self.repo_map_text {
            system.push_str("\n\n");
            system.push_str(map);
        }
        let mut messages = Vec::with_capacity(self.history.len() + 1);
        messages.push(Message::text(Role::System, system));
        messages.extend(self.history.iter().cloned());
        messages
    }

    /// Char count feeding both the size estimate and the calibration pair —
    /// the two must use the same measure so systematic omissions (e.g.
    /// tool-definition tokens) cancel out in the ratio. Includes the
    /// rendered repo map, which is real prompt weight every request.
    fn prompt_chars(&self) -> usize {
        message_chars(&self.system_prompt, &self.history)
            + self.repo_map_text.as_ref().map_or(0, |m| m.chars().count())
    }

    /// Rough token estimate for the current prompt, using the
    /// self-calibrated `chars_per_token`.
    fn estimate_prompt_tokens(&self) -> u32 {
        (self.prompt_chars() as f64 / self.chars_per_token).ceil() as u32
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

    /// Synthesizes and dispatches the configured `[verification] command`
    /// via the `run_command` tool — reusing its existing trust tier (the
    /// name must already be pre-approved through
    /// `[[permissions.allowed_commands]]`) rather than inventing a new one
    /// — pushing both the synthetic call and its result into history
    /// exactly like a normal round-trip: a synthetic assistant message
    /// carrying the one call (needed because OpenAI-compatible wire format
    /// requires every `Role::Tool` result to correspond to a preceding
    /// assistant `tool_calls` entry — the same reason prompted-mode
    /// SEARCH/REPLACE blocks get synthesized into real calls), then the
    /// dispatched result. Returns whether the command reported success.
    /// See ROADMAP.md Phase 12 Part B.
    async fn run_auto_verification(
        &mut self,
        command_name: &str,
        cwd: &Path,
        cancellation: &CancellationToken,
    ) -> bool {
        self.synthetic_seq += 1;
        let call = ToolCall {
            id: ToolCallId(format!("auto-verify-{}", self.synthetic_seq)),
            name: "run_command".to_string(),
            arguments: serde_json::json!({ "command": command_name }),
            source: ToolCallSource::AutoVerification,
        };
        self.emit(AgentEvent::ToolCallDetected(call.clone()));
        self.history.push(Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolCall(call.clone())],
            tool_call_id: None,
        });

        let result = self
            .executor
            .dispatch(call, cwd, cancellation.clone())
            .await;
        self.emit(AgentEvent::ToolResult(result.clone()));
        let passed = matches!(&result.output, ToolOutput::Ok(text) if command_reported_success(text));
        self.history.push(Message {
            role: Role::Tool,
            tool_call_id: Some(result.call_id.clone()),
            content: vec![ContentBlock::ToolResult(result)],
        });
        passed
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
        // Reset before every turn: a stale `true` from a previous paused
        // turn must not be misread as "this turn paused too" if this turn
        // takes a different path (e.g. a /council command, which never
        // pauses).
        self.last_turn_paused = false;
        // Commands are intercepted here, before the input can enter LLM
        // history — the raw `/council …` text is an instruction to aivyx,
        // not part of the conversation the model should see.
        let result = match crate::council::parse_command(&user_input) {
            Some(subject) => {
                let subject = subject.to_string();
                self.run_council_turn(&subject, cancellation).await
            }
            None => self.run_turn_inner(user_input, cwd, cancellation).await,
        };
        self.persist();
        result
    }

    /// Runs `/council`: convenes the configured council on `subject_arg`
    /// (or on the last assistant message when bare), streams the whole
    /// deliberation as `CouncilNote` events, and pushes only the chairman's
    /// synthesis to history. Council failures are transcript notes, not
    /// `AgentError`s — a failed second opinion must not look like a failed
    /// turn.
    async fn run_council_turn(
        &mut self,
        subject_arg: &str,
        cancellation: CancellationToken,
    ) -> Result<(), AgentError> {
        let Some(council) = &self.council else {
            self.emit(AgentEvent::CouncilNote(
                "no council is configured — add at least two [council] members and a \
                 chairman to config.toml (see the README's Council section)"
                    .to_string(),
            ));
            self.emit(AgentEvent::TurnComplete);
            return Ok(());
        };

        let subject = if subject_arg.is_empty() {
            match crate::council::last_assistant_text(&self.history) {
                Some(text) => text,
                None => {
                    self.emit(AgentEvent::CouncilNote(
                        "bare /council reviews the last assistant message, but there \
                         isn't one yet — use /council <question> instead"
                            .to_string(),
                    ));
                    self.emit(AgentEvent::TurnComplete);
                    return Ok(());
                }
            }
        } else {
            subject_arg.to_string()
        };

        let budget_chars = (council.tail_budget_tokens as f64 * self.chars_per_token) as usize;
        let digest = crate::council::tail_digest(&self.history, budget_chars);

        let entry =
            crate::council::convene(council, &subject, digest, &self.events_tx, &cancellation)
                .await;
        if let Some(message) = entry {
            self.history.push(message);
        }
        self.emit(AgentEvent::TurnComplete);
        Ok(())
    }

    /// Runs one user turn: sends the request, streams the response live via
    /// `AgentEvent`s, executes any tool calls the model makes, and repeats
    /// until the model produces a final answer with no further tool calls —
    /// or `max_tool_iterations` is hit, in which case the turn pauses
    /// (`AgentEvent::TurnPaused`) rather than failing; nothing dispatched so
    /// far is lost, and a further `run_turn` call continues from here.
    async fn run_turn_inner(
        &mut self,
        user_input: String,
        cwd: &Path,
        cancellation: CancellationToken,
    ) -> Result<(), AgentError> {
        self.history.push(Message::text(Role::User, user_input));
        // Once per turn, not per iteration: within a turn the map rarely
        // changes materially, and re-walking on every tool round-trip would
        // add latency exactly where slow local models already hurt.
        self.refresh_repo_map().await;

        for iteration in 1..=self.max_tool_iterations {
            if cancellation.is_cancelled() {
                break;
            }

            self.compact_if_needed();
            // Recorded here (not from `assemble_messages`) so it pairs with
            // the estimator's own char-count for a consistent calibration.
            self.last_request_chars = Some(self.prompt_chars());

            let request = ChatRequest {
                messages: self.assemble_messages(),
                // Re-evaluated every iteration, not once per turn, so a
                // mid-turn toggle takes effect on the very next request.
                tools: {
                    let mut tools = if self.plan_mode.active() {
                        self.executor.plan_definitions()
                    } else {
                        self.executor.definitions()
                    };
                    if self.edit_format == EditFormat::Prompted {
                        tools.retain(|d| !PROMPTED_EDIT_HIDDEN_TOOLS.contains(&d.name.as_str()));
                    }
                    if self.autonomous_mode.active() {
                        tools.retain(|d| !AUTONOMOUS_HIDDEN_TOOLS.contains(&d.name.as_str()));
                    }
                    tools
                },
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
                        "response was truncated (hit the model's output limit) before it \
                         finished. If this happens early in a response, the server's actual \
                         context window is probably smaller than backend.context_tokens — \
                         Ollama defaults to 4096 unless the model sets num_ctx or the server \
                         sets OLLAMA_CONTEXT_LENGTH; a reasoning model's thinking phase can \
                         consume the whole remainder invisibly."
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

            // In prompted edit mode, the assistant's plain text may carry
            // SEARCH/REPLACE blocks: turn each into an ordinary tool call
            // (source: TextFallback) so it flows through the exact same
            // gate / diff-preview / checkpoint path as a native call.
            // Malformed blocks become a call plus an error-shaped result so
            // the model gets precise corrective feedback in-history without
            // breaking the call/result balance invariant.
            let mut malformed_blocks: Vec<(ToolCall, String)> = Vec::new();
            if self.edit_format == EditFormat::Prompted {
                for parsed in edit_blocks::parse_edit_blocks(&assistant_text) {
                    self.synthetic_seq += 1;
                    let id = ToolCallId(format!("prompted-edit-{}", self.synthetic_seq));
                    match parsed {
                        BlockParse::Ok(block) => {
                            let (name, arguments) = if block.search.is_empty() {
                                (
                                    "write_file",
                                    serde_json::json!({
                                        "path": block.path,
                                        "content": block.replace,
                                    }),
                                )
                            } else {
                                (
                                    "edit_file",
                                    serde_json::json!({
                                        "path": block.path,
                                        "old_string": block.search,
                                        "new_string": block.replace,
                                    }),
                                )
                            };
                            let call = ToolCall {
                                id,
                                name: name.to_string(),
                                arguments,
                                source: ToolCallSource::TextFallback,
                            };
                            self.emit(AgentEvent::ToolCallDetected(call.clone()));
                            tool_calls.push(call);
                        }
                        BlockParse::Malformed(message) => {
                            malformed_blocks.push((
                                ToolCall {
                                    id,
                                    name: "edit_file".to_string(),
                                    arguments: serde_json::json!({}),
                                    source: ToolCallSource::TextFallback,
                                },
                                message,
                            ));
                        }
                    }
                }
            }

            let mut assistant_content = Vec::new();
            if !assistant_text.is_empty() {
                assistant_content.push(ContentBlock::Text(assistant_text));
            }
            for call in &tool_calls {
                assistant_content.push(ContentBlock::ToolCall(call.clone()));
            }
            for (call, _) in &malformed_blocks {
                assistant_content.push(ContentBlock::ToolCall(call.clone()));
            }
            if !assistant_content.is_empty() {
                self.history.push(Message {
                    role: Role::Assistant,
                    content: assistant_content,
                    tool_call_id: None,
                });
            }

            let had_malformed_blocks = !malformed_blocks.is_empty();
            for (call, message) in malformed_blocks {
                self.record_skipped_tool_result(
                    call,
                    &format!(
                        "malformed SEARCH/REPLACE block ({message}) — re-emit the complete \
                         block: path line, <<<<<<< SEARCH, old text, =======, new text, \
                         >>>>>>> REPLACE"
                    ),
                );
            }

            if tool_calls.is_empty() {
                // Nothing to execute — but a malformed block means the
                // model tried to act: keep the loop going so the feedback
                // just recorded can drive a corrected retry.
                if had_malformed_blocks {
                    continue;
                }

                if self.unverified_edits
                    && let Some(verification) = self.verification.clone()
                {
                    if self.verify_retries < verification.max_retries {
                        self.verify_retries += 1;
                        let passed = self
                            .run_auto_verification(&verification.command_name, cwd, &cancellation)
                            .await;
                        if passed {
                            self.unverified_edits = false;
                            self.verify_retries = 0;
                            self.pre_experiment_ref = None;
                            self.emit(AgentEvent::TurnComplete);
                            return Ok(());
                        }
                        // Failed: let the model see the result and try
                        // again next iteration instead of ending here.
                        continue;
                    }
                    // Retries exhausted for this round of edits. Interactive
                    // mode: end the turn anyway (never block TurnComplete —
                    // the Phase 12 Part B "loud notice, not a block"
                    // decision) with the worktree left as-is for a human to
                    // inspect. Autonomous mode: no human is coming, so
                    // additionally discard — rewind to the pre-experiment
                    // checkpoint (Phase 11c) so the loop's next attempt
                    // starts from known-good state instead of building on
                    // top of a broken one.
                    self.verify_retries = 0;
                    if self.autonomous_mode.active()
                        && let Some(pre_experiment_ref) = self.pre_experiment_ref.take()
                    {
                        match self
                            .executor
                            .restore_to_checkpoint(&pre_experiment_ref, &cancellation)
                            .await
                        {
                            Ok(()) => {
                                self.unverified_edits = false;
                                self.emit(AgentEvent::Error(format!(
                                    "verification (`{}`) still failing after {} attempt(s) — \
                                     discarded this round of edits and restored the worktree to \
                                     the pre-experiment checkpoint.",
                                    verification.command_name, verification.max_retries
                                )));
                            }
                            Err(err) => {
                                // Rewind itself failed (e.g. a git error) —
                                // fall back to interactive mode's behavior:
                                // leave the state as-is and say so loudly,
                                // rather than silently pretending the
                                // discard happened.
                                self.emit(AgentEvent::Error(format!(
                                    "verification (`{}`) still failing after {} attempt(s), and \
                                     the automatic discard/rewind itself failed ({err}) — ending \
                                     the turn with the worktree left as-is. The worktree was \
                                     checkpointed before each edit; `git log \
                                     refs/aivyx/checkpoints/` to inspect or rewind manually.",
                                    verification.command_name, verification.max_retries
                                )));
                            }
                        }
                    } else {
                        self.emit(AgentEvent::Error(format!(
                            "verification (`{}`) still failing after {} attempt(s) — ending the \
                             turn anyway. The worktree was checkpointed before each edit; `git \
                             log refs/aivyx/checkpoints/` to inspect or rewind.",
                            verification.command_name, verification.max_retries
                        )));
                    }
                }

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

                // Captured before the move below — feeds
                // `unverified_edits` for the enforced-verification check at
                // the top of this loop (Phase 12 Part B). Reuses the same
                // name list prompted mode already hides edit tools behind,
                // rather than a second hardcoded pair.
                let is_edit_call = PROMPTED_EDIT_HIDDEN_TOOLS.contains(&call.name.as_str());
                let was_already_unverified = self.unverified_edits;
                let result = self
                    .executor
                    .dispatch(call, cwd, cancellation.clone())
                    .await;
                if is_edit_call && matches!(result.output, ToolOutput::Ok(_)) {
                    self.unverified_edits = true;
                    if self.autonomous_mode.active() && !was_already_unverified {
                        // First edit of a new batch: the checkpoint dispatch
                        // just took (ToolExecutor::dispatch checkpoints
                        // before every mutating call) is exactly "state
                        // right before this edit" — remember it as the
                        // rewind target if this batch's verification never
                        // passes.
                        self.pre_experiment_ref =
                            self.executor.latest_checkpoint_ref(&cancellation).await;
                    }
                }
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
                // Hitting the cap while the model was still actively
                // dispatching tool calls (every call up to and including
                // this iteration already has a matching result in
                // history) is a pause, not a failure — see
                // `AgentEvent::TurnPaused`'s doc comment and ROADMAP.md
                // Phase 12 Part A.
                self.last_turn_paused = true;
                self.emit(AgentEvent::TurnPaused(format!(
                    "reached the {}-round-trip cap for this turn while still working — \
                     send another message to continue; nothing has been lost.",
                    self.max_tool_iterations
                )));
                return Ok(());
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

/// Parses `run_command`'s formatted output (see
/// `aivyx_tools::process::format_output`) for its pass/fail verdict. A
/// non-zero exit is normal `Ok` output for that tool — a failing test run
/// is expected, informative verification-loop data, not a tool-level error
/// — so this string check on the embedded verdict marker is the only signal
/// available without changing that tool's contract. Coupled to the exact
/// wording `format_output` emits; keep the two in sync if either changes.
fn command_reported_success(output: &str) -> bool {
    output.contains("exit status:") && output.contains("(success)")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::time::Duration;

    use aivyx_sandbox::{
        ActionKind, AutonomousMode, ExecutionConfiner, NoopConfiner, PermissionDecision,
        PermissionGate, PermissionRequest, PermissionTarget, PlanMode,
    };
    use aivyx_tools::{CommandSpec, RunCommandTool, Tool, ToolError, ToolExecutionContext, ToolRegistry};
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

    fn build_agent_with_config(
        responses: Vec<Vec<StreamEvent>>,
        registry: ToolRegistry,
        config: AgentConfig,
    ) -> (Agent, UnboundedReceiver<AgentEvent>, Arc<MockBackend>) {
        let (tx, rx) = unbounded_channel();
        let mock = Arc::new(MockBackend::new(responses));
        let llm: std::sync::Arc<dyn LlmBackend> = mock.clone();
        let gate: Arc<dyn PermissionGate> = Arc::new(AllowAllGate);
        let confiner: Arc<dyn ExecutionConfiner> = Arc::new(NoopConfiner);
        let executor = ToolExecutor::new(registry, gate, confiner);
        let agent = Agent::new(
            llm,
            executor,
            "system",
            config,
            Arc::default(),
            PlanMode::new(),
            AutonomousMode::new(),
            tx,
        );
        (agent, rx, mock)
    }

    fn build_agent(
        responses: Vec<Vec<StreamEvent>>,
        registry: ToolRegistry,
        max_iters: u32,
    ) -> (Agent, UnboundedReceiver<AgentEvent>, Arc<MockBackend>) {
        build_agent_with_config(
            responses,
            registry,
            AgentConfig {
                max_tool_iterations: max_iters,
                ..Default::default()
            },
        )
    }

    fn prompted_config() -> AgentConfig {
        AgentConfig {
            edit_format: EditFormat::Prompted,
            ..Default::default()
        }
    }

    fn text_response(text: &str) -> Vec<StreamEvent> {
        vec![
            StreamEvent::TextDelta(text.to_string()),
            StreamEvent::Done {
                finish_reason: FinishReason::Stop,
            },
        ]
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
    async fn plan_mode_filters_tools_and_annotates_the_system_prompt_per_request() {
        // Two turns against the same agent: one with plan mode on, one after
        // toggling it off — the request the backend actually receives must
        // flip both the tool list and the system-prompt note, proving the
        // flag is consulted per-request rather than latched at construction.
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(aivyx_tools::ReadFileTool));
        registry.register(Arc::new(aivyx_tools::WriteFileTool));

        let (tx, _rx) = unbounded_channel();
        let stop = || {
            vec![StreamEvent::Done {
                finish_reason: FinishReason::Stop,
            }]
        };
        let mock = Arc::new(MockBackend::new(vec![stop(), stop()]));
        let llm: Arc<dyn LlmBackend> = mock.clone();
        let gate: Arc<dyn PermissionGate> = Arc::new(AllowAllGate);
        let confiner: Arc<dyn ExecutionConfiner> = Arc::new(NoopConfiner);
        let executor = ToolExecutor::new(registry, gate, confiner);
        let plan_mode = PlanMode::new();
        plan_mode.set_active(true);
        let mut agent = Agent::new(
            llm,
            executor,
            "system",
            AgentConfig {
                max_tool_iterations: 10,
                ..Default::default()
            },
            Arc::default(),
            plan_mode.clone(),
            AutonomousMode::new(),
            tx,
        );

        agent
            .run_turn("plan".to_string(), Path::new("."), CancellationToken::new())
            .await
            .unwrap();
        plan_mode.set_active(false);
        agent
            .run_turn("act".to_string(), Path::new("."), CancellationToken::new())
            .await
            .unwrap();

        let received = mock.received.lock().unwrap();
        let plan_tools: Vec<&str> = received[0].tools.iter().map(|d| d.name.as_str()).collect();
        let act_tools: Vec<&str> = received[1].tools.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(plan_tools, vec!["read_file"]);
        assert_eq!(act_tools, vec!["read_file", "write_file"]);

        let plan_system = received[0].messages[0].text_content();
        let act_system = received[1].messages[0].text_content();
        assert!(plan_system.contains("PLAN MODE"));
        assert!(!act_system.contains("PLAN MODE"));
    }

    #[tokio::test]
    async fn prompted_blocks_apply_through_the_normal_tool_path() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("target.rs"),
            "fn old_name() {}\nfn other() {}\n",
        )
        .unwrap();
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(aivyx_tools::EditFileTool));

        let block = "target.rs\n<<<<<<< SEARCH\nfn old_name() {}\n=======\nfn renamed() {}\n>>>>>>> REPLACE";
        let (mut agent, _rx, mock) = build_agent_with_config(
            vec![text_response(block), text_response("done")],
            registry,
            prompted_config(),
        );

        agent
            .run_turn(
                "rename it".to_string(),
                dir.path(),
                CancellationToken::new(),
            )
            .await
            .unwrap();

        let content = std::fs::read_to_string(dir.path().join("target.rs")).unwrap();
        assert!(
            content.contains("fn renamed()"),
            "edit not applied: {content}"
        );
        assert!(content.contains("fn other()"));
        // The synthesized call is in history, marked TextFallback, balanced
        // by its result — and the loop continued for a second round-trip.
        assert_eq!(count_tool_calls(&agent.history), 1);
        assert_eq!(count_tool_results(&agent.history), 1);
        let synthetic = agent
            .history
            .iter()
            .flat_map(|m| &m.content)
            .find_map(|b| match b {
                ContentBlock::ToolCall(c) => Some(c),
                _ => None,
            })
            .unwrap();
        assert_eq!(synthetic.source, ToolCallSource::TextFallback);
        assert_eq!(mock.received.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn empty_search_block_creates_a_new_file_via_write_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(aivyx_tools::WriteFileTool));

        let block = "fresh.txt\n<<<<<<< SEARCH\n=======\nhello world\n>>>>>>> REPLACE";
        let (mut agent, _rx, _) = build_agent_with_config(
            vec![text_response(block), text_response("done")],
            registry,
            prompted_config(),
        );

        agent
            .run_turn(
                "create it".to_string(),
                dir.path(),
                CancellationToken::new(),
            )
            .await
            .unwrap();

        assert_eq!(
            std::fs::read_to_string(dir.path().join("fresh.txt")).unwrap(),
            "hello world\n"
        );
    }

    #[tokio::test]
    async fn malformed_block_feeds_an_error_back_and_the_turn_continues() {
        let broken = "target.rs\n<<<<<<< SEARCH\nfn a() {}\n=======\nfn b() {}\n"; // no terminator
        let (mut agent, _rx, mock) = build_agent_with_config(
            vec![text_response(broken), text_response("understood")],
            ToolRegistry::new(),
            prompted_config(),
        );

        agent
            .run_turn("edit".to_string(), Path::new("."), CancellationToken::new())
            .await
            .unwrap();

        assert_eq!(
            count_denied_containing(&agent.history, "malformed SEARCH/REPLACE"),
            1
        );
        // The feedback drove a second round-trip instead of ending the turn.
        assert_eq!(mock.received.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn native_mode_never_parses_block_syntax_out_of_text() {
        // A model quoting the format in conversation (or a file containing
        // markers being discussed) must not trigger edits in native mode.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("target.rs"), "fn old_name() {}\n").unwrap();
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(aivyx_tools::EditFileTool));

        let block = "target.rs\n<<<<<<< SEARCH\nfn old_name() {}\n=======\nfn changed() {}\n>>>>>>> REPLACE";
        let (mut agent, _rx, mock) =
            build_agent_with_config(vec![text_response(block)], registry, AgentConfig::default());

        agent
            .run_turn("hi".to_string(), dir.path(), CancellationToken::new())
            .await
            .unwrap();

        assert_eq!(count_tool_calls(&agent.history), 0);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("target.rs")).unwrap(),
            "fn old_name() {}\n"
        );
        assert_eq!(mock.received.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn prompted_mode_hides_edit_tools_and_teaches_the_block_format() {
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(aivyx_tools::ReadFileTool));
        registry.register(Arc::new(aivyx_tools::EditFileTool));
        registry.register(Arc::new(aivyx_tools::WriteFileTool));

        let (mut agent, _rx, mock) =
            build_agent_with_config(vec![text_response("hello")], registry, prompted_config());

        agent
            .run_turn("hi".to_string(), Path::new("."), CancellationToken::new())
            .await
            .unwrap();

        let received = mock.received.lock().unwrap();
        let tool_names: Vec<&str> = received[0].tools.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(tool_names, vec!["read_file"]);
        assert!(
            received[0].messages[0]
                .text_content()
                .contains("SEARCH/REPLACE")
        );
    }

    #[tokio::test]
    async fn repo_map_is_rendered_into_the_system_prompt_and_counted_by_the_estimator() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("widget.rs"),
            "pub fn extremely_distinctive_symbol() {}\n",
        )
        .unwrap();

        let (mut agent, _rx, mock) = build_agent(
            vec![vec![StreamEvent::Done {
                finish_reason: FinishReason::Stop,
            }]],
            ToolRegistry::new(),
            10,
        );
        agent.set_repo_map(
            Arc::new(aivyx_repomap::RepoMap::new(
                dir.path().to_path_buf(),
                vec![],
            )),
            1000,
        );
        let chars_without_map = agent.prompt_chars();

        agent
            .run_turn("hi".to_string(), Path::new("."), CancellationToken::new())
            .await
            .unwrap();

        let received = mock.received.lock().unwrap();
        let system = received[0].messages[0].text_content();
        assert!(system.contains("Repository map"));
        assert!(system.contains("extremely_distinctive_symbol"));
        // The estimator must see the map's weight, or compaction would run
        // blind to a block that's present in every request.
        assert!(agent.prompt_chars() > chars_without_map + 50);
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
        let mut agent = Agent::new(
            llm,
            executor,
            "system",
            AgentConfig {
                max_tool_iterations: 10,
                ..Default::default()
            },
            Arc::clone(&tasks),
            PlanMode::new(),
            AutonomousMode::new(),
            tx,
        );

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
    async fn max_tool_iterations_pauses_the_turn_without_losing_history() {
        // Both responses request a tool and never give a final answer, so the
        // loop must stop at the iteration cap — as a pause (ROADMAP.md Phase
        // 12 Part A), not an error: the turn itself must still return `Ok`.
        let looping = || {
            vec![
                StreamEvent::ToolCallComplete(tool_call("c", "read_file")),
                StreamEvent::Done {
                    finish_reason: FinishReason::ToolCalls,
                },
            ]
        };
        let (mut agent, mut rx, _) = build_agent(vec![looping(), looping()], ToolRegistry::new(), 2);

        agent
            .run_turn("go".to_string(), Path::new("."), CancellationToken::new())
            .await
            .unwrap();

        let events = drain(&mut rx);
        assert!(
            events
                .iter()
                .any(|e| matches!(e, AgentEvent::TurnPaused(msg) if msg.contains("2-round-trip"))),
            "expected a TurnPaused event, got {events:?}"
        );
        assert!(
            !events.iter().any(|e| matches!(e, AgentEvent::TurnComplete)),
            "a paused turn must not also claim completion"
        );
        assert!(
            !events.iter().any(|e| matches!(e, AgentEvent::Error(_))),
            "hitting the cap mid-work is a pause, not an error"
        );
        // Every dispatched tool call up to the cap still has a matching
        // result — nothing lost by pausing instead of failing.
        assert_eq!(count_tool_calls(&agent.history), 2);
        assert_eq!(count_tool_results(&agent.history), 2);
    }

    #[tokio::test]
    async fn a_paused_turn_resumes_cleanly_from_a_follow_up_message() {
        // After pausing on the cap, the agent's history/session already hold
        // everything dispatched so far; a plain follow-up `run_turn` call
        // (exactly what an interactive user would send next) must continue
        // the same conversation rather than starting over or erroring.
        let looping = vec![
            StreamEvent::ToolCallComplete(tool_call("c", "read_file")),
            StreamEvent::Done {
                finish_reason: FinishReason::ToolCalls,
            },
        ];
        let (mut agent, mut rx, mock) =
            build_agent(vec![looping, text_response("done")], ToolRegistry::new(), 1);

        agent
            .run_turn("go".to_string(), Path::new("."), CancellationToken::new())
            .await
            .unwrap();
        assert!(
            drain(&mut rx)
                .iter()
                .any(|e| matches!(e, AgentEvent::TurnPaused(_)))
        );

        agent
            .run_turn(
                "continue".to_string(),
                Path::new("."),
                CancellationToken::new(),
            )
            .await
            .unwrap();

        assert!(
            drain(&mut rx)
                .iter()
                .any(|e| matches!(e, AgentEvent::TurnComplete))
        );
        assert_eq!(agent.history.last().unwrap().text_content(), "done");
        // Both round-trips actually reached the backend — resuming is a
        // real continuation, not a silently-dropped no-op.
        assert_eq!(mock.received.lock().unwrap().len(), 2);
    }

    // ----- autonomous mode (Phase 11c) -----

    fn build_autonomous_agent(
        responses: Vec<Vec<StreamEvent>>,
        registry: ToolRegistry,
    ) -> (Agent, UnboundedReceiver<AgentEvent>, Arc<MockBackend>, AutonomousMode) {
        let (tx, rx) = unbounded_channel();
        let mock = Arc::new(MockBackend::new(responses));
        let llm: Arc<dyn LlmBackend> = mock.clone();
        let gate: Arc<dyn PermissionGate> = Arc::new(AllowAllGate);
        let confiner: Arc<dyn ExecutionConfiner> = Arc::new(NoopConfiner);
        let executor = ToolExecutor::new(registry, gate, confiner);
        let autonomous_mode = AutonomousMode::new();
        autonomous_mode.set_active(true);
        let agent = Agent::new(
            llm,
            executor,
            "system",
            AgentConfig {
                max_tool_iterations: 10,
                ..Default::default()
            },
            Arc::default(),
            PlanMode::new(),
            autonomous_mode.clone(),
            tx,
        );
        (agent, rx, mock, autonomous_mode)
    }

    #[tokio::test]
    async fn last_turn_paused_reflects_the_most_recent_turn_outcome() {
        // build_autonomous_agent's max_tool_iterations (10) is too high to
        // pause on a single tool call, so this test builds its own agent
        // directly with max_tool_iterations: 1 instead of using that helper.
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(aivyx_tools::ReadFileTool));
        let (tx, _rx2) = unbounded_channel();
        let mock = Arc::new(MockBackend::new(vec![vec![
            StreamEvent::ToolCallComplete(tool_call("c", "read_file")),
            StreamEvent::Done {
                finish_reason: FinishReason::ToolCalls,
            },
        ]]));
        let llm: Arc<dyn LlmBackend> = mock.clone();
        let gate: Arc<dyn PermissionGate> = Arc::new(AllowAllGate);
        let confiner: Arc<dyn ExecutionConfiner> = Arc::new(NoopConfiner);
        let executor = ToolExecutor::new(registry, gate, confiner);
        let mut agent = Agent::new(
            llm,
            executor,
            "system",
            AgentConfig {
                max_tool_iterations: 1,
                ..Default::default()
            },
            Arc::default(),
            PlanMode::new(),
            AutonomousMode::new(),
            tx,
        );

        assert!(!agent.last_turn_paused(), "false before any turn has run");
        agent
            .run_turn("go".to_string(), Path::new("."), CancellationToken::new())
            .await
            .unwrap();
        assert!(agent.last_turn_paused(), "the 1-iteration cap must have paused this turn");

        agent
            .run_turn(
                "continue".to_string(),
                Path::new("."),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        // Second call exhausts the mock queue -> a Stop response with no
        // tool calls -> TurnComplete, not another pause.
        assert!(!agent.last_turn_paused(), "a normal completion must clear the flag");
    }

    #[tokio::test]
    async fn autonomous_mode_hides_run_shell_and_git_commit() {
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(aivyx_tools::ReadFileTool));
        registry.register(Arc::new(aivyx_tools::RunShellTool));
        registry.register(Arc::new(aivyx_tools::GitCommitTool::new(vec![])));
        registry.register(Arc::new(aivyx_tools::GitReadTool::new(vec![])));

        let (mut agent, _rx, mock, _) =
            build_autonomous_agent(vec![text_response("hi")], registry);

        agent
            .run_turn("go".to_string(), Path::new("."), CancellationToken::new())
            .await
            .unwrap();

        let received = mock.received.lock().unwrap();
        let tool_names: Vec<&str> = received[0].tools.iter().map(|d| d.name.as_str()).collect();
        assert!(tool_names.contains(&"read_file"));
        assert!(tool_names.contains(&"git_read"));
        assert!(!tool_names.contains(&"run_shell"), "run_shell must be hidden");
        assert!(!tool_names.contains(&"git_commit"), "git_commit must be hidden");
    }

    #[tokio::test]
    async fn autonomous_mode_appends_the_autonomous_prompt_note() {
        let (mut agent, _rx, mock, _) =
            build_autonomous_agent(vec![text_response("hi")], ToolRegistry::new());

        agent
            .run_turn("go".to_string(), Path::new("."), CancellationToken::new())
            .await
            .unwrap();

        let received = mock.received.lock().unwrap();
        let system = received[0].messages[0].text_content();
        assert!(system.contains("unattended"), "system prompt: {system}");
    }

    async fn init_git_repo(dir: &Path) {
        for argv in [
            vec!["init", "-q", "-b", "main"],
            vec!["config", "user.name", "test"],
            vec!["config", "user.email", "test@test.invalid"],
        ] {
            tokio::process::Command::new("git")
                .args(&argv)
                .current_dir(dir)
                .output()
                .await
                .unwrap();
        }
        std::fs::write(dir.join("tracked.txt"), "v1\n").unwrap();
        tokio::process::Command::new("git")
            .args(["add", "-A"])
            .current_dir(dir)
            .output()
            .await
            .unwrap();
        tokio::process::Command::new("git")
            .args(["commit", "-q", "-m", "initial"])
            .current_dir(dir)
            .output()
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn autonomous_mode_discards_and_rewinds_on_exhausted_verification() {
        let dir = tempfile::tempdir().unwrap();
        // Real git repo, matching the checkpoint tests' own fixture style —
        // the discard path exercises real GitCheckpointer plumbing, not a
        // mock, since that's exactly the piece this test must prove works.
        init_git_repo(dir.path()).await;

        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(aivyx_tools::WriteFileTool));
        registry.register(Arc::new(RunCommandTool::new(vec![CommandSpec {
            name: "verify".to_string(),
            program: "sh".to_string(),
            args: vec!["-c".to_string(), "exit 1".to_string()], // always fails
            timeout: Duration::from_secs(5),
        }])));

        let write_call = vec![
            StreamEvent::ToolCallComplete(ToolCall {
                id: ToolCallId("c1".to_string()),
                name: "write_file".to_string(),
                arguments: serde_json::json!({ "path": "new.txt", "content": "hi\n" }),
                source: ToolCallSource::Native,
            }),
            StreamEvent::Done {
                finish_reason: FinishReason::ToolCalls,
            },
        ];
        let (tx, _rx) = unbounded_channel();
        let mock = Arc::new(MockBackend::new(vec![
            write_call,
            text_response("done"),
            text_response("still trying"),
        ]));
        let llm: Arc<dyn LlmBackend> = mock.clone();
        let gate: Arc<dyn PermissionGate> = Arc::new(AllowAllGate);
        let confiner: Arc<dyn ExecutionConfiner> = Arc::new(NoopConfiner);
        let mut executor = ToolExecutor::new(registry, gate, confiner);
        executor.set_checkpointer(Arc::new(
            aivyx_tools::GitCheckpointer::detect(dir.path(), vec![])
                .await
                .unwrap(),
        ));
        let autonomous_mode = AutonomousMode::new();
        autonomous_mode.set_active(true);
        let mut agent = Agent::new(
            llm,
            executor,
            "system",
            AgentConfig {
                max_tool_iterations: 10,
                ..Default::default()
            },
            Arc::default(),
            PlanMode::new(),
            autonomous_mode,
            tx,
        );
        agent.set_verification("verify".to_string(), 1);

        agent
            .run_turn("go".to_string(), dir.path(), CancellationToken::new())
            .await
            .unwrap();

        assert!(
            !dir.path().join("new.txt").exists(),
            "the file created by the discarded experiment must be gone after rewind"
        );
    }

    // ----- enforced verification (Phase 12 Part B) -----

    fn verify_command_spec(name: &str, exit_ok: bool) -> CommandSpec {
        CommandSpec {
            name: name.to_string(),
            program: "sh".to_string(),
            args: vec!["-c".to_string(), if exit_ok { "exit 0" } else { "exit 1" }.to_string()],
            timeout: Duration::from_secs(5),
        }
    }

    fn auto_verify_calls(history: &[Message]) -> usize {
        history
            .iter()
            .flat_map(|m| &m.content)
            .filter(|b| {
                matches!(
                    b,
                    ContentBlock::ToolCall(c) if c.source == ToolCallSource::AutoVerification
                )
            })
            .count()
    }

    #[tokio::test]
    async fn a_passing_verification_completes_the_turn_without_an_extra_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(aivyx_tools::WriteFileTool));
        registry.register(Arc::new(RunCommandTool::new(vec![verify_command_spec(
            "verify", true,
        )])));

        let write_call = vec![
            StreamEvent::ToolCallComplete(ToolCall {
                id: ToolCallId("c1".to_string()),
                name: "write_file".to_string(),
                arguments: serde_json::json!({ "path": "out.txt", "content": "hi\n" }),
                source: ToolCallSource::Native,
            }),
            StreamEvent::Done {
                finish_reason: FinishReason::ToolCalls,
            },
        ];
        let (mut agent, mut rx, mock) = build_agent(
            vec![write_call, text_response("done")],
            registry,
            10,
        );
        agent.set_verification("verify".to_string(), 3);

        agent
            .run_turn("go".to_string(), dir.path(), CancellationToken::new())
            .await
            .unwrap();

        let events = drain(&mut rx);
        assert!(events.iter().any(|e| matches!(e, AgentEvent::TurnComplete)));
        assert!(
            !events.iter().any(|e| matches!(e, AgentEvent::Error(_))),
            "a passing verification must not surface a failure notice"
        );
        assert_eq!(
            auto_verify_calls(&agent.history),
            1,
            "exactly one auto-verification call expected"
        );
        assert!(!agent.unverified_edits);
        assert_eq!(agent.verify_retries, 0);
        // Verification passing must not cost the model another round-trip
        // beyond the two real ones (the edit, then the model's own
        // no-more-tool-calls response) — it's dispatched directly, not
        // through another `stream_chat` call.
        assert_eq!(mock.received.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn a_failing_verification_feeds_back_and_retries_until_exhausted() {
        let dir = tempfile::tempdir().unwrap();
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(aivyx_tools::WriteFileTool));
        registry.register(Arc::new(RunCommandTool::new(vec![verify_command_spec(
            "verify", false,
        )])));

        let write_call = vec![
            StreamEvent::ToolCallComplete(ToolCall {
                id: ToolCallId("c1".to_string()),
                name: "write_file".to_string(),
                arguments: serde_json::json!({ "path": "out.txt", "content": "hi\n" }),
                source: ToolCallSource::Native,
            }),
            StreamEvent::Done {
                finish_reason: FinishReason::ToolCalls,
            },
        ];
        // One response ends the model's tool calls, then one more scripted
        // no-op response per retry (the loop re-enters the model after
        // each failed verification so it can react).
        let (mut agent, mut rx, _) = build_agent(
            vec![
                write_call,
                text_response("done"),
                text_response("trying again"),
                text_response("still trying"),
            ],
            registry,
            10,
        );
        agent.set_verification("verify".to_string(), 2);

        agent
            .run_turn("go".to_string(), dir.path(), CancellationToken::new())
            .await
            .unwrap();

        let events = drain(&mut rx);
        assert!(events.iter().any(|e| matches!(e, AgentEvent::TurnComplete)));
        assert!(
            events.iter().any(
                |e| matches!(e, AgentEvent::Error(msg) if msg.contains("still failing after 2 attempt"))
            ),
            "expected the exhausted-retries notice, got {events:?}"
        );
        assert_eq!(
            auto_verify_calls(&agent.history),
            2,
            "exactly max_auto_verify_retries auto-verification attempts expected"
        );
        // The retry budget resets so the feature isn't silently disabled
        // for the rest of the session, but the edits remain genuinely
        // unverified — the very next attempt to end a turn must re-check.
        assert_eq!(agent.verify_retries, 0);
        assert!(agent.unverified_edits);
    }

    #[tokio::test]
    async fn verification_never_fires_when_nothing_was_edited() {
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(RunCommandTool::new(vec![verify_command_spec(
            "verify", true,
        )])));
        let (mut agent, _rx, _) = build_agent(vec![text_response("hi there")], registry, 10);
        agent.set_verification("verify".to_string(), 3);

        agent
            .run_turn("go".to_string(), Path::new("."), CancellationToken::new())
            .await
            .unwrap();

        assert_eq!(auto_verify_calls(&agent.history), 0);
        assert!(!agent.unverified_edits);
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

    // ----- council mode (Phase 11a) -----

    fn council_seat(
        model: &str,
        responses: Vec<Vec<StreamEvent>>,
    ) -> (crate::council::CouncilSeat, Arc<MockBackend>) {
        let mock = Arc::new(MockBackend::new(responses));
        (
            crate::council::CouncilSeat {
                model: model.to_string(),
                backend: mock.clone(),
            },
            mock,
        )
    }

    fn council_notes(events: &[AgentEvent]) -> Vec<&str> {
        events
            .iter()
            .filter_map(|e| match e {
                AgentEvent::CouncilNote(text) => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    // Long enough to clear MIN_ANSWER_CHARS in council.rs.
    const ANSWER_A: &str = "Use tabs: accessibility tooling respects tab width settings.";
    const ANSWER_B: &str = "Use spaces: rendering is identical everywhere, zero ambiguity.";
    const RANKING: &str = "1. Advisor A — more concrete\n2. Advisor B — weaker rationale";
    const SYNTHESIS: &str = "Recommendation: adopt spaces, matching the dominant ecosystem.";

    #[tokio::test]
    async fn council_command_without_configuration_notes_and_ends_the_turn() {
        let (mut agent, mut rx, main_mock) = build_agent(vec![], ToolRegistry::new(), 10);

        agent
            .run_turn(
                "/council tabs or spaces?".to_string(),
                Path::new("."),
                CancellationToken::new(),
            )
            .await
            .unwrap();

        let events = drain(&mut rx);
        assert!(
            council_notes(&events)
                .iter()
                .any(|n| n.contains("no council is configured"))
        );
        assert!(events.iter().any(|e| matches!(e, AgentEvent::TurnComplete)));
        assert!(agent.history.is_empty(), "command must not enter history");
        assert!(
            main_mock.received.lock().unwrap().is_empty(),
            "no LLM request may be made without a council"
        );
    }

    #[tokio::test]
    async fn council_runs_the_protocol_and_pushes_only_the_synthesis() {
        let (mut agent, mut rx, main_mock) = build_agent(vec![], ToolRegistry::new(), 10);
        // Each member answers (stage 1), then ranks (stage 2).
        let (seat_a, mock_a) = council_seat(
            "model-a",
            vec![text_response(ANSWER_A), text_response(RANKING)],
        );
        let (seat_b, _) = council_seat(
            "model-b",
            vec![text_response(ANSWER_B), text_response(RANKING)],
        );
        let (chair, chair_mock) = council_seat("model-chair", vec![text_response(SYNTHESIS)]);
        agent.set_council(crate::council::Council {
            members: vec![seat_a, seat_b],
            chairman: chair,
            tail_budget_tokens: 3072,
        });

        agent
            .run_turn(
                "/council tabs or spaces?".to_string(),
                Path::new("."),
                CancellationToken::new(),
            )
            .await
            .unwrap();

        // Only the chairman's synthesis enters history, as a marked
        // user-role message; the raw command text never does.
        assert_eq!(agent.history.len(), 1);
        let entry = &agent.history[0];
        assert_eq!(entry.role, Role::User);
        let text = entry.text_content();
        assert!(text.contains("[Council synthesis"));
        assert!(text.contains(SYNTHESIS));
        assert!(text.contains("model-chair"));
        assert!(!text.contains("/council"));

        // The whole deliberation streamed as notes.
        let events = drain(&mut rx);
        let notes = council_notes(&events);
        assert!(notes.iter().any(|n| n.contains(ANSWER_A)));
        assert!(notes.iter().any(|n| n.contains(ANSWER_B)));
        assert!(notes.iter().any(|n| n.contains(RANKING)));
        assert!(notes.iter().any(|n| n.contains(SYNTHESIS)));
        assert!(events.iter().any(|e| matches!(e, AgentEvent::TurnComplete)));

        // Members were asked twice (answer, rank), toollessly, with the
        // advisor prompt; the agent's own backend was never touched.
        let member_requests = mock_a.received.lock().unwrap();
        assert_eq!(member_requests.len(), 2);
        assert!(member_requests.iter().all(|r| r.tools.is_empty()));
        assert!(member_requests[0].messages[0].text_content().contains("advisor"));
        assert!(
            member_requests[0].messages[1]
                .text_content()
                .contains("tabs or spaces?")
        );
        assert_eq!(chair_mock.received.lock().unwrap().len(), 1);
        assert!(main_mock.received.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn council_below_quorum_leaves_no_history_entry() {
        let (mut agent, mut rx, _) = build_agent(vec![], ToolRegistry::new(), 10);
        // One usable answer, one empty (e.g. an all-thinking response).
        let (seat_a, _) = council_seat("model-a", vec![text_response(ANSWER_A)]);
        let (seat_b, _) = council_seat("model-b", vec![text_response("")]);
        let (chair, chair_mock) = council_seat("model-chair", vec![text_response(SYNTHESIS)]);
        agent.set_council(crate::council::Council {
            members: vec![seat_a, seat_b],
            chairman: chair,
            tail_budget_tokens: 3072,
        });

        agent
            .run_turn(
                "/council anything".to_string(),
                Path::new("."),
                CancellationToken::new(),
            )
            .await
            .unwrap();

        assert!(agent.history.is_empty());
        assert!(
            chair_mock.received.lock().unwrap().is_empty(),
            "an aborted council must not consult the chairman"
        );
        let events = drain(&mut rx);
        assert!(
            council_notes(&events)
                .iter()
                .any(|n| n.contains("quorum"))
        );
        assert!(events.iter().any(|e| matches!(e, AgentEvent::TurnComplete)));
    }

    #[tokio::test]
    async fn council_chairman_failure_leaves_no_history_entry() {
        let (mut agent, mut rx, _) = build_agent(vec![], ToolRegistry::new(), 10);
        let (seat_a, _) = council_seat(
            "model-a",
            vec![text_response(ANSWER_A), text_response(RANKING)],
        );
        let (seat_b, _) = council_seat(
            "model-b",
            vec![text_response(ANSWER_B), text_response(RANKING)],
        );
        let (chair, _) = council_seat("model-chair", vec![text_response("")]);
        agent.set_council(crate::council::Council {
            members: vec![seat_a, seat_b],
            chairman: chair,
            tail_budget_tokens: 3072,
        });

        agent
            .run_turn(
                "/council anything".to_string(),
                Path::new("."),
                CancellationToken::new(),
            )
            .await
            .unwrap();

        assert!(
            agent.history.is_empty(),
            "nothing unsynthesized may enter history"
        );
        let events = drain(&mut rx);
        assert!(
            council_notes(&events)
                .iter()
                .any(|n| n.contains("no usable synthesis"))
        );
    }

    #[tokio::test]
    async fn bare_council_reviews_the_last_assistant_message_with_a_digest() {
        let (mut agent, mut rx, _) = build_agent(vec![], ToolRegistry::new(), 10);
        agent.history.push(user_msg("should we rewrite the parser?"));
        agent
            .history
            .push(assistant_msg("Plan: rewrite the parser with a PEG grammar."));
        let (seat_a, mock_a) = council_seat(
            "model-a",
            vec![text_response(ANSWER_A), text_response(RANKING)],
        );
        let (seat_b, _) = council_seat(
            "model-b",
            vec![text_response(ANSWER_B), text_response(RANKING)],
        );
        let (chair, _) = council_seat("model-chair", vec![text_response(SYNTHESIS)]);
        agent.set_council(crate::council::Council {
            members: vec![seat_a, seat_b],
            chairman: chair,
            tail_budget_tokens: 3072,
        });

        agent
            .run_turn(
                "/council".to_string(),
                Path::new("."),
                CancellationToken::new(),
            )
            .await
            .unwrap();

        let requests = mock_a.received.lock().unwrap();
        let prompt = requests[0].messages[1].text_content();
        assert!(
            prompt.contains("PEG grammar"),
            "bare /council must put the last assistant message before the council"
        );
        assert!(
            prompt.contains("should we rewrite the parser?"),
            "the conversation tail digest should accompany the question"
        );
        drop(requests);

        // Synthesis landed on top of the existing history.
        assert_eq!(agent.history.len(), 3);
        drain(&mut rx);
    }
}
