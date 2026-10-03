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

pub(super) const TEST_TIMEOUT: Duration =
    Duration::from_secs(crate::test_detect::TEST_TIMEOUT_SECS);
const NOTE_LINES: usize = 80;
const MAX_LINE_CHARS: usize = 2000;
/// Per-line cap *while reading*, in bytes — generous relative to
/// `MAX_LINE_CHARS` (which `clip` applies afterwards, in chars, for
/// display) so this only ever bites a line with no newline for a very
/// long time, not an ordinary over-long one. Keeps `CappedLines`'s
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

/// Wraps a reader with a persistent, cancel-safe line reader: tolerant of
/// non-UTF-8 bytes (decoded with `String::from_utf8_lossy` instead of
/// failing) and bounded in memory (at most `cap` bytes of the current
/// line are kept, the rest discarded while still reading through to the
/// real newline, so a line that never ends — `yes | tr -d '\n'` — can't
/// grow the buffer without bound).
///
/// The partial-line accumulator (`buf`) lives in `self`, not in a
/// `next_line` call's own stack frame — load-bearing for `tokio::select!`:
/// each iteration of `run_tests`'s drain loop calls `next_line()` on both
/// `stdout`/`stderr` concurrently, and whichever one doesn't win that
/// `select!` has its in-flight future *dropped*. Every await point inside
/// `next_line` (`fill_buf().await`) is reached only once the previous
/// `fill_buf` result has already been folded into `self.buf` and consumed
/// from the reader — so a drop at that point loses nothing: the next call
/// to `next_line()` resumes from `self.buf` exactly where the dropped one
/// left off. An earlier version kept this accumulator as a local `let mut
/// buf` inside a free `read_capped_line` function instead; a fresh call
/// was made each drain-loop iteration, so a drop mid-call (after one or
/// more fill_buf/consume cycles within that same call, each of which had
/// already appended to that call's *local* `buf`) silently discarded
/// those already-consumed-but-not-yet-returned bytes — reproduced as a
/// stdout line losing its leading bytes whenever a stderr line completed
/// first while stdout was mid-line (or vice versa).
struct CappedLines<R> {
    reader: R,
    buf: Vec<u8>,
    cap: usize,
}

impl<R> CappedLines<R>
where
    R: tokio::io::AsyncBufRead + Unpin,
{
    fn new(reader: R, cap: usize) -> Self {
        Self {
            reader,
            buf: Vec::new(),
            cap,
        }
    }

    /// Reads one line. Strips a trailing `\n` (and `\r\n`). Returns `None`
    /// only once the stream is truly exhausted: real EOF with no bytes
    /// left to return, or an I/O error, which is treated like EOF (the
    /// stream is closed) rather than stopping the drain early — see
    /// `drain_capped_tail` in `aivyx-tools/src/process.rs`, which the old
    /// `BufReader::lines()` violated by returning `Ok(None)` on an `Err`,
    /// looking identical to real EOF to its caller — the actual bug: a
    /// non-UTF-8 line made `.lines()` return `Err(InvalidData)`, which the
    /// old `_ => { out_open = false; None }` handling here treated as EOF,
    /// so draining stopped while the child was still writing and the
    /// child then blocked on a full pipe.
    async fn next_line(&mut self) -> Option<String> {
        loop {
            let (done, used) = match self.reader.fill_buf().await {
                Ok([]) => {
                    // Real EOF.
                    return if self.buf.is_empty() {
                        None
                    } else {
                        Some(finish_capped_line(std::mem::take(&mut self.buf)))
                    };
                }
                Ok(available) => {
                    if let Some(i) = available.iter().position(|&b| b == b'\n') {
                        if self.buf.len() < self.cap {
                            let take = (self.cap - self.buf.len()).min(i + 1);
                            self.buf.extend_from_slice(&available[..take]);
                        }
                        (true, i + 1)
                    } else {
                        if self.buf.len() < self.cap {
                            let take = (self.cap - self.buf.len()).min(available.len());
                            self.buf.extend_from_slice(&available[..take]);
                        }
                        (false, available.len())
                    }
                }
                Err(_) => {
                    // Treat an I/O error as the stream being closed,
                    // exactly like `drain_capped_tail` — never silently
                    // stop draining a pipe that's still open.
                    return if self.buf.is_empty() {
                        None
                    } else {
                        Some(finish_capped_line(std::mem::take(&mut self.buf)))
                    };
                }
            };
            // Nothing above awaited since `used` bytes were taken from
            // `self.reader`'s internal buffer, so this `consume` and the
            // `self.buf` append above are already both done by the time
            // execution can next suspend (at the top of the loop) — a
            // drop there loses only bytes the reader hasn't handed out
            // yet, never ones already folded into `self.buf`.
            self.reader.consume(used);
            if done {
                return Some(finish_capped_line(std::mem::take(&mut self.buf)));
            }
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
        let mut stdout = CappedLines::new(BufReader::new(child.stdout.take().expect("piped")), MAX_LINE_BYTES);
        let mut stderr = CappedLines::new(BufReader::new(child.stderr.take().expect("piped")), MAX_LINE_BYTES);
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
                    Some(line) => Some(line),
                    None => { out_open = false; None }
                },
                line = stderr.next_line(), if err_open => match line {
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

#[cfg(test)]
mod tests {
    use super::*;

    // Review finding 2 (MINOR): a direct unit test of `CappedLines` over
    // an in-memory reader, so the cap is exercised deterministically
    // rather than only inferred from `clip()` already truncating the
    // *display* string downstream (which `test_caps_a_very_long_line_...`
    // in `agent/tests.rs` can't tell apart from the read itself being
    // uncapped).
    #[tokio::test]
    async fn next_line_caps_bytes_kept_but_still_finds_the_next_line() {
        let data: &[u8] = b"aaaaaaaaaaaa\nnext\n";
        let mut lines = CappedLines::new(BufReader::new(data), 4);

        let first = lines.next_line().await.unwrap();
        assert_eq!(first, "aaaa", "{first:?}");
        let second = lines.next_line().await.unwrap();
        assert_eq!(second, "next", "{second:?}");
        assert!(lines.next_line().await.is_none());
    }
}
