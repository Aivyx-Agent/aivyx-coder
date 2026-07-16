use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use aivyx_sandbox::{ActionKind, PermissionRequest, PermissionTarget};
use aivyx_types::{ToolDefinition, ToolOutput};
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;

use crate::process::run;
use crate::{Tool, ToolError, ToolExecutionContext};

const GIT_BRANCH_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Deserialize, JsonSchema, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum GitBranchMode {
    Create,
    Switch,
}

#[derive(Deserialize, JsonSchema)]
struct GitBranchArgs {
    /// "create" makes a new branch and switches to it; "switch" moves to an already-existing branch.
    mode: GitBranchMode,
    /// Branch name.
    name: String,
    /// create only: base ref to branch from (omit to branch from the current HEAD).
    #[serde(default)]
    base: Option<String>,
}

/// Creates or switches git branches via the user's real `git`. A single
/// tool for both modes (mirroring `git_read`'s single-tool/multi-mode
/// shape) since they share everything but the argv; kept separate from
/// `git_read`'s `branches` listing mode because this mutates the current
/// branch and must stay confirm-gated, unlike listing.
pub struct GitBranchTool;

impl GitBranchTool {
    pub fn new() -> Self {
        Self
    }

    fn build_argv(args: &GitBranchArgs) -> Vec<String> {
        match args.mode {
            GitBranchMode::Create => {
                let mut argv = vec!["checkout".to_string(), "-b".to_string(), args.name.clone()];
                if let Some(base) = &args.base {
                    argv.push(base.clone());
                }
                argv
            }
            GitBranchMode::Switch => vec!["checkout".to_string(), args.name.clone()],
        }
    }
}

impl Default for GitBranchTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for GitBranchTool {
    fn name(&self) -> &str {
        "git_branch"
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name().to_string(),
            description: "Create a new branch (and switch to it) or switch to an existing \
                branch. \"create\" takes an optional base ref to branch from (defaults to the \
                current HEAD); \"switch\" moves to an already-existing branch."
                .to_string(),
            parameters_schema: serde_json::Value::from(schemars::schema_for!(GitBranchArgs)),
        }
    }

    fn permission_request(
        &self,
        arguments: &serde_json::Value,
        cwd: &Path,
    ) -> Result<PermissionRequest, ToolError> {
        let args: GitBranchArgs = serde_json::from_value(arguments.clone())
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;
        if args.name.trim().is_empty() {
            return Err(ToolError::InvalidArguments(
                "branch name must not be empty".to_string(),
            ));
        }
        let argv = Self::build_argv(&args);
        let current = current_branch(cwd).unwrap_or_else(|| "(unknown)".to_string());
        let preview = match args.mode {
            GitBranchMode::Create => format!(
                "{current} -> {} (new, from {})",
                args.name,
                args.base.as_deref().unwrap_or("HEAD")
            ),
            GitBranchMode::Switch => format!("{current} -> {}", args.name),
        };

        Ok(PermissionRequest {
            tool_name: self.name().to_string(),
            action: ActionKind::Execute,
            target: PermissionTarget::Command {
                program: "git".to_string(),
                args: argv,
            },
            arguments_preview: arguments.clone(),
            preview: Some(preview),
        })
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        ctx: &ToolExecutionContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: GitBranchArgs = serde_json::from_value(arguments)
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;
        let argv = Self::build_argv(&args);
        run(
            git_command(&argv, ctx),
            GIT_BRANCH_TIMEOUT,
            ctx.cancellation.clone(),
        )
        .await
    }
}

fn git_command(args: &[String], ctx: &ToolExecutionContext) -> tokio::process::Command {
    let mut command = tokio::process::Command::new("git");
    command
        .args(args)
        .current_dir(&ctx.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    ctx.confiner.confine(command)
}

fn current_branch(cwd: &Path) -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["branch", "--show-current"])
        .current_dir(cwd)
        .stdin(Stdio::null())
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
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

    #[tokio::test]
    async fn create_switches_to_a_new_branch_from_head() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path()).await;

        let tool = GitBranchTool::new();
        let output = tool
            .execute(json!({ "mode": "create", "name": "feature-x" }), &ctx(dir.path()))
            .await
            .unwrap();
        let ToolOutput::Ok(text) = output else {
            panic!("expected Ok output")
        };
        assert!(text.contains("success"), "checkout failed: {text}");

        let current = git(dir.path(), &["branch", "--show-current"]).await;
        assert_eq!(current.trim(), "feature-x");
    }

    #[tokio::test]
    async fn create_branches_from_a_given_base() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path()).await;
        git(dir.path(), &["checkout", "-b", "base-branch"]).await;
        std::fs::write(dir.path().join("tracked.txt"), "v2\n").unwrap();
        git(dir.path(), &["commit", "-aqm", "on base-branch"]).await;
        git(dir.path(), &["checkout", "main"]).await;

        let tool = GitBranchTool::new();
        tool.execute(
            json!({ "mode": "create", "name": "from-base", "base": "base-branch" }),
            &ctx(dir.path()),
        )
        .await
        .unwrap();

        let log = git(dir.path(), &["log", "--oneline", "-n", "1"]).await;
        assert!(log.contains("on base-branch"));
    }

    #[tokio::test]
    async fn switch_moves_to_an_existing_branch() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path()).await;
        git(dir.path(), &["checkout", "-b", "feature-x"]).await;
        git(dir.path(), &["checkout", "main"]).await;

        let tool = GitBranchTool::new();
        tool.execute(
            json!({ "mode": "switch", "name": "feature-x" }),
            &ctx(dir.path()),
        )
        .await
        .unwrap();

        let current = git(dir.path(), &["branch", "--show-current"]).await;
        assert_eq!(current.trim(), "feature-x");
    }

    #[tokio::test]
    async fn permission_request_preview_shows_current_and_target_branch() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path()).await;

        let tool = GitBranchTool::new();
        let request = tool
            .permission_request(
                &json!({ "mode": "create", "name": "feature-x" }),
                dir.path(),
            )
            .unwrap();

        assert_eq!(request.action, ActionKind::Execute);
        let PermissionTarget::Command { program, .. } = &request.target else {
            panic!("expected a Command target, got {:?}", request.target);
        };
        assert_eq!(program, "git");
        let preview = request.preview.expect("preview expected");
        assert!(preview.contains("main"), "preview: {preview}");
        assert!(preview.contains("feature-x"), "preview: {preview}");
    }

    #[tokio::test]
    async fn an_empty_branch_name_is_invalid() {
        let tool = GitBranchTool::new();
        let result = tool.permission_request(
            &json!({ "mode": "create", "name": "  " }),
            Path::new("."),
        );
        assert!(matches!(result, Err(ToolError::InvalidArguments(_))));
    }
}
