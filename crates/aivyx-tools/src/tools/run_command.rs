use std::path::Path;
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use aivyx_sandbox::{ActionKind, PermissionRequest, PermissionTarget};
use aivyx_types::{ToolDefinition, ToolOutput};
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;
use tokio::io::AsyncReadExt;

use crate::{Tool, ToolError, ToolExecutionContext};

/// Per-stream cap on captured output — tail-truncated (keep the *last* N
/// bytes), not head-truncated like `grep`/`glob`'s match caps: the
/// actionable signal in build/test output (the actual failing test, the
/// final compiler error) is almost always at the end.
const MAX_OUTPUT_BYTES: usize = 50 * 1024;

/// A single user-configured, fully-fixed command the model may run by name.
/// Deliberately not `aivyx_config::AllowedCommand` — this crate doesn't
/// depend on `aivyx-config`, mirroring how `deny_paths` is passed to
/// `GrepTool`/`GlobTool` as plain data rather than a config type.
#[derive(Debug, Clone)]
pub struct CommandSpec {
    pub name: String,
    pub program: String,
    pub args: Vec<String>,
    pub timeout: Duration,
}

#[derive(Deserialize, JsonSchema)]
struct RunCommandArgs {
    /// Name of a pre-configured command to run (see the tool description for available names).
    command: String,
}

/// Runs one of a fixed, user-configured set of commands (e.g. the project's
/// own build/test/lint) and reports its output back. The model selects a
/// command by name; it never supplies a program or arbitrary arguments —
/// that's what makes it safe to auto-cache repeated runs via the normal
/// Always-Allow flow without waiting on Phase 5's real process sandboxing.
pub struct RunCommandTool {
    commands: Vec<CommandSpec>,
}

impl RunCommandTool {
    pub fn new(commands: Vec<CommandSpec>) -> Self {
        Self { commands }
    }

    fn find(&self, name: &str) -> Result<&CommandSpec, ToolError> {
        self.commands
            .iter()
            .find(|c| c.name == name)
            .ok_or_else(|| {
                let available = self
                    .commands
                    .iter()
                    .map(|c| c.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                ToolError::InvalidArguments(format!(
                    "unknown command '{name}'; available: {available}"
                ))
            })
    }
}

#[async_trait]
impl Tool for RunCommandTool {
    fn name(&self) -> &str {
        "run_command"
    }

    fn definition(&self) -> ToolDefinition {
        let names = self
            .commands
            .iter()
            .map(|c| c.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        ToolDefinition {
            name: self.name().to_string(),
            description: format!(
                "Run a pre-configured project command (e.g. build/test/lint) and get its exit \
                 status and output back. Commands are fixed by user configuration — you select \
                 one by name and cannot supply a program or arbitrary arguments. Available \
                 commands: {names}."
            ),
            parameters_schema: serde_json::Value::from(schemars::schema_for!(RunCommandArgs)),
        }
    }

    fn permission_request(
        &self,
        arguments: &serde_json::Value,
        _cwd: &Path,
    ) -> Result<PermissionRequest, ToolError> {
        let args: RunCommandArgs = serde_json::from_value(arguments.clone())
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;
        let spec = self.find(&args.command)?;

        Ok(PermissionRequest {
            tool_name: self.name().to_string(),
            action: ActionKind::Execute,
            target: PermissionTarget::Command {
                program: spec.program.clone(),
                args: spec.args.clone(),
            },
            // Minimal — the TUI's `target_line` already renders
            // `Command: {program} {args}` from `target` above.
            arguments_preview: json!({ "name": spec.name }),
            preview: None,
        })
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        ctx: &ToolExecutionContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: RunCommandArgs = serde_json::from_value(arguments)
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;
        let spec = self.find(&args.command)?;

        let mut command = tokio::process::Command::new(&spec.program);
        command
            .args(&spec.args)
            .current_dir(&ctx.cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let command = ctx.confiner.confine(command);

        run(command, spec.timeout, ctx.cancellation.clone()).await
    }
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

async fn run(
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

    fn spec(name: &str, program: &str, args: &[&str], timeout: Duration) -> CommandSpec {
        CommandSpec {
            name: name.to_string(),
            program: program.to_string(),
            args: args.iter().map(|s| s.to_string()).collect(),
            timeout,
        }
    }

    async fn run_tool(tool: &RunCommandTool, command: &str) -> Result<ToolOutput, ToolError> {
        let ctx = ToolExecutionContext {
            cwd: std::env::temp_dir(),
            confiner: std::sync::Arc::new(aivyx_sandbox::NoopConfiner),
            cancellation: tokio_util::sync::CancellationToken::new(),
        };
        tool.execute(json!({ "command": command }), &ctx).await
    }

    #[tokio::test]
    async fn runs_a_configured_command_and_reports_real_output() {
        let tool = RunCommandTool::new(vec![spec(
            "greet",
            "echo",
            &["hello"],
            Duration::from_secs(5),
        )]);

        let ToolOutput::Ok(text) = run_tool(&tool, "greet").await.unwrap() else {
            panic!("expected Ok output")
        };
        assert!(text.contains("exit status: 0 (success)"));
        assert!(text.contains("hello"));
    }

    #[tokio::test]
    async fn a_failing_command_is_ok_output_not_a_tool_error() {
        // A non-zero exit is normal, informative verification-loop output —
        // not a tool-level failure.
        let tool = RunCommandTool::new(vec![spec(
            "fail",
            "sh",
            &["-c", "echo boom >&2; exit 7"],
            Duration::from_secs(5),
        )]);

        let ToolOutput::Ok(text) = run_tool(&tool, "fail").await.unwrap() else {
            panic!("expected Ok output even for a failing command")
        };
        assert!(text.contains("exit status: 7 (failed)"));
        assert!(text.contains("boom"));
    }

    #[tokio::test]
    async fn unknown_command_name_lists_available_names() {
        let tool = RunCommandTool::new(vec![spec("test", "true", &[], Duration::from_secs(5))]);

        let err = run_tool(&tool, "nonexistent").await.unwrap_err();
        let ToolError::InvalidArguments(msg) = err else {
            panic!("expected InvalidArguments")
        };
        assert!(msg.contains("nonexistent"));
        assert!(msg.contains("test"));
    }

    #[tokio::test]
    async fn a_long_running_command_is_killed_on_timeout() {
        let tool = RunCommandTool::new(vec![spec(
            "slow",
            "sleep",
            &["10"],
            Duration::from_millis(200),
        )]);

        let err = run_tool(&tool, "slow").await.unwrap_err();
        let ToolError::ExecutionFailed(msg) = err else {
            panic!("expected ExecutionFailed")
        };
        assert!(msg.contains("timed out"));
    }

    #[tokio::test]
    async fn cancellation_kills_an_in_flight_command() {
        let tool = RunCommandTool::new(vec![spec(
            "slow",
            "sleep",
            &["10"],
            Duration::from_secs(30),
        )]);
        let cancellation = tokio_util::sync::CancellationToken::new();
        let ctx = ToolExecutionContext {
            cwd: std::env::temp_dir(),
            confiner: std::sync::Arc::new(aivyx_sandbox::NoopConfiner),
            cancellation: cancellation.clone(),
        };

        let handle =
            tokio::spawn(async move { tool.execute(json!({ "command": "slow" }), &ctx).await });
        tokio::time::sleep(Duration::from_millis(100)).await;
        cancellation.cancel();

        let err = handle.await.unwrap().unwrap_err();
        let ToolError::ExecutionFailed(msg) = err else {
            panic!("expected ExecutionFailed")
        };
        assert!(msg.contains("cancelled"));
    }

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
