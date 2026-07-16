mod protocol;
mod transport;

use std::path::Path;
use std::sync::Arc;

use aivyx_sandbox::ExecutionConfiner;
use tokio::sync::Mutex;

use crate::ToolError;
use transport::McpConnection;

pub use protocol::ToolInfo;

struct Started {
    connection: Arc<McpConnection>,
    child: Option<tokio::process::Child>,
}

/// Owns one configured MCP server's spawned child process (or, in tests, an
/// in-memory transport wired directly) and its JSON-RPC session. One
/// `McpClient` per configured `[[mcp.servers]]` entry, shared via `Arc`
/// between every `McpToolAdapter`/meta-tool that talks to it.
pub struct McpClient {
    server_name: String,
    program: String,
    args: Vec<String>,
    env: Vec<(String, String)>,
    state: Mutex<Option<Started>>,
}

impl McpClient {
    pub fn new(server_name: String, program: String, args: Vec<String>, env: Vec<(String, String)>) -> Self {
        Self {
            server_name,
            program,
            args,
            env,
            state: Mutex::new(None),
        }
    }

    pub fn server_name(&self) -> &str {
        &self.server_name
    }

    #[cfg(test)]
    pub(crate) async fn wire_for_test(
        &self,
        reader: impl tokio::io::AsyncRead + Unpin + Send + 'static,
        writer: impl tokio::io::AsyncWrite + Unpin + Send + 'static,
    ) {
        *self.state.lock().await = Some(Started {
            connection: Arc::new(McpConnection::new(reader, writer)),
            child: None,
        });
    }

    /// Ensures a live, initialized MCP session exists, spawning (or
    /// respawning, if the previous process died) as needed. A respawn only
    /// redoes the `initialize` handshake — never full rediscovery
    /// (`tools/list`/`resources/list`/`prompts/list` only ever run once, at
    /// startup, to populate the static `ToolRegistry`) — on the assumption
    /// a given server binary's capability set is stable across its own
    /// restarts.
    pub async fn ensure_started(
        &self,
        cwd: &Path,
        confiner: &Arc<dyn ExecutionConfiner>,
    ) -> Result<(), ToolError> {
        let mut guard = self.state.lock().await;

        if let Some(started) = guard.as_mut() {
            let dead = started.connection.is_dead()
                || started
                    .child
                    .as_mut()
                    .map(|child| matches!(child.try_wait(), Ok(Some(_))))
                    .unwrap_or(false);
            if !dead {
                return Ok(());
            }
        }

        let mut command = tokio::process::Command::new(&self.program);
        command
            .args(&self.args)
            .current_dir(cwd)
            .envs(self.env.iter().cloned());
        command = confiner.confine(command);
        command
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);

        let mut child = command.spawn().map_err(|err| {
            ToolError::ExecutionFailed(format!(
                "MCP server \"{}\" ({}) not found or failed to start: {err}",
                self.server_name, self.program
            ))
        })?;
        let stdin = child.stdin.take().expect("stdin was piped");
        let stdout = child.stdout.take().expect("stdout was piped");
        let connection = McpConnection::new(stdout, stdin);

        initialize(&connection, &self.server_name).await?;

        *guard = Some(Started {
            connection: Arc::new(connection),
            child: Some(child),
        });
        Ok(())
    }

    async fn connection(&self) -> Result<Arc<McpConnection>, ToolError> {
        let guard = self.state.lock().await;
        guard
            .as_ref()
            .map(|s| Arc::clone(&s.connection))
            .ok_or_else(|| {
                ToolError::ExecutionFailed(format!(
                    "MCP server \"{}\" is not connected — call ensure_started first",
                    self.server_name
                ))
            })
    }

    pub async fn list_tools(&self) -> Result<Vec<protocol::ToolInfo>, ToolError> {
        let connection = self.connection().await?;
        let result = connection.request("tools/list", serde_json::json!({})).await?;
        let parsed: protocol::ToolsListResult = serde_json::from_value(result).map_err(|err| {
            ToolError::ExecutionFailed(format!("malformed tools/list response: {err}"))
        })?;
        Ok(parsed.tools)
    }

    pub async fn call_tool(
        &self,
        name: &str,
        arguments: serde_json::Value,
    ) -> Result<(String, bool), ToolError> {
        let connection = self.connection().await?;
        let params = serde_json::json!({ "name": name, "arguments": arguments });
        let result = connection.request("tools/call", params).await?;
        let parsed: protocol::ToolCallResult = serde_json::from_value(result).map_err(|err| {
            ToolError::ExecutionFailed(format!("malformed tools/call response: {err}"))
        })?;
        Ok((render_content_blocks(&parsed.content), parsed.is_error))
    }

    pub async fn list_resources(&self) -> Result<Vec<protocol::ResourceInfo>, ToolError> {
        let connection = self.connection().await?;
        let result = connection
            .request("resources/list", serde_json::json!({}))
            .await?;
        let parsed: protocol::ResourcesListResult =
            serde_json::from_value(result).map_err(|err| {
                ToolError::ExecutionFailed(format!("malformed resources/list response: {err}"))
            })?;
        Ok(parsed.resources)
    }

    pub async fn read_resource(&self, uri: &str) -> Result<String, ToolError> {
        let connection = self.connection().await?;
        let params = serde_json::json!({ "uri": uri });
        let result = connection.request("resources/read", params).await?;
        let parsed: protocol::ResourcesReadResult =
            serde_json::from_value(result).map_err(|err| {
                ToolError::ExecutionFailed(format!("malformed resources/read response: {err}"))
            })?;
        Ok(render_resource_contents(&parsed.contents))
    }

    pub async fn list_prompts(&self) -> Result<Vec<protocol::PromptInfo>, ToolError> {
        let connection = self.connection().await?;
        let result = connection
            .request("prompts/list", serde_json::json!({}))
            .await?;
        let parsed: protocol::PromptsListResult = serde_json::from_value(result).map_err(|err| {
            ToolError::ExecutionFailed(format!("malformed prompts/list response: {err}"))
        })?;
        Ok(parsed.prompts)
    }

    pub async fn get_prompt(
        &self,
        name: &str,
        arguments: Option<serde_json::Value>,
    ) -> Result<String, ToolError> {
        let connection = self.connection().await?;
        let mut params = serde_json::json!({ "name": name });
        if let Some(arguments) = arguments {
            params["arguments"] = arguments;
        }
        let result = connection.request("prompts/get", params).await?;
        let parsed: protocol::PromptGetResult = serde_json::from_value(result).map_err(|err| {
            ToolError::ExecutionFailed(format!("malformed prompts/get response: {err}"))
        })?;
        Ok(render_prompt_messages(&parsed.messages))
    }
}

async fn initialize(connection: &McpConnection, server_name: &str) -> Result<(), ToolError> {
    let params = serde_json::json!({
        "protocolVersion": "2025-06-18",
        "capabilities": {},
        "clientInfo": { "name": "aivyx-coder", "version": env!("CARGO_PKG_VERSION") },
    });
    connection.request("initialize", params).await.map_err(|err| {
        ToolError::ExecutionFailed(format!(
            "MCP server \"{server_name}\" failed to initialize: {err}"
        ))
    })?;
    connection
        .notify("notifications/initialized", serde_json::json!({}))
        .await?;
    Ok(())
}

fn render_content_blocks(blocks: &[protocol::ContentBlock]) -> String {
    let mut rendered = String::new();
    for block in blocks {
        if !rendered.is_empty() {
            rendered.push('\n');
        }
        if block.kind == "text" {
            rendered.push_str(block.text.as_deref().unwrap_or(""));
        } else {
            rendered.push_str(&format!(
                "[non-text content of type \"{}\" omitted]",
                block.kind
            ));
        }
    }
    rendered
}

fn render_resource_contents(contents: &[protocol::ResourceContent]) -> String {
    let mut rendered = String::new();
    for content in contents {
        if !rendered.is_empty() {
            rendered.push('\n');
        }
        if let Some(text) = &content.text {
            rendered.push_str(text);
        } else if content.blob.is_some() {
            rendered.push_str(&format!(
                "[binary resource content at {} omitted]",
                content.uri
            ));
        }
    }
    rendered
}

fn render_prompt_messages(messages: &[protocol::PromptMessage]) -> String {
    messages
        .iter()
        .map(|m| {
            if m.content.kind == "text" {
                format!("{}: {}", m.role, m.content.text.clone().unwrap_or_default())
            } else {
                format!(
                    "{}: [non-text content of type \"{}\" omitted]",
                    m.role, m.content.kind
                )
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
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
                let response = serde_json::json!({"jsonrpc": "2.0", "id": id, "result": result});
                let mut body = serde_json::to_vec(&response).unwrap();
                body.push(b'\n');
                let _ = server_writer.write_all(&body).await;
                let _ = server_writer.flush().await;
            }
        }
    }

    async fn wired_client(
        response_for: impl Fn(&str) -> serde_json::Value + Send + 'static,
    ) -> McpClient {
        let (client_reader, server_writer) = tokio::io::duplex(65536);
        let (server_reader, client_writer) = tokio::io::duplex(65536);
        tokio::spawn(server_task(server_reader, server_writer, response_for));
        let client = McpClient::new(
            "test-server".to_string(),
            "unused".to_string(),
            vec![],
            vec![],
        );
        client.wire_for_test(client_reader, client_writer).await;
        client
    }

    #[tokio::test]
    async fn ensure_started_reports_a_clear_error_when_the_program_is_missing() {
        let client = McpClient::new(
            "broken".to_string(),
            "definitely-not-a-real-binary-xyz".to_string(),
            vec![],
            vec![],
        );
        let confiner: Arc<dyn ExecutionConfiner> = Arc::new(aivyx_sandbox::NoopConfiner);
        let err = client
            .ensure_started(Path::new("."), &confiner)
            .await
            .unwrap_err();
        let message = err.to_string();
        assert!(message.contains("broken"));
    }

    #[tokio::test]
    async fn ensure_started_reuses_an_already_healthy_session_without_respawning() {
        let client = wired_client(|_| serde_json::json!({})).await;
        let confiner: Arc<dyn ExecutionConfiner> = Arc::new(aivyx_sandbox::NoopConfiner);
        // The wired-for-test connection has no real child process behind
        // it, so a respawn attempt would try to spawn "unused" and fail —
        // two Ok(()) calls in a row prove the healthy session was reused,
        // not respawned.
        client.ensure_started(Path::new("."), &confiner).await.unwrap();
        client.ensure_started(Path::new("."), &confiner).await.unwrap();
    }

    #[tokio::test]
    async fn list_tools_parses_tools_from_a_scripted_response() {
        let client = wired_client(|method| {
            assert_eq!(method, "tools/list");
            serde_json::json!({
                "tools": [
                    {"name": "search_docs", "description": "search the docs", "inputSchema": {"type": "object"}}
                ]
            })
        })
        .await;
        let tools = client.list_tools().await.unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "search_docs");
        assert_eq!(tools[0].description, "search the docs");
    }

    #[tokio::test]
    async fn call_tool_concatenates_text_content_blocks_and_reports_is_error() {
        let client = wired_client(|method| {
            assert_eq!(method, "tools/call");
            serde_json::json!({
                "content": [
                    {"type": "text", "text": "first"},
                    {"type": "text", "text": "second"},
                    {"type": "image", "data": "base64stuff"}
                ],
                "isError": false
            })
        })
        .await;
        let (text, is_error) = client
            .call_tool("search_docs", serde_json::json!({"q": "rust"}))
            .await
            .unwrap();
        assert!(text.contains("first"));
        assert!(text.contains("second"));
        assert!(text.contains("[non-text content of type \"image\" omitted]"));
        assert!(!is_error);
    }

    #[tokio::test]
    async fn call_tool_reports_is_error_true_when_the_server_says_so() {
        let client = wired_client(|_| {
            serde_json::json!({
                "content": [{"type": "text", "text": "boom"}],
                "isError": true
            })
        })
        .await;
        let (text, is_error) = client
            .call_tool("search_docs", serde_json::json!({}))
            .await
            .unwrap();
        assert_eq!(text, "boom");
        assert!(is_error);
    }

    #[tokio::test]
    async fn list_resources_parses_resources_from_a_scripted_response() {
        let client = wired_client(|method| {
            assert_eq!(method, "resources/list");
            serde_json::json!({
                "resources": [
                    {"uri": "file:///a.txt", "name": "a.txt", "description": "a file"}
                ]
            })
        })
        .await;
        let resources = client.list_resources().await.unwrap();
        assert_eq!(resources.len(), 1);
        assert_eq!(resources[0].uri, "file:///a.txt");
    }

    #[tokio::test]
    async fn read_resource_renders_text_and_binary_placeholder_content() {
        let client = wired_client(|method| {
            assert_eq!(method, "resources/read");
            serde_json::json!({
                "contents": [
                    {"uri": "file:///a.txt", "text": "hello"},
                    {"uri": "file:///b.png", "blob": "base64stuff"}
                ]
            })
        })
        .await;
        let text = client.read_resource("file:///a.txt").await.unwrap();
        assert!(text.contains("hello"));
        assert!(text.contains("[binary resource content at file:///b.png omitted]"));
    }

    #[tokio::test]
    async fn list_prompts_parses_prompts_from_a_scripted_response() {
        let client = wired_client(|method| {
            assert_eq!(method, "prompts/list");
            serde_json::json!({
                "prompts": [
                    {"name": "summarize", "description": "summarize text", "arguments": [{"name": "text", "required": true}]}
                ]
            })
        })
        .await;
        let prompts = client.list_prompts().await.unwrap();
        assert_eq!(prompts.len(), 1);
        assert_eq!(prompts[0].name, "summarize");
        assert_eq!(prompts[0].arguments[0].name, "text");
        assert!(prompts[0].arguments[0].required);
    }

    #[tokio::test]
    async fn get_prompt_renders_expanded_messages_from_a_scripted_response() {
        let client = wired_client(|method| {
            assert_eq!(method, "prompts/get");
            serde_json::json!({
                "messages": [
                    {"role": "user", "content": {"type": "text", "text": "summarize this"}}
                ]
            })
        })
        .await;
        let text = client
            .get_prompt("summarize", Some(serde_json::json!({"text": "hi"})))
            .await
            .unwrap();
        assert!(text.contains("user"));
        assert!(text.contains("summarize this"));
    }
}
