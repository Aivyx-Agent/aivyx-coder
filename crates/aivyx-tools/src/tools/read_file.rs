use std::path::Path;

use aivyx_sandbox::{ActionKind, PermissionRequest, PermissionTarget};
use aivyx_types::{ToolDefinition, ToolOutput};
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use crate::bounded_read;
use crate::path_resolve::resolve;
use crate::{Tool, ToolError, ToolExecutionContext};

#[derive(Deserialize, JsonSchema)]
struct ReadFileArgs {
    /// Path to the file to read, absolute or relative to the working directory.
    path: String,
}

pub struct ReadFileTool;

#[async_trait]
impl Tool for ReadFileTool {
    fn name(&self) -> &str {
        "read_file"
    }

    fn mutates_outside_session(&self) -> bool {
        false
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name().to_string(),
            description: "Read the contents of a text file (the first 1 MiB of a larger one).".to_string(),
            parameters_schema: serde_json::Value::from(schemars::schema_for!(ReadFileArgs)),
        }
    }

    fn permission_request(
        &self,
        arguments: &serde_json::Value,
        cwd: &Path,
    ) -> Result<PermissionRequest, ToolError> {
        let args: ReadFileArgs = serde_json::from_value(arguments.clone())
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;
        let resolved = resolve(cwd, &args.path);

        Ok(PermissionRequest {
            tool_name: self.name().to_string(),
            action: ActionKind::Read,
            target: PermissionTarget::Path(resolved),
            arguments_preview: json!({ "path": args.path }),
            preview: None,
            diff: None,
        })
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        ctx: &ToolExecutionContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: ReadFileArgs = serde_json::from_value(arguments)
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;
        let resolved = resolve(&ctx.cwd, &args.path);

        let capped = tokio::task::spawn_blocking(move || {
            bounded_read::read_capped(&resolved, bounded_read::READ_FILE_LIMIT)
        })
        .await
        .map_err(|err| ToolError::ExecutionFailed(format!("read_file task failed: {err}")))??;
        let mut content = capped.text;
        if capped.truncated {
            content.push_str(&format!(
                "\n\n[read_file: showing the first {} of {} — use grep to find the part you need]",
                bounded_read::human_size(bounded_read::READ_FILE_LIMIT),
                bounded_read::human_size(capped.size)
            ));
        }
        Ok(ToolOutput::Ok(content))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn ctx(dir: &Path) -> ToolExecutionContext {
        ToolExecutionContext {
            cwd: dir.to_path_buf(),
            confiner: Arc::new(aivyx_sandbox::NoopConfiner),
            cancellation: tokio_util::sync::CancellationToken::new(),
        }
    }

    #[tokio::test]
    async fn a_device_is_refused_rather_than_read_forever() {
        let dir = tempfile::tempdir().unwrap();
        let out = ReadFileTool.execute(json!({"path": "/dev/zero"}), &ctx(dir.path())).await;
        let err = out.expect_err("/dev/zero must be refused");
        assert!(err.to_string().contains("not a regular file"), "{err}");
    }

    #[tokio::test]
    async fn a_large_file_is_cut_with_a_note() {
        let dir = tempfile::tempdir().unwrap();
        let big = "x".repeat(crate::bounded_read::READ_FILE_LIMIT as usize * 2);
        std::fs::write(dir.path().join("big.log"), &big).unwrap();
        let ToolOutput::Ok(text) =
            ReadFileTool.execute(json!({"path": "big.log"}), &ctx(dir.path())).await.unwrap()
        else {
            panic!("expected Ok");
        };
        assert!(text.len() < big.len());
        assert!(text.contains("showing the first 1.0 MiB of 2.0 MiB"), "{}", &text[text.len() - 200..]);
        assert!(text.contains("grep"));
    }
}
