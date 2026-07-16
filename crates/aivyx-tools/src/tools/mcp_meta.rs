use std::path::Path;
use std::sync::Arc;

use aivyx_sandbox::{ActionKind, PermissionRequest, PermissionTarget};
use aivyx_types::{ToolDefinition, ToolOutput};
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;

use crate::mcp::McpClient;
use crate::{Tool, ToolError, ToolExecutionContext};

fn find_client<'a>(clients: &'a [Arc<McpClient>], server: &str) -> Option<&'a Arc<McpClient>> {
    clients.iter().find(|c| c.server_name() == server)
}

#[derive(Deserialize, JsonSchema)]
struct ServerFilterArgs {
    /// Restrict to one configured MCP server by name; omit to aggregate
    /// across every connected server.
    server: Option<String>,
}

/// Unlike arbitrary MCP tools (see `McpToolAdapter`), MCP's resources and
/// prompts primitives are protocol-guaranteed read-only, so all four
/// meta-tools in this file use `ActionKind::Read` and are available in
/// Plan Mode (`mutates_outside_session() == false`) — the read-only
/// guarantee comes from what a resource/prompt *is* under the MCP spec, not
/// from trusting any individual server's self-description.
pub struct ListMcpResourcesTool {
    clients: Vec<Arc<McpClient>>,
}

impl ListMcpResourcesTool {
    pub fn new(clients: Vec<Arc<McpClient>>) -> Self {
        Self { clients }
    }
}

#[async_trait]
impl Tool for ListMcpResourcesTool {
    fn name(&self) -> &str {
        "list_mcp_resources"
    }

    fn mutates_outside_session(&self) -> bool {
        false
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name().to_string(),
            description: "List resources exposed by connected MCP servers (uri, name, \
                description). Pass `server` to restrict to one server, or omit to aggregate \
                across all of them."
                .to_string(),
            parameters_schema: serde_json::Value::from(schemars::schema_for!(ServerFilterArgs)),
        }
    }

    fn permission_request(
        &self,
        arguments: &serde_json::Value,
        _cwd: &Path,
    ) -> Result<PermissionRequest, ToolError> {
        Ok(PermissionRequest {
            tool_name: self.name().to_string(),
            action: ActionKind::Read,
            target: PermissionTarget::Other("mcp resources".to_string()),
            arguments_preview: arguments.clone(),
            preview: None,
        })
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        _ctx: &ToolExecutionContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: ServerFilterArgs = serde_json::from_value(arguments)
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;

        let targets: Vec<&Arc<McpClient>> = match &args.server {
            Some(name) => match find_client(&self.clients, name) {
                Some(client) => vec![client],
                None => {
                    return Ok(ToolOutput::Error(format!(
                        "no connected MCP server named \"{name}\""
                    )));
                }
            },
            None => self.clients.iter().collect(),
        };

        let mut lines = Vec::new();
        for client in targets {
            let resources = client.list_resources().await?;
            for resource in resources {
                lines.push(format!(
                    "{} | {} | {} | {}",
                    client.server_name(),
                    resource.uri,
                    resource.name,
                    resource.description
                ));
            }
        }
        if lines.is_empty() {
            Ok(ToolOutput::Ok("no MCP resources found".to_string()))
        } else {
            Ok(ToolOutput::Ok(lines.join("\n")))
        }
    }
}

#[derive(Deserialize, JsonSchema)]
struct ReadResourceArgs {
    /// Which MCP server the URI belongs to.
    server: String,
    /// The resource URI to fetch, as returned by list_mcp_resources.
    uri: String,
}

pub struct ReadMcpResourceTool {
    clients: Vec<Arc<McpClient>>,
}

impl ReadMcpResourceTool {
    pub fn new(clients: Vec<Arc<McpClient>>) -> Self {
        Self { clients }
    }
}

#[async_trait]
impl Tool for ReadMcpResourceTool {
    fn name(&self) -> &str {
        "read_mcp_resource"
    }

    fn mutates_outside_session(&self) -> bool {
        false
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name().to_string(),
            description: "Fetch one MCP resource's content by server name and URI (see \
                list_mcp_resources). Binary resources render as a placeholder note, not \
                decoded."
                .to_string(),
            parameters_schema: serde_json::Value::from(schemars::schema_for!(ReadResourceArgs)),
        }
    }

    fn permission_request(
        &self,
        arguments: &serde_json::Value,
        _cwd: &Path,
    ) -> Result<PermissionRequest, ToolError> {
        Ok(PermissionRequest {
            tool_name: self.name().to_string(),
            action: ActionKind::Read,
            target: PermissionTarget::Other("mcp resource".to_string()),
            arguments_preview: arguments.clone(),
            preview: None,
        })
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        _ctx: &ToolExecutionContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: ReadResourceArgs = serde_json::from_value(arguments)
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;
        let Some(client) = find_client(&self.clients, &args.server) else {
            return Ok(ToolOutput::Error(format!(
                "no connected MCP server named \"{}\"",
                args.server
            )));
        };
        let text = client.read_resource(&args.uri).await?;
        Ok(ToolOutput::Ok(text))
    }
}

pub struct ListMcpPromptsTool {
    clients: Vec<Arc<McpClient>>,
}

impl ListMcpPromptsTool {
    pub fn new(clients: Vec<Arc<McpClient>>) -> Self {
        Self { clients }
    }
}

#[async_trait]
impl Tool for ListMcpPromptsTool {
    fn name(&self) -> &str {
        "list_mcp_prompts"
    }

    fn mutates_outside_session(&self) -> bool {
        false
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name().to_string(),
            description: "List prompt templates exposed by connected MCP servers (name, \
                description, declared arguments). Pass `server` to restrict to one server, or \
                omit to aggregate across all of them."
                .to_string(),
            parameters_schema: serde_json::Value::from(schemars::schema_for!(ServerFilterArgs)),
        }
    }

    fn permission_request(
        &self,
        arguments: &serde_json::Value,
        _cwd: &Path,
    ) -> Result<PermissionRequest, ToolError> {
        Ok(PermissionRequest {
            tool_name: self.name().to_string(),
            action: ActionKind::Read,
            target: PermissionTarget::Other("mcp prompts".to_string()),
            arguments_preview: arguments.clone(),
            preview: None,
        })
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        _ctx: &ToolExecutionContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: ServerFilterArgs = serde_json::from_value(arguments)
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;

        let targets: Vec<&Arc<McpClient>> = match &args.server {
            Some(name) => match find_client(&self.clients, name) {
                Some(client) => vec![client],
                None => {
                    return Ok(ToolOutput::Error(format!(
                        "no connected MCP server named \"{name}\""
                    )));
                }
            },
            None => self.clients.iter().collect(),
        };

        let mut lines = Vec::new();
        for client in targets {
            let prompts = client.list_prompts().await?;
            for prompt in prompts {
                let arg_names: Vec<&str> =
                    prompt.arguments.iter().map(|a| a.name.as_str()).collect();
                lines.push(format!(
                    "{} | {} | {} | args: [{}]",
                    client.server_name(),
                    prompt.name,
                    prompt.description,
                    arg_names.join(", ")
                ));
            }
        }
        if lines.is_empty() {
            Ok(ToolOutput::Ok("no MCP prompts found".to_string()))
        } else {
            Ok(ToolOutput::Ok(lines.join("\n")))
        }
    }
}

#[derive(Deserialize, JsonSchema)]
struct GetPromptArgs {
    /// Which MCP server the prompt belongs to.
    server: String,
    /// The prompt's name, as returned by list_mcp_prompts.
    name: String,
    /// Arguments the prompt template declares, if any.
    arguments: Option<serde_json::Value>,
}

pub struct GetMcpPromptTool {
    clients: Vec<Arc<McpClient>>,
}

impl GetMcpPromptTool {
    pub fn new(clients: Vec<Arc<McpClient>>) -> Self {
        Self { clients }
    }
}

#[async_trait]
impl Tool for GetMcpPromptTool {
    fn name(&self) -> &str {
        "get_mcp_prompt"
    }

    fn mutates_outside_session(&self) -> bool {
        false
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name().to_string(),
            description: "Fetch one MCP prompt template's expanded content by server and name \
                (see list_mcp_prompts), with optional arguments. Returns the resolved messages \
                as text for you to read and use directly."
                .to_string(),
            parameters_schema: serde_json::Value::from(schemars::schema_for!(GetPromptArgs)),
        }
    }

    fn permission_request(
        &self,
        arguments: &serde_json::Value,
        _cwd: &Path,
    ) -> Result<PermissionRequest, ToolError> {
        Ok(PermissionRequest {
            tool_name: self.name().to_string(),
            action: ActionKind::Read,
            target: PermissionTarget::Other("mcp prompt".to_string()),
            arguments_preview: arguments.clone(),
            preview: None,
        })
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        _ctx: &ToolExecutionContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: GetPromptArgs = serde_json::from_value(arguments)
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;
        let Some(client) = find_client(&self.clients, &args.server) else {
            return Ok(ToolOutput::Error(format!(
                "no connected MCP server named \"{}\"",
                args.server
            )));
        };
        let text = client.get_prompt(&args.name, args.arguments).await?;
        Ok(ToolOutput::Ok(text))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn server_task(
        mut server_reader: tokio::io::DuplexStream,
        mut server_writer: tokio::io::DuplexStream,
        response_for: impl Fn(&str) -> serde_json::Value + Send + 'static,
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
                let result = response_for(method);
                let msg = serde_json::json!({"jsonrpc": "2.0", "id": id, "result": result});
                let mut body = serde_json::to_vec(&msg).unwrap();
                body.push(b'\n');
                let _ = server_writer.write_all(&body).await;
                let _ = server_writer.flush().await;
            }
        }
    }

    async fn wired_client(
        name: &str,
        response_for: impl Fn(&str) -> serde_json::Value + Send + 'static,
    ) -> Arc<McpClient> {
        let (client_reader, server_writer) = tokio::io::duplex(65536);
        let (server_reader, client_writer) = tokio::io::duplex(65536);
        tokio::spawn(server_task(server_reader, server_writer, response_for));
        let client = McpClient::new(name.to_string(), "unused".to_string(), vec![], vec![]);
        client.wire_for_test(client_reader, client_writer).await;
        Arc::new(client)
    }

    fn ctx() -> ToolExecutionContext {
        ToolExecutionContext {
            cwd: std::path::PathBuf::from("."),
            confiner: Arc::new(aivyx_sandbox::NoopConfiner),
            cancellation: tokio_util::sync::CancellationToken::new(),
        }
    }

    #[tokio::test]
    async fn list_mcp_resources_aggregates_across_all_connected_servers() {
        let a = wired_client("a", |_| {
            serde_json::json!({"resources": [{"uri": "file:///a.txt", "name": "a", "description": "d"}]})
        })
        .await;
        let b = wired_client("b", |_| {
            serde_json::json!({"resources": [{"uri": "file:///b.txt", "name": "b", "description": "d"}]})
        })
        .await;
        let tool = ListMcpResourcesTool::new(vec![a, b]);
        let output = tool.execute(serde_json::json!({}), &ctx()).await.unwrap();
        let ToolOutput::Ok(text) = output else { panic!("expected Ok") };
        assert!(text.contains("file:///a.txt"));
        assert!(text.contains("file:///b.txt"));
    }

    #[tokio::test]
    async fn list_mcp_resources_filters_to_one_server_when_given() {
        let a = wired_client("a", |_| {
            serde_json::json!({"resources": [{"uri": "file:///a.txt", "name": "a", "description": "d"}]})
        })
        .await;
        let b = wired_client("b", |_| {
            serde_json::json!({"resources": [{"uri": "file:///b.txt", "name": "b", "description": "d"}]})
        })
        .await;
        let tool = ListMcpResourcesTool::new(vec![a, b]);
        let output = tool
            .execute(serde_json::json!({"server": "a"}), &ctx())
            .await
            .unwrap();
        let ToolOutput::Ok(text) = output else { panic!("expected Ok") };
        assert!(text.contains("file:///a.txt"));
        assert!(!text.contains("file:///b.txt"));
    }

    #[tokio::test]
    async fn list_mcp_resources_reports_ok_with_a_clear_message_when_empty() {
        let a = wired_client("a", |_| serde_json::json!({"resources": []})).await;
        let tool = ListMcpResourcesTool::new(vec![a]);
        let output = tool.execute(serde_json::json!({}), &ctx()).await.unwrap();
        assert!(matches!(output, ToolOutput::Ok(text) if text == "no MCP resources found"));
    }

    #[tokio::test]
    async fn read_mcp_resource_returns_the_named_servers_content() {
        let a = wired_client("a", |_| serde_json::json!({"contents": [{"uri": "file:///a.txt", "text": "hello"}]})).await;
        let tool = ReadMcpResourceTool::new(vec![a]);
        let output = tool
            .execute(
                serde_json::json!({"server": "a", "uri": "file:///a.txt"}),
                &ctx(),
            )
            .await
            .unwrap();
        assert!(matches!(output, ToolOutput::Ok(text) if text.contains("hello")));
    }

    #[tokio::test]
    async fn read_mcp_resource_errors_clearly_for_an_unknown_server() {
        let a = wired_client("a", |_| serde_json::json!({})).await;
        let tool = ReadMcpResourceTool::new(vec![a]);
        let output = tool
            .execute(
                serde_json::json!({"server": "nonexistent", "uri": "file:///a.txt"}),
                &ctx(),
            )
            .await
            .unwrap();
        assert!(matches!(output, ToolOutput::Error(msg) if msg.contains("nonexistent")));
    }

    #[tokio::test]
    async fn list_mcp_prompts_aggregates_across_all_connected_servers() {
        let a = wired_client("a", |_| {
            serde_json::json!({"prompts": [{"name": "p1", "description": "d", "arguments": []}]})
        })
        .await;
        let tool = ListMcpPromptsTool::new(vec![a]);
        let output = tool.execute(serde_json::json!({}), &ctx()).await.unwrap();
        assert!(matches!(output, ToolOutput::Ok(text) if text.contains("p1")));
    }

    #[tokio::test]
    async fn get_mcp_prompt_returns_the_expanded_prompt_text() {
        let a = wired_client("a", |_| {
            serde_json::json!({"messages": [{"role": "user", "content": {"type": "text", "text": "hi"}}]})
        })
        .await;
        let tool = GetMcpPromptTool::new(vec![a]);
        let output = tool
            .execute(serde_json::json!({"server": "a", "name": "p1"}), &ctx())
            .await
            .unwrap();
        assert!(matches!(output, ToolOutput::Ok(text) if text.contains("hi")));
    }

    #[test]
    fn all_four_meta_tools_do_not_mutate_outside_session() {
        assert!(!ListMcpResourcesTool::new(vec![]).mutates_outside_session());
        assert!(!ReadMcpResourceTool::new(vec![]).mutates_outside_session());
        assert!(!ListMcpPromptsTool::new(vec![]).mutates_outside_session());
        assert!(!GetMcpPromptTool::new(vec![]).mutates_outside_session());
    }
}
