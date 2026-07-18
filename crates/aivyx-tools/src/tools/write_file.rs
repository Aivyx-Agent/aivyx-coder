use std::path::Path;

use aivyx_sandbox::{ActionKind, DiffContent, PermissionRequest, PermissionTarget};
use aivyx_types::{ToolDefinition, ToolOutput};
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use crate::diff::unified_diff;
use crate::path_resolve::resolve;
use crate::{Tool, ToolError, ToolExecutionContext};

#[derive(Deserialize, JsonSchema)]
struct WriteFileArgs {
    /// Path to the file to create or overwrite, absolute or relative to the working directory.
    path: String,
    /// Full contents to write. Overwrites the entire file if it already exists.
    content: String,
}

pub struct WriteFileTool;

#[async_trait]
impl Tool for WriteFileTool {
    fn name(&self) -> &str {
        "write_file"
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name().to_string(),
            description: "Create a file or overwrite it entirely with new content. Creates parent \
                directories as needed. For changing part of an existing file, prefer edit_file."
                .to_string(),
            parameters_schema: serde_json::Value::from(schemars::schema_for!(WriteFileArgs)),
        }
    }

    fn permission_request(
        &self,
        arguments: &serde_json::Value,
        cwd: &Path,
    ) -> Result<PermissionRequest, ToolError> {
        let args: WriteFileArgs = serde_json::from_value(arguments.clone())
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;
        let resolved = resolve(cwd, &args.path);

        let (preview, diff) = match std::fs::read_to_string(&resolved) {
            Ok(old) => (
                Some(unified_diff(&resolved.display().to_string(), &old, &args.content)),
                Some(DiffContent { old_content: old, new_content: args.content.clone() }),
            ),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => (
                Some(unified_diff(&resolved.display().to_string(), "", &args.content)),
                Some(DiffContent { old_content: String::new(), new_content: args.content.clone() }),
            ),
            // Fails open on the gate decision (the user can still approve
            // or deny) but must NOT look identical to the new-file case —
            // an existing binary/non-UTF8 file is about to be destroyed.
            // No structured diff either: there's no text content to hand a
            // diff viewer.
            Err(_) => (
                Some(format!(
                    "WARNING: {} already exists but could not be read as text (binary file?). \
                     This write will overwrite it entirely.",
                    resolved.display()
                )),
                None,
            ),
        };

        Ok(PermissionRequest {
            tool_name: self.name().to_string(),
            action: ActionKind::Write,
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
        let args: WriteFileArgs = serde_json::from_value(arguments)
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;
        let resolved = resolve(&ctx.cwd, &args.path);

        if let Some(parent) = resolved.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&resolved, &args.content).await?;

        Ok(ToolOutput::Ok(format!(
            "wrote {} bytes to {}",
            args.content.len(),
            resolved.display()
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_file_preview_shows_additions_only() {
        let dir = tempfile::tempdir().unwrap();
        let args = serde_json::json!({ "path": "new.txt", "content": "hello\n" });

        let request = WriteFileTool.permission_request(&args, dir.path()).unwrap();

        let preview = request.preview.expect("expected a preview");
        assert!(preview.contains("+hello"));
    }

    #[test]
    fn existing_binary_file_gets_a_warning_instead_of_a_silent_diff() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("existing.bin");
        std::fs::write(&target, [0xFF, 0xFE, 0x00, 0xD8, 0x00]).unwrap();
        let args = serde_json::json!({ "path": "existing.bin", "content": "hello\n" });

        let request = WriteFileTool.permission_request(&args, dir.path()).unwrap();

        let preview = request
            .preview
            .expect("expected a warning preview, not None");
        assert!(preview.contains("WARNING"));
        assert!(preview.contains("binary"));
    }

    #[test]
    fn new_file_diff_has_empty_old_content() {
        let dir = tempfile::tempdir().unwrap();
        let args = serde_json::json!({ "path": "new.txt", "content": "hello\n" });

        let request = WriteFileTool.permission_request(&args, dir.path()).unwrap();

        let diff = request.diff.expect("expected diff content for a new file");
        assert_eq!(diff.old_content, "");
        assert_eq!(diff.new_content, "hello\n");
    }

    #[test]
    fn existing_binary_file_has_no_structured_diff() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("existing.bin");
        std::fs::write(&target, [0xFF, 0xFE, 0x00, 0xD8, 0x00]).unwrap();
        let args = serde_json::json!({ "path": "existing.bin", "content": "hello\n" });

        let request = WriteFileTool.permission_request(&args, dir.path()).unwrap();

        assert!(request.diff.is_none(), "a binary file has no text diff content");
    }
}
