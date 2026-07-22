use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use aivyx_llm::{ChatRequest, FinishReason, LlmBackend, StreamEvent, ToolChoice};
use aivyx_repomap::RepoMap;
use aivyx_sandbox::{AutonomousMode, InjectionTaint, PlanMode};
use aivyx_tools::ToolExecutor;
use aivyx_tools::wiki::StalePage;
use aivyx_types::{
    ContentBlock, Message, Role, ToolCall, ToolCallId, ToolCallSource, ToolOutput, ToolResult,
};
use futures::StreamExt;
use time::OffsetDateTime;
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

use crate::edit_blocks::{self, BlockParse};
use crate::editor_context;
use crate::session::{self, SessionState, Task};

#[cfg(test)]
mod tests;
mod types;

pub use types::{AgentConfig, AgentError, AgentEvent, EditFormat};
use types::{AgentsFileConfig, EditorContextConfig, VerificationConfig};

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

/// A single `/wiki` page's turn can pause (`AgentEvent::TurnPaused`) and
/// auto-continue at most this many times before the page is abandoned for
/// this run (left stale, retried on the next `/wiki` invocation) rather
/// than looping forever. Interactive turns have no such cap because a human
/// is the natural circuit breaker on `TurnPaused`; `/wiki`'s per-page
/// auto-continue has no human in that loop, so it needs its own bound —
/// the same reason Phase 11c's autonomous driver caps its own
/// continuation loop (see `aivyx-tui`'s `AutonomousRun`).
const MAX_WIKI_PAGE_CONTINUATIONS: u32 = 5;

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
    /// Shared record of whether injection-flagged content has been
    /// ingested this session — set here when scanning tool outputs, the
    /// repo map, AGENTS.md, or the editor-context descriptor; consulted
    /// by `ConfirmationGate` (autonomous mode) and the TUI's autonomous
    /// driver. Defaults to a fresh, never-flagged `InjectionTaint` unless
    /// `set_injection_taint` attaches the same shared instance those
    /// other consumers hold — see docs/superpowers/specs/
    /// 2026-07-22-autonomous-mode-injection-guard-design.md.
    injection_taint: InjectionTaint,
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
    /// `AGENTS.md` support when configured (`set_agents_file`); `None`
    /// disables the feature entirely (both files).
    agents_file_config: Option<AgentsFileConfig>,
    /// Editor-context support when configured (`set_editor_context`);
    /// `None` disables the feature entirely.
    editor_context_config: Option<EditorContextConfig>,
    /// The rendered slice appended to the system prompt; also counted by
    /// the context estimator — a ~1k-token block compaction can't see would
    /// silently eat the window's headroom.
    repo_map_text: Option<String>,
    /// Combined, labeled rendering of the project's and/or user's
    /// `AGENTS.md` — re-rendered once per turn by `refresh_agents_files`,
    /// mirroring `repo_map_text`'s own per-turn cadence.
    agents_files_text: Option<String>,
    /// One-line "currently open in editor" status, re-rendered once per
    /// turn by `refresh_editor_context` — same per-turn cadence as
    /// `agents_files_text`/`repo_map_text`. Metadata only, deliberately —
    /// see the module doc on `editor_context` for why file content never
    /// flows through this field.
    editor_context_text: Option<String>,
    edit_format: EditFormat,
    /// Monotonic id source for tool calls synthesized from SEARCH/REPLACE
    /// blocks — they need ids that can't collide with the backend's.
    synthetic_seq: u64,
    /// `/council` support when configured; `None` makes the command explain
    /// how to enable itself instead of running.
    council: Option<crate::council::Council>,
    /// `/architect` support when configured; `None` makes the command
    /// explain how to enable itself instead of running.
    architect: Option<crate::architect::Architect>,
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
    /// The raw `run_auto_verification` output text from the immediately
    /// preceding verification run in this session, *regardless of whether
    /// that run passed or failed* — `None` until the first verification
    /// call ever happens, then updated after every subsequent call (pass
    /// or fail alike). Used to distinguish a genuinely new failure line
    /// from one that was already present in the last attempt, whatever its
    /// outcome. In-memory only; never persisted to the session JSON — a
    /// resumed session starts with nothing to compare against, same as
    /// before the first verification call in a fresh session.
    last_verification_output: Option<String>,
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
            injection_taint: InjectionTaint::new(),
            last_turn_paused: false,
            repo_map: None,
            agents_file_config: None,
            editor_context_config: None,
            repo_map_text: None,
            agents_files_text: None,
            editor_context_text: None,
            edit_format: config.edit_format,
            synthetic_seq: 0,
            council: None,
            architect: None,
            verification: None,
            unverified_edits: false,
            verify_retries: 0,
            last_verification_output: None,
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

    /// Sends an informational notice into the transcript without going through
    /// a model turn — used by the autonomous driver to report why it stopped
    /// (goal achieved, budget exhausted, cancelled), since none of those are
    /// otherwise visible once the loop stops producing turns.
    pub fn notify(&self, message: impl Into<String>) {
        self.emit(AgentEvent::Error(message.into()));
    }

    /// Enables `/council` (Phase 11a). The caller builds the seats — each
    /// is any `LlmBackend`, typically Ollama-swapped models alongside the
    /// resident daily driver.
    pub fn set_council(&mut self, council: crate::council::Council) {
        self.council = Some(council);
    }

    /// Enables `/architect` (ROADMAP.md Phase 9). The caller builds the
    /// seat from config — see `crate::architect::Architect`.
    pub fn set_architect(&mut self, architect: crate::architect::Architect) {
        self.architect = Some(architect);
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

    /// Enables `AGENTS.md` support: `global_path` is the resolved
    /// user-global location (`None` if `Settings::agents_file_path()`
    /// couldn't resolve one), applied identically per turn alongside the
    /// project-level `<cwd>/AGENTS.md`. `budget_tokens` applies to each
    /// file independently.
    pub fn set_agents_file(&mut self, global_path: Option<PathBuf>, budget_tokens: u32) {
        self.agents_file_config = Some(AgentsFileConfig {
            global_path,
            budget_tokens,
        });
    }

    /// Enables editor-context awareness: a per-project JSON file an editor
    /// integration writes to (see the `editor_context` module), re-read
    /// and surfaced as a one-line system-prompt addition every turn.
    /// `deny_paths` is checked against the reported file path before it's
    /// ever surfaced, same as every other path-reporting tool in this
    /// project.
    pub fn set_editor_context(&mut self, deny_paths: Vec<PathBuf>) {
        self.editor_context_config = Some(EditorContextConfig { deny_paths });
    }

    /// Attaches the shared `InjectionTaint` handle `ConfirmationGate` and
    /// the TUI's autonomous driver also hold. See the field doc comment
    /// above for why this must be the same instance.
    pub fn set_injection_taint(&mut self, injection_taint: InjectionTaint) {
        self.injection_taint = injection_taint;
    }

    /// Re-reads the editor-context file (if configured) and stores a
    /// one-line "currently open in editor" status, or clears it to `None`
    /// on any of: feature disabled, file missing/unreadable/malformed,
    /// unrecognized `schema_version`, stale `updated_at` (>5 minutes old),
    /// `workspace_root` not matching this session's own `cwd`, or the
    /// reported file falling under a configured `deny_paths` entry. None
    /// of these are user-facing notices — an editor integration not
    /// running, or a stale leftover file, is a normal silent state, not a
    /// misconfiguration (unlike `AGENTS.md`'s over-budget notice).
    async fn refresh_editor_context(&mut self, cwd: &Path) {
        self.editor_context_text = None;

        let Some(config) = &self.editor_context_config else {
            return;
        };
        let Some(path) = editor_context::editor_context_file_path(cwd) else {
            return;
        };
        let Some(context) = editor_context::read_editor_context(&path).await else {
            return;
        };
        if context.schema_version != editor_context::SCHEMA_VERSION {
            return;
        }
        if (OffsetDateTime::now_utc() - context.updated_at).abs() > time::Duration::minutes(5) {
            return;
        }

        let canonical_cwd = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
        let canonical_root = context
            .workspace_root
            .canonicalize()
            .unwrap_or_else(|_| context.workspace_root.clone());
        if canonical_root != canonical_cwd {
            return;
        }

        let resolved_file = context.workspace_root.join(&context.file);
        if aivyx_sandbox::path_is_denied(&resolved_file, &config.deny_paths) {
            return;
        }

        let file_display = sanitize_for_display(&context.file.display().to_string());
        self.editor_context_text = Some(match &context.selection {
            None => format!(
                "Currently open in editor: {file_display}, cursor at line {}.",
                context.cursor.line
            ),
            Some(sel) => format!(
                "Currently open in editor: {file_display}, cursor at line {}, with lines \
                 {}-{} selected.",
                context.cursor.line, sel.start_line, sel.end_line
            ),
        });
        if let Some(text) = &self.editor_context_text
            && let Some(finding) = aivyx_sandbox::scan_for_injection_markers(text, "editor context")
        {
            self.injection_taint.flag(finding);
        }
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
        if let Some(text) = &self.repo_map_text
            && let Some(finding) = aivyx_sandbox::scan_for_injection_markers(text, "repo map")
        {
            self.injection_taint.flag(finding);
        }
    }

    /// Re-reads both `AGENTS.md` files off the async runtime. Best-effort:
    /// any read error for either file (missing, permission denied, not
    /// valid UTF-8) just means that source contributes nothing — this
    /// never fails the turn. Called once per turn (not once per LLM
    /// round-trip), mirroring `refresh_repo_map`'s cadence exactly.
    async fn refresh_agents_files(&mut self, cwd: &Path) {
        let Some(config) = &self.agents_file_config else {
            return;
        };
        let budget_chars = (config.budget_tokens as f64 * self.chars_per_token) as usize;
        let global_path = config.global_path.clone();
        let project_path = cwd.join("AGENTS.md");
        let budget_tokens = config.budget_tokens;

        let mut sections: Vec<String> = Vec::new();
        let mut over_budget_labels: Vec<&str> = Vec::new();

        if let Some(path) = &global_path
            && let Ok(content) = tokio::fs::read_to_string(path).await
        {
            let content = content.trim();
            if !content.is_empty() {
                if content.chars().count() > budget_chars {
                    over_budget_labels.push("user-level AGENTS.md");
                }
                if let Some(finding) =
                    aivyx_sandbox::scan_for_injection_markers(content, "user-level AGENTS.md")
                {
                    self.injection_taint.flag(finding);
                }
                sections.push(format!("User preferences ({}):\n{content}", path.display()));
            }
        }

        if let Ok(content) = tokio::fs::read_to_string(&project_path).await {
            let content = content.trim();
            if !content.is_empty() {
                if content.chars().count() > budget_chars {
                    over_budget_labels.push("project AGENTS.md");
                }
                if let Some(finding) =
                    aivyx_sandbox::scan_for_injection_markers(content, "project AGENTS.md")
                {
                    self.injection_taint.flag(finding);
                }
                sections.push(format!("Project instructions (AGENTS.md):\n{content}"));
            }
        }

        for label in &over_budget_labels {
            self.notify(format!(
                "{label} exceeds the configured [agents_file] budget_tokens ({budget_tokens} \
                 tokens) — trim it or raise the budget; the full content was still included."
            ));
        }

        self.agents_files_text = match sections.len() {
            0 => None,
            1 => Some(sections.remove(0)),
            _ => {
                let project = sections.remove(1);
                let global = sections.remove(0);
                Some(format!(
                    "{global}\n\n(Project instructions take precedence over user preferences \
                     if they conflict.)\n\n{project}"
                ))
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
        if let Some(text) = &self.agents_files_text {
            system.push_str("\n\n");
            system.push_str(text);
        }
        if let Some(map) = &self.repo_map_text {
            system.push_str("\n\n");
            system.push_str(map);
        }
        if let Some(text) = &self.editor_context_text {
            system.push_str("\n\n");
            system.push_str(text);
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
            + self
                .agents_files_text
                .as_ref()
                .map_or(0, |m| m.chars().count())
            + self
                .editor_context_text
                .as_ref()
                .map_or(0, |m| m.chars().count())
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

    /// Emits `AgentEvent::ToolResult` and appends the matching
    /// `Role::Tool` history entry — the shared tail both
    /// `run_auto_verification` and the main per-turn dispatch loop need,
    /// since every dispatched `ToolCall` requires a matching `Role::Tool`
    /// result or the next request's unanswered `tool_calls` entry gets
    /// rejected by most OpenAI-compatible backends. Also where injection
    /// scanning happens: a flagged `ToolOutput::Ok` result taints
    /// `self.injection_taint`, consulted by `ConfirmationGate` and the
    /// autonomous driver. See docs/superpowers/specs/
    /// 2026-07-22-autonomous-mode-injection-guard-design.md.
    fn record_tool_result(&mut self, result: ToolResult, source: &str) {
        if let ToolOutput::Ok(text) = &result.output
            && let Some(finding) = aivyx_sandbox::scan_for_injection_markers(text, source)
        {
            self.injection_taint.flag(finding);
        }
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

        let source = describe_tool_call_target(&call);
        let mut result = self
            .executor
            .dispatch(call, cwd, cancellation.clone())
            .await;
        let passed =
            matches!(&result.output, ToolOutput::Ok(text) if command_reported_success(text));

        // Compare against the immediately preceding run (whatever its own
        // outcome was) using the *original* text, then store that same
        // original (not the enriched) text as the new reference for next
        // time — the enrichment note is this turn's feedback only, not
        // something a future comparison should treat as real command
        // output. Only a FAILING result gets the note: a passing result's
        // output almost always differs hugely from a preceding failure
        // (clean output vs. lines full of FAILED), and `command_reported_
        // success` already communicates "this passed" plainly — appending
        // a large "what changed" note to already-unambiguous good news
        // would be pure noise. `last_verification_output` is still updated
        // unconditionally on both outcomes, though — comparing against
        // the last run rather than only the last *successful* one is the
        // whole point of this feature (see the design spec's Decision 2).
        if let ToolOutput::Ok(text) = &mut result.output {
            let current_text = text.clone();
            if !passed
                && let Some(previous) = &self.last_verification_output
                && let Some(note) = new_lines_note(previous, &current_text)
            {
                text.push_str(&note);
            }
            self.last_verification_output = Some(current_text);
        }

        self.record_tool_result(result, &source);
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
            None => match crate::wiki::parse_command(&user_input) {
                Some(command) => self.run_wiki_turn(command, cwd, cancellation).await,
                None => match crate::architect::parse_command(&user_input) {
                    Some(subject) => {
                        let subject = subject.to_string();
                        self.run_architect_turn(&subject, cwd, cancellation).await
                    }
                    None => self.run_turn_inner(user_input, cwd, cancellation).await,
                },
            },
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

    /// Runs `/architect`: makes one planning call to the configured
    /// architect, then hands the plan directly to `run_turn_inner` so the
    /// primary (editor) model begins acting on it in the same call — one
    /// continuous action, no re-prompt needed. A missing task argument or
    /// any planning failure ends the turn without ever invoking the editor.
    async fn run_architect_turn(
        &mut self,
        subject: &str,
        cwd: &Path,
        cancellation: CancellationToken,
    ) -> Result<(), AgentError> {
        if self.architect.is_none() {
            self.emit(AgentEvent::ArchitectNote(
                "no architect is configured — add [architect] base_url and model to \
                 config.toml (see the README's Architect/editor section)"
                    .to_string(),
            ));
            self.emit(AgentEvent::TurnComplete);
            return Ok(());
        }
        if subject.trim().is_empty() {
            self.emit(AgentEvent::ArchitectNote(
                "usage: /architect <task> — describe the task you want planned, e.g. \
                 /architect refactor the auth module to use the new token type"
                    .to_string(),
            ));
            self.emit(AgentEvent::TurnComplete);
            return Ok(());
        }

        self.refresh_repo_map().await;
        let architect = self.architect.as_ref().expect("checked above");
        let budget_chars = (architect.tail_budget_tokens as f64 * self.chars_per_token) as usize;
        let digest = crate::council::tail_digest(&self.history, budget_chars);

        let mut context = String::new();
        if let Some(map) = &self.repo_map_text {
            context.push_str(map);
            context.push_str("\n\n");
        }
        if let Some(digest) = &digest {
            context.push_str(digest);
        }
        let context = if context.is_empty() {
            None
        } else {
            Some(context)
        };

        let seat = &self.architect.as_ref().expect("checked above").seat;
        let plan_text =
            crate::architect::plan(seat, subject, context, &self.events_tx, &cancellation).await;

        let Some(plan_text) = plan_text else {
            self.emit(AgentEvent::TurnComplete);
            return Ok(());
        };
        let model_name = seat.model.clone();

        let formatted = format!(
            "[Architect plan — {model_name} planned this task; you are executing it]\n\n\
             Task: {subject}\n\nPlan:\n{plan_text}"
        );
        self.run_turn_inner(formatted, cwd, cancellation).await
    }

    /// Runs `/wiki` (Phase 11b): resolves `command` into the pages that
    /// need regenerating, then drives one turn per page through
    /// `run_wiki_turn_for_pages`. See `aivyx_tools::wiki` for the
    /// staleness/frontmatter mechanics and `crate::wiki` for this project's
    /// fixed page skeleton.
    async fn run_wiki_turn(
        &mut self,
        command: crate::wiki::WikiCommand,
        cwd: &Path,
        cancellation: CancellationToken,
    ) -> Result<(), AgentError> {
        let wiki_dir = cwd.join(crate::wiki::WIKI_DIR);
        let specs = crate::wiki::page_specs(cwd);

        let pages: Vec<StalePage> = match command {
            crate::wiki::WikiCommand::Batch => {
                aivyx_tools::wiki::stale_pages(cwd, &wiki_dir, &specs, &cancellation).await
            }
            crate::wiki::WikiCommand::Forced(name) => {
                let Some(spec) = specs.iter().find(|s| s.name == name) else {
                    let valid: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();
                    self.emit(AgentEvent::Error(format!(
                        "unknown wiki page '{name}' — valid pages: {}",
                        valid.join(", ")
                    )));
                    self.emit(AgentEvent::TurnComplete);
                    return Ok(());
                };
                vec![StalePage {
                    name: spec.name.clone(),
                    covers: spec.covers.clone(),
                    reason: aivyx_tools::wiki::StaleReason::Forced,
                }]
            }
        };

        if pages.is_empty() {
            self.emit(AgentEvent::Error(
                "wiki is up to date, nothing to regenerate".to_string(),
            ));
            self.emit(AgentEvent::TurnComplete);
            return Ok(());
        }

        self.run_wiki_turn_for_pages(pages, cwd, cancellation).await
    }

    /// Drives one turn per page in `pages`, in order — split out from
    /// `run_wiki_turn` so tests can exercise a known page list directly
    /// without depending on filesystem-driven staleness discovery. Reuses
    /// `run_turn_inner` (not `run_turn`, so a synthesized instruction is
    /// never re-checked against `/council`/`/wiki`), continuing on
    /// `TurnPaused` exactly like a normal multi-round-trip turn, up to
    /// `MAX_WIKI_PAGE_CONTINUATIONS` auto-continues. A page whose turn ends
    /// in error, whose `write_file` call never actually happened, or that
    /// is still pausing after the continuation cap, is skipped (left stale
    /// for the next `/wiki` run) rather than aborting pages still queued
    /// behind it.
    async fn run_wiki_turn_for_pages(
        &mut self,
        pages: Vec<StalePage>,
        cwd: &Path,
        cancellation: CancellationToken,
    ) -> Result<(), AgentError> {
        let wiki_dir = cwd.join(crate::wiki::WIKI_DIR);

        for page in pages {
            if cancellation.is_cancelled() {
                break;
            }

            let covers_list: String = page
                .covers
                .iter()
                .map(|c| format!("  - \"{c}\"\n"))
                .collect();
            let instruction = format!(
                "Regenerate the wiki page `{}/{}.md`. It should document: {}. Write clear, \
                 accurate Markdown covering what this part of the codebase does, its key \
                 types/functions, and how it's used — a reader with no context should be able \
                 to orient quickly. Start the file with frontmatter exactly in this form (fill \
                 in `summary` with a genuinely useful one-line description; \
                 `generated_at_commit` and `covers` will be overwritten automatically \
                 afterward, so their values here don't matter):\n\
                 ---\n\
                 generated_at_commit: (placeholder)\n\
                 covers:\n{covers_list}\
                 summary: \"...\"\n\
                 ---\n\
                 Then the page body. Call write_file with the complete file content in one \
                 call.",
                crate::wiki::WIKI_DIR,
                page.name,
                page.covers.join(", "),
            );

            self.last_turn_paused = false;
            let mut result = self
                .run_turn_inner(instruction, cwd, cancellation.clone())
                .await;
            let mut continuations_sent = 0u32;
            while result.is_ok() && self.last_turn_paused && !cancellation.is_cancelled() {
                if continuations_sent >= MAX_WIKI_PAGE_CONTINUATIONS {
                    // Still paused after the cap's worth of "continue"
                    // attempts — stop driving this page. `result` and
                    // `self.last_turn_paused` are left exactly as the last
                    // iteration set them, so the check below recognizes
                    // this as the cap-reached case (Ok + still paused).
                    break;
                }
                continuations_sent += 1;
                self.last_turn_paused = false;
                result = self
                    .run_turn_inner("continue".to_string(), cwd, cancellation.clone())
                    .await;
            }

            if cancellation.is_cancelled() {
                break;
            }
            if result.is_ok() && self.last_turn_paused {
                self.emit(AgentEvent::Error(format!(
                    "page `{}` paused too many times ({} continuation attempts) — leaving it \
                     stale for the next /wiki run",
                    page.name, MAX_WIKI_PAGE_CONTINUATIONS
                )));
                continue;
            }
            if result.is_err() {
                // `run_turn_inner` already emitted an `AgentEvent::Error`
                // describing the failure — this page just stays stale for
                // the next `/wiki` run.
                continue;
            }

            if let Err(err) = aivyx_tools::wiki::stamp_page(
                &wiki_dir,
                cwd,
                &page.name,
                &page.covers,
                &cancellation,
            )
            .await
            {
                self.emit(AgentEvent::Error(format!(
                    "page `{}` did not save correctly ({err}) — it will be retried on the next \
                     /wiki run",
                    page.name
                )));
            }
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
        self.refresh_agents_files(cwd).await;
        self.refresh_editor_context(cwd).await;

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
                    Ok(StreamEvent::ReasoningDelta(text)) => {
                        // Deliberately not accumulated into `assistant_text`
                        // and no size-cap check — reasoning never enters
                        // `self.history`, so there's no unbounded-growth
                        // risk on this side to guard against. See
                        // docs/superpowers/specs/
                        // 2026-07-19-reasoning-visibility-design.md.
                        self.emit(AgentEvent::ReasoningDelta(text));
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

            // Batch-tracking for automatic rollback: if a later call in this
            // same response fails after earlier calls in it already mutated
            // the working tree, every one of those earlier successes gets
            // rolled back so a cross-file change either fully applies or
            // leaves no trace. Independent of the is_edit_call/
            // pre_experiment_ref bookkeeping below — that mechanism only
            // rewinds on an exhausted *verification* retry loop in
            // autonomous mode; this one fires on any mutating tool
            // returning `ToolOutput::Error` within the same response,
            // regardless of mode.
            let mut last_checkpoint_ref = self.executor.latest_checkpoint_ref(&cancellation).await;
            let mut batch_start_ref: Option<String> = None;
            let mut batch_touched_paths: Vec<String> = Vec::new();
            let mut batch_rolled_back = false;

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
                if batch_rolled_back {
                    self.record_skipped_tool_result(
                        call,
                        "skipped — a failure earlier in this response rolled back the batch of edits",
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
                let call_description = describe_tool_call_target(&call);
                let mut result = self
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

                // Batch-checkpoint tracking, independent of the
                // pre_experiment_ref bookkeeping above. `ToolExecutor::
                // dispatch` checkpoints *before* running the tool, so the
                // ref that becomes "latest" right after a successful
                // mutating call is the snapshot of the worktree as it stood
                // immediately before that call ran — exactly the anchor to
                // restore to in order to undo this call (and everything
                // after it). Only the first successful call in the batch
                // gets to set `batch_start_ref`; later ones must not move
                // it forward.
                let ref_after_this_call = self.executor.latest_checkpoint_ref(&cancellation).await;
                let minted_new_checkpoint = ref_after_this_call != last_checkpoint_ref;
                last_checkpoint_ref = ref_after_this_call.clone();
                if matches!(result.output, ToolOutput::Ok(_)) && minted_new_checkpoint {
                    if batch_start_ref.is_none() {
                        batch_start_ref = ref_after_this_call;
                    }
                    batch_touched_paths.push(call_description.clone());
                }
                if let ToolOutput::Error(original_error) = &result.output
                    && let Some(start_ref) = batch_start_ref.take()
                {
                    match self.executor.restore_to_checkpoint(&start_ref, &cancellation).await {
                        Ok(()) => {
                            result.output = ToolOutput::Error(format!(
                                "{original_error}\n\nThis failure automatically rolled back {} \
                                 earlier edit(s) in this same response to keep the codebase \
                                 consistent: {}. The codebase is now back to its state before \
                                 this response's edits began.",
                                batch_touched_paths.len(),
                                batch_touched_paths.join(", "),
                            ));
                        }
                        Err(restore_err) => {
                            result.output = ToolOutput::Error(format!(
                                "{original_error}\n\nAdditionally, an automatic rollback of {} \
                                 earlier edit(s) in this same response was attempted (to keep the \
                                 codebase consistent) but FAILED ({restore_err}) — the codebase \
                                 may now be in a partially-edited, inconsistent state. Affected \
                                 files: {}. Inspect manually via `git log \
                                 refs/aivyx/checkpoints/`.",
                                batch_touched_paths.len(),
                                batch_touched_paths.join(", "),
                            ));
                        }
                    }
                    batch_rolled_back = true;
                    batch_touched_paths.clear();
                }

                self.record_tool_result(result, &call_description);
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

/// Best-effort human-readable description of what a tool call touched, for
/// the batch-rollback notice — most mutating tools (`write_file`,
/// `edit_file`, `delete_file`) take a `"path"` argument; anything else
/// falls back to just the tool's name.
fn describe_tool_call_target(call: &ToolCall) -> String {
    match call.arguments.get("path").and_then(|v| v.as_str()) {
        Some(path) => format!("{path} ({})", call.name),
        None => call.name.clone(),
    }
}

/// Bound on the displayed length of the editor-context `file` value injected
/// into the trusted system prompt — this is a display string, not a real
/// path used for I/O, so an arbitrarily long value is just truncated rather
/// than rejected outright.
const EDITOR_CONTEXT_FILE_DISPLAY_MAX_CHARS: usize = 512;

/// Strips ASCII control characters (below 0x20, plus 0x7F/DEL — this
/// includes `\n`, `\r`, and tab) from `file` before it is interpolated into
/// the injected editor-context note, and clamps its length. `file` is a
/// free-form string from a JSON descriptor an attacker may influence; the
/// note it feeds is dropped verbatim into the *trusted* system prompt, so a
/// crafted value containing newlines could otherwise forge additional
/// "instructions" at that trust level. This only affects what is displayed —
/// `context.file` itself (used to resolve the real path for the
/// `deny_paths` check) is left untouched.
fn sanitize_for_display(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .map(|c| if c.is_control() { '\u{FFFD}' } else { c })
        .collect();
    if cleaned.chars().count() <= EDITOR_CONTEXT_FILE_DISPLAY_MAX_CHARS {
        cleaned
    } else {
        let truncated: String = cleaned
            .chars()
            .take(EDITOR_CONTEXT_FILE_DISPLAY_MAX_CHARS)
            .collect();
        format!("{truncated}...")
    }
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

/// Bound on the rendered new-lines note appended to a failing verification
/// result — small and fixed, unlike `elide_oversized_tool_results`'s own
/// dynamic, context-budget-driven cap, since this note is a bounded
/// addition to an already-capped tool result, not the whole history.
const NEW_LINES_NOTE_CAP: usize = 2000;

/// Compares `current` (this verification run's raw output) against
/// `previous` (the immediately preceding run's raw output, whatever its
/// outcome — see docs/superpowers/specs/
/// 2026-07-19-structured-verification-memory-design.md's Decision 2) line
/// by line, and returns a short note listing lines present in `current`
/// but absent from `previous` — a rough "what's new since the last
/// attempt" signal. `None` if every line in `current` already appeared in
/// `previous` (nothing new to report). Deliberately coarse: this has no
/// notion of what a "test" is, so a line that only differs by e.g. a
/// timestamp will still look new.
fn new_lines_note(previous: &str, current: &str) -> Option<String> {
    let previous_lines: std::collections::HashSet<&str> = previous.lines().collect();
    let new_lines: Vec<&str> = current
        .lines()
        .filter(|line| !previous_lines.contains(line))
        .collect();
    if new_lines.is_empty() {
        return None;
    }
    Some(format!(
        "\n\n{} line(s) of this output were not present in the immediately preceding \
         verification attempt — a rough signal for what's new since then, not a precise \
         test-level diff (some noise is possible, e.g. timestamps or other \
         non-deterministic content):\n{}",
        new_lines.len(),
        elide(&new_lines.join("\n"), NEW_LINES_NOTE_CAP)
    ))
}
