use std::path::{Path, PathBuf};

use aivyx_sandbox::{ActionKind, PermissionRequest, PermissionTarget};
use aivyx_types::{ToolDefinition, ToolOutput};
use async_trait::async_trait;
use globset::Glob;
use ignore::WalkBuilder;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use crate::path_resolve::{is_denied, resolve};
use crate::{Tool, ToolError, ToolExecutionContext};

/// See `grep.rs`'s `MAX_MATCHES` for the same reasoning — bounded,
/// actionable output rather than an unbounded dump.
const MAX_PATHS: usize = 500;

#[derive(Deserialize, JsonSchema)]
struct GlobArgs {
    /// Glob pattern to match file paths against, relative to `path` (e.g. "**/*.rs", "src/*.py").
    pattern: String,
    /// Directory to search, absolute or relative to the working directory. Defaults to the working directory.
    #[serde(default)]
    path: Option<String>,
}

/// Path search (glob-equivalent). See `GrepTool`'s doc comment for why
/// `deny_paths` is threaded in separately rather than relying solely on
/// `permission_request`'s root-level check.
pub struct GlobTool {
    deny_paths: Vec<PathBuf>,
}

impl GlobTool {
    pub fn new(deny_paths: Vec<PathBuf>) -> Self {
        Self { deny_paths }
    }
}

#[async_trait]
impl Tool for GlobTool {
    fn name(&self) -> &str {
        "glob"
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name().to_string(),
            description: "Find file paths matching a glob pattern (e.g. \"**/*.rs\") under a directory, \
                defaulting to the working directory. The pattern is matched against paths relative to that \
                directory. Respects .gitignore and does not follow symlinks. Capped at 500 results."
                .to_string(),
            parameters_schema: serde_json::Value::from(schemars::schema_for!(GlobArgs)),
        }
    }

    fn permission_request(
        &self,
        arguments: &serde_json::Value,
        cwd: &Path,
    ) -> Result<PermissionRequest, ToolError> {
        let args: GlobArgs = serde_json::from_value(arguments.clone())
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;
        let root = resolve(cwd, args.path.as_deref().unwrap_or("."));

        Ok(PermissionRequest {
            tool_name: self.name().to_string(),
            action: ActionKind::Read,
            target: PermissionTarget::Path(root),
            arguments_preview: json!({ "pattern": args.pattern, "path": args.path }),
            preview: None,
        })
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        ctx: &ToolExecutionContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: GlobArgs = serde_json::from_value(arguments)
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;
        let root = resolve(&ctx.cwd, args.path.as_deref().unwrap_or("."));
        let cwd = ctx.cwd.clone();
        let deny_paths = self.deny_paths.clone();

        let handle =
            tokio::task::spawn_blocking(move || run_glob(&args.pattern, &root, &cwd, &deny_paths));

        // See `grep.rs`'s `execute` for why this only makes the UI
        // responsive to Ctrl+C, not the underlying walk itself — tokio has
        // no way to forcibly preempt a `spawn_blocking` thread.
        tokio::select! {
            result = handle => {
                let output = result
                    .map_err(|err| ToolError::ExecutionFailed(format!("glob task panicked: {err}")))??;
                Ok(ToolOutput::Ok(output))
            }
            _ = ctx.cancellation.cancelled() => {
                Err(ToolError::ExecutionFailed("search was cancelled".to_string()))
            }
        }
    }
}

fn run_glob(
    pattern: &str,
    root: &Path,
    cwd: &Path,
    deny_paths: &[PathBuf],
) -> Result<String, ToolError> {
    let matcher = Glob::new(pattern)
        .map_err(|err| ToolError::InvalidArguments(format!("invalid glob pattern: {err}")))?
        .compile_matcher();

    let mut results: Vec<String> = Vec::new();
    let mut truncated = false;

    // See `grep.rs`'s `run_grep` for why `WalkBuilder`'s defaults
    // (gitignore-aware, no symlink following) are load-bearing here, not
    // just a convenience.
    for entry in WalkBuilder::new(root).build() {
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        if is_denied(path, deny_paths) {
            continue;
        }
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }

        let relative = path.strip_prefix(root).unwrap_or(path);
        if !matcher.is_match(relative) {
            continue;
        }

        if results.len() >= MAX_PATHS {
            truncated = true;
            break;
        }
        results.push(path.strip_prefix(cwd).unwrap_or(path).display().to_string());
    }

    let mut output = results.join("\n");
    if truncated {
        output.push_str(&format!(
            "\n... {MAX_PATHS}+ matches, showing first {MAX_PATHS} — narrow your pattern or path\n"
        ));
    }
    if output.is_empty() {
        output = "no matching paths found".to_string();
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(pattern: &str, path: Option<&str>) -> serde_json::Value {
        json!({ "pattern": pattern, "path": path })
    }

    async fn run(tool: &GlobTool, dir: &Path, arguments: serde_json::Value) -> ToolOutput {
        let ctx = ToolExecutionContext {
            cwd: dir.to_path_buf(),
            confiner: std::sync::Arc::new(aivyx_sandbox::NoopConfiner),
            cancellation: tokio_util::sync::CancellationToken::new(),
        };
        tool.execute(arguments, &ctx).await.unwrap()
    }

    #[tokio::test]
    async fn matches_a_glob_pattern() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/main.rs"), "").unwrap();
        std::fs::write(dir.path().join("README.md"), "").unwrap();

        let tool = GlobTool::new(vec![]);
        let output = run(&tool, dir.path(), args("**/*.rs", None)).await;

        let ToolOutput::Ok(text) = output else {
            panic!("expected Ok output")
        };
        assert!(text.contains("src/main.rs"));
        assert!(!text.contains("README.md"));
    }

    #[tokio::test]
    async fn respects_gitignore() {
        let dir = tempfile::tempdir().unwrap();
        // See grep.rs's respects_gitignore test for why a bare `.git`
        // directory is needed here.
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        std::fs::write(dir.path().join(".gitignore"), "ignored.rs\n").unwrap();
        std::fs::write(dir.path().join("ignored.rs"), "").unwrap();
        std::fs::write(dir.path().join("kept.rs"), "").unwrap();

        let tool = GlobTool::new(vec![]);
        let output = run(&tool, dir.path(), args("*.rs", None)).await;

        let ToolOutput::Ok(text) = output else {
            panic!("expected Ok output")
        };
        assert!(text.contains("kept.rs"));
        assert!(!text.contains("ignored.rs"));
    }

    #[tokio::test]
    async fn does_not_descend_into_a_denied_subtree_even_as_an_ancestor_root() {
        let dir = tempfile::tempdir().unwrap();
        let secret_dir = dir.path().join("secret");
        std::fs::create_dir(&secret_dir).unwrap();
        std::fs::write(secret_dir.join("id_rsa.txt"), "").unwrap();
        std::fs::write(dir.path().join("public.txt"), "").unwrap();

        let tool = GlobTool::new(vec![secret_dir.canonicalize().unwrap()]);
        let output = run(&tool, dir.path(), args("**/*.txt", None)).await;

        let ToolOutput::Ok(text) = output else {
            panic!("expected Ok output")
        };
        assert!(text.contains("public.txt"));
        assert!(!text.contains("id_rsa.txt"));
        assert!(!text.contains("secret"));
    }

    #[tokio::test]
    async fn does_not_follow_a_symlink_out_of_the_tree() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.txt"), "").unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("link")).unwrap();
        std::fs::write(dir.path().join("public.txt"), "").unwrap();

        let tool = GlobTool::new(vec![]);
        let output = run(&tool, dir.path(), args("**/*.txt", None)).await;

        let ToolOutput::Ok(text) = output else {
            panic!("expected Ok output")
        };
        assert!(text.contains("public.txt"));
        assert!(!text.contains("secret.txt"));
    }

    #[tokio::test]
    async fn caps_results_when_exceeded() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..(MAX_PATHS + 5) {
            std::fs::write(dir.path().join(format!("f{i}.txt")), "").unwrap();
        }

        let tool = GlobTool::new(vec![]);
        let output = run(&tool, dir.path(), args("*.txt", None)).await;

        let ToolOutput::Ok(text) = output else {
            panic!("expected Ok output")
        };
        let match_lines = text.lines().filter(|l| l.ends_with(".txt")).count();
        assert_eq!(match_lines, MAX_PATHS);
        assert!(text.contains("narrow your pattern"));
    }

    #[tokio::test]
    async fn no_matches_reports_clearly_instead_of_empty_string() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "").unwrap();

        let tool = GlobTool::new(vec![]);
        let output = run(&tool, dir.path(), args("*.rs", None)).await;

        let ToolOutput::Ok(text) = output else {
            panic!("expected Ok output")
        };
        assert_eq!(text, "no matching paths found");
    }
}
