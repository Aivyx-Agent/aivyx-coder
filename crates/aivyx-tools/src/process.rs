//! Process-execution helpers shared by `run_command` and `run_shell`: race a
//! concurrent stdout/stderr drain against a timeout and cancellation,
//! killing the child on either, then format tail-truncated output.

use std::process::ExitStatus;
use std::time::Duration;

use aivyx_types::ToolOutput;
use tokio::io::AsyncReadExt;

use crate::ToolError;

/// Per-stream cap on captured output — tail-truncated (keep the *last* N
/// bytes), not head-truncated like `grep`/`glob`'s match caps: the
/// actionable signal in build/test output (the actual failing test, the
/// final compiler error) is almost always at the end.
pub(crate) const MAX_OUTPUT_BYTES: usize = 50 * 1024;

/// A single named, pre-configured command — used both as `run_command`'s
/// full menu and as `run_shell`'s auto-allow tier (an exact `(program,
/// args)` match skips the confirmation prompt entirely).
#[derive(Debug, Clone)]
pub struct CommandSpec {
    pub name: String,
    pub program: String,
    pub args: Vec<String>,
    pub timeout: Duration,
}

enum RunOutcome {
    Exited {
        status: ExitStatus,
        stdout: Vec<u8>,
        stderr: Vec<u8>,
    },
    TimedOut,
    Cancelled,
}

/// Runs an already-built (and already-confined, if applicable) command,
/// returning formatted output. A non-zero exit is normal `Ok` output — a
/// failing test/build run is expected, informative verification-loop
/// information, not a tool-level failure. `Err` is reserved for the tool
/// failing to run the command at all (spawn failure, timeout, cancellation).
pub(crate) async fn run(
    mut command: tokio::process::Command,
    timeout: Duration,
    cancellation: tokio_util::sync::CancellationToken,
) -> Result<ToolOutput, ToolError> {
    let mut child = command
        .spawn()
        .map_err(|err| ToolError::ExecutionFailed(format!("failed to start command: {err}")))?;

    // Piped handles are taken separately from `child` itself, so `child`
    // stays available for `.kill()` in the other `select!` branches below —
    // `child.wait_with_output()` would consume `child` by value up front,
    // making it impossible to kill if a different branch wins the race.
    let mut stdout = child.stdout.take().expect("stdout was piped");
    let mut stderr = child.stderr.take().expect("stderr was piped");
    let mut out_buf = Vec::new();
    let mut err_buf = Vec::new();

    // Draining concurrently with waiting (rather than after `.wait()`
    // returns) avoids a deadlock: if the child fills its stdout/stderr OS
    // pipe buffer (commonly 64KB) before exiting, it blocks writing to a
    // full pipe while we'd be blocked in `.wait()` not yet reading it.
    //
    // Must be `futures::future::join` (an actual `Future` value that stays
    // unpolled until `select!` polls it), not `tokio::join!` — that macro
    // drives both reads to completion on its own statement, before control
    // ever reaches `select!`, which would defeat the race against the
    // timeout/cancellation branches below entirely.
    let drain = futures::future::join(
        stdout.read_to_end(&mut out_buf),
        stderr.read_to_end(&mut err_buf),
    );

    let outcome = tokio::select! {
        _ = drain => {
            let status = child
                .wait()
                .await
                .map_err(|err| ToolError::ExecutionFailed(format!("failed to reap command: {err}")))?;
            RunOutcome::Exited { status, stdout: out_buf, stderr: err_buf }
        }
        _ = tokio::time::sleep(timeout) => {
            let _ = child.kill().await;
            RunOutcome::TimedOut
        }
        _ = cancellation.cancelled() => {
            let _ = child.kill().await;
            RunOutcome::Cancelled
        }
    };

    match outcome {
        RunOutcome::Exited {
            status,
            stdout,
            stderr,
        } => Ok(ToolOutput::Ok(format_output(status, &stdout, &stderr))),
        RunOutcome::TimedOut => Err(ToolError::ExecutionFailed(format!(
            "command timed out after {:.0}s and was killed",
            timeout.as_secs_f64()
        ))),
        RunOutcome::Cancelled => Err(ToolError::ExecutionFailed(
            "command was cancelled".to_string(),
        )),
    }
}

fn format_output(status: ExitStatus, stdout: &[u8], stderr: &[u8]) -> String {
    let verdict = if status.success() {
        "success"
    } else {
        "failed"
    };
    let code = status
        .code()
        .map(|c| c.to_string())
        .unwrap_or_else(|| "signal".to_string());
    format!(
        "exit status: {code} ({verdict})\n--- stdout ---\n{}\n--- stderr ---\n{}",
        tail_truncated(stdout),
        tail_truncated(stderr),
    )
}

/// Keeps the *last* `MAX_OUTPUT_BYTES` of `data` (see the module doc for
/// why tail rather than head), decoded lossily so a truncation boundary
/// landing mid-UTF-8-sequence can't produce invalid output.
fn tail_truncated(data: &[u8]) -> String {
    if data.len() <= MAX_OUTPUT_BYTES {
        return String::from_utf8_lossy(data).into_owned();
    }
    let tail = &data[data.len() - MAX_OUTPUT_BYTES..];
    format!(
        "[... {} bytes truncated ...]\n{}",
        data.len() - MAX_OUTPUT_BYTES,
        String::from_utf8_lossy(tail)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tail_truncation_keeps_the_end_not_the_start() {
        let data = "a".repeat(MAX_OUTPUT_BYTES) + "END_MARKER";
        let result = tail_truncated(data.as_bytes());
        assert!(result.contains("truncated"));
        assert!(result.ends_with("END_MARKER"));
    }

    #[test]
    fn small_output_is_not_truncated() {
        let result = tail_truncated(b"hello");
        assert_eq!(result, "hello");
    }
}
