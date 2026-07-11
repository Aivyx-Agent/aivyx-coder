use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use aivyx_sandbox::{ActionKind, PermissionRequest, PermissionTarget};
use aivyx_types::{ToolDefinition, ToolOutput};
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;

use crate::checkpoint::exclude_pathspecs;
use crate::path_resolve::resolve;
use crate::process::run;
use crate::{Tool, ToolError, ToolExecutionContext};

/// Local-only inspection commands; generous for huge repos.
const GIT_READ_TIMEOUT: Duration = Duration::from_secs(60);

const MAX_LOG_COUNT: u32 = 50;
const DEFAULT_LOG_COUNT: u32 = 10;

#[derive(Deserialize, JsonSchema)]
struct GitReadArgs {
    /// What to inspect: "status" (working tree status), "diff" (changes), or "log" (recent commits).
    mode: GitReadMode,
    /// diff only: limit the diff to this file or directory (absolute or relative to the working directory).
    #[serde(default)]
    path: Option<String>,
    /// diff only: show staged (index) changes instead of unstaged ones.
    #[serde(default)]
    staged: bool,
    /// log only: how many commits to show (default 10, max 50).
    #[serde(default)]
    count: Option<u32>,
}

#[derive(Deserialize, JsonSchema, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum GitReadMode {
    Status,
    Diff,
    Log,
}

/// Read-only git inspection (status / diff / log). A single tool rather
/// than three because the modes share everything but the argv; a *separate*
/// tool from `git_commit` because plan-mode tool filtering is static
/// (`mutates_outside_session`) — inspection must stay available while
/// planning, committing must not.
pub struct GitReadTool {
    deny_paths: Vec<PathBuf>,
}

impl GitReadTool {
    pub fn new(deny_paths: Vec<PathBuf>) -> Self {
        Self { deny_paths }
    }

    /// Fixed argv shapes only — model input is never passed through as
    /// flags. Path arguments are resolved to absolute paths first, which
    /// also makes them inert as `-options` or `:(magic)` pathspecs.
    fn build_args(&self, args: &GitReadArgs, cwd: &Path) -> Vec<String> {
        let excludes = exclude_pathspecs(cwd, &self.deny_paths);
        match args.mode {
            GitReadMode::Status => {
                let mut argv: Vec<String> =
                    vec!["status".into(), "--short".into(), "--branch".into()];
                if !excludes.is_empty() {
                    argv.push("--".into());
                    argv.push(".".into());
                    argv.extend(excludes);
                }
                argv
            }
            GitReadMode::Diff => {
                let mut argv: Vec<String> = vec!["diff".into()];
                if args.staged {
                    argv.push("--cached".into());
                }
                argv.push("--".into());
                match &args.path {
                    Some(path) => argv.push(resolve(cwd, path).display().to_string()),
                    None => argv.push(".".into()),
                }
                argv.extend(excludes);
                argv
            }
            GitReadMode::Log => {
                let count = args
                    .count
                    .unwrap_or(DEFAULT_LOG_COUNT)
                    .clamp(1, MAX_LOG_COUNT);
                vec![
                    "log".into(),
                    "--oneline".into(),
                    "-n".into(),
                    count.to_string(),
                ]
            }
        }
    }
}

#[async_trait]
impl Tool for GitReadTool {
    fn name(&self) -> &str {
        "git_read"
    }

    fn mutates_outside_session(&self) -> bool {
        false
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name().to_string(),
            description: "Inspect the git repository in the working directory (read-only): \
                mode \"status\" shows branch and changed files, mode \"diff\" shows unstaged \
                changes (set staged=true for staged ones, path to limit to one file/directory), \
                mode \"log\" shows recent commits (count, default 10)."
                .to_string(),
            parameters_schema: serde_json::Value::from(schemars::schema_for!(GitReadArgs)),
        }
    }

    fn permission_request(
        &self,
        arguments: &serde_json::Value,
        cwd: &Path,
    ) -> Result<PermissionRequest, ToolError> {
        let args: GitReadArgs = serde_json::from_value(arguments.clone())
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;
        // Target the diff path when one is given so the gate's deny_paths
        // check covers it; otherwise the repository root (the cwd).
        let target = match &args.path {
            Some(path) => resolve(cwd, path),
            None => cwd.to_path_buf(),
        };

        Ok(PermissionRequest {
            tool_name: self.name().to_string(),
            action: ActionKind::Read,
            target: PermissionTarget::Path(target),
            arguments_preview: arguments.clone(),
            preview: None,
        })
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        ctx: &ToolExecutionContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: GitReadArgs = serde_json::from_value(arguments)
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;

        let mut command = tokio::process::Command::new("git");
        command
            .args(self.build_args(&args, &ctx.cwd))
            .current_dir(&ctx.cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let command = ctx.confiner.confine(command);

        run(command, GIT_READ_TIMEOUT, ctx.cancellation.clone()).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::checkpoint::test_support::{git, init_repo};
    use serde_json::json;

    fn ctx(dir: &Path) -> ToolExecutionContext {
        ToolExecutionContext {
            cwd: dir.to_path_buf(),
            confiner: std::sync::Arc::new(aivyx_sandbox::NoopConfiner),
            cancellation: tokio_util::sync::CancellationToken::new(),
        }
    }

    async fn run_tool(tool: &GitReadTool, dir: &Path, args: serde_json::Value) -> String {
        let output = tool.execute(args, &ctx(dir)).await.unwrap();
        let ToolOutput::Ok(text) = output else {
            panic!("expected Ok output")
        };
        text
    }

    #[tokio::test]
    async fn status_shows_branch_and_changes() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path()).await;
        std::fs::write(dir.path().join("new.txt"), "x\n").unwrap();

        let tool = GitReadTool::new(vec![]);
        let text = run_tool(&tool, dir.path(), json!({ "mode": "status" })).await;

        assert!(text.contains("main"));
        assert!(text.contains("new.txt"));
    }

    #[tokio::test]
    async fn diff_shows_unstaged_changes_and_honors_a_path_filter() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path()).await;
        std::fs::write(dir.path().join("tracked.txt"), "changed\n").unwrap();
        std::fs::write(dir.path().join("other.txt"), "also\n").unwrap();
        git(dir.path(), &["add", "other.txt"]).await;
        git(dir.path(), &["commit", "-q", "-m", "add other"]).await;
        std::fs::write(dir.path().join("other.txt"), "edited\n").unwrap();

        let tool = GitReadTool::new(vec![]);
        let full = run_tool(&tool, dir.path(), json!({ "mode": "diff" })).await;
        assert!(full.contains("tracked.txt") && full.contains("other.txt"));

        let scoped = run_tool(
            &tool,
            dir.path(),
            json!({ "mode": "diff", "path": "tracked.txt" }),
        )
        .await;
        assert!(scoped.contains("tracked.txt"));
        assert!(!scoped.contains("other.txt"));
    }

    #[tokio::test]
    async fn log_lists_recent_commits() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path()).await;

        let tool = GitReadTool::new(vec![]);
        let text = run_tool(&tool, dir.path(), json!({ "mode": "log", "count": 5 })).await;
        assert!(text.contains("initial"));
    }

    #[tokio::test]
    async fn status_and_diff_exclude_denied_subpaths() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path()).await;
        let cwd = dir.path().canonicalize().unwrap();
        let secret = cwd.join("secret");
        std::fs::create_dir(&secret).unwrap();
        std::fs::write(secret.join("key"), "TOP-SECRET\n").unwrap();
        std::fs::write(cwd.join("public.txt"), "fine\n").unwrap();

        let tool = GitReadTool::new(vec![secret]);
        let status = run_tool(&tool, &cwd, json!({ "mode": "status" })).await;
        assert!(status.contains("public.txt"));
        assert!(!status.contains("secret"), "denied name leaked: {status}");

        let diff = run_tool(&tool, &cwd, json!({ "mode": "diff" })).await;
        assert!(
            !diff.contains("TOP-SECRET"),
            "denied content leaked: {diff}"
        );
    }

    #[tokio::test]
    async fn outside_a_repo_reports_instead_of_erroring() {
        let dir = tempfile::tempdir().unwrap();
        let tool = GitReadTool::new(vec![]);
        let text = run_tool(&tool, dir.path(), json!({ "mode": "status" })).await;
        // Non-zero git exit is informative Ok output (same convention as
        // run_command), so the model can see "not a repository" and adapt.
        assert!(text.contains("failed"));
        assert!(text.to_lowercase().contains("not a git repository"));
    }
}
