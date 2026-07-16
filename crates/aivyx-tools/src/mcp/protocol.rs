//! Minimal MCP JSON-RPC result shapes — just enough of each method's
//! response to drive this client's tools/resources/prompts surface.
//! Mirrors `crate::lsp::protocol`'s minimal-parsing approach.

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct ToolInfo {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(rename = "inputSchema")]
    pub input_schema: serde_json::Value,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ToolsListResult {
    #[serde(default)]
    pub(crate) tools: Vec<ToolInfo>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ContentBlock {
    #[serde(rename = "type")]
    pub(crate) kind: String,
    #[serde(default)]
    pub(crate) text: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ToolCallResult {
    #[serde(default)]
    pub(crate) content: Vec<ContentBlock>,
    #[serde(default, rename = "isError")]
    pub(crate) is_error: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ResourceInfo {
    pub uri: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ResourcesListResult {
    #[serde(default)]
    pub(crate) resources: Vec<ResourceInfo>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ResourceContent {
    #[serde(default)]
    pub(crate) uri: String,
    #[serde(default)]
    pub(crate) text: Option<String>,
    #[serde(default)]
    pub(crate) blob: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ResourcesReadResult {
    #[serde(default)]
    pub(crate) contents: Vec<ResourceContent>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PromptArgumentInfo {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub required: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PromptInfo {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub arguments: Vec<PromptArgumentInfo>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct PromptsListResult {
    #[serde(default)]
    pub(crate) prompts: Vec<PromptInfo>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct PromptMessage {
    #[serde(default)]
    pub(crate) role: String,
    pub(crate) content: ContentBlock,
}

#[derive(Debug, Deserialize)]
pub(crate) struct PromptGetResult {
    #[serde(default)]
    pub(crate) messages: Vec<PromptMessage>,
}
