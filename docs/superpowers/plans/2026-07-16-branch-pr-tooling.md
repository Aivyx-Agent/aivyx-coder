# Branch/PR Tooling Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give aivyx-coder purpose-built tools for branch listing/creation/switching, pushing, and PR creation, replacing the current `run_shell`-only path for these operations.

**Architecture:** Extend the existing `git_read` tool with a fourth, read-only `branches` mode; add three new tools (`git_branch`, `git_push`, `git_pr`) that each follow `git_commit`'s exact established shape (`ActionKind::Execute` + `PermissionTarget::Command`, fixed argv, confirm-gated, no new `ActionKind`).

**Tech Stack:** Rust, the user's real `git` and `gh` CLIs (shelled out to, never reimplemented), the existing `crate::process::run` execution helper.

## Global Constraints

- Every new tool uses `ActionKind::Execute` + `PermissionTarget::Command` — the exact tier `git_commit` already uses. No new `ActionKind`.
- Fixed argv shapes only — model input is validated and passed as discrete arguments, never interpolated into a shell string.
- `git_read`'s new `branches` mode is `ActionKind::Read` (auto-allow) on the *existing* `GitReadTool` — no new tool for listing.
- `git_branch`'s `create` mode always switches to the new branch (`git checkout -b`), matching how `git_commit` already does "stage + commit" as one action.
- `git_push` always runs `git push -u <remote> <branch>` (the `-u` flag is a no-op once tracking exists) — **no `--force`/`--force-with-lease` support anywhere in the argv construction**, not even as an unexposed internal flag.
- `git_pr` checks `git rev-parse --abbrev-ref --symbolic-full-name @{u}` *before* ever invoking `gh` — any non-zero exit means "no upstream," full stop, never parsed for specific stderr text (git's exact wording isn't a stable contract to depend on).
- `git_pr` also preflight-checks `gh auth status` before the real `gh pr create` call, distinguishing "gh not found" (spawn failure) from "gh found but not authenticated" (spawns, exits non-zero) — again by exit behavior, never by parsing `gh`'s stderr text.
- `git_pr` is **always registered**, no config flag — a missing/unauthenticated `gh` is an environmental accident, the same reasoning `go_to_definition`/`find_references` already apply to a missing `rust-analyzer`.
- All HTTP/process-touching tests run against real local git repos (and, for `git_push`, a real local **bare** repo used as the test remote) or a fake `gh` stand-in — never a real network call, never a real GitHub account.
- `git_branch`/`git_push`/`git_pr` take no `deny_paths` parameter — unlike `git_read`/`git_commit`, none of them accept a file-path argument to scope or deny.

---

### Task 1: `git_read`'s new `branches` mode

**Files:**
- Modify: `crates/aivyx-tools/src/tools/git_read.rs`

**Interfaces:**
- Produces: `GitReadMode::Branches` variant (parsed from `"branches"` via the existing `#[serde(rename_all = "snake_case")]`), reachable via the existing `GitReadTool` — no new public API.

- [ ] **Step 1: Write the failing test**

In `crates/aivyx-tools/src/tools/git_read.rs`'s `#[cfg(test)] mod tests` block, add (after the existing `log_lists_recent_commits` test):

```rust
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
```

- [ ] **Step 2: Run to confirm it fails**

Run: `cargo test -p aivyx-tools git_read::tests::branches_mode`
Expected: FAIL to compile (`"branches"` doesn't deserialize to any `GitReadMode` variant yet).

- [ ] **Step 3: Add the `Branches` variant and its argv**

In `crates/aivyx-tools/src/tools/git_read.rs`, change:

```rust
#[derive(Deserialize, JsonSchema, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum GitReadMode {
    Status,
    Diff,
    Log,
}
```

to:

```rust
#[derive(Deserialize, JsonSchema, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum GitReadMode {
    Status,
    Diff,
    Log,
    Branches,
}
```

In `build_args`'s `match args.mode` block, add a new arm right after the `GitReadMode::Log` arm (`path`/`staged`/`count` are simply irrelevant to this mode, exactly as `count`/`staged` are already irrelevant to `Status`):

```rust
GitReadMode::Branches => vec!["branch".into(), "-vv".into()],
```

Update `GitReadArgs.mode`'s doc comment (currently `/// What to inspect: "status" (working tree status), "diff" (changes), or "log" (recent commits).`) to:

```rust
/// What to inspect: "status" (working tree status), "diff" (changes), "log" (recent commits), or "branches" (local branches with upstream tracking info).
mode: GitReadMode,
```

Update `GitReadTool`'s `definition()` description string (currently ending `..."log" shows recent commits (count, default 10)."`) to append the new mode:

```rust
description: "Inspect the git repository in the working directory (read-only): \
    mode \"status\" shows branch and changed files, mode \"diff\" shows unstaged \
    changes (set staged=true for staged ones, path to limit to one file/directory), \
    mode \"log\" shows recent commits (count, default 10), mode \"branches\" shows \
    local branches with upstream tracking info."
    .to_string(),
```

- [ ] **Step 4: Run to confirm it passes**

Run: `cargo test -p aivyx-tools git_read::`
Expected: PASS — all `git_read` tests including the new `branches_mode_lists_local_branches_with_the_current_one_marked`.

- [ ] **Step 5: Run the whole crate's tests and clippy**

Run: `cargo test -p aivyx-tools`
Run: `cargo clippy -p aivyx-tools --all-targets`
Expected: all pass, clippy clean.

- [ ] **Step 6: Commit**

```bash
git add crates/aivyx-tools/src/tools/git_read.rs
git commit -m "Branch/PR tooling: add branches mode to git_read (Task 1)"
```

---

### Task 2: `git_branch` tool

**Files:**
- Create: `crates/aivyx-tools/src/tools/git_branch.rs`
- Modify: `crates/aivyx-tools/src/tools/mod.rs`
- Modify: `crates/aivyx-tools/src/lib.rs`

**Interfaces:**
- Consumes: `crate::process::run` (existing), `crate::checkpoint::test_support::{git, init_repo}` (existing, test-only).
- Produces: `pub struct GitBranchTool` with `pub fn new() -> Self`. Registered by Task 5's `main.rs` wiring.

- [ ] **Step 1: Write the failing tests**

Create `crates/aivyx-tools/src/tools/git_branch.rs`:

```rust
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
```

- [ ] **Step 2: Run to confirm it fails**

Run: `cargo test -p aivyx-tools git_branch`
Expected: FAIL to compile (`git_branch` module not yet wired into `tools/mod.rs`).

- [ ] **Step 3: Wire the module in**

In `crates/aivyx-tools/src/tools/mod.rs`, add alphabetically (`git_branch` sorts before `git_commit`):

```rust
mod git_branch;
```

and:

```rust
pub use git_branch::GitBranchTool;
```

In `crates/aivyx-tools/src/lib.rs`'s `pub use tools::{ ... };` block, add `GitBranchTool` alphabetically.

- [ ] **Step 4: Run to confirm it passes**

Run: `cargo test -p aivyx-tools git_branch::`
Expected: PASS — 5 tests (`create_switches_to_a_new_branch_from_head`, `create_branches_from_a_given_base`, `switch_moves_to_an_existing_branch`, `permission_request_preview_shows_current_and_target_branch`, `an_empty_branch_name_is_invalid`).

- [ ] **Step 5: Run the whole crate's tests and clippy**

Run: `cargo test -p aivyx-tools`
Run: `cargo clippy -p aivyx-tools --all-targets`
Expected: all pass, clippy clean.

- [ ] **Step 6: Commit**

```bash
git add crates/aivyx-tools/src/tools/git_branch.rs crates/aivyx-tools/src/tools/mod.rs crates/aivyx-tools/src/lib.rs
git commit -m "Branch/PR tooling: git_branch tool (Task 2)"
```

---

### Task 3: `git_push` tool

**Files:**
- Create: `crates/aivyx-tools/src/tools/git_push.rs`
- Modify: `crates/aivyx-tools/src/tools/mod.rs`
- Modify: `crates/aivyx-tools/src/lib.rs`

**Interfaces:**
- Consumes: `crate::process::run` (existing), `crate::checkpoint::test_support::{git, init_repo}` (existing, test-only).
- Produces: `pub struct GitPushTool` with `pub fn new() -> Self`. Registered by Task 5's `main.rs` wiring.

- [ ] **Step 1: Write the failing tests**

Create `crates/aivyx-tools/src/tools/git_push.rs`:

```rust
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
}
```

- [ ] **Step 2: Run to confirm it fails**

Run: `cargo test -p aivyx-tools git_push`
Expected: FAIL to compile (`git_push` module not yet wired into `tools/mod.rs`).

- [ ] **Step 3: Wire the module in**

In `crates/aivyx-tools/src/tools/mod.rs`, add alphabetically (`git_push` sorts between `git_pr` from Task 4 and `git_read` — for now, right after `git_commit` since Task 4 hasn't landed yet; Task 4's implementer will re-sort if needed, but landing this alphabetically relative to what already exists is enough):

```rust
mod git_push;
```

and:

```rust
pub use git_push::GitPushTool;
```

In `crates/aivyx-tools/src/lib.rs`'s `pub use tools::{ ... };` block, add `GitPushTool` alphabetically.

- [ ] **Step 4: Run to confirm it passes**

Run: `cargo test -p aivyx-tools git_push::`
Expected: PASS — 4 tests (`first_push_sets_upstream_and_succeeds`, `subsequent_push_succeeds_with_upstream_already_set`, `preview_shows_commits_ahead_of_the_remote`, `preview_is_graceful_when_there_is_no_upstream_yet`).

- [ ] **Step 5: Run the whole crate's tests and clippy**

Run: `cargo test -p aivyx-tools`
Run: `cargo clippy -p aivyx-tools --all-targets`
Expected: all pass, clippy clean.

- [ ] **Step 6: Commit**

```bash
git add crates/aivyx-tools/src/tools/git_push.rs crates/aivyx-tools/src/tools/mod.rs crates/aivyx-tools/src/lib.rs
git commit -m "Branch/PR tooling: git_push tool (Task 3)"
```

---

### Task 4: `git_pr` tool

**Files:**
- Create: `crates/aivyx-tools/src/tools/git_pr.rs`
- Modify: `crates/aivyx-tools/src/tools/mod.rs`
- Modify: `crates/aivyx-tools/src/lib.rs`

**Interfaces:**
- Consumes: `crate::process::run` (existing).
- Produces: `pub struct GitPrTool` with `pub fn new() -> Self` and `#[cfg(test)] pub(crate) fn with_gh_program(program: &str) -> Self` (test-only constructor pointing at a fake `gh` stand-in — mirrors the `LspClient::with_program`/`McpClient::new`-style test-only constructor pattern already established in `crates/aivyx-tools/src/lsp/mod.rs`). Registered by Task 5's `main.rs` wiring.

- [ ] **Step 1: Write the failing tests**

Create `crates/aivyx-tools/src/tools/git_pr.rs`:

```rust
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
```

- [ ] **Step 2: Run to confirm it fails**

Run: `cargo test -p aivyx-tools git_pr`
Expected: FAIL to compile (`git_pr` module not yet wired into `tools/mod.rs`).

- [ ] **Step 3: Wire the module in**

In `crates/aivyx-tools/src/tools/mod.rs`, this crate now has both `git_pr` (from this task) and `git_push` (from Task 3) to place — the correct final alphabetical order for every git module is: `git_branch`, `git_commit`, `git_pr`, `git_push`, `git_read`. Re-sort the `mod` list and the `pub use` list to match this exact order (moving `git_push`'s lines if Task 3 placed them elsewhere), adding:

```rust
mod git_pr;
```

and:

```rust
pub use git_pr::GitPrTool;
```

In `crates/aivyx-tools/src/lib.rs`'s `pub use tools::{ ... };` block, add `GitPrTool` alphabetically.

- [ ] **Step 4: Run to confirm it passes**

Run: `cargo test -p aivyx-tools git_pr::`
Expected: PASS — 6 tests (`returns_a_clear_error_when_there_is_no_upstream`, `returns_a_clear_error_when_gh_is_not_authenticated`, `returns_a_clear_error_when_gh_is_not_found`, `succeeds_and_returns_the_pr_url_when_everything_is_ready`, `permission_request_argv_includes_body_base_and_draft`, `an_empty_title_is_invalid`).

- [ ] **Step 5: Run the whole crate's tests and clippy**

Run: `cargo test -p aivyx-tools`
Run: `cargo clippy -p aivyx-tools --all-targets`
Expected: all pass, clippy clean.

- [ ] **Step 6: Commit**

```bash
git add crates/aivyx-tools/src/tools/git_pr.rs crates/aivyx-tools/src/tools/mod.rs crates/aivyx-tools/src/lib.rs
git commit -m "Branch/PR tooling: git_pr tool (Task 4)"
```

---

### Task 5: `main.rs` wiring

**Files:**
- Modify: `crates/aivyx/src/main.rs`

**Interfaces:**
- Consumes: `aivyx_tools::{GitBranchTool, GitPrTool, GitPushTool}` (Tasks 2–4).
- Produces: nothing further downstream — this is the final integration point.

- [ ] **Step 1: Extend the import list**

In `crates/aivyx/src/main.rs`, extend the existing `aivyx_tools::{...}` import to include the three new types, keeping the existing alphabetical ordering:

```rust
use aivyx_tools::{
    CommandSpec, EditFileTool, FindReferencesTool, GetMcpPromptTool, GitBranchTool,
    GitCheckpointer, GitCommitTool, GitPrTool, GitPushTool, GitReadTool, GlobTool,
    GoToDefinitionTool, GrepTool, ListMcpPromptsTool, ListMcpResourcesTool, LspClient, McpClient,
    McpToolAdapter, ReadFileTool, ReadMcpResourceTool, RunCommandTool, RunShellTool, SetTasksTool,
    ToolExecutor, ToolRegistry, WebFetchTool, WebSearchTool, WriteFileTool,
};
```

(Adjust to match whatever the actual current import list looks like at the time this task runs — insert `GitBranchTool`, `GitPrTool`, `GitPushTool` alphabetically into it; `GitReadTool` should already be present since it's pre-existing.)

- [ ] **Step 2: Register the three new tools**

In `crates/aivyx/src/main.rs`, find the existing line `registry.register(Arc::new(GitCommitTool::new(deny_paths.clone())));` (in the initial tool-registration block, alongside `GitReadTool`) and add immediately after it:

```rust
    registry.register(Arc::new(GitBranchTool::new()));
    registry.register(Arc::new(GitPushTool::new()));
    registry.register(Arc::new(GitPrTool::new()));
```

All three are unconditional (no config flag, no `if` guard) — matching `git_read`/`git_commit`'s own always-registered pattern, and the design's explicit reasoning for why `git_pr` in particular needs no gate (a missing/unauthenticated `gh` is an environmental accident, not a deliberate off-switch).

- [ ] **Step 3: Build**

Run: `cargo build --workspace`
Expected: clean build.

- [ ] **Step 4: Manual smoke check**

There is no way to unit-test `main.rs`'s own wiring in isolation (matching this project's established precedent for wiring tasks). Verify by running the built binary in a real git repo (a scratch directory, not this project's own repo) and confirming, via a quick manual conversation:
1. Asking the model to create a new branch shows a confirmation modal naming `git`/`checkout -b` and succeeds on approval.
2. Asking the model to list branches (`git_read` with `mode: "branches"`) shows no confirmation modal and returns real branch names.

- [ ] **Step 5: Run the full workspace test suite and clippy**

Run: `cargo test --workspace`
Run: `cargo clippy --workspace --all-targets`
Expected: all tests pass (no new automated tests in this task — wiring-only, matching prior phases' own main.rs tasks), clippy clean.

- [ ] **Step 6: Commit**

```bash
git add crates/aivyx/src/main.rs
git commit -m "Branch/PR tooling: wire git_branch/git_push/git_pr into main.rs (Task 5)"
```

---

### Task 6: Docs + live E2E + final check

**Files:**
- Modify: `README.md`
- Modify: `ROADMAP.md`

**Interfaces:**
- Consumes: the complete feature from Tasks 1–5.

- [ ] **Step 1: Live E2E through the real binary**

Using the same PTY-harness pattern as the most recent phases (a `python-pyte`-driven PTY session, verifying results via the persisted session JSON rather than raw screen text). This phase's live E2E covers `git_branch` and `git_push` only — **not** `git_pr`, per the design's own explicit non-goal: there is no safe, repeatable way to live-test a real `gh pr create` call against a real GitHub repository in this environment, and the unit tests already cover `git_pr`'s logic against a fake `gh` stand-in.

Set up a scratch git repository (with an initial commit) and a local bare repository as its `origin` remote (same technique as Task 3's tests — a plain absolute path, no network). Confirm, through the real TUI:
1. A direct message asking the model to create a new branch via `git_branch` — the tool call and result appear live in the transcript, **with a confirmation modal** (matching `ActionKind::Execute`'s confirm-gated tier), and the branch is genuinely created and checked out (verified via the persisted session JSON, per this project's established grading method, and by inspecting the scratch repo's actual `git branch --show-current` afterward).
2. A direct message asking the model to push the branch via `git_push` — the tool call and result appear live, again with a confirmation modal, and the commit genuinely lands in the local bare "remote" repo (verified by inspecting the bare repo directly afterward, e.g. `git --git-dir=<bare-repo> log --oneline`).
3. A direct message asking the model to list branches via `git_read` (`mode: "branches"`) — appears live with **no confirmation modal**, and the real branch list is returned.

- [ ] **Step 2: Update `README.md`**

Add a new paragraph after the MCP client support paragraph, documenting: `git_branch`/`git_push`/`git_pr`, the `ActionKind::Execute`+`Command`-target tier they share with `git_commit` (confirm-gated, no new `ActionKind`), `git_read`'s new `branches` mode (auto-allow), `git_push`'s deliberate lack of any `--force` support, and `git_pr`'s two deterministic preflight checks (upstream configured, `gh` authenticated) plus its always-registered/no-config-flag status mirroring the LSP client's missing-`rust-analyzer` precedent.

- [ ] **Step 3: Update `ROADMAP.md`**

In the Phase 9 section, insert a "built and live-verified" paragraph (matching the style of the `AGENTS.md`/`web_fetch`/`web_search`/MCP entries immediately above it), covering: this closing out the lowest-priority remaining item from the original tool/capability audit; the decision to reuse `git_commit`'s exact permission tier rather than introduce a new one (these are structured invocations of the same trusted `git`/`gh` CLIs `git_commit` already uses, unlike MCP's genuinely novel arbitrary-third-party-code trust situation); the split between `git_read`'s new read-only `branches` mode and the three new mutating tools; `git_push`'s deliberate exclusion of `--force` support; `git_pr`'s two deterministic preflight checks and its always-registered status; the test count delta; and the live E2E results (explicitly noting `git_pr` was verified only via its fake-`gh` unit tests, not a live `gh pr create` call, per the design's own non-goal).

- [ ] **Step 4: Final full-workspace check**

Run: `cargo test --workspace`
Expected: all tests pass. State the actual counted new-test delta in the docs (Task 1: 1, Task 2: 5, Task 3: 4, Task 4: 6, Tasks 5–6: 0 — 16 new tests total on top of whatever the baseline is at the time this task runs), following this project's own precedent of correcting any projected count against the real one rather than trusting the plan's estimate.

Run: `cargo clippy --workspace --all-targets`
Expected: clean, no warnings.

- [ ] **Step 5: Commit**

```bash
git add README.md ROADMAP.md
git commit -m "Docs: branch/PR tooling + live E2E verification (Phase 9)"
```
