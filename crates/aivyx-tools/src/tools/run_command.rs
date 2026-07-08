use std::path::Path;
use std::process::Stdio;

use aivyx_sandbox::{ActionKind, PermissionRequest, PermissionTarget};
use aivyx_types::{ToolDefinition, ToolOutput};
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use crate::process::{CommandSpec, run};
use crate::{Tool, ToolError, ToolExecutionContext};

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

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
}
