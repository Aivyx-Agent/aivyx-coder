//! `/test`: run the session's test command (configured, else detected —
//! see `crate::test_detect`) without asking — the user typed it — confined
//! like `run_command`, streaming each line to the frontend, then leave the
//! outcome as a note the model reads with the next message.

use std::collections::VecDeque;
use std::process::Stdio;
use std::time::{Duration, Instant};

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio_util::sync::CancellationToken;

use super::Agent;
use crate::agent::AgentEvent;

pub(super) const TEST_TIMEOUT: Duration = Duration::from_secs(600);
const NOTE_LINES: usize = 80;
const MAX_LINE_CHARS: usize = 2000;
const NO_TEST_COMMAND: &str = "No test command — set [verification] command in config.toml.";

pub(super) fn parse(user_input: &str) -> bool {
    crate::commands::parse_slash_command(user_input, "/test").is_some()
}

enum Outcome {
    Exited(std::process::ExitStatus),
    Cancelled,
    TimedOut,
}

/// More backticks than any run in `text`, at least three.
fn fence_for(text: &str) -> String {
    let mut longest = 0;
    let mut run = 0;
    for c in text.chars() {
        run = if c == '`' { run + 1 } else { 0 };
        longest = longest.max(run);
    }
    "`".repeat(longest.max(2) + 1)
}

fn clip(mut line: String) -> String {
    if line.chars().count() > MAX_LINE_CHARS {
        line = line.chars().take(MAX_LINE_CHARS).collect::<String>() + "…";
    }
    line
}

impl Agent {
    pub(super) async fn run_tests(&mut self, cwd: &std::path::Path, cancellation: CancellationToken) {
        let Some(tests) = self.tests.clone() else {
            self.info(NO_TEST_COMMAND);
            return;
        };
        let shown = tests.display();

        let mut command = tokio::process::Command::new(&tests.program);
        command
            .args(&tests.args)
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut command = self.executor.confiner().confine(command);
        // Own process group, so a timeout or Ctrl+C kills everything the
        // test run started (see `aivyx_tools::kill_process_group`).
        command.process_group(0);

        let started = Instant::now();
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(err) => {
                self.emit(AgentEvent::TestFinished {
                    summary: format!("Couldn't run `{shown}`: {err}"),
                    tail: String::new(),
                });
                return;
            }
        };
        let mut stdout = BufReader::new(child.stdout.take().expect("piped")).lines();
        let mut stderr = BufReader::new(child.stderr.take().expect("piped")).lines();
        let (mut out_open, mut err_open) = (true, true);
        let mut tail: VecDeque<String> = VecDeque::with_capacity(NOTE_LINES);
        let deadline = tokio::time::sleep(self.test_timeout);
        tokio::pin!(deadline);

        // Phase 1: stream lines until both pipes close (or time/cancel
        // runs out). No branch future borrows `child`, so the handlers can
        // kill it.
        let mut stopped: Option<Outcome> = None;
        while out_open || err_open {
            let line = tokio::select! {
                line = stdout.next_line(), if out_open => match line {
                    Ok(Some(line)) => Some(line),
                    _ => { out_open = false; None }
                },
                line = stderr.next_line(), if err_open => match line {
                    Ok(Some(line)) => Some(line),
                    _ => { err_open = false; None }
                },
                _ = &mut deadline => {
                    aivyx_tools::kill_process_group(&child);
                    stopped = Some(Outcome::TimedOut);
                    break;
                }
                _ = cancellation.cancelled() => {
                    aivyx_tools::kill_process_group(&child);
                    stopped = Some(Outcome::Cancelled);
                    break;
                }
            };
            if let Some(line) = line {
                let line = clip(line);
                if tail.len() == NOTE_LINES {
                    tail.pop_front();
                }
                tail.push_back(line.clone());
                self.emit(AgentEvent::TestOutput(line));
            }
        }
        // Phase 2: reap. A child that closed its pipes but keeps running
        // is still bounded by the same deadline and Ctrl+C.
        let outcome = match stopped {
            Some(outcome) => {
                let _ = child.wait().await;
                outcome
            }
            None => {
                let waited = tokio::select! {
                    status = child.wait() => Some(status),
                    _ = &mut deadline => None,
                    _ = cancellation.cancelled() => None,
                };
                match waited {
                    Some(Ok(status)) => Outcome::Exited(status),
                    Some(Err(_)) => Outcome::Cancelled,
                    None => {
                        aivyx_tools::kill_process_group(&child);
                        let _ = child.wait().await;
                        if cancellation.is_cancelled() { Outcome::Cancelled } else { Outcome::TimedOut }
                    }
                }
            }
        };

        let secs = started.elapsed().as_secs_f64();
        let summary = match outcome {
            Outcome::Exited(status) if status.success() => format!("Tests passed ({secs:.1} s)"),
            Outcome::Exited(status) => match status.code() {
                Some(code) => format!("Tests failed (exit {code}, {secs:.1} s)"),
                None => format!("Tests failed (killed by a signal, {secs:.1} s)"),
            },
            Outcome::Cancelled => "Tests cancelled".to_string(),
            Outcome::TimedOut => {
                format!("Tests timed out after {} min", self.test_timeout.as_secs() / 60)
            }
        };
        let tail = Vec::from(tail).join("\n");

        // Test output is text the model will read — scanned like any
        // tool result (see `record_tool_result`).
        if let Some(finding) = aivyx_sandbox::scan_for_injection_markers(&tail, "/test output") {
            self.injection_taint.flag(finding);
        }
        let fence = fence_for(&tail);
        self.pending_notes.push(format!(
            "The user ran the tests (`{shown}`): {summary}. Last lines of output:\n{fence}\n{tail}\n{fence}"
        ));
        self.persist_if_owned();
        self.emit(AgentEvent::TestFinished { summary, tail });
    }
}
