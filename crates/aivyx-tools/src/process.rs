//! Process-execution helpers shared by `run_command` and `run_shell`: race a
//! concurrent, memory-bounded stdout/stderr drain against a timeout and
//! cancellation, killing the whole process group on either, then format
//! tail-truncated output.

use std::collections::VecDeque;
use std::process::ExitStatus;
use std::time::Duration;

use aivyx_types::ToolOutput;
use tokio::io::{AsyncRead, AsyncReadExt};

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
        stdout: (Vec<u8>, usize),
        stderr: (Vec<u8>, usize),
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
    // Makes the child its own process-group leader, so a timeout/cancellation
    // can kill the whole group (including anything it backgrounded, e.g.
    // `sh -c 'sleep 999 &'`) instead of only the direct child, which would
    // otherwise survive indefinitely as an orphan.
    command.process_group(0);

    let mut child = command
        .spawn()
        .map_err(|err| ToolError::ExecutionFailed(format!("failed to start command: {err}")))?;

    // Piped handles are taken separately from `child` itself, so `child`
    // stays available for killing in the other `select!` branches below —
    // `child.wait_with_output()` would consume `child` by value up front,
    // making it impossible to kill if a different branch wins the race.
    let stdout = child.stdout.take().expect("stdout was piped");
    let stderr = child.stderr.take().expect("stderr was piped");

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
        drain_capped_tail(stdout, MAX_OUTPUT_BYTES),
        drain_capped_tail(stderr, MAX_OUTPUT_BYTES),
    );

    let outcome = tokio::select! {
        (stdout, stderr) = drain => {
            let status = child
                .wait()
                .await
                .map_err(|err| ToolError::ExecutionFailed(format!("failed to reap command: {err}")))?;
            RunOutcome::Exited { status, stdout, stderr }
        }
        _ = tokio::time::sleep(timeout) => {
            kill_process_group(&child);
            let _ = child.wait().await;
            RunOutcome::TimedOut
        }
        _ = cancellation.cancelled() => {
            kill_process_group(&child);
            let _ = child.wait().await;
            RunOutcome::Cancelled
        }
    };

    match outcome {
        RunOutcome::Exited {
            status,
            stdout,
            stderr,
        } => Ok(ToolOutput::Ok(format_output(status, stdout, stderr))),
        RunOutcome::TimedOut => Err(ToolError::ExecutionFailed(format!(
            "command timed out after {:.0}s and was killed",
            timeout.as_secs_f64()
        ))),
        RunOutcome::Cancelled => Err(ToolError::ExecutionFailed(
            "command was cancelled".to_string(),
        )),
    }
}

/// Sends `SIGKILL` to the whole process group rather than just the direct
/// child (`Child::kill()`'s target) — a backgrounded/detached grandchild
/// (`sh -c 'sleep 999 &'`, `nohup ... &`) survives a single-PID kill
/// indefinitely as an orphan, since the direct `sh` process exits almost
/// immediately after backgrounding it. Requires the command to have been
/// spawned with `.process_group(0)` (done in `run` above), which makes the
/// child its own process-group leader — killing `-pid` then reaches it and
/// everything it spawned into the same group.
pub(crate) fn kill_process_group(child: &tokio::process::Child) {
    if let Some(pid) = child.id() {
        // SAFETY: sending a signal to a pid is a plain syscall wrapper with
        // no memory-safety concerns. `pid` is a process we just spawned
        // with `.process_group(0)`, so negating it targets that exact
        // group, which we're allowed to signal.
        unsafe {
            libc::kill(-(pid as i32), libc::SIGKILL);
        }
    }
}

/// Reads `reader` to completion, keeping only the *last* `cap` bytes seen
/// (evicting older bytes as new ones arrive, via a `VecDeque` used as a
/// ring buffer) rather than accumulating everything before truncating at
/// the end. This bounds memory throughout the *entire* read — a command
/// like `yes` or `cat /dev/zero` can no longer exhaust memory in the
/// seconds before a timeout fires — while still preserving Phase 4's
/// deliberate tail-truncation behavior (the actionable signal in build/test
/// output is almost always at the end). Returns the capped bytes plus the
/// true total byte count, so the caller can still report how much was cut.
///
/// Deliberately not `.take(cap)`: that stops *reading* once the cap is hit,
/// which would leave the underlying pipe undrained — the child would then
/// block writing to a full pipe, reintroducing the exact deadlock the
/// concurrent-drain design exists to avoid. This keeps reading (and
/// discarding the oldest bytes) for as long as the child keeps producing
/// output.
async fn drain_capped_tail(mut reader: impl AsyncRead + Unpin, cap: usize) -> (Vec<u8>, usize) {
    let mut buf: VecDeque<u8> = VecDeque::with_capacity(cap.min(64 * 1024));
    let mut scratch = [0u8; 8192];
    let mut total = 0usize;

    loop {
        match reader.read(&mut scratch).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                total += n;
                buf.extend(scratch[..n].iter().copied());
                if buf.len() > cap {
                    let excess = buf.len() - cap;
                    buf.drain(0..excess);
                }
            }
        }
    }

    (buf.into_iter().collect(), total)
}

fn format_output(status: ExitStatus, stdout: (Vec<u8>, usize), stderr: (Vec<u8>, usize)) -> String {
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
        format_stream(stdout.0, stdout.1),
        format_stream(stderr.0, stderr.1),
    )
}

/// Renders one already-capped stream, prefixing a truncation notice if the
/// true total (`total_bytes`) exceeded what was kept — the data itself is
/// never larger than `MAX_OUTPUT_BYTES` by the time this runs (capped
/// during collection by `drain_capped_tail`), so unlike the old
/// after-the-fact truncation this can't recompute "was it truncated" from
/// the data's own length; the caller must pass the true total along.
fn format_stream(data: Vec<u8>, total_bytes: usize) -> String {
    let text = String::from_utf8_lossy(&data).into_owned();
    if total_bytes > data.len() {
        format!(
            "[... {} bytes truncated ...]\n{text}",
            total_bytes - data.len()
        )
    } else {
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_stream_marks_truncation_using_the_true_total() {
        let result = format_stream(b"END_MARKER".to_vec(), MAX_OUTPUT_BYTES + 10);
        assert!(result.contains("truncated"));
        assert!(result.ends_with("END_MARKER"));
    }

    #[test]
    fn format_stream_is_unmarked_when_nothing_was_cut() {
        let result = format_stream(b"hello".to_vec(), 5);
        assert_eq!(result, "hello");
    }

    #[tokio::test]
    async fn drain_capped_tail_bounds_memory_and_keeps_the_tail() {
        // Feed far more than `cap` through a pipe so the reader must drain
        // it across many chunks rather than in one `read_to_end`-style call.
        let (mut writer, reader) = tokio::io::duplex(4096);
        let cap = 100;
        let total_written = cap * 50;

        let write_task = tokio::spawn(async move {
            use tokio::io::AsyncWriteExt;
            for i in 0..total_written {
                let byte = b'a' + (i % 26) as u8;
                writer.write_all(&[byte]).await.unwrap();
            }
            writer.write_all(b"END").await.unwrap();
            drop(writer);
        });

        let (data, total) = drain_capped_tail(reader, cap).await;
        write_task.await.unwrap();

        assert_eq!(total, total_written + 3);
        assert_eq!(data.len(), cap);
        assert!(data.ends_with(b"END"));
    }
}
