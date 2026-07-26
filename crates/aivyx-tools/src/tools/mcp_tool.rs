use std::path::Path;
use std::sync::Arc;

use aivyx_sandbox::{ActionKind, PermissionRequest, PermissionTarget};
use aivyx_types::{ToolDefinition, ToolOutput};
use async_trait::async_trait;

use crate::mcp::{McpClient, ToolInfo};
use crate::{Tool, ToolError, ToolExecutionContext};

/// Wraps one MCP tool discovered from `tools/list` on a specific server,
/// presenting it through the ordinary `Tool` trait so it's callable exactly
/// like any hand-written tool. Registered under `mcp__<server>__<tool>`.
/// Does NOT override `mutates_outside_session()` — the trait default
/// (`true`) applies, so MCP tools are hidden in Plan Mode like
/// `Write`/`Execute`/`Delete` tools, since this project can't verify what a
/// third-party server's tool actually does.
pub struct McpToolAdapter {
    client: Arc<McpClient>,
    registered_name: String,
    tool_info: ToolInfo,
}

impl McpToolAdapter {
    pub fn new(client: Arc<McpClient>, server_name: &str, tool_info: ToolInfo) -> Self {
        let registered_name = format!("mcp__{server_name}__{}", tool_info.name);
        Self {
            client,
            registered_name,
            tool_info,
        }
    }
}

#[async_trait]
impl Tool for McpToolAdapter {
    fn name(&self) -> &str {
        &self.registered_name
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.registered_name.clone(),
            description: if self.tool_info.description.is_empty() {
                format!(
                    "MCP tool \"{}\" from server \"{}\".",
                    self.tool_info.name,
                    self.client.server_name()
                )
            } else {
                self.tool_info.description.clone()
            },
            parameters_schema: self.tool_info.input_schema.clone(),
        }
    }

    fn permission_request(
        &self,
        arguments: &serde_json::Value,
        _cwd: &Path,
    ) -> Result<PermissionRequest, ToolError> {
        Ok(PermissionRequest {
            tool_name: self.registered_name.clone(),
            action: ActionKind::McpTool,
            // Arguments are baked into the target description, not just
            // `tool`/`server`, so the Always-Allow cache (keyed on this
            // description alone for `Other` targets) can't bless a future
            // call with different arguments — the same reason
            // `PermissionTarget::Command` keys on full argv rather than
            // `program` alone.
            target: PermissionTarget::Other(format!(
                "{} (server: {}, args: {})",
                self.tool_info.name,
                self.client.server_name(),
                arguments
            )),
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
        self.client.ensure_started(&ctx.cwd, &ctx.confiner).await?;
        let (text, is_error) = self.client.call_tool(&self.tool_info.name, arguments).await?;
        if is_error {
            Ok(ToolOutput::Error(text))
        } else {
            Ok(ToolOutput::Ok(text))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(dir: &Path) -> ToolExecutionContext {
        ToolExecutionContext {
            cwd: dir.to_path_buf(),
            confiner: Arc::new(aivyx_sandbox::NoopConfiner),
            cancellation: tokio_util::sync::CancellationToken::new(),
        }
    }

    fn tool_info() -> ToolInfo {
        ToolInfo {
            name: "search_docs".to_string(),
            description: "search the docs".to_string(),
            input_schema: serde_json::json!({"type": "object"}),
        }
    }

    async fn server_task(
        mut server_reader: tokio::io::DuplexStream,
        mut server_writer: tokio::io::DuplexStream,
        response: serde_json::Value,
    ) {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        let mut reader = BufReader::new(&mut server_reader);
        loop {
            let mut line = String::new();
            let n = reader.read_line(&mut line).await.unwrap_or(0);
            if n == 0 {
                break;
            }
            let request: serde_json::Value = match serde_json::from_str(line.trim_end()) {
                Ok(v) => v,
                Err(_) => continue,
            };
            let Some(method) = request.get("method").and_then(|m| m.as_str()) else {
                continue;
            };
            if let Some(id) = request.get("id") {
                let result = if method == "tools/call" {
                    response.clone()
                } else {
                    serde_json::json!({})
                };
                let msg = serde_json::json!({"jsonrpc": "2.0", "id": id, "result": result});
                let mut body = serde_json::to_vec(&msg).unwrap();
                body.push(b'\n');
                let _ = server_writer.write_all(&body).await;
                let _ = server_writer.flush().await;
            }
        }
    }

    async fn wired_client(response: serde_json::Value) -> Arc<crate::mcp::McpClient> {
        let (client_reader, server_writer) = tokio::io::duplex(65536);
        let (server_reader, client_writer) = tokio::io::duplex(65536);
        tokio::spawn(server_task(server_reader, server_writer, response));
        let client = crate::mcp::McpClient::new(
            "docs".to_string(),
            "unused".to_string(),
            vec![],
            vec![],
        );
        client.wire_for_test(client_reader, client_writer).await;
        Arc::new(client)
    }

    #[test]
    fn definition_uses_the_prefixed_name_and_passes_through_the_servers_schema() {
        let client = Arc::new(crate::mcp::McpClient::new(
            "docs".to_string(),
            "unused".to_string(),
            vec![],
            vec![],
        ));
        let adapter = McpToolAdapter::new(client, "docs", tool_info());
        assert_eq!(adapter.name(), "mcp__docs__search_docs");
        let def = adapter.definition();
        assert_eq!(def.name, "mcp__docs__search_docs");
        assert_eq!(def.description, "search the docs");
        assert_eq!(def.parameters_schema, serde_json::json!({"type": "object"}));
    }

    #[test]
    fn permission_request_always_declares_action_kind_mcp_tool() {
        let client = Arc::new(crate::mcp::McpClient::new(
            "docs".to_string(),
            "unused".to_string(),
            vec![],
            vec![],
        ));
        let adapter = McpToolAdapter::new(client, "docs", tool_info());
        let request = adapter
            .permission_request(&serde_json::json!({"q": "rust"}), Path::new("."))
            .unwrap();
        assert_eq!(request.action, ActionKind::McpTool);
        assert!(matches!(request.target, PermissionTarget::Other(_)));
    }

    #[test]
    fn permission_request_target_varies_by_arguments() {
        // Regression test: `ConfirmationGate`'s Always-Allow cache keys an
        // `Other` target on its description string alone
        // (`PermissionKey::Other { action, description }`,
        // `crates/aivyx-sandbox/src/confirmation.rs`). If this description
        // doesn't vary by call arguments, approving one call with "Always
        // Allow" silently blesses every future call to the same tool
        // regardless of what arguments are passed — the exact failure
        // mode `PermissionTarget::Command` already guards against by
        // keying on full argv (see
        // `always_allow_for_a_command_does_not_cover_a_different_argv_with_the_same_program`
        // in confirmation.rs).
        let client = Arc::new(crate::mcp::McpClient::new(
            "docs".to_string(),
            "unused".to_string(),
            vec![],
            vec![],
        ));
        let adapter = McpToolAdapter::new(client, "docs", tool_info());
        let first = adapter
            .permission_request(&serde_json::json!({"q": "rust"}), Path::new("."))
            .unwrap();
        let second = adapter
            .permission_request(&serde_json::json!({"q": "python"}), Path::new("."))
            .unwrap();
        let PermissionTarget::Other(first_desc) = first.target else {
            panic!("expected an Other target");
        };
        let PermissionTarget::Other(second_desc) = second.target else {
            panic!("expected an Other target");
        };
        assert_ne!(
            first_desc, second_desc,
            "different arguments must produce different Always-Allow cache keys"
        );
    }

    #[tokio::test]
    async fn execute_returns_ok_with_the_servers_text_content() {
        let client = wired_client(serde_json::json!({
            "content": [{"type": "text", "text": "found 3 matches"}],
            "isError": false
        }))
        .await;
        let adapter = McpToolAdapter::new(client, "docs", tool_info());
        let dir = tempfile::tempdir().unwrap();
        let output = adapter
            .execute(serde_json::json!({"q": "rust"}), &ctx(dir.path()))
            .await
            .unwrap();
        assert!(matches!(output, ToolOutput::Ok(text) if text == "found 3 matches"));
    }

    #[tokio::test]
    async fn execute_maps_is_error_true_to_tool_output_error() {
        let client = wired_client(serde_json::json!({
            "content": [{"type": "text", "text": "no docs configured"}],
            "isError": true
        }))
        .await;
        let adapter = McpToolAdapter::new(client, "docs", tool_info());
        let dir = tempfile::tempdir().unwrap();
        let output = adapter
            .execute(serde_json::json!({"q": "rust"}), &ctx(dir.path()))
            .await
            .unwrap();
        assert!(matches!(output, ToolOutput::Error(text) if text == "no docs configured"));
    }

    #[tokio::test]
    async fn execute_propagates_a_connection_failure_as_tool_error() {
        let client = Arc::new(crate::mcp::McpClient::new(
            "broken".to_string(),
            "definitely-not-a-real-binary-xyz".to_string(),
            vec![],
            vec![],
        ));
        let adapter = McpToolAdapter::new(client, "broken", tool_info());
        let dir = tempfile::tempdir().unwrap();
        let err = adapter
            .execute(serde_json::json!({}), &ctx(dir.path()))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::ExecutionFailed(_)));
    }
}
