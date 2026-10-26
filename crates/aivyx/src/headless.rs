#![allow(dead_code)] // wired up in the next commit
//! `--auto` without the TUI: for scripts, CI and the benchmark. Progress
//! goes to stderr, one line per event, prefixed with the time since the run
//! started; the stop notice goes to stdout; the exit code says how the run
//! ended. Runs the same loop as the TUI (`aivyx_core::autonomous::drive`).

use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aivyx_core::autonomous::{AutonomousRun, Outcome, RunEnd};
use aivyx_core::{Agent, AgentEvent, TaskStatus};
use aivyx_sandbox::{PermissionPrompter, PermissionRequest, UserResponse};
use aivyx_types::{ToolCall, ToolOutput};
use tokio::sync::mpsc::UnboundedReceiver;
use tokio_util::sync::CancellationToken;

/// Denies anything that would need a person. Autonomous mode's gate settles
/// every request itself, so this is only a backstop.
pub struct NoHumanPrompter;

#[async_trait::async_trait]
impl PermissionPrompter for NoHumanPrompter {
    async fn prompt(&self, request: &PermissionRequest) -> UserResponse {
        tracing::warn!(
            tool = %request.tool_name,
            "headless: a permission prompt was denied (no one to ask)"
        );
        UserResponse::Deny
    }
}

/// The process exit code for how the run ended.
pub fn exit_code(end: &RunEnd) -> i32 {
    match end.outcome {
        Outcome::GoalAchieved
            if end.last_verification_passed != Some(false) && end.final_checks_passed =>
        {
            0
        }
        Outcome::GoalAchieved => 1,
        Outcome::BudgetExhausted => 2,
        Outcome::Failed => 3,
        Outcome::InjectionDetected => 4,
        Outcome::Cancelled => 130,
    }
}

/// Longest a one-line summary gets before it's cut.
const MAX_SUMMARY_CHARS: usize = 160;

/// The first line of `text`, cut to `MAX_SUMMARY_CHARS`.
fn first_line(text: &str) -> String {
    let line = text.lines().next().unwrap_or_default();
    if line.chars().count() > MAX_SUMMARY_CHARS {
        let cut: String = line.chars().take(MAX_SUMMARY_CHARS).collect();
        format!("{cut}…")
    } else {
        line.to_string()
    }
}

/// What a tool call acts on, from its usual argument names.
fn tool_target(call: &ToolCall) -> String {
    ["path", "command", "from", "url", "query", "pattern"]
        .iter()
        .find_map(|key| call.arguments.get(*key).and_then(|v| v.as_str()))
        .map(|target| format!(" {}", first_line(target)))
        .unwrap_or_default()
}

/// The progress line for one agent event, if it gets one. Streamed text is
/// handled by `Printer` (only with `--verbose`).
fn progress_line(event: &AgentEvent) -> Option<String> {
    match event {
        AgentEvent::ToolCallDetected(call) => {
            Some(format!("tool {}{}", call.name, tool_target(call)))
        }
        AgentEvent::ToolResult(result) => match &result.output {
            ToolOutput::Ok(text) if text.starts_with("exit status:") => {
                Some(format!("  {}", first_line(text)))
            }
            ToolOutput::Ok(_) => None,
            ToolOutput::Error(text) => Some(format!("  error: {}", first_line(text))),
            ToolOutput::Denied(text) => Some(format!("  denied: {}", first_line(text))),
        },
        AgentEvent::TasksUpdated(tasks) => {
            let done = tasks.iter().filter(|t| t.status == TaskStatus::Done).count();
            Some(format!("tasks: {done}/{} done", tasks.len()))
        }
        AgentEvent::TurnPaused(text) => Some(format!("paused: {}", first_line(text))),
        AgentEvent::Error(text) => Some(format!("error: {text}")),
        AgentEvent::Warning(text) => Some(format!("warning: {text}")),
        AgentEvent::Info(text) => Some(text.clone()),
        AgentEvent::ModelRouted { model, reason } => Some(format!("model {model}: {reason}")),
        AgentEvent::SubAgentActivity(inner) => {
            progress_line(inner).map(|line| format!("sub-agent: {line}"))
        }
        _ => None,
    }
}

/// Writes progress lines with the time since `started` in front:
/// `[0:12:31] tool run_command test`. With `verbose`, the model's streamed
/// text goes out as it arrives, and the next progress line starts on a new line.
struct Printer {
    started: Instant,
    verbose: bool,
    mid_text: bool,
}

impl Printer {
    fn new(verbose: bool) -> Self {
        Self { started: Instant::now(), verbose, mid_text: false }
    }

    fn stamp(&self) -> String {
        let secs = self.started.elapsed().as_secs();
        format!("[{}:{:02}:{:02}]", secs / 3600, secs / 60 % 60, secs % 60)
    }

    fn line(&mut self, text: &str, out: &mut impl Write) {
        if self.mid_text {
            let _ = writeln!(out);
            self.mid_text = false;
        }
        let stamp = self.stamp();
        for (i, part) in text.lines().enumerate() {
            let _ = if i == 0 {
                writeln!(out, "{stamp} {part}")
            } else {
                writeln!(out, "{} {part}", " ".repeat(stamp.len()))
            };
        }
    }

    fn event(&mut self, event: &AgentEvent, out: &mut impl Write) {
        if let AgentEvent::TextDelta(text) = event
            && self.verbose
        {
            let _ = write!(out, "{text}");
            self.mid_text = true;
            return;
        }
        if let Some(line) = progress_line(event) {
            self.line(&line, out);
        }
    }
}

/// Runs an `--auto` session without the TUI and returns the exit code.
pub async fn run(
    mut agent: Agent,
    mut events_rx: UnboundedReceiver<AgentEvent>,
    cwd: PathBuf,
    run: AutonomousRun,
    stop: CancellationToken,
    verbose: bool,
) -> i32 {
    let printer = Arc::new(Mutex::new(Printer::new(verbose)));
    printer.lock().unwrap().line(&format!("goal: {}", run.goal), &mut std::io::stderr().lock());
    if let Some(tests) = agent.tests() {
        printer.lock().unwrap().line(&tests.auto_line(), &mut std::io::stderr().lock());
    }
    let events_printer = Arc::clone(&printer);
    let printing = tokio::spawn(async move {
        while let Some(event) = events_rx.recv().await {
            events_printer.lock().unwrap().event(&event, &mut std::io::stderr().lock());
        }
    });
    let iteration_printer = Arc::clone(&printer);
    let mut on_iteration = move |n: u32| {
        let line = format!("iteration {n}");
        iteration_printer.lock().unwrap().line(&line, &mut std::io::stderr().lock());
    };
    let active = Mutex::new(None);
    let end =
        aivyx_core::autonomous::drive(&mut agent, &run, &cwd, &stop, &active, &mut on_iteration)
            .await;
    // Dropping the agent closes the event channel unless a tool kept a
    // sender; give the printer a moment to drain either way.
    drop(agent);
    if tokio::time::timeout(Duration::from_secs(2), printing).await.is_err() {
        tracing::warn!("headless: progress printer didn't finish draining");
    }
    let mut stdout = std::io::stdout().lock();
    let _ = writeln!(stdout, "{}", end.notice);
    let _ = stdout.flush();
    exit_code(&end)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aivyx_types::{ToolCallId, ToolCallSource, ToolResult};

    fn end(outcome: Outcome, verified: Option<bool>, checks: bool) -> RunEnd {
        RunEnd {
            outcome,
            notice: String::new(),
            iterations: 1,
            last_verification_passed: verified,
            final_checks_passed: checks,
        }
    }

    #[test]
    fn exit_codes_follow_the_outcome() {
        assert_eq!(exit_code(&end(Outcome::GoalAchieved, Some(true), true)), 0);
        assert_eq!(exit_code(&end(Outcome::GoalAchieved, None, true)), 0, "verification never ran");
        assert_eq!(exit_code(&end(Outcome::GoalAchieved, Some(false), true)), 1);
        assert_eq!(exit_code(&end(Outcome::GoalAchieved, Some(true), false)), 1, "a final check failed");
        assert_eq!(exit_code(&end(Outcome::BudgetExhausted, None, true)), 2);
        assert_eq!(exit_code(&end(Outcome::Failed, None, true)), 3);
        assert_eq!(exit_code(&end(Outcome::InjectionDetected, None, true)), 4);
        assert_eq!(exit_code(&end(Outcome::Cancelled, None, true)), 130);
    }

    fn call(name: &str, arguments: serde_json::Value) -> AgentEvent {
        AgentEvent::ToolCallDetected(ToolCall {
            id: ToolCallId("c1".into()),
            name: name.into(),
            arguments,
            source: ToolCallSource::Native,
        })
    }

    fn result(output: ToolOutput) -> AgentEvent {
        AgentEvent::ToolResult(ToolResult { call_id: ToolCallId("c1".into()), output })
    }

    #[test]
    fn events_become_short_progress_lines() {
        let line = |e: &AgentEvent| progress_line(e);
        assert_eq!(line(&call("run_command", serde_json::json!({"command": "test"}))).unwrap(), "tool run_command test");
        assert_eq!(line(&call("edit_file", serde_json::json!({"path": "src/lib.rs"}))).unwrap(), "tool edit_file src/lib.rs");
        assert_eq!(line(&call("set_tasks", serde_json::json!({"tasks": []}))).unwrap(), "tool set_tasks");
        assert_eq!(
            line(&result(ToolOutput::Ok("exit status: 101 (failed)\n--- stdout ---\nlots".into()))).unwrap(),
            "  exit status: 101 (failed)"
        );
        assert_eq!(line(&result(ToolOutput::Ok("file contents".into()))), None);
        assert_eq!(line(&result(ToolOutput::Denied("outside the worktree".into()))).unwrap(), "  denied: outside the worktree");
        assert_eq!(line(&AgentEvent::TextDelta("hi".into())), None);
        assert_eq!(line(&AgentEvent::Warning("Checkpoint failed".into())).unwrap(), "warning: Checkpoint failed");
        let sub = AgentEvent::SubAgentActivity(Box::new(call("grep", serde_json::json!({"pattern": "fn main"}))));
        assert_eq!(line(&sub).unwrap(), "sub-agent: tool grep fn main");
    }

    #[test]
    fn long_targets_are_cut() {
        let long = "x".repeat(500);
        let line = progress_line(&call("read_file", serde_json::json!({"path": long}))).unwrap();
        assert!(line.chars().count() < 200, "{line}");
        assert!(line.ends_with('…'));
    }

    #[test]
    fn the_printer_stamps_lines_and_keeps_streamed_text_apart() {
        let mut out = Vec::new();
        let mut printer = Printer::new(true);
        printer.event(&AgentEvent::TextDelta("thinking".into()), &mut out);
        printer.line("iteration 2", &mut out);
        printer.line("error: one\ntwo", &mut out);
        let text = String::from_utf8(out).unwrap();
        assert_eq!(text, "thinking\n[0:00:00] iteration 2\n[0:00:00] error: one\n          two\n");
    }

    #[test]
    fn without_verbose_streamed_text_is_not_printed() {
        let mut out = Vec::new();
        let mut printer = Printer::new(false);
        printer.event(&AgentEvent::TextDelta("thinking".into()), &mut out);
        assert!(out.is_empty());
    }
}
