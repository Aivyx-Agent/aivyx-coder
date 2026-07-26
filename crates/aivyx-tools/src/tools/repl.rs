use std::collections::VecDeque;
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
// `AsyncWriteExt` isn't used by this task's own code (only `repl_start`
// lives here so far) — it's needed for the `stdin.write_all(...)` call
// Task 4's `repl_send` adds to this same file. Importing it now (per the
// brief's exact `use` list) rather than waiting for Task 4 to add it.
#[allow(unused_imports)]
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
    // Neither `stdin` nor `output` is read by this task's own code (only
    // `repl_start`, which just constructs the session, lives here so
    // far) — `stdin` is written to by Task 4's `repl_send`, and `output`
    // is drained by both Task 4's `repl_send` and Task 5's `repl_stop`.
    // `#[allow(dead_code)]` rather than leaving these unused for now.
    #[allow(dead_code)]
    stdin: tokio::process::ChildStdin,
    /// Combined stdout+stderr, continuously appended to by two background
    /// reader tasks spawned in `ReplStartTool::execute` — this is what
    /// makes polling a long-running process (no `repl_send` input, just
    /// checking for new output) work, since output keeps accumulating
    /// even between calls. A plain `std::sync::Mutex`, not the async one
    /// above: every touch of this buffer is a short, synchronous
    /// append-or-drain, never held across an `.await`.
    #[allow(dead_code)]
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

// Not called by this task's own code — Task 4's `repl_send` and Task 5's
// `repl_stop` both call this to report a process's exit code/signal once
// they detect the child has exited.
#[allow(dead_code)]
fn format_exit_status(status: std::process::ExitStatus) -> String {
    status
        .code()
        .map(|c| c.to_string())
        .unwrap_or_else(|| "signal".to_string())
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

/// Starts a persistent, piped-stdin/stdout/stderr child process and
/// stores it in the shared slot. `ActionKind::Execute` — goes through the
/// normal gate (prompt / Always-Allow cache / pre-approved
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

        {
            let guard = self.session.lock().await;
            if let Some(existing) = guard.as_ref() {
                return Ok(ToolOutput::Error(format!(
                    "a REPL session is already running (`{} {}`) — call repl_stop first",
                    existing.program,
                    existing.args.join(" ")
                )));
            }
        }

        let mut command = tokio::process::Command::new(&args.program);
        command
            .args(&args.args)
            .current_dir(&ctx.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        let mut command = ctx.confiner.confine(command);
        let mut child = command.spawn().map_err(|err| {
            ToolError::ExecutionFailed(format!("failed to start `{}`: {err}", args.program))
        })?;

        let stdin = child.stdin.take().expect("stdin was piped");
        let stdout = child.stdout.take().expect("stdout was piped");
        let stderr = child.stderr.take().expect("stderr was piped");

        let output: Arc<std::sync::Mutex<VecDeque<u8>>> =
            Arc::new(std::sync::Mutex::new(VecDeque::new()));
        spawn_output_reader(stdout, Arc::clone(&output));
        spawn_output_reader(stderr, Arc::clone(&output));

        // Store the session BEFORE the quiet-window wait below (not
        // after): the idle watcher spawned right after this checks the
        // shared slot every 1s starting immediately, and `max_wait`
        // (default 10s) is longer than that — storing late would let the
        // watcher's very first wake-up see `None` and exit immediately,
        // permanently orphaning idle-timeout protection for this session.
        *self.session.lock().await = Some(ReplSession {
            child,
            stdin,
            output: Arc::clone(&output),
            last_activity: Instant::now(),
            program: args.program.clone(),
            args: args.args.clone(),
        });

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
        let mut command = tokio::process::Command::new(&program);
        command
            .args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        let mut child = command.spawn().unwrap();
        let stdin = child.stdin.take().unwrap();
        let pid = child.id().unwrap() as i32;

        let session = ReplSession {
            child,
            stdin,
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
}
