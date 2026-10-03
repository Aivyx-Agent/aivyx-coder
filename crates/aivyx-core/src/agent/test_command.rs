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
/// Per-line cap *while reading*, in bytes — generous relative to
/// `MAX_LINE_CHARS` (which `clip` applies afterwards, in chars, for
/// display) so this only ever bites a line with no newline for a very
/// long time, not an ordinary over-long one. Keeps `read_capped_line`'s
/// buffer bounded for output like `yes | tr -d '\n'`, which never hits a
/// newline at all.
const MAX_LINE_BYTES: usize = 4 * MAX_LINE_CHARS;
const NO_TEST_COMMAND: &str = "No test command — set [verification] command in config.toml.";
/// Identifies a `/test` note among `pending_notes` so a new run replaces
/// the previous one instead of stacking (other notes, e.g. `/undo`'s, use
/// a different prefix and are left alone).
const TEST_NOTE_PREFIX: &str = "The user ran the tests (";

pub(super) fn parse(user_input: &str) -> bool {
    crate::commands::parse_slash_command(user_input, "/test").is_some()
}

enum Outcome {
    Exited(std::process::ExitStatus),
    Cancelled,
    TimedOut,
    /// `child.wait()` itself returned an `Err` (e.g. the OS failed to
    /// reap the process) — distinct from `Cancelled`/`TimedOut`, which are
    /// deliberate outcomes we chose; this one is a real I/O failure we
    /// didn't ask for and shouldn't misreport as a cancellation.
    WaitFailed(String),
}

/// Reads one line from `reader`, tolerant of non-UTF-8 bytes (decoded with
/// `String::from_utf8_lossy` instead of failing) and bounded in memory: at
/// most `cap` bytes of the line are kept, with the rest discarded while
/// still reading through to the real newline — so a line that never ends
/// (`yes | tr -d '\n'`) can't grow the buffer without bound. Strips a
/// trailing `\n` (and `\r\n`).
///
/// Returns `None` only once the stream is truly exhausted: real EOF with
/// no bytes left to return, or an I/O error, which is treated like EOF
/// (the stream is closed) rather than stopping the drain early — see
/// `drain_capped_tail` in `aivyx-tools/src/process.rs`, which the old
/// `BufReader::lines()` violated by returning `Ok(None)` on an `Err`,
/// looking identical to real EOF to its caller — the actual bug: a
/// non-UTF-8 line made `.lines()` return `Err(InvalidData)`, which this
/// module's old `_ => { out_open = false; None }` handling treated as
/// EOF, so draining stopped while the child was still writing and the
/// child then blocked on a full pipe.
async fn read_capped_line<R>(reader: &mut R, cap: usize) -> Option<String>
where
    R: tokio::io::AsyncBufRead + Unpin,
{
    let mut buf: Vec<u8> = Vec::new();
    loop {
        let (done, used) = match reader.fill_buf().await {
            Ok([]) => {
                // Real EOF.
                return if buf.is_empty() {
                    None
                } else {
                    Some(finish_capped_line(buf))
                };
            }
            Ok(available) => {
                if let Some(i) = available.iter().position(|&b| b == b'\n') {
                    if buf.len() < cap {
                        let take = (cap - buf.len()).min(i + 1);
                        buf.extend_from_slice(&available[..take]);
                    }
                    (true, i + 1)
                } else {
                    if buf.len() < cap {
                        let take = (cap - buf.len()).min(available.len());
                        buf.extend_from_slice(&available[..take]);
                    }
                    (false, available.len())
                }
            }
            Err(_) => {
                // Treat an I/O error as the stream being closed, exactly
                // like `drain_capped_tail` — never silently stop draining
                // a pipe that's still open.
                return if buf.is_empty() {
                    None
                } else {
                    Some(finish_capped_line(buf))
                };
            }
        };
        reader.consume(used);
        if done {
            return Some(finish_capped_line(buf));
        }
    }
}

fn finish_capped_line(mut buf: Vec<u8>) -> String {
    if buf.last() == Some(&b'\n') {
        buf.pop();
        if buf.last() == Some(&b'\r') {
            buf.pop();
        }
    }
    String::from_utf8_lossy(&buf).into_owned()
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
        let mut stdout = BufReader::new(child.stdout.take().expect("piped"));
        let mut stderr = BufReader::new(child.stderr.take().expect("piped"));
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
                line = read_capped_line(&mut stdout, MAX_LINE_BYTES), if out_open => match line {
                    Some(line) => Some(line),
                    None => { out_open = false; None }
                },
                line = read_capped_line(&mut stderr, MAX_LINE_BYTES), if err_open => match line {
                    Some(line) => Some(line),
                    None => { err_open = false; None }
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
                    Some(Err(err)) => Outcome::WaitFailed(err.to_string()),
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
            Outcome::WaitFailed(err) => format!("Tests failed (couldn't wait for the process: {err})"),
        };
        let tail = Vec::from(tail).join("\n");

        // Test output is text the model will read — scanned like any
        // tool result (see `record_tool_result`).
        if let Some(finding) = aivyx_sandbox::scan_for_injection_markers(&tail, "/test output") {
            self.injection_taint.flag(finding);
        }
        let fence = fence_for(&tail);
        // A new `/test` note replaces any earlier one still pending (e.g.
        // the model hasn't had a turn since) rather than stacking —
        // other notes (`/undo`'s, etc.) use a different prefix and are
        // left untouched, and removing then re-pushing keeps their
        // relative order otherwise.
        self.pending_notes
            .retain(|note| !note.starts_with(TEST_NOTE_PREFIX));
        self.pending_notes.push(format!(
            "{TEST_NOTE_PREFIX}`{shown}`): {summary}. Last lines of output:\n{fence}\n{tail}\n{fence}"
        ));
        self.persist_if_owned();
        self.emit(AgentEvent::TestFinished { summary, tail });
    }
}
