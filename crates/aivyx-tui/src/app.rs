use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use aivyx_core::{Agent, AgentEvent};
use aivyx_types::ToolOutput;
use crossterm::event::{Event as CtEvent, EventStream, KeyCode, KeyEventKind, KeyModifiers};
use futures::StreamExt;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tui_textarea::TextArea;

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
        }
    }

    Ok(())
}

struct App {
    transcript: Vec<ChatLine>,
    input: TextArea<'static>,
    streaming_active: bool,
}

impl App {
    fn new() -> Self {
        Self {
            transcript: Vec::new(),
            input: new_input_box(),
            streaming_active: false,
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
        let scroll = (lines.len() as u16).saturating_sub(viewport_height);

        let transcript = Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL).title("aivyx-coder"))
            .wrap(Wrap { trim: false })
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
    }
}

fn new_input_box() -> TextArea<'static> {
    let mut input = TextArea::default();
    input.set_placeholder_text("Type a message and press Enter to send...");
    input.set_block(Block::default().borders(Borders::ALL).title("Message"));
    input
}

fn chat_line_to_lines(line: &ChatLine) -> Vec<Line<'static>> {
    match line {
        ChatLine::User(text) => prefixed_lines(text, "you  > ", Style::default().fg(Color::Cyan)),
        ChatLine::Assistant(text) => prefixed_lines(text, "aivyx> ", Style::default()),
        ChatLine::ToolCall(text) => vec![
            Line::from(format!("  tool call: {text}")).style(Style::default().fg(Color::Yellow)),
        ],
        ChatLine::ToolResult(text) => vec![
            Line::from(format!("  tool result: {text}")).style(Style::default().fg(Color::Green)),
        ],
        ChatLine::Notice(text) => {
            vec![
                Line::from(format!("  ! {text}"))
                    .style(Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)),
            ]
        }
    }
}

fn prefixed_lines(text: &str, prefix: &'static str, style: Style) -> Vec<Line<'static>> {
    let indent = " ".repeat(prefix.len());
    text.lines()
        .enumerate()
        .map(|(i, line)| {
            let prefix = if i == 0 { prefix } else { indent.as_str() };
            Line::from(format!("{prefix}{line}")).style(style)
        })
        .collect()
}
