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

const GIT_PR_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Deserialize, JsonSchema)]
struct GitPrArgs {
    /// PR title.
    title: String,
    /// PR body/description.
    #[serde(default)]
    body: Option<String>,
    /// Base branch to open the PR against (omit to use the repo's default branch).
    #[serde(default)]
    base: Option<String>,
    /// Open as a draft PR.
    #[serde(default)]
    draft: bool,
}

/// Opens a pull request via the user's real `gh` CLI (their auth, their
/// GitHub account). Requires the current branch already pushed with a
/// remote tracking branch — checked deterministically before ever
/// invoking `gh` (`check_upstream_configured`), so a missing push produces
/// a clear "call git_push first" error rather than a confusing `gh`
/// failure. `gh` itself is also preflight-checked
/// (`check_gh_authenticated`) so "not installed" and "installed but not
/// authenticated" get distinct, actionable error messages. Always
/// registered (no config flag): a missing/unauthenticated `gh` is an
/// environmental accident, the same reasoning `go_to_definition`/
/// `find_references` already apply to a missing `rust-analyzer`.
pub struct GitPrTool {
    gh_program: String,
}

impl GitPrTool {
    pub fn new() -> Self {
        Self {
            gh_program: "gh".to_string(),
        }
    }

    /// Test-only: points at a fake `gh` stand-in instead of the real
    /// binary, so tests never depend on a real GitHub account, real
    /// authentication, or network access.
    #[cfg(test)]
    pub(crate) fn with_gh_program(program: &str) -> Self {
        Self {
            gh_program: program.to_string(),
        }
    }

    fn build_argv(args: &GitPrArgs) -> Vec<String> {
        let mut argv = vec![
            "pr".to_string(),
            "create".to_string(),
            "--title".to_string(),
            args.title.clone(),
        ];
        if let Some(body) = &args.body {
            argv.push("--body".to_string());
            argv.push(body.clone());
        }
        if let Some(base) = &args.base {
            argv.push("--base".to_string());
            argv.push(base.clone());
        }
        if args.draft {
            argv.push("--draft".to_string());
        }
        argv
    }
}

impl Default for GitPrTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for GitPrTool {
    fn name(&self) -> &str {
        "git_pr"
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name().to_string(),
            description: "Open a pull request for the current branch via the gh CLI. The \
                branch must already be pushed (use git_push first) — this returns a clear \
                error naming that fix if there's no upstream configured yet. Returns the \
                created PR's URL."
                .to_string(),
            parameters_schema: serde_json::Value::from(schemars::schema_for!(GitPrArgs)),
        }
    }

    fn permission_request(
        &self,
        arguments: &serde_json::Value,
        _cwd: &Path,
    ) -> Result<PermissionRequest, ToolError> {
        let args: GitPrArgs = serde_json::from_value(arguments.clone())
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;
        if args.title.trim().is_empty() {
            return Err(ToolError::InvalidArguments(
                "PR title must not be empty".to_string(),
            ));
        }
        let argv = Self::build_argv(&args);

        Ok(PermissionRequest {
            tool_name: self.name().to_string(),
            action: ActionKind::Execute,
            target: PermissionTarget::Command {
                program: self.gh_program.clone(),
                args: argv,
            },
            arguments_preview: arguments.clone(),
            preview: Some(format!("Opens a pull request titled \"{}\"", args.title)),
            diff: None,
        })
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        ctx: &ToolExecutionContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: GitPrArgs = serde_json::from_value(arguments)
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;

        if let Err(message) = check_upstream_configured(&ctx.cwd) {
            return Ok(ToolOutput::Error(message));
        }
        if let Err(message) = check_gh_authenticated(&self.gh_program, &ctx.cwd) {
            return Ok(ToolOutput::Error(message));
        }

        let argv = Self::build_argv(&args);
        run(
            gh_command(&self.gh_program, &argv, ctx),
            GIT_PR_TIMEOUT,
            ctx.cancellation.clone(),
        )
        .await
    }
}

fn gh_command(
    gh_program: &str,
    args: &[String],
    ctx: &ToolExecutionContext,
) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(gh_program);
    command
        .args(args)
        .current_dir(&ctx.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    ctx.confiner.confine(command)
}

/// Deterministic preflight: a non-zero exit means no upstream is
/// configured for the current branch, regardless of the exact stderr text
/// (which varies across git versions) — the model is told to call
/// `git_push` first rather than the tool silently pushing on its behalf.
fn check_upstream_configured(cwd: &Path) -> Result<(), String> {
    let status = std::process::Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"])
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    match status {
        Ok(status) if status.success() => Ok(()),
        _ => Err(
            "the current branch has no upstream — call git_push first, then try git_pr again"
                .to_string(),
        ),
    }
}

/// Deterministic preflight for `gh` itself: distinguishes "not installed"
/// (spawn failure) from "installed but not authenticated" (spawns fine,
/// exits non-zero) so the error names the actual fix — never by parsing
/// `gh`'s own stderr text.
fn check_gh_authenticated(gh_program: &str, cwd: &Path) -> Result<(), String> {
    let status = std::process::Command::new(gh_program)
        .args(["auth", "status"])
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    match status {
        Ok(status) if status.success() => Ok(()),
        Ok(_) => Err(
            "gh is installed but not authenticated — run `gh auth login`, then try git_pr again"
                .to_string(),
        ),
        Err(_) => Err(format!(
            "gh CLI not found (tried \"{gh_program}\") — install it from \
             https://cli.github.com, then try git_pr again"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::checkpoint::test_support::{git, init_repo};
    use serde_json::json;
    use std::os::unix::fs::PermissionsExt;

    fn ctx(dir: &Path) -> ToolExecutionContext {
        ToolExecutionContext {
            cwd: dir.to_path_buf(),
            confiner: std::sync::Arc::new(aivyx_sandbox::NoopConfiner),
            cancellation: tokio_util::sync::CancellationToken::new(),
        }
    }

    /// Writes a tiny, executable fake `gh` script to a temp dir and returns
    /// its absolute path. `auth status` succeeds (authenticated); `pr
    /// create` echoes a fake PR URL to stdout — no real GitHub account or
    /// network access involved anywhere in this test suite.
    fn fake_gh(script_dir: &Path) -> std::path::PathBuf {
        let script = script_dir.join("gh");
        std::fs::write(
            &script,
            "#!/bin/sh\n\
             if [ \"$1\" = \"auth\" ]; then exit 0; fi\n\
             if [ \"$1\" = \"pr\" ]; then echo \"https://github.com/example/repo/pull/1\"; exit 0; fi\n\
             exit 1\n",
        )
        .unwrap();
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).unwrap();
        script
    }

    /// Same shape as `fake_gh`, but `auth status` fails (not authenticated).
    fn fake_gh_unauthenticated(script_dir: &Path) -> std::path::PathBuf {
        let script = script_dir.join("gh-unauth");
        std::fs::write(&script, "#!/bin/sh\nexit 1\n").unwrap();
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).unwrap();
        script
    }

    /// `git_pr`'s own checks (the local upstream-tracking preflight, and
    /// the fake `gh` stub) never touch the actual remote again after the
    /// push completes, so the bare repo can simply drop at the end of this
    /// function — no need to keep it alive for the caller.
    async fn push_to_bare_origin(dir: &Path) {
        let bare_dir = tempfile::tempdir().unwrap();
        git(bare_dir.path(), &["init", "--bare", "-q"]).await;
        git(
            dir,
            &["remote", "add", "origin", bare_dir.path().to_str().unwrap()],
        )
        .await;
        git(dir, &["push", "-u", "origin", "main"]).await;
    }

    #[tokio::test]
    async fn returns_a_clear_error_when_there_is_no_upstream() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path()).await;
        let script_dir = tempfile::tempdir().unwrap();
        let gh = fake_gh(script_dir.path());

        let tool = GitPrTool::with_gh_program(gh.to_str().unwrap());
        let output = tool
            .execute(json!({ "title": "my pr" }), &ctx(dir.path()))
            .await
            .unwrap();
        let ToolOutput::Error(message) = output else {
            panic!("expected Error output, got {output:?}")
        };
        assert!(message.contains("git_push"), "message: {message}");
    }

    #[tokio::test]
    async fn returns_a_clear_error_when_gh_is_not_authenticated() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path()).await;
        push_to_bare_origin(dir.path()).await;
        let script_dir = tempfile::tempdir().unwrap();
        let gh = fake_gh_unauthenticated(script_dir.path());

        let tool = GitPrTool::with_gh_program(gh.to_str().unwrap());
        let output = tool
            .execute(json!({ "title": "my pr" }), &ctx(dir.path()))
            .await
            .unwrap();
        let ToolOutput::Error(message) = output else {
            panic!("expected Error output, got {output:?}")
        };
        assert!(message.contains("gh auth login"), "message: {message}");
    }

    #[tokio::test]
    async fn returns_a_clear_error_when_gh_is_not_found() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path()).await;
        push_to_bare_origin(dir.path()).await;

        let tool = GitPrTool::with_gh_program("definitely-not-a-real-binary-xyz");
        let output = tool
            .execute(json!({ "title": "my pr" }), &ctx(dir.path()))
            .await
            .unwrap();
        let ToolOutput::Error(message) = output else {
            panic!("expected Error output, got {output:?}")
        };
        assert!(message.contains("not found"), "message: {message}");
    }

    #[tokio::test]
    async fn succeeds_and_returns_the_pr_url_when_everything_is_ready() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path()).await;
        push_to_bare_origin(dir.path()).await;
        let script_dir = tempfile::tempdir().unwrap();
        let gh = fake_gh(script_dir.path());

        let tool = GitPrTool::with_gh_program(gh.to_str().unwrap());
        let output = tool
            .execute(json!({ "title": "my pr" }), &ctx(dir.path()))
            .await
            .unwrap();
        let ToolOutput::Ok(text) = output else {
            panic!("expected Ok output, got {output:?}")
        };
        assert!(
            text.contains("https://github.com/example/repo/pull/1"),
            "text: {text}"
        );
    }

    #[tokio::test]
    async fn permission_request_argv_includes_body_base_and_draft() {
        let tool = GitPrTool::new();
        let request = tool
            .permission_request(
                &json!({
                    "title": "my pr",
                    "body": "description here",
                    "base": "develop",
                    "draft": true
                }),
                Path::new("."),
            )
            .unwrap();

        let PermissionTarget::Command { args, .. } = &request.target else {
            panic!("expected a Command target, got {:?}", request.target);
        };
        assert!(args.contains(&"--body".to_string()));
        assert!(args.contains(&"description here".to_string()));
        assert!(args.contains(&"--base".to_string()));
        assert!(args.contains(&"develop".to_string()));
        assert!(args.contains(&"--draft".to_string()));
    }

    #[tokio::test]
    async fn an_empty_title_is_invalid() {
        let tool = GitPrTool::new();
        let result = tool.permission_request(&json!({ "title": "  " }), Path::new("."));
        assert!(matches!(result, Err(ToolError::InvalidArguments(_))));
    }
}
