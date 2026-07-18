use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use aivyx_sandbox::{ActionKind, PermissionRequest, PermissionTarget};
use aivyx_types::{ToolDefinition, ToolOutput};
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;

use crate::checkpoint::exclude_pathspecs;
use crate::path_resolve::{is_denied, resolve};
use crate::process::run;
use crate::{Tool, ToolError, ToolExecutionContext};

const GIT_COMMIT_TIMEOUT: Duration = Duration::from_secs(60);

/// The modal preview is advisory context for a human; cap it so a huge
/// worktree can't blow up the modal.
const MAX_PREVIEW_CHARS: usize = 4000;

#[derive(Deserialize, JsonSchema)]
struct GitCommitArgs {
    /// Commit message. The first line is the subject; add a blank line before any body.
    message: String,
    /// Specific files/directories to commit (absolute or relative to the working directory). Omit to commit all current changes.
    #[serde(default)]
    paths: Vec<String>,
}

/// Stages and commits via the user's real `git` (their identity, hooks,
/// and config). Always prompted: the permission target is the literal
/// `git commit` argv (a `Command`, deliberately not a `Path`), so an
/// Always-Allow can never blanket-approve future commits — every distinct
/// message is a distinct cache key.
pub struct GitCommitTool {
    deny_paths: Vec<PathBuf>,
}

impl GitCommitTool {
    pub fn new(deny_paths: Vec<PathBuf>) -> Self {
        Self { deny_paths }
    }

    fn resolved_paths(&self, args: &GitCommitArgs, cwd: &Path) -> Result<Vec<PathBuf>, ToolError> {
        let mut resolved = Vec::new();
        for path in &args.paths {
            let path = resolve(cwd, path);
            if is_denied(&path, &self.deny_paths) {
                return Err(ToolError::ExecutionFailed(format!(
                    "path `{}` is under a configured deny_paths entry",
                    path.display()
                )));
            }
            resolved.push(path);
        }
        Ok(resolved)
    }

    /// Pathspecs shared by the `add` and the pathspec-scoped `commit`:
    /// explicit paths as given, or "everything under the cwd minus
    /// deny_paths". The commit being pathspec-scoped (rather than
    /// whole-index) keeps anything the user had staged *outside* the
    /// requested scope out of the agent's commit.
    fn pathspecs(&self, resolved: &[PathBuf], cwd: &Path) -> Vec<String> {
        if resolved.is_empty() {
            let mut specs = vec![".".to_string()];
            specs.extend(exclude_pathspecs(cwd, &self.deny_paths));
            specs
        } else {
            resolved.iter().map(|p| p.display().to_string()).collect()
        }
    }
}

#[async_trait]
impl Tool for GitCommitTool {
    fn name(&self) -> &str {
        "git_commit"
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name().to_string(),
            description: "Stage and commit changes to the git repository in the working \
                directory, with the given commit message. Commits all current changes unless \
                specific paths are given. Note: this (re)stages the affected paths — any \
                partially staged hunks within them are staged in full. The user sees a summary \
                of what will be committed and must approve."
                .to_string(),
            parameters_schema: serde_json::Value::from(schemars::schema_for!(GitCommitArgs)),
        }
    }

    fn permission_request(
        &self,
        arguments: &serde_json::Value,
        cwd: &Path,
    ) -> Result<PermissionRequest, ToolError> {
        let args: GitCommitArgs = serde_json::from_value(arguments.clone())
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;
        if args.message.trim().is_empty() {
            return Err(ToolError::InvalidArguments(
                "commit message must not be empty".to_string(),
            ));
        }
        let resolved = self.resolved_paths(&args, cwd)?;

        let mut argv = vec!["commit".to_string(), "-m".to_string(), args.message.clone()];
        if !resolved.is_empty() {
            argv.push("--".to_string());
            argv.extend(resolved.iter().map(|p| p.display().to_string()));
        }

        Ok(PermissionRequest {
            tool_name: self.name().to_string(),
            action: ActionKind::Execute,
            target: PermissionTarget::Command {
                program: "git".to_string(),
                args: argv,
            },
            arguments_preview: arguments.clone(),
            preview: Some(build_preview(cwd, &self.pathspecs(&resolved, cwd))),
            diff: None,
        })
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        ctx: &ToolExecutionContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: GitCommitArgs = serde_json::from_value(arguments)
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;
        let resolved = self.resolved_paths(&args, &ctx.cwd)?;
        let pathspecs = self.pathspecs(&resolved, &ctx.cwd);

        // Stage first so untracked files become committable; the commit
        // itself is pathspec-scoped to the same set either way.
        let mut add_args: Vec<String> = vec!["add".into(), "-A".into(), "--".into()];
        add_args.extend(pathspecs.iter().cloned());
        let add_output = run(
            git_command(&add_args, ctx),
            GIT_COMMIT_TIMEOUT,
            ctx.cancellation.clone(),
        )
        .await?;
        if let ToolOutput::Ok(text) = &add_output
            && text.contains("(failed)")
        {
            return Ok(add_output); // e.g. not a repository — informative as-is
        }

        let mut commit_args: Vec<String> =
            vec!["commit".into(), "-m".into(), args.message, "--".into()];
        commit_args.extend(pathspecs);
        run(
            git_command(&commit_args, ctx),
            GIT_COMMIT_TIMEOUT,
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

/// What the human sees in the modal before approving: a file-granularity
/// change summary plus the working-tree status (which is what surfaces
/// untracked files and anything of the user's own that the commit scope
/// would sweep in). Synchronous and bounded — the `Tool` contract allows a
/// bounded read here, and both commands are local metadata operations.
fn build_preview(cwd: &Path, pathspecs: &[String]) -> String {
    let mut preview = String::new();
    let mut stat_args = vec!["diff", "HEAD", "--stat", "--"];
    stat_args.extend(pathspecs.iter().map(String::as_str));
    if let Some(stat) = run_git_capture(cwd, &stat_args)
        && !stat.trim().is_empty()
    {
        preview.push_str("Changes vs HEAD:\n");
        preview.push_str(&stat);
    }
    if let Some(status) = run_git_capture(cwd, &["status", "--short"]) {
        preview.push_str("\nWorking tree status:\n");
        preview.push_str(&status);
    }
    if preview.is_empty() {
        preview = "(no preview available — is this a git repository?)".to_string();
    }
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

    #[tokio::test]
    async fn commits_all_changes_including_untracked_files() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path()).await;
        std::fs::write(dir.path().join("tracked.txt"), "edited\n").unwrap();
        std::fs::write(dir.path().join("new.txt"), "brand new\n").unwrap();

        let tool = GitCommitTool::new(vec![]);
        let output = tool
            .execute(json!({ "message": "agent: test commit" }), &ctx(dir.path()))
            .await
            .unwrap();

        let ToolOutput::Ok(text) = output else {
            panic!("expected Ok output")
        };
        assert!(text.contains("success"), "commit failed: {text}");

        let log = git(dir.path(), &["log", "--oneline", "-n", "1"]).await;
        assert!(log.contains("agent: test commit"));
        let status = git(dir.path(), &["status", "--porcelain"]).await;
        assert_eq!(status.trim(), "", "worktree should be clean: {status}");
    }

    #[tokio::test]
    async fn a_scoped_commit_leaves_other_changes_uncommitted() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path()).await;
        std::fs::write(dir.path().join("tracked.txt"), "edited\n").unwrap();
        std::fs::write(dir.path().join("other.txt"), "leave me\n").unwrap();

        let tool = GitCommitTool::new(vec![]);
        tool.execute(
            json!({ "message": "scoped", "paths": ["tracked.txt"] }),
            &ctx(dir.path()),
        )
        .await
        .unwrap();

        let status = git(dir.path(), &["status", "--porcelain"]).await;
        assert!(
            status.contains("other.txt"),
            "other.txt should remain: {status}"
        );
        assert!(!status.contains("tracked.txt"));
    }

    #[tokio::test]
    async fn a_denied_explicit_path_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path()).await;
        let cwd = dir.path().canonicalize().unwrap();
        let secret = cwd.join("secret");
        std::fs::create_dir(&secret).unwrap();
        std::fs::write(secret.join("key"), "TOP-SECRET\n").unwrap();

        let tool = GitCommitTool::new(vec![secret]);
        let result = tool
            .execute(
                json!({ "message": "sneaky", "paths": ["secret/key"] }),
                &ctx(&cwd),
            )
            .await;
        assert!(matches!(result, Err(ToolError::ExecutionFailed(_))));
    }

    #[tokio::test]
    async fn commit_all_excludes_denied_subpaths() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path()).await;
        let cwd = dir.path().canonicalize().unwrap();
        let secret = cwd.join("secret");
        std::fs::create_dir(&secret).unwrap();
        std::fs::write(secret.join("key"), "TOP-SECRET\n").unwrap();
        std::fs::write(cwd.join("public.txt"), "fine\n").unwrap();

        let tool = GitCommitTool::new(vec![secret]);
        tool.execute(json!({ "message": "all" }), &ctx(&cwd))
            .await
            .unwrap();

        let tree = git(&cwd, &["ls-tree", "-r", "--name-only", "HEAD"]).await;
        assert!(tree.contains("public.txt"));
        assert!(!tree.contains("secret"), "denied subtree committed: {tree}");
    }

    #[tokio::test]
    async fn permission_request_is_a_command_target_with_a_stat_preview() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path()).await;
        std::fs::write(dir.path().join("tracked.txt"), "edited\n").unwrap();

        let tool = GitCommitTool::new(vec![]);
        let request = tool
            .permission_request(&json!({ "message": "preview me" }), dir.path())
            .unwrap();

        assert_eq!(request.action, ActionKind::Execute);
        let PermissionTarget::Command { program, args } = &request.target else {
            panic!("expected a Command target, got {:?}", request.target);
        };
        assert_eq!(program, "git");
        assert!(args.contains(&"preview me".to_string()));
        let preview = request.preview.expect("preview expected");
        assert!(preview.contains("tracked.txt"), "preview: {preview}");
    }

    #[tokio::test]
    async fn an_empty_message_is_invalid() {
        let tool = GitCommitTool::new(vec![]);
        let result = tool.permission_request(&json!({ "message": "  " }), Path::new("."));
        assert!(matches!(result, Err(ToolError::InvalidArguments(_))));
    }
}
