use std::path::{Path, PathBuf};

use aivyx_sandbox::{ActionKind, DiffContent, PermissionRequest, PermissionTarget};
use aivyx_types::{ToolDefinition, ToolOutput};
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use crate::diff::unified_diff;
use crate::{Tool, ToolError, ToolExecutionContext};

const TARGET_DESCRIPTION: &str = "your global preferences (AGENTS.md)";

#[derive(Deserialize, JsonSchema)]
struct RememberPreferenceArgs {
    /// The complete new content of your global AGENTS.md file — not a
    /// diff, not an append, the full replacement text.
    content: String,
}

/// Lets the agent propose an update to a single, fixed, server-resolved
/// path (the global `AGENTS.md` — see `Settings::agents_file_path()`),
/// never a model-supplied one. See `docs/superpowers/specs/
/// 2026-07-21-agent-learned-preferences-design.md`.
pub struct RememberPreferenceTool {
    path: PathBuf,
}

impl RememberPreferenceTool {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

#[async_trait]
impl Tool for RememberPreferenceTool {
    fn name(&self) -> &str {
        "remember_preference"
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name().to_string(),
            description: "Propose an update to your own long-term memory of the user's \
                preferences and working style — stored in a file that's automatically included \
                in every future project, not just this one. Use this when the user explicitly \
                asks you to remember something, or when you notice a clear, repeated pattern \
                worth remembering (not a one-off — don't propose from a single occurrence, and \
                don't re-propose something already declined this session). The user reviews \
                every change as a diff before it's saved."
                .to_string(),
            parameters_schema: serde_json::Value::from(schemars::schema_for!(
                RememberPreferenceArgs
            )),
        }
    }

    fn permission_request(
        &self,
        arguments: &serde_json::Value,
        _cwd: &Path,
    ) -> Result<PermissionRequest, ToolError> {
        let args: RememberPreferenceArgs = serde_json::from_value(arguments.clone())
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;

        let (preview, diff) = match std::fs::read_to_string(&self.path) {
            Ok(old) => (
                Some(unified_diff(&self.path.display().to_string(), &old, &args.content)),
                Some(DiffContent {
                    old_content: old,
                    new_content: args.content.clone(),
                }),
            ),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => (
                Some(unified_diff(&self.path.display().to_string(), "", &args.content)),
                Some(DiffContent {
                    old_content: String::new(),
                    new_content: args.content.clone(),
                }),
            ),
            Err(_) => (
                Some(format!(
                    "WARNING: {} already exists but could not be read as text. This write will \
                     overwrite it entirely.",
                    self.path.display()
                )),
                None,
            ),
        };

        Ok(PermissionRequest {
            tool_name: self.name().to_string(),
            action: ActionKind::Memory,
            target: PermissionTarget::Other(TARGET_DESCRIPTION.to_string()),
            arguments_preview: json!({}),
            preview,
            diff,
        })
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        _ctx: &ToolExecutionContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: RememberPreferenceArgs = serde_json::from_value(arguments)
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;

        if let Some(parent) = self.path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&self.path, &args.content).await?;

        Ok(ToolOutput::Ok(format!(
            "updated {} ({} bytes)",
            self.path.display(),
            args.content.len()
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permission_request_targets_memory_action_and_other_target() {
        let tool = RememberPreferenceTool::new(PathBuf::from("/nonexistent/AGENTS.md"));
        let request = tool
            .permission_request(&json!({"content": "be terse"}), Path::new("/irrelevant"))
            .unwrap();

        assert_eq!(request.action, ActionKind::Memory);
        assert_eq!(
            request.target,
            PermissionTarget::Other(TARGET_DESCRIPTION.to_string())
        );
    }

    #[test]
    fn permission_request_diffs_against_real_existing_content_not_model_supplied_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("AGENTS.md");
        std::fs::write(&path, "old preference\n").unwrap();
        let tool = RememberPreferenceTool::new(path);

        let request = tool
            .permission_request(&json!({"content": "new preference\n"}), Path::new("/irrelevant"))
            .unwrap();

        let diff = request.diff.expect("expected a structured diff");
        assert_eq!(diff.old_content, "old preference\n");
        assert_eq!(diff.new_content, "new preference\n");
    }

    #[test]
    fn permission_request_handles_a_not_yet_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("does-not-exist-yet").join("AGENTS.md");
        let tool = RememberPreferenceTool::new(path);

        let request = tool
            .permission_request(&json!({"content": "first preference\n"}), Path::new("/irrelevant"))
            .unwrap();

        let diff = request.diff.expect("expected a structured diff");
        assert_eq!(diff.old_content, "");
        assert_eq!(diff.new_content, "first preference\n");
    }

    // No shared `ToolExecutionContext` test helper exists in this crate —
    // every tool's own test module defines a local one (e.g.
    // `crates/aivyx-tools/src/tools/git_read.rs`'s `fn ctx(dir: &Path)`).
    // Mirror that exact pattern here rather than inventing a shared one
    // this task doesn't need.
    fn ctx(dir: &Path) -> ToolExecutionContext {
        ToolExecutionContext {
            cwd: dir.to_path_buf(),
            confiner: std::sync::Arc::new(aivyx_sandbox::NoopConfiner),
            cancellation: tokio_util::sync::CancellationToken::new(),
        }
    }

    #[tokio::test]
    async fn execute_writes_the_proposed_content_to_the_fixed_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("AGENTS.md");
        let tool = RememberPreferenceTool::new(path.clone());

        tool.execute(json!({"content": "remembered\n"}), &ctx(dir.path()))
            .await
            .unwrap();

        assert_eq!(std::fs::read_to_string(&path).unwrap(), "remembered\n");
    }

    // Mirrors write_file.rs's existing_binary_file_gets_a_warning_instead_of_a_silent_diff
    // / existing_binary_file_has_no_structured_diff tests exactly — same
    // fallback path, same fixed byte sequence, just retargeted at this
    // tool's fixed path instead of a `path` argument.
    #[test]
    fn existing_non_utf8_file_gets_a_warning_instead_of_a_silent_diff() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("AGENTS.md");
        std::fs::write(&path, [0xFF, 0xFE, 0x00, 0xD8, 0x00]).unwrap();
        let tool = RememberPreferenceTool::new(path);

        let request = tool
            .permission_request(&json!({"content": "hello\n"}), Path::new("/irrelevant"))
            .unwrap();

        let preview = request.preview.expect("expected a warning preview, not None");
        assert!(preview.contains("WARNING"));
    }

    #[test]
    fn existing_non_utf8_file_has_no_structured_diff() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("AGENTS.md");
        std::fs::write(&path, [0xFF, 0xFE, 0x00, 0xD8, 0x00]).unwrap();
        let tool = RememberPreferenceTool::new(path);

        let request = tool
            .permission_request(&json!({"content": "hello\n"}), Path::new("/irrelevant"))
            .unwrap();

        assert!(request.diff.is_none(), "a non-UTF-8 file has no text diff content");
    }
}
