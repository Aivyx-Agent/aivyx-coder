use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use aivyx_sandbox::{ActionKind, PermissionRequest, PermissionTarget};
use aivyx_types::{ToolDefinition, ToolOutput};
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;

use aivyx_checkpoint::exclude_pathspecs;
use crate::path_resolve::resolve;
use crate::process::run;
use crate::{Tool, ToolError, ToolExecutionContext};

/// Local-only inspection commands; generous for huge repos.
const GIT_READ_TIMEOUT: Duration = Duration::from_secs(60);

const MAX_LOG_COUNT: u32 = 50;
const DEFAULT_LOG_COUNT: u32 = 10;

#[derive(Deserialize, JsonSchema)]
struct GitReadArgs {
    /// What to inspect: "status" (working tree status), "diff" (changes), "log" (recent commits), or "branches" (local branches with upstream tracking info).
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
    Branches,
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
        // Global `-c core.fsmonitor=false` on every invocation: defense in
        // depth alongside the gate's hard-deny on writing `.git/config`
        // (audit finding A1) -- this is a confined process with the
        // network open, so a repo's own config naming an fsmonitor hook
        // must not get the chance to run even read-only.
        let mut argv: Vec<String> = vec!["-c".into(), "core.fsmonitor=false".into()];
        match args.mode {
            GitReadMode::Status => {
                argv.push("status".into());
                argv.push("--short".into());
                argv.push("--branch".into());
                if !excludes.is_empty() {
                    argv.push("--".into());
                    argv.push(".".into());
                    argv.extend(excludes);
                }
            }
            GitReadMode::Diff => {
                argv.push("diff".into());
                if args.staged {
                    argv.push("--cached".into());
                }
                // A `diff.<driver>.textconv`/`diff.*.command` entry in the
                // repo's own config can run an arbitrary program to render
                // a "diff" -- disabled regardless of what the config says.
                argv.push("--no-ext-diff".into());
                argv.push("--no-textconv".into());
                argv.push("--".into());
                match &args.path {
                    Some(path) => argv.push(resolve(cwd, path).display().to_string()),
                    None => argv.push(".".into()),
                }
                argv.extend(excludes);
            }
            GitReadMode::Log => {
                let count = args
                    .count
                    .unwrap_or(DEFAULT_LOG_COUNT)
                    .clamp(1, MAX_LOG_COUNT);
                // `log.showSignature`/a configured `gpg.program` could run
                // an arbitrary signature-verification helper per commit --
                // disabled both ways (global config key and the per-run
                // flag) regardless of what the repo's config says.
                argv.push("-c".into());
                argv.push("log.showSignature=false".into());
                argv.push("log".into());
                argv.push("--oneline".into());
                argv.push("--no-show-signature".into());
                argv.push("-n".into());
                argv.push(count.to_string());
            }
            GitReadMode::Branches => {
                argv.push("branch".into());
                argv.push("-vv".into());
            }
        }
        argv
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
                mode \"log\" shows recent commits (count, default 10), mode \"branches\" shows \
                local branches with upstream tracking info."
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
            diff: None,
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
    use aivyx_checkpoint::test_support::{git, init_repo};
    use serde_json::json;

    // --- build_args argv assertions (audit finding A1 item 3) ---
    //
    // `git_read` already runs confined (Landlock + seccomp), but the
    // network is open to a confined process -- so a planted
    // `core.fsmonitor`/`diff.*.textconv`/a signature-verification helper
    // in the repo's own `.git/config` could still exfiltrate or run
    // arbitrary logic when `status`/`diff`/`log` runs. Global `-c`
    // overrides neutralize that regardless of what the repo's config says,
    // as defense in depth alongside A1.1's hard-deny on writing there.

    #[test]
    fn every_mode_disables_fsmonitor_via_a_global_dash_c() {
        let tool = GitReadTool::new(vec![]);
        for args in [
            GitReadArgs { mode: GitReadMode::Status, path: None, staged: false, count: None },
            GitReadArgs { mode: GitReadMode::Diff, path: None, staged: false, count: None },
            GitReadArgs { mode: GitReadMode::Log, path: None, staged: false, count: None },
            GitReadArgs { mode: GitReadMode::Branches, path: None, staged: false, count: None },
        ] {
            let argv = tool.build_args(&args, Path::new("/repo"));
            assert_eq!(
                &argv[..2],
                &["-c".to_string(), "core.fsmonitor=false".to_string()],
                "argv did not start with the global fsmonitor override: {argv:?}"
            );
        }
    }

    #[test]
    fn diff_mode_disables_ext_diff_and_textconv() {
        let tool = GitReadTool::new(vec![]);
        let args = GitReadArgs { mode: GitReadMode::Diff, path: None, staged: false, count: None };
        let argv = tool.build_args(&args, Path::new("/repo"));
        assert!(argv.contains(&"--no-ext-diff".to_string()), "argv: {argv:?}");
        assert!(argv.contains(&"--no-textconv".to_string()), "argv: {argv:?}");
    }

    #[test]
    fn log_mode_disables_signature_verification() {
        let tool = GitReadTool::new(vec![]);
        let args = GitReadArgs { mode: GitReadMode::Log, path: None, staged: false, count: None };
        let argv = tool.build_args(&args, Path::new("/repo"));
        assert!(
            argv.windows(2).any(|w| w == ["-c".to_string(), "log.showSignature=false".to_string()]),
            "argv missing a global -c log.showSignature=false: {argv:?}"
        );
        assert!(
            argv.contains(&"--no-show-signature".to_string()),
            "argv: {argv:?}"
        );
    }

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
    async fn branches_mode_lists_local_branches_with_the_current_one_marked() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path()).await;
        git(dir.path(), &["checkout", "-b", "feature-x"]).await;

        let tool = GitReadTool::new(vec![]);
        let text = run_tool(&tool, dir.path(), json!({ "mode": "branches" })).await;

        assert!(text.contains("feature-x"));
        assert!(text.contains("main"));
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
