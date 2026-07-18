use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use aivyx_sandbox::{ActionKind, PermissionRequest, PermissionTarget};
use aivyx_types::{ToolDefinition, ToolOutput};
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use crate::process::run;
use crate::{Tool, ToolError, ToolExecutionContext};

/// Applied to any command not covered by a per-command `timeout` in a
/// config-defined `CommandSpec` — `run_shell` runs arbitrary commands with
/// no such config entry, so it always uses this fixed default.
const SHELL_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Deserialize, JsonSchema)]
struct RunShellArgs {
    /// A full shell command line, e.g. "git status" or "cargo test -- --nocapture".
    command: String,
}

/// Runs an arbitrary shell command via `sh -c`, confined by whatever
/// `ExecutionConfiner` the executor was built with (real Landlock + seccomp
/// confinement by default — see `LandlockConfiner`). Unlike `run_command`,
/// the model is not restricted to a fixed named menu; every command still
/// goes through the normal `ConfirmationGate` flow unless it exactly
/// matches a configured `allowed_commands` entry, which `ConfirmationGate`
/// pre-approves at construction time (see its doc comment) — that's the
/// "command-level allowlisting" trust tier.
pub struct RunShellTool;

#[async_trait]
impl Tool for RunShellTool {
    fn name(&self) -> &str {
        "run_shell"
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name().to_string(),
            description: "Run an arbitrary shell command (via `sh -c`) and get its exit status \
                and output back. Runs inside a real OS-level sandbox (filesystem access limited \
                to the working directory plus common system/toolchain paths; several \
                privileged/introspection syscalls are always blocked) — most commands need \
                one-time user confirmation before running, unless they match a pre-configured \
                allowed command."
                .to_string(),
            parameters_schema: serde_json::Value::from(schemars::schema_for!(RunShellArgs)),
        }
    }

    fn permission_request(
        &self,
        arguments: &serde_json::Value,
        _cwd: &Path,
    ) -> Result<PermissionRequest, ToolError> {
        let args: RunShellArgs = serde_json::from_value(arguments.clone())
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;

        Ok(PermissionRequest {
            tool_name: self.name().to_string(),
            action: ActionKind::Execute,
            target: PermissionTarget::Command {
                program: "sh".to_string(),
                args: vec!["-c".to_string(), args.command],
            },
            arguments_preview: json!({}),
            preview: None,
            diff: None,
        })
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        ctx: &ToolExecutionContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: RunShellArgs = serde_json::from_value(arguments)
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;

        let mut command = tokio::process::Command::new("sh");
        command
            .arg("-c")
            .arg(&args.command)
            .current_dir(&ctx.cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let command = ctx.confiner.confine(command);

        run(command, SHELL_TIMEOUT, ctx.cancellation.clone()).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn run_tool(command: &str) -> Result<ToolOutput, ToolError> {
        let ctx = ToolExecutionContext {
            cwd: std::env::temp_dir(),
            confiner: std::sync::Arc::new(aivyx_sandbox::NoopConfiner),
            cancellation: tokio_util::sync::CancellationToken::new(),
        };
        RunShellTool
            .execute(json!({ "command": command }), &ctx)
            .await
    }

    #[tokio::test]
    async fn runs_an_arbitrary_command_and_reports_real_output() {
        let ToolOutput::Ok(text) = run_tool("echo hello-shell").await.unwrap() else {
            panic!("expected Ok output")
        };
        assert!(text.contains("exit status: 0 (success)"));
        assert!(text.contains("hello-shell"));
    }

    #[tokio::test]
    async fn supports_pipes_and_shell_syntax() {
        let ToolOutput::Ok(text) = run_tool("echo abc | tr a-z A-Z").await.unwrap() else {
            panic!("expected Ok output")
        };
        assert!(text.contains("ABC"));
    }

    #[tokio::test]
    async fn permission_request_targets_sh_dash_c() {
        let request = RunShellTool
            .permission_request(&json!({ "command": "ls -la" }), Path::new("/tmp"))
            .unwrap();
        match request.target {
            PermissionTarget::Command { program, args } => {
                assert_eq!(program, "sh");
                assert_eq!(args, vec!["-c".to_string(), "ls -la".to_string()]);
            }
            _ => panic!("expected a Command target"),
        }
    }
}
