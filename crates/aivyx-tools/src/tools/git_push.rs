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

const GIT_PUSH_TIMEOUT: Duration = Duration::from_secs(120);

/// The modal preview is advisory context for a human; cap it the same way
/// `git_commit`'s own preview is capped.
const MAX_PREVIEW_CHARS: usize = 4000;

#[derive(Deserialize, JsonSchema)]
struct GitPushArgs {
    /// Remote name (default "origin").
    #[serde(default)]
    remote: Option<String>,
}

/// Pushes the current branch via the user's real `git` (their credentials,
/// their remotes). Always runs with `-u` (set-upstream) — a no-op on
/// subsequent pushes once tracking is already configured, so one code path
/// handles both first-push and later pushes. Deliberately never
/// constructs a `--force`/`--force-with-lease` argv anywhere in this file
/// — force-pushing is a materially more destructive operation class this
/// tool doesn't take on.
pub struct GitPushTool;

impl GitPushTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for GitPushTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for GitPushTool {
    fn name(&self) -> &str {
        "git_push"
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name().to_string(),
            description: "Push the current branch to a remote (default \"origin\"), setting \
                upstream tracking if not already configured. Never force-pushes."
                .to_string(),
            parameters_schema: serde_json::Value::from(schemars::schema_for!(GitPushArgs)),
        }
    }

    fn permission_request(
        &self,
        arguments: &serde_json::Value,
        cwd: &Path,
    ) -> Result<PermissionRequest, ToolError> {
        let args: GitPushArgs = serde_json::from_value(arguments.clone())
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;
        let remote = args.remote.clone().unwrap_or_else(|| "origin".to_string());
        if remote.starts_with('-') {
            return Err(ToolError::InvalidArguments(
                "remote must not start with '-' — this could be misinterpreted as a \
                    command-line flag"
                    .to_string(),
            ));
        }
        let current = current_branch(cwd).unwrap_or_else(|| "(unknown)".to_string());
        let argv = vec![
            "push".to_string(),
            "-u".to_string(),
            remote.clone(),
            current.clone(),
        ];

        Ok(PermissionRequest {
            tool_name: self.name().to_string(),
            action: ActionKind::Execute,
            target: PermissionTarget::Command {
                program: "git".to_string(),
                args: argv,
            },
            arguments_preview: arguments.clone(),
            preview: Some(build_preview(cwd, &remote, &current)),
            diff: None,
        })
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        ctx: &ToolExecutionContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: GitPushArgs = serde_json::from_value(arguments)
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;
        let remote = args.remote.unwrap_or_else(|| "origin".to_string());
        let current = current_branch(&ctx.cwd).ok_or_else(|| {
            ToolError::ExecutionFailed("could not determine the current branch".to_string())
        })?;
        let argv = vec!["push".to_string(), "-u".to_string(), remote, current];
        run(
            git_command(&argv, ctx),
            GIT_PUSH_TIMEOUT,
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

/// What the human sees before approving: the commits that would be pushed.
/// Gracefully empty (not an error) if there's no upstream yet (first
/// push) — `git log <remote>/<branch>..HEAD` exits non-zero when the
/// remote-tracking ref doesn't exist, which `run_git_capture`'s
/// `output.status.success()` gate already treats as "nothing to show."
fn build_preview(cwd: &Path, remote: &str, branch: &str) -> String {
    let range = format!("{remote}/{branch}..HEAD");
    let mut preview = match run_git_capture(cwd, &["log", "--oneline", &range]) {
        Some(log) if !log.trim().is_empty() => format!("Commits to push:\n{log}"),
        _ => "No commits ahead of the remote yet (or this is the first push).".to_string(),
    };
    if preview.chars().count() > MAX_PREVIEW_CHARS {
        let truncated: String = preview.chars().take(MAX_PREVIEW_CHARS).collect();
        preview = format!("{truncated}\n[... preview truncated ...]");
    }
    preview
}

fn run_git_capture(cwd: &Path, args: &[&str]) -> Option<String> {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
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

    /// A real, local **bare** repository used as the test remote — a
    /// genuine `git remote add`/`git push` round-trip entirely on local
    /// disk, no network, no GitHub. A plain absolute filesystem path
    /// works directly as a git remote URL for a local bare repo (no
    /// `file://` prefix needed).
    async fn add_bare_remote(dir: &Path, name: &str) -> tempfile::TempDir {
        let bare_dir = tempfile::tempdir().unwrap();
        git(bare_dir.path(), &["init", "--bare", "-q"]).await;
        git(
            dir,
            &["remote", "add", name, bare_dir.path().to_str().unwrap()],
        )
        .await;
        bare_dir
    }

    #[tokio::test]
    async fn first_push_sets_upstream_and_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path()).await;
        let _bare_dir = add_bare_remote(dir.path(), "origin").await;

        let tool = GitPushTool::new();
        let output = tool.execute(json!({}), &ctx(dir.path())).await.unwrap();
        let ToolOutput::Ok(text) = output else {
            panic!("expected Ok output")
        };
        assert!(text.contains("success"), "push failed: {text}");

        let tracking = git(
            dir.path(),
            &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"],
        )
        .await;
        assert_eq!(tracking.trim(), "origin/main");
    }

    #[tokio::test]
    async fn subsequent_push_succeeds_with_upstream_already_set() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path()).await;
        let _bare_dir = add_bare_remote(dir.path(), "origin").await;

        let tool = GitPushTool::new();
        tool.execute(json!({}), &ctx(dir.path())).await.unwrap();

        std::fs::write(dir.path().join("tracked.txt"), "v2\n").unwrap();
        git(dir.path(), &["commit", "-aqm", "second commit"]).await;

        let output = tool.execute(json!({}), &ctx(dir.path())).await.unwrap();
        let ToolOutput::Ok(text) = output else {
            panic!("expected Ok output")
        };
        assert!(text.contains("success"), "second push failed: {text}");
    }

    #[tokio::test]
    async fn preview_shows_commits_ahead_of_the_remote() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path()).await;
        let _bare_dir = add_bare_remote(dir.path(), "origin").await;
        git(dir.path(), &["push", "-u", "origin", "main"]).await;
        std::fs::write(dir.path().join("tracked.txt"), "v2\n").unwrap();
        git(dir.path(), &["commit", "-aqm", "ahead commit"]).await;

        let tool = GitPushTool::new();
        let request = tool.permission_request(&json!({}), dir.path()).unwrap();
        let preview = request.preview.expect("preview expected");
        assert!(preview.contains("ahead commit"), "preview: {preview}");
    }

    #[tokio::test]
    async fn preview_is_graceful_when_there_is_no_upstream_yet() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path()).await;

        let tool = GitPushTool::new();
        let request = tool.permission_request(&json!({}), dir.path()).unwrap();
        let preview = request.preview.expect("preview expected");
        assert!(preview.contains("first push") || preview.contains("No commits"));
    }

    #[tokio::test]
    async fn remote_starting_with_dash_is_rejected() {
        let tool = GitPushTool::new();
        let result = tool.permission_request(&json!({ "remote": "-f" }), Path::new("."));
        assert!(matches!(result, Err(ToolError::InvalidArguments(_))));
    }
}
