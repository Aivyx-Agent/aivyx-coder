use std::collections::VecDeque;
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use aivyx_sandbox::{ActionKind, PermissionRequest, PermissionTarget};
use aivyx_types::{ToolDefinition, ToolOutput};
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex as AsyncMutex;

use crate::{Tool, ToolError, ToolExecutionContext};

/// Shared, process-lifetime-scoped slot for the one REPL session this
/// project supports at a time (see the design spec's "Concurrency"
/// decision). `tokio::sync::Mutex`, not `std::sync::Mutex` — every tool in
/// this file needs to hold the lock across `.await` points (writing to
/// stdin, reaping the child on stop/idle-timeout), which is exactly what
/// `mcp/mod.rs` and `lsp/mod.rs` already use `tokio::sync::Mutex` for.
pub type SharedReplSession = Arc<AsyncMutex<Option<ReplSession>>>;

/// Constructed once in `agent_builder.rs` and cloned into all three
/// `repl_*` tool instances — same pattern as `SetTasksTool`'s shared task
/// list.
pub fn new_shared_repl_session() -> SharedReplSession {
    Arc::new(AsyncMutex::new(None))
}

/// One live, persistent child process and everything needed to interact
/// with it across multiple tool calls. Never persisted (no `Serialize`) —
/// a REPL session is process-lifetime-scoped, not conversation-lifetime-
/// scoped, and does not survive `--resume`.
pub struct ReplSession {
    child: tokio::process::Child,
    /// The pty master — used for both writing input (`repl_send`) and
    /// resizing (`resize`, Task 3). A `tokio::fs::File` wrapping a raw fd
    /// gives async read/write via tokio's blocking-pool dispatch (fine
    /// for this tool's modest I/O volume) while `AsRawFd` stays available
    /// for the `ioctl(TIOCSWINSZ)` resize call. A separate fd, dup'd from
    /// the same master, is handed to the background reader task in
    /// `ReplStartTool::execute` — reading and writing happen from two
    /// different tasks concurrently, so each needs its own `File` value;
    /// duplicated fds on a character device like a pty share no file
    /// offset to worry about (unlike a real file), so this is safe.
    pty_master: tokio::fs::File,
    /// Everything the pty master has produced, continuously appended to
    /// by the single background reader task spawned in
    /// `ReplStartTool::execute` — this is what makes polling a
    /// long-running process (no `repl_send` input, just checking for new
    /// output) work, since output keeps accumulating even between calls.
    /// A plain `std::sync::Mutex`, not the async one above: every touch
    /// of this buffer is a short, synchronous append-or-drain, never held
    /// across an `.await`.
    output: Arc<std::sync::Mutex<VecDeque<u8>>>,
    last_activity: Instant,
    program: String,
    args: Vec<String>,
}

/// Process-exit safety net: kills the process group whenever a
/// `ReplSession` value is dropped, however that happens — including
/// `aivyx-coder`'s own shutdown (Ctrl+C-to-quit or a normal quit), when
/// the `Arc`-shared state holding it is torn down along with the rest of
/// `Agent`. Deliberately NOT `Command::kill_on_drop(true)` (used by
/// `mcp/mod.rs` for its own child processes) — that only kills the direct
/// child PID, not the whole process group, so a backgrounded grandchild
/// (`npm run dev` spawning its own child watcher) would survive as an
/// orphan. `Drop::drop` is synchronous, so this can only kill, not reap
/// (`.wait()` is async) — that's fine: every code path that removes a
/// session from the shared slot on purpose (`idle_watcher`, `repl_stop`,
/// `repl_send`'s exit detection) already reaps explicitly; this impl
/// firing again afterward on the same, already-dead process group is a
/// harmless no-op (`kill` on an already-gone pid just returns `ESRCH`).
impl Drop for ReplSession {
    fn drop(&mut self) {
        crate::process::kill_process_group(&self.child);
    }
}

impl ReplSession {
    /// Sets the pty's window size — called at `repl_start` time (sized to
    /// `aivyx-coder`'s own current terminal, or a fixed 80x24 fallback
    /// under a frontend with no real terminal) and live, whenever the
    /// TUI's own terminal resizes (see Task 3's `ReplResizeTarget`).
    pub(crate) fn resize(&self, cols: u16, rows: u16) {
        let ws = libc::winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // SAFETY: `ioctl(TIOCSWINSZ)` on a valid, still-open pty master
        // fd is always safe to attempt; a failure here (e.g. the pty was
        // just torn down) has no observable effect worth surfacing — a
        // resize is best-effort by nature.
        unsafe {
            libc::ioctl(self.pty_master.as_raw_fd(), libc::TIOCSWINSZ, &ws);
        }
    }
}

/// Reads `reader` to EOF (or error), appending every chunk into `output`
/// and evicting the oldest bytes past `crate::process::MAX_OUTPUT_BYTES` —
/// same tail-cap eviction technique as `process.rs`'s `drain_capped_tail`,
/// but persistent (runs for the process's whole lifetime, appending
/// across many `repl_send` calls) rather than one-shot-to-EOF, which is
/// why this is a fresh implementation rather than a call to that function.
fn spawn_output_reader(
    mut reader: impl tokio::io::AsyncRead + Unpin + Send + 'static,
    output: Arc<std::sync::Mutex<VecDeque<u8>>>,
) {
    tokio::spawn(async move {
        let mut scratch = [0u8; 8192];
        loop {
            match reader.read(&mut scratch).await {
                Ok(0) | Err(_) => return,
                Ok(n) => {
                    let mut buf = output.lock().unwrap();
                    buf.extend(scratch[..n].iter().copied());
                    if buf.len() > crate::process::MAX_OUTPUT_BYTES {
                        let excess = buf.len() - crate::process::MAX_OUTPUT_BYTES;
                        buf.drain(0..excess);
                    }
                }
            }
        }
    });
}

/// Polls `output` every 20ms, resetting a "quiet since" timer on every
/// change in length, and returns once that timer exceeds `quiet_window`
/// with nothing new — or once `max_wait` has elapsed in total regardless,
/// whichever comes first. This is how `repl_start`/`repl_send` decide
/// "the process is done producing output for this call" with no
/// per-program prompt-pattern knowledge (see the design spec's "Output
/// timing" decision).
async fn wait_for_quiet(
    output: &Arc<std::sync::Mutex<VecDeque<u8>>>,
    quiet_window: Duration,
    max_wait: Duration,
    cancellation: &tokio_util::sync::CancellationToken,
) {
    const POLL_INTERVAL: Duration = Duration::from_millis(20);
    let start = Instant::now();
    let mut last_len = output.lock().unwrap().len();
    let mut last_change = Instant::now();
    loop {
        if cancellation.is_cancelled() || start.elapsed() >= max_wait {
            return;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
        let current_len = output.lock().unwrap().len();
        if current_len != last_len {
            last_len = current_len;
            last_change = Instant::now();
        } else if last_change.elapsed() >= quiet_window {
            return;
        }
    }
}

/// Takes everything currently buffered, leaving the buffer empty — a
/// "drain," never a "peek," so output is never reported twice across
/// calls.
fn drain_output(output: &Arc<std::sync::Mutex<VecDeque<u8>>>) -> String {
    let mut buf = output.lock().unwrap();
    let drained: VecDeque<u8> = std::mem::take(&mut *buf);
    String::from_utf8_lossy(&drained.into_iter().collect::<Vec<u8>>()).into_owned()
}

/// Called by both `repl_send` and `repl_stop` to report a process's exit
/// code/signal once they detect the child has exited.
fn format_exit_status(status: std::process::ExitStatus) -> String {
    status
        .code()
        .map(|c| c.to_string())
        .unwrap_or_else(|| "signal".to_string())
}

/// The size to give a freshly-started pty: `aivyx-coder`'s own current
/// terminal size (queried directly off fd 1 — works for the TUI
/// frontend, whose stdout is a real terminal), or a fixed 80x24 fallback
/// whenever that query fails (e.g. under the ACP frontend, whose stdio is
/// a pipe to the editor, not a tty — `ioctl(TIOCGWINSZ)` there fails with
/// `ENOTTY`). Live resizing after this point is handled separately, by
/// `ReplResizeTarget` (Task 3/4).
fn current_terminal_size() -> (u16, u16) {
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    // SAFETY: `ioctl` with a valid, zeroed buffer on a fixed, well-known
    // fd is always safe to attempt regardless of what fd 1 actually is.
    let ok = unsafe { libc::ioctl(1, libc::TIOCGWINSZ, &mut ws) } == 0;
    if ok && ws.ws_col > 0 && ws.ws_row > 0 {
        (ws.ws_col, ws.ws_row)
    } else {
        (80, 24)
    }
}

/// Auto-kills a forgotten session — a safety net, not a limit on how long
/// a legitimate long-running process may stay useful (as long as
/// `repl_send` is called at least once per `idle_timeout`, this never
/// fires). Spawned once by `ReplStartTool::execute`, right after the new
/// session is stored (see the comment at that call site for why the
/// ordering matters). Exits on its own once the session it's watching is
/// gone, however that happened (explicit `repl_stop`, spontaneous exit
/// detected by `repl_send`/`repl_stop`, or this same watcher's own
/// idle-kill) — there is only ever one session and one watcher at a time,
/// so no generation counter is needed to tell "my session" apart from "a
/// different, later session."
async fn idle_watcher(state: SharedReplSession, idle_timeout: Duration) {
    loop {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let mut guard = state.lock().await;
        let idle = match guard.as_ref() {
            None => return,
            Some(session) => session.last_activity.elapsed() >= idle_timeout,
        };
        if !idle {
            continue;
        }
        let mut session = guard.take().expect("checked Some above");
        drop(guard);
        crate::process::kill_process_group(&session.child);
        let _ = session.child.wait().await;
        return;
    }
}

#[derive(Deserialize, JsonSchema)]
struct ReplStartArgs {
    /// The program to run, e.g. "python3", "psql", "npm".
    program: String,
    /// Arguments to pass, e.g. ["-i"] for python3's interactive flag, or
    /// ["run", "dev"] for `npm run dev`. Empty array if none needed.
    #[serde(default)]
    args: Vec<String>,
}

/// Starts a persistent, interactive process on a real pseudo-terminal,
/// and stores it in the shared slot. `ActionKind::Execute` — goes through
/// the normal gate (prompt / Always-Allow cache / pre-approved
/// `allowed_commands`), gets checkpointed (trait default
/// `mutates_outside_session() == true`, not overridden), and is hidden
/// from the model in Plan mode (same mechanism) and Autonomous mode
/// (`AUTONOMOUS_HIDDEN_TOOLS` in `aivyx-core`).
pub struct ReplStartTool {
    session: SharedReplSession,
    quiet_window: Duration,
    max_wait: Duration,
    idle_timeout: Duration,
}

impl ReplStartTool {
    pub fn new(
        session: SharedReplSession,
        quiet_window: Duration,
        max_wait: Duration,
        idle_timeout: Duration,
    ) -> Self {
        Self {
            session,
            quiet_window,
            max_wait,
            idle_timeout,
        }
    }
}

#[async_trait]
impl Tool for ReplStartTool {
    fn name(&self) -> &str {
        "repl_start"
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name().to_string(),
            description: "Start a persistent, interactive process (e.g. a language REPL like \
                `python3 -i`, a database CLI like `psql mydb`, or a long-running dev server like \
                `npm run dev`) and get any output it produces immediately (e.g. a startup banner) \
                back. Only one process may run at a time — call repl_stop before starting another. \
                Use repl_send to interact with it afterward."
                .to_string(),
            parameters_schema: serde_json::Value::from(schemars::schema_for!(ReplStartArgs)),
        }
    }

    fn permission_request(
        &self,
        arguments: &serde_json::Value,
        _cwd: &Path,
    ) -> Result<PermissionRequest, ToolError> {
        let args: ReplStartArgs = serde_json::from_value(arguments.clone())
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;
        Ok(PermissionRequest {
            tool_name: self.name().to_string(),
            action: ActionKind::Execute,
            target: PermissionTarget::Command {
                program: args.program,
                args: args.args,
            },
            arguments_preview: arguments.clone(),
            preview: None,
            diff: None,
        })
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        ctx: &ToolExecutionContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: ReplStartArgs = serde_json::from_value(arguments)
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;

        // A single guard held across both the "already running" check and
        // the eventual store below — not two separate lock acquisitions —
        // so the check-then-store is atomic with respect to this mutex.
        // (In practice this project's tool dispatch is strictly sequential,
        // so two racing `repl_start` calls can't happen today anyway; this
        // just avoids a redundant second lock acquisition.) Nothing between
        // the check and the store below is `.await`-ing, so holding the
        // guard across the spawn doesn't block anything else that needs it.
        let mut guard = self.session.lock().await;
        if let Some(existing) = guard.as_ref() {
            return Ok(ToolOutput::Error(format!(
                "a REPL session is already running (`{} {}`) — call repl_stop first",
                existing.program,
                existing.args.join(" ")
            )));
        }

        let (master, slave) = crate::pty::open_pty().map_err(|err| {
            ToolError::ExecutionFailed(format!("failed to allocate a pty: {err}"))
        })?;
        let (cols, rows) = current_terminal_size();
        let initial_ws = libc::winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // SAFETY: `master` was just successfully opened above; setting
        // its initial size before the child ever writes anything is
        // always safe to attempt (a failure here just leaves the pty at
        // its kernel default size, not fatal).
        unsafe {
            libc::ioctl(master.as_raw_fd(), libc::TIOCSWINSZ, &initial_ws);
        }

        let mut command = tokio::process::Command::new(&args.program);
        command
            .args(&args.args)
            .current_dir(&ctx.cwd)
            .stdin(Stdio::from(slave.try_clone().map_err(|err| {
                ToolError::ExecutionFailed(format!("failed to duplicate the pty slave fd: {err}"))
            })?))
            .stdout(Stdio::from(slave.try_clone().map_err(|err| {
                ToolError::ExecutionFailed(format!("failed to duplicate the pty slave fd: {err}"))
            })?))
            .stderr(Stdio::from(slave));
        let mut command = ctx.confiner.confine(command);
        // SAFETY: `setsid()`/`ioctl(TIOCSCTTY)` are both async-signal-safe
        // libc calls. This closure runs after fork, after the Stdio
        // redirections above have already been applied (dup2'd onto fds
        // 0/1/2 — confirmed empirically during planning), so fd 0 here is
        // the pty slave. `setsid()` makes the child both a new session
        // leader and, atomically, the sole member of a new process group
        // — this is why `.process_group(0)` (used before this feature)
        // was removed rather than kept alongside: calling both would make
        // `setsid()` fail (POSIX: it errors if the caller is already a
        // process-group leader, which `.process_group(0)` would have just
        // made it). `kill_process_group`'s `-(pid)` target still works
        // unchanged, since `setsid()`'s new group's pgid equals the
        // child's own pid, same as `.process_group(0)` provided before.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() < 0 {
                    return Err(io::Error::last_os_error());
                }
                if libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = command.spawn().map_err(|err| {
            ToolError::ExecutionFailed(format!("failed to start `{}`: {err}", args.program))
        })?;

        // The parent's own fd handling: one clone of the master for the
        // continuous background reader, one for `ReplSession` itself
        // (writes + resize). `slave` needs no further handling here —
        // each of its three dup'd copies was consumed by a `Stdio::from`
        // above, and std closes the parent's own copy of each after the
        // corresponding dup2 into the child (the same mechanism already
        // relied on for `Stdio::from(File)` elsewhere).
        let reader_fd = master.try_clone().map_err(|err| {
            ToolError::ExecutionFailed(format!("failed to duplicate the pty master fd: {err}"))
        })?;
        let reader_file = tokio::fs::File::from_std(std::fs::File::from(reader_fd));
        let pty_master = tokio::fs::File::from_std(std::fs::File::from(master));

        let output: Arc<std::sync::Mutex<VecDeque<u8>>> =
            Arc::new(std::sync::Mutex::new(VecDeque::new()));
        spawn_output_reader(reader_file, Arc::clone(&output));

        // Store the session BEFORE the quiet-window wait below (not
        // after): the idle watcher spawned right after this checks the
        // shared slot every 1s starting immediately, and `max_wait`
        // (default 10s) is longer than that — storing late would let the
        // watcher's very first wake-up see `None` and exit immediately,
        // permanently orphaning idle-timeout protection for this session.
        *guard = Some(ReplSession {
            child,
            pty_master,
            output: Arc::clone(&output),
            last_activity: Instant::now(),
            program: args.program.clone(),
            args: args.args.clone(),
        });
        drop(guard);

        tokio::spawn(idle_watcher(Arc::clone(&self.session), self.idle_timeout));

        wait_for_quiet(&output, self.quiet_window, self.max_wait, &ctx.cancellation).await;
        let initial_output = drain_output(&output);

        Ok(ToolOutput::Ok(format!(
            "started `{} {}`\n{initial_output}",
            args.program,
            args.args.join(" ")
        )))
    }
}

#[derive(Deserialize, JsonSchema)]
struct ReplSendArgs {
    /// Text to send to the running process's stdin, e.g. "print(1+1)". \
    /// Omit or send an empty string to just check for new output without \
    /// sending anything (useful for polling a long-running process like a \
    /// dev server).
    #[serde(default)]
    input: Option<String>,
}

/// Sends input to (or, with no input, just polls) the running session
/// started by `repl_start`. `ActionKind::Interact` — auto-allowed, no
/// re-prompt (see the design spec's "send-gating" decision and
/// `ActionKind::Interact`'s own doc comment for why this is safe: the
/// real boundary is `repl_start`'s own `Execute`-tier approval plus
/// Landlock/seccomp confinement on the process itself, not per-line
/// review). `mutates_outside_session()` overridden to `false` — no new
/// checkpoint per send.
pub struct ReplSendTool {
    session: SharedReplSession,
    quiet_window: Duration,
    max_wait: Duration,
}

impl ReplSendTool {
    pub fn new(session: SharedReplSession, quiet_window: Duration, max_wait: Duration) -> Self {
        Self {
            session,
            quiet_window,
            max_wait,
        }
    }
}

#[async_trait]
impl Tool for ReplSendTool {
    fn name(&self) -> &str {
        "repl_send"
    }

    fn mutates_outside_session(&self) -> bool {
        false
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name().to_string(),
            description: "Send input to the process started by repl_start and get its output \
                back. Omit input (or send an empty string) to just check for new output without \
                sending anything — useful for polling a long-running process. Errors if no \
                session is running."
                .to_string(),
            parameters_schema: serde_json::Value::from(schemars::schema_for!(ReplSendArgs)),
        }
    }

    fn permission_request(
        &self,
        _arguments: &serde_json::Value,
        _cwd: &Path,
    ) -> Result<PermissionRequest, ToolError> {
        Ok(PermissionRequest {
            tool_name: self.name().to_string(),
            action: ActionKind::Interact,
            target: PermissionTarget::Other("repl session".to_string()),
            arguments_preview: serde_json::json!({}),
            preview: None,
            diff: None,
        })
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        ctx: &ToolExecutionContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: ReplSendArgs = serde_json::from_value(arguments)
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;

        let mut guard = self.session.lock().await;
        let Some(session) = guard.as_mut() else {
            return Ok(ToolOutput::Error(
                "no REPL session is running — call repl_start first".to_string(),
            ));
        };

        // Opportunistic, non-blocking: if the child already exited on its
        // own since the last call, report that instead of trying to
        // interact with a dead process.
        if let Ok(Some(status)) = session.child.try_wait() {
            // Best-effort, not a guarantee: the background reader's final
            // read and this `try_wait()` observation aren't atomic, so the
            // very last bytes written right before exit can occasionally
            // be missed.
            let final_output = drain_output(&session.output);
            *guard = None;
            return Ok(ToolOutput::Ok(format!(
                "process exited with status {}\n{final_output}",
                format_exit_status(status)
            )));
        }

        session.last_activity = Instant::now();

        if let Some(input) = args.input.as_deref().filter(|s| !s.is_empty()) {
            let mut line = input.to_string();
            line.push('\n');
            if session.pty_master.write_all(line.as_bytes()).await.is_err() {
                // Most likely a broken pipe in the narrow window since the
                // try_wait() check above — the process exited right then.
                let final_output = drain_output(&session.output);
                *guard = None;
                return Ok(ToolOutput::Ok(format!(
                    "process exited (broken pipe while sending input)\n{final_output}"
                )));
            }
        }

        wait_for_quiet(&session.output, self.quiet_window, self.max_wait, &ctx.cancellation).await;
        let output = drain_output(&session.output);
        Ok(ToolOutput::Ok(output))
    }
}

/// Stops the running session started by `repl_start`. `ActionKind::
/// Interact` (same reasoning as `ReplSendTool` — no re-prompt to stop
/// something already approved). `mutates_outside_session()` overridden to
/// `false`, same reasoning as `ReplSendTool`.
pub struct ReplStopTool {
    session: SharedReplSession,
}

impl ReplStopTool {
    pub fn new(session: SharedReplSession) -> Self {
        Self { session }
    }
}

#[async_trait]
impl Tool for ReplStopTool {
    fn name(&self) -> &str {
        "repl_stop"
    }

    fn mutates_outside_session(&self) -> bool {
        false
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name().to_string(),
            description: "Stop the process started by repl_start. Errors if no session is \
                running."
                .to_string(),
            parameters_schema: serde_json::json!({ "type": "object", "properties": {} }),
        }
    }

    fn permission_request(
        &self,
        _arguments: &serde_json::Value,
        _cwd: &Path,
    ) -> Result<PermissionRequest, ToolError> {
        Ok(PermissionRequest {
            tool_name: self.name().to_string(),
            action: ActionKind::Interact,
            target: PermissionTarget::Other("repl session".to_string()),
            arguments_preview: serde_json::json!({}),
            preview: None,
            diff: None,
        })
    }

    async fn execute(
        &self,
        _arguments: serde_json::Value,
        _ctx: &ToolExecutionContext,
    ) -> Result<ToolOutput, ToolError> {
        let mut guard = self.session.lock().await;
        let Some(mut session) = guard.take() else {
            return Ok(ToolOutput::Error(
                "no REPL session is running — call repl_start first".to_string(),
            ));
        };
        drop(guard);

        if let Ok(Some(status)) = session.child.try_wait() {
            let final_output = drain_output(&session.output);
            return Ok(ToolOutput::Ok(format!(
                "process had already exited with status {} before repl_stop was called\n{final_output}",
                format_exit_status(status)
            )));
        }

        crate::process::kill_process_group(&session.child);
        let _ = session.child.wait().await;
        let final_output = drain_output(&session.output);
        Ok(ToolOutput::Ok(format!("process stopped\n{final_output}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::Duration;

    fn ctx() -> ToolExecutionContext {
        ToolExecutionContext {
            cwd: PathBuf::from("."),
            confiner: Arc::new(aivyx_sandbox::NoopConfiner),
            cancellation: tokio_util::sync::CancellationToken::new(),
        }
    }

    fn short_timing() -> (Duration, Duration, Duration) {
        // (quiet_window, max_wait, idle_timeout) — short so tests run fast.
        (Duration::from_millis(80), Duration::from_secs(3), Duration::from_secs(120))
    }

    /// A deterministic fake "REPL": reads lines from stdin, echoes each
    /// back prefixed "echo: ", and exits with status 7 if sent "quit" —
    /// available everywhere via `sh`, no reliance on python/node being
    /// installed.
    fn fake_repl_args() -> (String, Vec<String>) {
        (
            "sh".to_string(),
            vec![
                "-c".to_string(),
                "while IFS= read -r line; do \
                     if [ \"$line\" = \"quit\" ]; then exit 7; fi; \
                     echo \"echo: $line\"; \
                 done"
                    .to_string(),
            ],
        )
    }

    #[tokio::test]
    async fn repl_start_reports_the_program_and_any_immediate_output() {
        let (quiet_window, max_wait, idle_timeout) = short_timing();
        let session = new_shared_repl_session();
        let tool = ReplStartTool::new(session, quiet_window, max_wait, idle_timeout);
        let (program, args) = fake_repl_args();

        let output = tool
            .execute(serde_json::json!({ "program": program, "args": args }), &ctx())
            .await
            .unwrap();

        let aivyx_types::ToolOutput::Ok(text) = output else {
            panic!("expected Ok output");
        };
        assert!(text.contains("sh"));
    }

    #[tokio::test]
    async fn repl_start_errors_if_a_session_is_already_running() {
        let (quiet_window, max_wait, idle_timeout) = short_timing();
        let session = new_shared_repl_session();
        let tool = ReplStartTool::new(session, quiet_window, max_wait, idle_timeout);
        let (program, args) = fake_repl_args();
        let args_json = serde_json::json!({ "program": program, "args": args });

        tool.execute(args_json.clone(), &ctx()).await.unwrap();
        let second = tool.execute(args_json, &ctx()).await.unwrap();

        let aivyx_types::ToolOutput::Error(text) = second else {
            panic!("expected Error output for a double start");
        };
        assert!(text.contains("already running"));
    }

    #[tokio::test]
    async fn repl_start_fails_gracefully_for_a_nonexistent_program() {
        let (quiet_window, max_wait, idle_timeout) = short_timing();
        let session = new_shared_repl_session();
        let tool = ReplStartTool::new(session, quiet_window, max_wait, idle_timeout);

        let result = tool
            .execute(
                serde_json::json!({ "program": "definitely-not-a-real-binary-xyz", "args": [] }),
                &ctx(),
            )
            .await;

        assert!(matches!(result, Err(ToolError::ExecutionFailed(_))));
    }

    #[tokio::test]
    async fn a_program_that_requires_a_real_tty_now_runs_successfully() {
        let (quiet_window, max_wait, idle_timeout) = short_timing();
        let session = new_shared_repl_session();
        let tool = ReplStartTool::new(session, quiet_window, max_wait, idle_timeout);

        // `test -t 0` is a shell builtin (no external program dependency,
        // matching this file's existing `fake_repl_args()` philosophy)
        // reporting whether fd 0 is a real tty — false under the old
        // plain-pipe design, true now.
        let output = tool
            .execute(
                serde_json::json!({
                    "program": "sh",
                    "args": ["-c", "if [ -t 0 ]; then echo IS_A_TTY; else echo NOT_A_TTY; fi"]
                }),
                &ctx(),
            )
            .await
            .unwrap();

        let aivyx_types::ToolOutput::Ok(text) = output else {
            panic!("expected Ok output");
        };
        assert!(text.contains("IS_A_TTY"), "got: {text}");
    }

    #[tokio::test]
    async fn permission_request_is_execute_with_a_command_target() {
        let session = new_shared_repl_session();
        let tool = ReplStartTool::new(
            session,
            Duration::from_millis(1),
            Duration::from_millis(1),
            Duration::from_secs(1),
        );
        let request = tool
            .permission_request(
                &serde_json::json!({ "program": "python3", "args": ["-i"] }),
                std::path::Path::new("."),
            )
            .unwrap();
        assert_eq!(request.action, ActionKind::Execute);
        assert_eq!(
            request.target,
            PermissionTarget::Command {
                program: "python3".to_string(),
                args: vec!["-i".to_string()],
            }
        );
    }

    #[test]
    fn repl_start_mutates_outside_session_by_default() {
        // Trait default (true), unchanged — hidden in Plan mode like every
        // other mutating tool. Not overridden anywhere in this file.
        let tool = ReplStartTool::new(
            new_shared_repl_session(),
            Duration::from_millis(1),
            Duration::from_millis(1),
            Duration::from_secs(1),
        );
        assert!(tool.mutates_outside_session());
    }

    #[tokio::test]
    async fn an_idle_session_is_auto_killed_after_the_configured_timeout() {
        let session = new_shared_repl_session();
        // idle_timeout shorter than the watcher's 1s poll interval isn't
        // meaningful — use 1s idle_timeout so the watcher's very first
        // wake-up already sees it expired, and poll (well past 1s) for the
        // session to clear.
        let tool = ReplStartTool::new(
            Arc::clone(&session),
            Duration::from_millis(50),
            Duration::from_secs(1),
            Duration::from_secs(1),
        );
        let (program, args) = fake_repl_args();
        tool.execute(serde_json::json!({ "program": program, "args": args }), &ctx())
            .await
            .unwrap();

        assert!(session.lock().await.is_some(), "session should be running right after start");

        tokio::time::sleep(Duration::from_millis(2500)).await;
        assert!(
            session.lock().await.is_none(),
            "session should have been auto-killed by the idle watcher"
        );
    }

    #[tokio::test]
    async fn dropping_a_repl_session_kills_the_process_group() {
        // Constructs a `ReplSession` directly (this test lives inside
        // `repl`'s own `mod tests`, so private fields are visible) rather
        // than through `ReplStartTool`, so dropping it here isn't
        // entangled with the shared `Arc` other tool instances also hold
        // a clone of.
        let (program, args) = fake_repl_args();
        let (master, slave) = crate::pty::open_pty().unwrap();
        let mut command = tokio::process::Command::new(&program);
        command
            .args(&args)
            .stdin(Stdio::from(slave.try_clone().unwrap()))
            .stdout(Stdio::from(slave.try_clone().unwrap()))
            .stderr(Stdio::from(slave));
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() < 0 {
                    return Err(io::Error::last_os_error());
                }
                if libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = command.spawn().unwrap();
        let pid = child.id().unwrap() as i32;
        let pty_master = tokio::fs::File::from_std(std::fs::File::from(master));

        let session = ReplSession {
            child,
            pty_master,
            output: Arc::new(std::sync::Mutex::new(VecDeque::new())),
            last_activity: Instant::now(),
            program,
            args,
        };
        drop(session);

        tokio::time::sleep(Duration::from_millis(200)).await;

        // Signal 0: existence check only, no signal actually delivered.
        // A non-zero return (ESRCH) is expected once SIGKILL has taken
        // effect and the process is gone.
        let alive = unsafe { libc::kill(pid, 0) == 0 };
        assert!(!alive, "process should have been killed when ReplSession was dropped");
    }

    async fn started_session(
        quiet_window: Duration,
        max_wait: Duration,
    ) -> (SharedReplSession, ReplStartTool) {
        let session = new_shared_repl_session();
        let start_tool = ReplStartTool::new(
            Arc::clone(&session),
            quiet_window,
            max_wait,
            Duration::from_secs(120),
        );
        let (program, args) = fake_repl_args();
        start_tool
            .execute(serde_json::json!({ "program": program, "args": args }), &ctx())
            .await
            .unwrap();
        (session, start_tool)
    }

    #[tokio::test]
    async fn repl_send_writes_input_and_returns_the_echoed_response() {
        let (quiet_window, max_wait, _) = short_timing();
        let (session, _start) = started_session(quiet_window, max_wait).await;
        let send_tool = ReplSendTool::new(Arc::clone(&session), quiet_window, max_wait);

        let output = send_tool
            .execute(serde_json::json!({ "input": "hello" }), &ctx())
            .await
            .unwrap();

        let aivyx_types::ToolOutput::Ok(text) = output else {
            panic!("expected Ok output");
        };
        // The pty's cooked-mode line discipline echoes the raw input
        // back first (new, pty-only behavior — this is deliberate, not
        // suppressed, see the design spec's "Echo behavior" decision)...
        assert!(text.contains("hello"), "got: {text}");
        // ...followed by the fake repl's own explicit response.
        assert!(text.contains("echo: hello"), "got: {text}");
    }

    #[tokio::test]
    async fn repl_send_with_no_input_only_polls_without_writing() {
        let (quiet_window, max_wait, _) = short_timing();
        let (session, _start) = started_session(quiet_window, max_wait).await;
        let send_tool = ReplSendTool::new(Arc::clone(&session), quiet_window, max_wait);

        let output = send_tool
            .execute(serde_json::json!({}), &ctx())
            .await
            .unwrap();

        let aivyx_types::ToolOutput::Ok(text) = output else {
            panic!("expected Ok output");
        };
        assert!(
            !text.contains("echo:"),
            "a poll with no input must not trigger any echoed output, got: {text}"
        );
    }

    #[tokio::test]
    async fn repl_send_errors_when_no_session_is_running() {
        let (quiet_window, max_wait, _) = short_timing();
        let session = new_shared_repl_session();
        let send_tool = ReplSendTool::new(session, quiet_window, max_wait);

        let output = send_tool
            .execute(serde_json::json!({ "input": "hello" }), &ctx())
            .await
            .unwrap();

        let aivyx_types::ToolOutput::Error(text) = output else {
            panic!("expected Error output");
        };
        assert!(text.contains("no REPL session is running"));
    }

    #[tokio::test]
    async fn repl_send_detects_and_reports_a_spontaneous_exit() {
        let (quiet_window, max_wait, _) = short_timing();
        let (session, _start) = started_session(quiet_window, max_wait).await;
        let send_tool = ReplSendTool::new(Arc::clone(&session), quiet_window, max_wait);

        // "quit" makes the fake REPL exit(7) on its own.
        send_tool
            .execute(serde_json::json!({ "input": "quit" }), &ctx())
            .await
            .unwrap();
        // The exit itself races the pipe closing; a short sleep lets the
        // child's exit status become observable via try_wait() on the
        // next call.
        tokio::time::sleep(Duration::from_millis(200)).await;

        let output = send_tool
            .execute(serde_json::json!({}), &ctx())
            .await
            .unwrap();
        let aivyx_types::ToolOutput::Ok(text) = output else {
            panic!("expected Ok output reporting the exit");
        };
        assert!(text.contains("exited with status 7"), "got: {text}");

        assert!(
            session.lock().await.is_none(),
            "state must be cleared so repl_start works again without an explicit repl_stop"
        );
    }

    #[test]
    fn repl_send_permission_request_is_interact() {
        let tool = ReplSendTool::new(
            new_shared_repl_session(),
            Duration::from_millis(1),
            Duration::from_millis(1),
        );
        let request = tool
            .permission_request(&serde_json::json!({ "input": "x" }), std::path::Path::new("."))
            .unwrap();
        assert_eq!(request.action, ActionKind::Interact);
    }

    #[test]
    fn repl_send_does_not_mutate_outside_session() {
        // No new checkpoint per send — see the design spec's reasoning
        // (mirrors git_read overriding to false despite touching the
        // outside world in a read-only way).
        let tool = ReplSendTool::new(
            new_shared_repl_session(),
            Duration::from_millis(1),
            Duration::from_millis(1),
        );
        assert!(!tool.mutates_outside_session());
    }

    #[tokio::test]
    async fn repl_stop_kills_the_process_and_a_subsequent_send_errors() {
        let (quiet_window, max_wait, _) = short_timing();
        let (session, _start) = started_session(quiet_window, max_wait).await;
        let stop_tool = ReplStopTool::new(Arc::clone(&session));
        let send_tool = ReplSendTool::new(Arc::clone(&session), quiet_window, max_wait);

        let output = stop_tool.execute(serde_json::json!({}), &ctx()).await.unwrap();
        let aivyx_types::ToolOutput::Ok(text) = output else {
            panic!("expected Ok output");
        };
        assert!(text.contains("stopped"), "got: {text}");
        assert!(session.lock().await.is_none());

        let after = send_tool
            .execute(serde_json::json!({ "input": "hi" }), &ctx())
            .await
            .unwrap();
        let aivyx_types::ToolOutput::Error(text) = after else {
            panic!("expected Error output after stop");
        };
        assert!(text.contains("no REPL session is running"));
    }

    #[tokio::test]
    async fn repl_stop_errors_when_no_session_is_running() {
        let stop_tool = ReplStopTool::new(new_shared_repl_session());
        let output = stop_tool.execute(serde_json::json!({}), &ctx()).await.unwrap();
        let aivyx_types::ToolOutput::Error(text) = output else {
            panic!("expected Error output");
        };
        assert!(text.contains("no REPL session is running"));
    }

    #[tokio::test]
    async fn repl_stop_reports_an_already_exited_process_instead_of_pretending_to_kill_it() {
        let (quiet_window, max_wait, _) = short_timing();
        let (session, _start) = started_session(quiet_window, max_wait).await;
        let send_tool = ReplSendTool::new(Arc::clone(&session), quiet_window, max_wait);
        let stop_tool = ReplStopTool::new(Arc::clone(&session));

        // Re-inject a session snapshot manually is unnecessary here: send
        // "quit" to make the fake REPL exit on its own, but WITHOUT
        // calling repl_send again afterward (which would already clear
        // state) — call repl_stop directly while the exited-but-not-yet-
        // observed child is still sitting in the shared slot.
        {
            let mut guard = session.lock().await;
            let s = guard.as_mut().unwrap();
            use tokio::io::AsyncWriteExt;
            s.pty_master.write_all(b"quit\n").await.unwrap();
        }
        tokio::time::sleep(Duration::from_millis(200)).await;

        let output = stop_tool.execute(serde_json::json!({}), &ctx()).await.unwrap();
        let aivyx_types::ToolOutput::Ok(text) = output else {
            panic!("expected Ok output");
        };
        assert!(
            text.contains("already exited") && text.contains("7"),
            "got: {text}"
        );
        let _ = send_tool; // silence unused-var lint if not otherwise referenced
    }

    #[test]
    fn repl_stop_permission_request_is_interact() {
        let tool = ReplStopTool::new(new_shared_repl_session());
        let request = tool
            .permission_request(&serde_json::json!({}), std::path::Path::new("."))
            .unwrap();
        assert_eq!(request.action, ActionKind::Interact);
    }

    #[test]
    fn repl_stop_does_not_mutate_outside_session() {
        let tool = ReplStopTool::new(new_shared_repl_session());
        assert!(!tool.mutates_outside_session());
    }
}
