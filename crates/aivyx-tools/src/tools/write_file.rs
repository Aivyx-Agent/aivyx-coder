use std::path::Path;

use aivyx_sandbox::{ActionKind, PermissionRequest, PermissionTarget};
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

        let old_content = match std::fs::read_to_string(&resolved) {
            Ok(content) => Some(content),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Some(String::new()),
            // Fails open on the preview only (unreadable/binary existing file) — the gate
            // still gets to decide, it just won't have a diff to show.
            Err(_) => None,
        };
        let preview = old_content
            .map(|old| unified_diff(&resolved.display().to_string(), &old, &args.content));

        Ok(PermissionRequest {
            tool_name: self.name().to_string(),
            action: ActionKind::Write,
            target: PermissionTarget::Path(resolved),
            arguments_preview: json!({ "path": args.path }),
            preview,
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
