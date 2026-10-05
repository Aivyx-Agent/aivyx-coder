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
///
/// The command runs in its own process group, and the whole group is
/// killed as soon as the direct child exits, times out, is cancelled, or
/// this future is dropped (see `ProcessGroup`): nothing it backgrounded
/// (`cargo run &`, a daemonised grandchild) outlives the tool call, and a
/// background job still holding the stdout/stderr pipes can't keep the
/// call waiting until the timeout.
pub async fn run(
    mut command: tokio::process::Command,
    timeout: Duration,
    cancellation: tokio_util::sync::CancellationToken,
) -> Result<ToolOutput, ToolError> {
    // Makes the child its own process-group leader (`LandlockConfiner`
    // already does; `NoopConfiner` doesn't), so the group can be killed
    // as a whole instead of only the direct child.
    command.process_group(0);

    let mut child = command
        .spawn()
        .map_err(|err| ToolError::ExecutionFailed(format!("failed to start command: {err}")))?;
    let mut group = ProcessGroup::of(&child);

    // Piped handles are taken separately from `child` itself, so `child`
    // stays available for killing after the race below.
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
    // Once the direct child exits, kill the rest of its group: whatever
    // it left behind holds no claim on the call, and killing it closes
    // any pipe ends it inherited, so the drain above can finish.
    let reap = async {
        let status = child.wait().await;
        group.kill();
        status
    };

    let outcome = tokio::select! {
        ((stdout, stderr), status) = futures::future::join(drain, reap) => {
            let status = status
                .map_err(|err| ToolError::ExecutionFailed(format!("failed to reap command: {err}")))?;
            RunOutcome::Exited { status, stdout, stderr }
        }
        _ = tokio::time::sleep(timeout) => RunOutcome::TimedOut,
        _ = cancellation.cancelled() => RunOutcome::Cancelled,
    };
    if !matches!(outcome, RunOutcome::Exited { .. }) {
        group.kill();
        let _ = child.wait().await;
    }

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

/// `Command::output` for a confined command: spawns `command` as the
/// leader of its own process group, collects whatever stdout/stderr the
/// caller piped (an unpiped stream comes back empty; stdin is the
/// caller's to set, normally `Stdio::null()`), and kills the group as soon
/// as the direct child exits, or when this future is dropped (a timeout
/// or cancellation around it). Like `run`, it doesn't wait for a
/// background job that still holds the pipes.
pub async fn output_in_group(
    mut command: tokio::process::Command,
) -> std::io::Result<std::process::Output> {
    command.process_group(0);
    let mut child = command.spawn()?;
    let mut group = ProcessGroup::of(&child);
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let read_all = |stream: Option<Box<dyn AsyncRead + Unpin + Send>>| async move {
        let mut buf = Vec::new();
        if let Some(mut stream) = stream {
            let _ = stream.read_to_end(&mut buf).await;
        }
        buf
    };
    let reap = async {
        let status = child.wait().await;
        group.kill();
        status
    };
    let ((stdout, stderr), status) = futures::future::join(
        futures::future::join(
            read_all(stdout.map(|s| Box::new(s) as Box<dyn AsyncRead + Unpin + Send>)),
            read_all(stderr.map(|s| Box::new(s) as Box<dyn AsyncRead + Unpin + Send>)),
        ),
        reap,
    )
    .await;
    Ok(std::process::Output {
        status: status?,
        stdout,
        stderr,
    })
}

/// The process group a spawned command leads, recorded right after
/// `spawn()` (`Child::id()` is gone once the child has been reaped).
/// `kill` sends `SIGKILL` to the whole group, once; dropping a
/// `ProcessGroup` that hasn't been killed yet kills it, so a call that
/// ends any way at all (finished, timed out, cancelled, its future
/// dropped, its owner torn down) takes everything it started with it.
///
/// The command must actually lead the group: spawned with
/// `.process_group(0)` (or `setsid()`), which `LandlockConfiner` does for
/// every confined command and the helpers here do for `NoopConfiner`
/// too. A `kill` on a pid that leads no group is a harmless `ESRCH`. Kill
/// promptly once the leader is reaped: the number is only reserved while
/// some member of the group is alive.
#[derive(Debug)]
pub struct ProcessGroup {
    pgid: Option<u32>,
}

impl ProcessGroup {
    pub fn of(child: &tokio::process::Child) -> Self {
        Self { pgid: child.id() }
    }

    /// Kills the group if it hasn't been killed already.
    pub fn kill(&mut self) {
        if let Some(pgid) = self.pgid.take() {
            kill_process_group(pgid);
        }
    }
}

impl Drop for ProcessGroup {
    fn drop(&mut self) {
        self.kill();
    }
}

/// Sends `SIGKILL` to process group `pgid` (see `ProcessGroup`). With the
/// `sandbox-backend` feature this is `aivyx-confine`'s own
/// `kill_process_group`; without it that function is a no-op (its
/// `NoopConfiner` never makes a group), but the helpers here still put
/// every command in one, so this sends the signal itself. Failures (the
/// group is already gone) are ignored.
pub fn kill_process_group(pgid: u32) {
    #[cfg(feature = "sandbox-backend")]
    {
        let _ = aivyx_sandbox::kill_process_group(pgid);
    }
    #[cfg(not(feature = "sandbox-backend"))]
    {
        // Same validation as aivyx-confine's: never 0 (our own group) or
        // a value that isn't a positive pid_t.
        if let Ok(pgid) = libc::pid_t::try_from(pgid)
            && pgid > 0
        {
            // SAFETY: plain syscall with a validated, positive group id.
            unsafe {
                libc::killpg(pgid, libc::SIGKILL);
            }
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

    fn sh(script: &str, cwd: &std::path::Path) -> tokio::process::Command {
        let mut command = tokio::process::Command::new("sh");
        command
            .arg("-c")
            .arg(script)
            .current_dir(cwd)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        command
    }

    /// A background job that would write `late.txt` a second after the
    /// command returns.
    const LATE_WRITER: &str = "(sleep 1; echo late > late.txt) >/dev/null 2>&1 & echo started";

    #[tokio::test]
    async fn run_kills_a_background_job_when_the_command_finishes() {
        let dir = tempfile::tempdir().unwrap();
        let output = run(
            sh(LATE_WRITER, dir.path()),
            Duration::from_secs(20),
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap();
        let ToolOutput::Ok(text) = output else { panic!("expected Ok") };
        assert!(text.contains("started"), "{text}");

        tokio::time::sleep(Duration::from_millis(1500)).await;
        assert!(
            !dir.path().join("late.txt").exists(),
            "the background job outlived the tool call"
        );
    }

    #[tokio::test]
    async fn run_returns_when_the_command_exits_even_if_a_background_job_holds_its_pipes() {
        let dir = tempfile::tempdir().unwrap();
        let started = std::time::Instant::now();
        let output = run(
            sh("sleep 30 & echo started", dir.path()),
            Duration::from_secs(20),
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap();
        let ToolOutput::Ok(text) = output else { panic!("expected Ok") };
        assert!(text.contains("exit status: 0 (success)"), "{text}");
        assert!(text.contains("started"), "{text}");
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "waited for the background job instead of the command"
        );
    }

    #[tokio::test]
    async fn run_kills_a_background_job_when_cancelled() {
        let dir = tempfile::tempdir().unwrap();
        let cancellation = tokio_util::sync::CancellationToken::new();
        let script = "(sleep 1; echo late > late.txt) >/dev/null 2>&1 & sleep 30";
        let cancel = cancellation.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            cancel.cancel();
        });
        let result = run(sh(script, dir.path()), Duration::from_secs(20), cancellation).await;
        assert!(result.is_err());

        tokio::time::sleep(Duration::from_millis(1500)).await;
        assert!(!dir.path().join("late.txt").exists());
    }

    #[tokio::test]
    async fn dropping_the_run_future_kills_the_process_group() {
        let dir = tempfile::tempdir().unwrap();
        let script = "(sleep 1; echo late > late.txt) >/dev/null 2>&1 & sleep 30";
        let call = run(
            sh(script, dir.path()),
            Duration::from_secs(20),
            tokio_util::sync::CancellationToken::new(),
        );
        // Dropped mid-run, the way an aborted turn drops it.
        assert!(tokio::time::timeout(Duration::from_millis(200), call).await.is_err());

        tokio::time::sleep(Duration::from_millis(1500)).await;
        assert!(!dir.path().join("late.txt").exists());
    }

    #[tokio::test]
    async fn output_in_group_collects_output_and_kills_a_background_job() {
        let dir = tempfile::tempdir().unwrap();
        let output = output_in_group(sh(LATE_WRITER, dir.path())).await.unwrap();
        assert!(output.status.success());
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "started");

        tokio::time::sleep(Duration::from_millis(1500)).await;
        assert!(!dir.path().join("late.txt").exists());
    }

    #[tokio::test]
    async fn output_in_group_does_not_wait_for_a_background_job_holding_its_pipes() {
        let dir = tempfile::tempdir().unwrap();
        let started = std::time::Instant::now();
        let output = output_in_group(sh("sleep 30 & echo started", dir.path()))
            .await
            .unwrap();
        assert!(output.status.success());
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[tokio::test]
    async fn dropping_a_process_group_kills_every_member() {
        let dir = tempfile::tempdir().unwrap();
        let mut command = sh("(sleep 1; echo late > late.txt) & sleep 30", dir.path());
        command.process_group(0);
        let mut child = command.spawn().unwrap();
        let group = ProcessGroup::of(&child);
        drop(group);
        let _ = child.wait().await;

        tokio::time::sleep(Duration::from_millis(1500)).await;
        assert!(!dir.path().join("late.txt").exists());
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
