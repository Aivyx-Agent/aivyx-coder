use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use aivyx_core::{Agent, AgentEvent};
use aivyx_sandbox::{PermissionRequest, PermissionTarget, UserResponse};
use aivyx_types::ToolOutput;
use crossterm::event::{Event as CtEvent, EventStream, KeyCode, KeyEventKind, KeyModifiers};
use futures::StreamExt;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tui_textarea::TextArea;

use crate::permission::{ModalRequest, PermissionModalReceiver};
use crate::terminal::TerminalGuard;

enum ChatLine {
    User(String),
    Assistant(String),
    ToolCall(String),
    ToolResult(String),
    Notice(String),
}

/// Owns the ratatui render loop. Takes an already-constructed `Agent` (the
/// caller built it with the `LlmBackend` + `ToolExecutor` it wants) and the
/// receiving half of the channel that `Agent` was constructed with; `run`
/// drives the agent on a background task and renders its events live.
pub async fn run(
    mut agent: Agent,
    mut agent_events_rx: mpsc::UnboundedReceiver<AgentEvent>,
    cwd: PathBuf,
    mut permission_rx: PermissionModalReceiver,
) -> anyhow::Result<()> {
    let (input_tx, mut input_rx) = mpsc::unbounded_channel::<String>();
    let active_cancellation: Arc<Mutex<Option<CancellationToken>>> = Arc::new(Mutex::new(None));

    let background_cancellation = Arc::clone(&active_cancellation);
    tokio::spawn(async move {
        while let Some(input) = input_rx.recv().await {
            let cancellation = CancellationToken::new();
            *background_cancellation.lock().unwrap() = Some(cancellation.clone());
            let _ = agent.run_turn(input, &cwd, cancellation).await;
            *background_cancellation.lock().unwrap() = None;
        }
    });

    let mut guard = TerminalGuard::init()?;
    let mut app = App::new();
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
                                app.push_user_message(text.clone());
                                let _ = input_tx.send(text);
                            }
                            continue;
                        }
                        _ => {}
                    }
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
        }
    }

    Ok(())
}

struct App {
    transcript: Vec<ChatLine>,
    input: TextArea<'static>,
    streaming_active: bool,
    pending_permission: Option<ModalRequest>,
}

impl App {
    fn new() -> Self {
        Self {
            transcript: Vec::new(),
            input: new_input_box(),
            streaming_active: false,
            pending_permission: None,
        }
    }

    fn resolve_permission(&mut self, response: UserResponse) {
        if let Some(modal) = self.pending_permission.take() {
            let _ = modal.reply_tx.send(response);
        }
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

    fn handle_agent_event(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::TextDelta(text) => {
                if let Some(ChatLine::Assistant(existing)) = self.transcript.last_mut() {
                    existing.push_str(&text);
                } else {
                    self.transcript.push(ChatLine::Assistant(text));
                }
            }
            AgentEvent::ToolCallDetected(call) => {
                self.transcript.push(ChatLine::ToolCall(format!(
                    "{}({})",
                    call.name, call.arguments
                )));
            }
            AgentEvent::ToolResult(result) => {
                let text = match result.output {
                    ToolOutput::Ok(s) => s,
                    ToolOutput::Error(s) => format!("error: {s}"),
                    ToolOutput::Denied(s) => format!("denied: {s}"),
                };
                self.transcript.push(ChatLine::ToolResult(text));
            }
            AgentEvent::TurnComplete => self.streaming_active = false,
            AgentEvent::Error(message) => {
                self.transcript.push(ChatLine::Notice(message));
                self.streaming_active = false;
            }
        }
    }

    fn render(&self, frame: &mut ratatui::Frame) {
        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(1),
                Constraint::Length(3),
                Constraint::Length(1),
            ])
            .split(frame.area());

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

        frame.render_widget(&self.input, layout[1]);

        let status = if self.streaming_active {
            "streaming... (Ctrl+C to cancel)"
        } else {
            "ready — Enter to send, Ctrl+C to quit"
        };
        frame.render_widget(
            Paragraph::new(status).style(Style::default().fg(Color::DarkGray)),
            layout[2],
        );

        if let Some(modal) = &self.pending_permission {
            render_permission_modal(frame, &modal.request);
        }
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

    let mut lines: Vec<Line> = vec![
        Line::from(format!("Tool: {}", request.tool_name))
            .style(Style::default().add_modifier(Modifier::BOLD)),
        Line::from(format!("Action: {:?}", request.action)),
        target_line(&request.target),
        Line::from(""),
    ];

    match request.preview.as_deref() {
        Some(preview) if !preview.is_empty() => lines.extend(preview.lines().map(diff_line)),
        _ => lines.push(Line::from(format!("Args: {}", request.arguments_preview))),
    }

    lines.push(Line::from(""));
    lines.push(
        Line::from("[y] Allow    [a] Always Allow    [n] / [Esc] / [Enter] Deny")
            .style(Style::default().fg(Color::DarkGray)),
    );

    let modal = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Permission required")
                .style(Style::default().fg(Color::Yellow)),
        )
        .wrap(Wrap { trim: false });
    frame.render_widget(modal, area);
}

fn target_line(target: &PermissionTarget) -> Line<'static> {
    match target {
        PermissionTarget::Path(path) => Line::from(format!("Target: {}", path.display())),
        PermissionTarget::Command { program, args } => {
            Line::from(format!("Command: {program} {}", args.join(" ")))
        }
        PermissionTarget::Other(description) => Line::from(format!("Target: {description}")),
    }
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
