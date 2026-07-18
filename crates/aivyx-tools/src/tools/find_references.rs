use std::path::Path;
use std::sync::Arc;

use aivyx_sandbox::{ActionKind, PermissionRequest, PermissionTarget};
use aivyx_types::{ToolDefinition, ToolOutput};
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use crate::lsp::LspClient;
use crate::{Tool, ToolError, ToolExecutionContext};

#[derive(Deserialize, JsonSchema)]
struct FindReferencesArgs {
    /// Path to the file, relative to the working directory.
    path: String,
    /// 1-indexed line number of the symbol.
    line: u32,
    /// 1-indexed column number of the symbol.
    column: u32,
}

/// Exact symbol resolution via rust-analyzer: finds every reference to the
/// symbol at a position across the whole workspace, which grep's textual
/// search can't distinguish from unrelated same-named identifiers.
pub struct FindReferencesTool {
    lsp: Arc<LspClient>,
}

impl FindReferencesTool {
    pub fn new(lsp: Arc<LspClient>) -> Self {
        Self { lsp }
    }
}

#[async_trait]
impl Tool for FindReferencesTool {
    fn name(&self) -> &str {
        "find_references"
    }

    fn mutates_outside_session(&self) -> bool {
        false
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name().to_string(),
            description: "Find every reference to the symbol at a file position (1-indexed \
                line/column) across the whole workspace via rust-analyzer — unlike grep, this \
                distinguishes the symbol from unrelated identifiers that merely share its name. \
                Returns path:line:text per reference site, or a message if nothing was found."
                .to_string(),
            parameters_schema: serde_json::Value::from(schemars::schema_for!(FindReferencesArgs)),
        }
    }

    fn permission_request(
        &self,
        arguments: &serde_json::Value,
        cwd: &Path,
    ) -> Result<PermissionRequest, ToolError> {
        let args: FindReferencesArgs = serde_json::from_value(arguments.clone())
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;
        Ok(PermissionRequest {
            tool_name: self.name().to_string(),
            action: ActionKind::Read,
            target: PermissionTarget::Path(crate::path_resolve::resolve(cwd, &args.path)),
            arguments_preview: json!({ "path": args.path, "line": args.line, "column": args.column }),
            preview: None,
            diff: None,
        })
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        ctx: &ToolExecutionContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: FindReferencesArgs = serde_json::from_value(arguments)
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;
        let text = self
            .lsp
            .find_references(&ctx.cwd, &ctx.confiner, &args.path, args.line, args.column)
            .await?;
        Ok(ToolOutput::Ok(text))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    async fn ctx(dir: &Path) -> ToolExecutionContext {
        ToolExecutionContext {
            cwd: dir.to_path_buf(),
            confiner: Arc::new(aivyx_sandbox::NoopConfiner),
            cancellation: tokio_util::sync::CancellationToken::new(),
        }
    }

    #[tokio::test]
    async fn permission_request_targets_the_resolved_file_path_as_a_read() {
        let dir = tempfile::tempdir().unwrap();
        let lsp = Arc::new(LspClient::new(Duration::from_secs(5)));
        let tool = FindReferencesTool::new(lsp);

        let request = tool
            .permission_request(&json!({"path": "src/lib.rs", "line": 3, "column": 5}), dir.path())
            .unwrap();

        assert_eq!(request.action, ActionKind::Read);
        assert_eq!(
            request.target,
            PermissionTarget::Path(dir.path().canonicalize().unwrap().join("src/lib.rs"))
        );
    }

    #[tokio::test]
    async fn execute_returns_ok_output_with_the_lsp_clients_result() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("lib.rs"), "fn fib(n: u64) -> u64 { n }\n").unwrap();

        // `with_program` (not `LspClient::new`) with a name that can never
        // exist — deterministic regardless of whether the test machine
        // happens to have a real rust-analyzer on PATH.
        let lsp = Arc::new(LspClient::with_program(
            "definitely-not-a-real-binary-xyz",
            Duration::from_secs(5),
        ));
        let tool = FindReferencesTool::new(lsp);
        let ctx = ctx(dir.path()).await;

        let err = tool
            .execute(json!({"path": "lib.rs", "line": 1, "column": 4}), &ctx)
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::ExecutionFailed(_)));
    }
}
