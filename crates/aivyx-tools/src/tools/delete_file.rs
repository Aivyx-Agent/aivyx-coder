use std::path::Path;

use aivyx_sandbox::{ActionKind, DiffContent, PermissionRequest, PermissionTarget};
use aivyx_types::{ToolDefinition, ToolOutput};
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use crate::path_resolve::resolve;
use crate::{Tool, ToolError, ToolExecutionContext};

#[derive(Deserialize, JsonSchema)]
struct DeleteFileArgs {
    /// Path to the file to delete, absolute or relative to the working directory.
    path: String,
}

/// `ActionKind::Delete`'s first real constructor — every other tool in
/// this codebase declares `Read`/`Write`/`Execute`/`Internal`/`McpTool`.
/// Single-file only: a directory target is refused with a clear error
/// rather than silently doing something unexpected or requiring a
/// `recursive` flag this tool doesn't offer — `run_shell` remains the
/// path for directory removal. Deletion is pure `tokio::fs`, no
/// subprocess — no argv exists to be misparsed, unlike a `rm` shell-out.
pub struct DeleteFileTool;

#[async_trait]
impl Tool for DeleteFileTool {
    fn name(&self) -> &str {
        "delete_file"
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name().to_string(),
            description: "Delete a single file. Refuses to delete directories — use run_shell \
                for that. The user sees the file's content (or a binary-file warning) and must \
                approve before it's removed."
                .to_string(),
            parameters_schema: serde_json::Value::from(schemars::schema_for!(DeleteFileArgs)),
        }
    }

    fn permission_request(
        &self,
        arguments: &serde_json::Value,
        cwd: &Path,
    ) -> Result<PermissionRequest, ToolError> {
        let args: DeleteFileArgs = serde_json::from_value(arguments.clone())
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;
        let resolved = resolve(cwd, &args.path);

        let metadata = std::fs::metadata(&resolved).map_err(|_| {
            ToolError::ExecutionFailed(format!("{} does not exist", resolved.display()))
        })?;
        if metadata.is_dir() {
            return Err(ToolError::ExecutionFailed(format!(
                "{} is a directory — delete_file only removes single files; use run_shell for \
                 directory removal",
                resolved.display()
            )));
        }

        let (preview, diff) = match std::fs::read_to_string(&resolved) {
            Ok(content) => (
                Some(content.clone()),
                Some(DiffContent { old_content: content, new_content: String::new() }),
            ),
            Err(_) => (
                Some(format!(
                    "WARNING: {} could not be read as text (binary file?). This will delete it \
                     entirely.",
                    resolved.display()
                )),
                None,
            ),
        };

        Ok(PermissionRequest {
            tool_name: self.name().to_string(),
            action: ActionKind::Delete,
            target: PermissionTarget::Path(resolved),
            arguments_preview: json!({ "path": args.path }),
            preview,
            diff,
        })
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        ctx: &ToolExecutionContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: DeleteFileArgs = serde_json::from_value(arguments)
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;
        let resolved = resolve(&ctx.cwd, &args.path);

        tokio::fs::remove_file(&resolved).await?;

        Ok(ToolOutput::Ok(format!("deleted {}", resolved.display())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(dir: &Path) -> ToolExecutionContext {
        ToolExecutionContext {
            cwd: dir.to_path_buf(),
            confiner: std::sync::Arc::new(aivyx_sandbox::NoopConfiner),
            cancellation: tokio_util::sync::CancellationToken::new(),
        }
    }

    #[tokio::test]
    async fn execute_deletes_the_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("gone.txt"), "bye\n").unwrap();

        let tool = DeleteFileTool;
        let output = tool
            .execute(json!({ "path": "gone.txt" }), &ctx(dir.path()))
            .await
            .unwrap();

        let ToolOutput::Ok(text) = output else {
            panic!("expected Ok output")
        };
        assert!(text.contains("deleted"), "text: {text}");
        assert!(!dir.path().join("gone.txt").exists());
    }

    #[test]
    fn permission_request_is_action_delete_with_a_path_target() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("target.txt"), "content\n").unwrap();

        let tool = DeleteFileTool;
        let request = tool
            .permission_request(&json!({ "path": "target.txt" }), dir.path())
            .unwrap();

        assert_eq!(request.action, ActionKind::Delete);
        let PermissionTarget::Path(path) = &request.target else {
            panic!("expected a Path target, got {:?}", request.target);
        };
        assert_eq!(path, &dir.path().join("target.txt"));
    }

    #[test]
    fn preview_shows_file_content_for_a_text_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("readme.txt"), "important notes\n").unwrap();

        let tool = DeleteFileTool;
        let request = tool
            .permission_request(&json!({ "path": "readme.txt" }), dir.path())
            .unwrap();

        let preview = request.preview.expect("expected a preview");
        assert!(preview.contains("important notes"));
    }

    #[test]
    fn preview_warns_instead_of_showing_content_for_a_binary_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("data.bin"), [0xFF, 0xFE, 0x00, 0xD8, 0x00]).unwrap();

        let tool = DeleteFileTool;
        let request = tool
            .permission_request(&json!({ "path": "data.bin" }), dir.path())
            .unwrap();

        let preview = request.preview.expect("expected a warning preview, not None");
        assert!(preview.contains("WARNING"));
        assert!(preview.contains("binary"));
    }

    #[test]
    fn a_nonexistent_path_is_rejected() {
        let dir = tempfile::tempdir().unwrap();

        let tool = DeleteFileTool;
        let result = tool.permission_request(&json!({ "path": "does-not-exist.txt" }), dir.path());

        assert!(matches!(result, Err(ToolError::ExecutionFailed(_))));
    }

    #[test]
    fn a_directory_target_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("a_directory")).unwrap();

        let tool = DeleteFileTool;
        let result = tool.permission_request(&json!({ "path": "a_directory" }), dir.path());

        let Err(ToolError::ExecutionFailed(message)) = result else {
            panic!("expected ExecutionFailed, got {result:?}");
        };
        assert!(message.contains("directory"), "message: {message}");
    }

    #[test]
    fn diff_carries_old_content_with_empty_new_content() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("readme.txt"), "important notes\n").unwrap();

        let tool = DeleteFileTool;
        let request = tool
            .permission_request(&json!({ "path": "readme.txt" }), dir.path())
            .unwrap();

        let diff = request.diff.expect("expected diff content for a text file");
        assert_eq!(diff.old_content, "important notes\n");
        assert_eq!(diff.new_content, "");
    }

    #[test]
    fn binary_file_has_no_structured_diff() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("data.bin"), [0xFF, 0xFE, 0x00, 0xD8, 0x00]).unwrap();

        let tool = DeleteFileTool;
        let request = tool
            .permission_request(&json!({ "path": "data.bin" }), dir.path())
            .unwrap();

        assert!(request.diff.is_none(), "a binary file has no text diff content");
    }
}
