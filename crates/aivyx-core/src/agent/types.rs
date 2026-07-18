use std::path::PathBuf;

use aivyx_llm::LlmError;
use aivyx_types::{ToolCall, ToolResult};
use thiserror::Error;

use crate::session::Task;

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
    /// One block of architect-mode output (the planning-in-progress note,
    /// the produced plan, or a failure note) — mirrors `CouncilNote`
    /// exactly, but for the single-seat architect/editor pairing feature.
    /// See `crate::architect::plan`.
    ArchitectNote(String),
    /// One event produced by a `delegate_task` sub-agent's own turn loop,
    /// forwarded verbatim from its private `AgentEvent` channel so it can
    /// render in the transcript distinguished from the parent's own
    /// activity — see `crate::delegate::DelegateTaskTool`. `Box`ed since
    /// `AgentEvent` itself isn't `Copy` and this variant would otherwise
    /// make every `AgentEvent` at least as large as its own biggest
    /// variant recursively.
    SubAgentActivity(Box<AgentEvent>),
}

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

/// Configures the enforced verification loop (ROADMAP.md Phase 12 Part B):
/// after file edits, before a turn is allowed to end, the named
/// `allowed_commands` entry is auto-run via `run_command`.
#[derive(Debug, Clone)]
pub(crate) struct VerificationConfig {
    pub(crate) command_name: String,
    /// Clamped to a minimum of 1 by `Agent::set_verification` — a 0 here
    /// would report "still failing" without ever actually attempting a
    /// verification run.
    pub(crate) max_retries: u32,
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

/// Where to find the user-global `AGENTS.md` (if resolvable) and the
/// per-file token budget both the global and project files share.
pub(crate) struct AgentsFileConfig {
    pub(crate) global_path: Option<PathBuf>,
    pub(crate) budget_tokens: u32,
}

/// `deny_paths` needed to check a reported editor-context file path before
/// surfacing it — see `Agent::refresh_editor_context`. No budget/enable
/// fields here: unlike `AGENTS.md`, there's no token-budget concept for a
/// one-line status string, and `Some`/`None` on the outer
/// `editor_context_config` field is itself the enable/disable signal,
/// mirroring `AgentsFileConfig`'s own pattern.
pub(crate) struct EditorContextConfig {
    pub(crate) deny_paths: Vec<PathBuf>,
}
