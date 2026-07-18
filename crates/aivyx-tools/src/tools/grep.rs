use std::path::{Path, PathBuf};

use aivyx_sandbox::{ActionKind, PermissionRequest, PermissionTarget};
use aivyx_types::{ToolDefinition, ToolOutput};
use async_trait::async_trait;
use grep_regex::RegexMatcherBuilder;
use grep_searcher::SearcherBuilder;
use grep_searcher::sinks::UTF8;
use ignore::WalkBuilder;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use crate::path_resolve::{is_denied, resolve};
use crate::{Tool, ToolError, ToolExecutionContext};

/// Caps unbounded output from a search matching far more than a model
/// could usefully act on in one turn — matches the tool-design-hygiene
/// principle already applied elsewhere in this codebase (right-sized,
/// actionable output rather than dumping everything).
const MAX_MATCHES: usize = 200;

#[derive(Deserialize, JsonSchema)]
struct GrepArgs {
    /// Regex pattern to search for. Uses Rust `regex` crate syntax (not PCRE) — no lookaheads/lookbehinds.
    pattern: String,
    /// Directory to search, absolute or relative to the working directory. Defaults to the working directory.
    #[serde(default)]
    path: Option<String>,
    /// Case-insensitive match. Defaults to false.
    #[serde(default)]
    case_insensitive: bool,
}

/// Content search (grep-equivalent). `deny_paths` is required separately
/// from the usual `permission_request`-level check: that check only sees
/// the search *root*, which passes if the root is merely an *ancestor* of a
/// denied path rather than the denied path itself — so a walk still needs
/// its own per-entry check to avoid silently reading into a denied subtree.
pub struct GrepTool {
    deny_paths: Vec<PathBuf>,
}

impl GrepTool {
    pub fn new(deny_paths: Vec<PathBuf>) -> Self {
        Self { deny_paths }
    }
}

#[async_trait]
impl Tool for GrepTool {
    fn name(&self) -> &str {
        "grep"
    }

    fn mutates_outside_session(&self) -> bool {
        false
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name().to_string(),
            description: "Search file contents for a regex pattern (Rust regex syntax, not PCRE) under a \
                directory, defaulting to the working directory. Respects .gitignore and does not follow \
                symlinks. Returns matching lines as path:line_number:text, capped at 200 matches."
                .to_string(),
            parameters_schema: serde_json::Value::from(schemars::schema_for!(GrepArgs)),
        }
    }

    fn permission_request(
        &self,
        arguments: &serde_json::Value,
        cwd: &Path,
    ) -> Result<PermissionRequest, ToolError> {
        let args: GrepArgs = serde_json::from_value(arguments.clone())
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;
        let root = resolve(cwd, args.path.as_deref().unwrap_or("."));

        Ok(PermissionRequest {
            tool_name: self.name().to_string(),
            action: ActionKind::Read,
            target: PermissionTarget::Path(root),
            arguments_preview: json!({ "pattern": args.pattern, "path": args.path }),
            preview: None,
            diff: None,
        })
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        ctx: &ToolExecutionContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: GrepArgs = serde_json::from_value(arguments)
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;
        let root = resolve(&ctx.cwd, args.path.as_deref().unwrap_or("."));
        let cwd = ctx.cwd.clone();
        let deny_paths = self.deny_paths.clone();

        let handle = tokio::task::spawn_blocking(move || {
            run_grep(
                &args.pattern,
                args.case_insensitive,
                &root,
                &cwd,
                &deny_paths,
            )
        });

        // `spawn_blocking` runs on a real OS thread tokio can't forcibly
        // preempt — this can't stop an in-flight walk, but it does stop the
        // *UI* from blocking on Ctrl+C while a large/slow search finishes;
        // the abandoned thread keeps running detached until it naturally
        // completes, its result simply discarded.
        tokio::select! {
            result = handle => {
                let output = result
                    .map_err(|err| ToolError::ExecutionFailed(format!("grep task panicked: {err}")))??;
                Ok(ToolOutput::Ok(output))
            }
            _ = ctx.cancellation.cancelled() => {
                Err(ToolError::ExecutionFailed("search was cancelled".to_string()))
            }
        }
    }
}

fn run_grep(
    pattern: &str,
    case_insensitive: bool,
    root: &Path,
    cwd: &Path,
    deny_paths: &[PathBuf],
) -> Result<String, ToolError> {
    let mut builder = RegexMatcherBuilder::new();
    builder.case_insensitive(case_insensitive);
    let matcher = builder
        .build(pattern)
        .map_err(|err| ToolError::InvalidArguments(format!("invalid regex pattern: {err}")))?;

    let mut results: Vec<String> = Vec::new();
    // Set only when an *actual* extra match is encountered beyond the cap
    // (checked before pushing, in the sink below) — not derived from
    // `results.len() >= MAX_MATCHES` after the fact, which can't tell
    // "there were exactly MAX_MATCHES matches, no more" apart from "there
    // were more". Mirrors `glob.rs`'s already-correct pattern.
    let mut truncated = false;

    // `WalkBuilder` respects .gitignore and does not follow symlinks by
    // default — reusing ripgrep's own default closes the same class of
    // symlink-based deny_paths escape that `path_resolve::resolve_symlinks`
    // fixes for single-file tools, without reimplementing it here.
    for entry in WalkBuilder::new(root).build() {
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        if is_denied(path, deny_paths) {
            continue;
        }
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }

        let display_path = path.strip_prefix(cwd).unwrap_or(path).display().to_string();
        let mut searcher = SearcherBuilder::new().build();
        let sink_results = &mut results;
        let sink_truncated = &mut truncated;
        // Errors here (binary content, permission denied, non-UTF8 match)
        // are skipped rather than propagated — matches grep's own behavior
        // of silently passing over files it can't search as text.
        let _ = searcher.search_path(
            &matcher,
            path,
            UTF8(|line_number, line| {
                if sink_results.len() >= MAX_MATCHES {
                    *sink_truncated = true;
                    return Ok(false);
                }
                sink_results.push(format!(
                    "{display_path}:{line_number}:{}",
                    line.trim_end_matches('\n')
                ));
                Ok(true)
            }),
        );
        if truncated {
            break;
        }
    }

    let mut output = results.join("\n");
    if truncated {
        output.push_str(&format!(
            "\n... {MAX_MATCHES}+ matches, showing first {MAX_MATCHES} — narrow your pattern or path\n"
        ));
    }
    if output.is_empty() {
        output = "no matches found".to_string();
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(pattern: &str, path: Option<&str>) -> serde_json::Value {
        json!({ "pattern": pattern, "path": path, "case_insensitive": false })
    }

    async fn run(tool: &GrepTool, dir: &Path, arguments: serde_json::Value) -> ToolOutput {
        let ctx = ToolExecutionContext {
            cwd: dir.to_path_buf(),
            confiner: std::sync::Arc::new(aivyx_sandbox::NoopConfiner),
            cancellation: tokio_util::sync::CancellationToken::new(),
        };
        tool.execute(arguments, &ctx).await.unwrap()
    }

    #[tokio::test]
    async fn finds_a_match_in_a_nested_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        std::fs::write(
            dir.path().join("src/main.rs"),
            "fn main() {\n    needle();\n}\n",
        )
        .unwrap();

        let tool = GrepTool::new(vec![]);
        let output = run(&tool, dir.path(), args("needle", None)).await;

        let ToolOutput::Ok(text) = output else {
            panic!("expected Ok output")
        };
        assert!(text.contains("src/main.rs:2:"));
        assert!(text.contains("needle"));
    }

    #[tokio::test]
    async fn respects_gitignore() {
        let dir = tempfile::tempdir().unwrap();
        // `ignore::WalkBuilder` only honors .gitignore inside an actual git
        // repo by default (matching ripgrep's own default) — a bare `.git`
        // directory is enough to satisfy that check without a real repo.
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        std::fs::write(dir.path().join(".gitignore"), "ignored.txt\n").unwrap();
        std::fs::write(dir.path().join("ignored.txt"), "needle\n").unwrap();
        std::fs::write(dir.path().join("kept.txt"), "needle\n").unwrap();

        let tool = GrepTool::new(vec![]);
        let output = run(&tool, dir.path(), args("needle", None)).await;

        let ToolOutput::Ok(text) = output else {
            panic!("expected Ok output")
        };
        assert!(text.contains("kept.txt"));
        assert!(!text.contains("ignored.txt"));
    }

    #[tokio::test]
    async fn does_not_descend_into_a_denied_subtree_even_as_an_ancestor_root() {
        // Regression test for the gap this tool closes: the search root
        // (dir.path()) is only an *ancestor* of the denied path, not the
        // denied path itself, so `permission_request`'s own check (which
        // ConfirmationGate applies to the root) would not catch this alone.
        let dir = tempfile::tempdir().unwrap();
        let secret_dir = dir.path().join("secret");
        std::fs::create_dir(&secret_dir).unwrap();
        std::fs::write(secret_dir.join("id_rsa"), "needle\n").unwrap();
        std::fs::write(dir.path().join("public.txt"), "needle\n").unwrap();

        let tool = GrepTool::new(vec![secret_dir.canonicalize().unwrap()]);
        let output = run(&tool, dir.path(), args("needle", None)).await;

        let ToolOutput::Ok(text) = output else {
            panic!("expected Ok output")
        };
        assert!(text.contains("public.txt"));
        assert!(!text.contains("id_rsa"));
        assert!(!text.contains("secret"));
    }

    #[tokio::test]
    async fn does_not_follow_a_symlink_out_of_the_tree() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.txt"), "needle\n").unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("link")).unwrap();
        std::fs::write(dir.path().join("public.txt"), "needle\n").unwrap();

        let tool = GrepTool::new(vec![]);
        let output = run(&tool, dir.path(), args("needle", None)).await;

        let ToolOutput::Ok(text) = output else {
            panic!("expected Ok output")
        };
        assert!(text.contains("public.txt"));
        assert!(!text.contains("secret.txt"));
    }

    #[tokio::test]
    async fn exactly_max_matches_is_not_reported_as_truncated() {
        let dir = tempfile::tempdir().unwrap();
        let content = "needle\n".repeat(MAX_MATCHES);
        std::fs::write(dir.path().join("a.txt"), content).unwrap();

        let tool = GrepTool::new(vec![]);
        let output = run(&tool, dir.path(), args("needle", None)).await;

        let ToolOutput::Ok(text) = output else {
            panic!("expected Ok output")
        };
        assert_eq!(text.lines().count(), MAX_MATCHES);
        assert!(!text.contains("narrow your pattern"));
    }

    #[tokio::test]
    async fn more_than_max_matches_is_reported_as_truncated() {
        let dir = tempfile::tempdir().unwrap();
        let content = "needle\n".repeat(MAX_MATCHES + 1);
        std::fs::write(dir.path().join("a.txt"), content).unwrap();

        let tool = GrepTool::new(vec![]);
        let output = run(&tool, dir.path(), args("needle", None)).await;

        let ToolOutput::Ok(text) = output else {
            panic!("expected Ok output")
        };
        assert!(text.contains("narrow your pattern"));
    }

    #[tokio::test]
    async fn no_matches_reports_clearly_instead_of_empty_string() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hello\n").unwrap();

        let tool = GrepTool::new(vec![]);
        let output = run(&tool, dir.path(), args("needle", None)).await;

        let ToolOutput::Ok(text) = output else {
            panic!("expected Ok output")
        };
        assert_eq!(text, "no matches found");
    }
}
