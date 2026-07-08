use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: Vec<ContentBlock>,
    /// Set when `role == Role::Tool`: which call this message is the result of.
    pub tool_call_id: Option<ToolCallId>,
}

impl Message {
    pub fn text(role: Role, text: impl Into<String>) -> Self {
        Self {
            role,
            content: vec![ContentBlock::Text(text.into())],
            tool_call_id: None,
        }
    }

    /// Concatenation of every `ContentBlock::Text` in this message.
    pub fn text_content(&self) -> String {
        self.content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text(text) => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ContentBlock {
    Text(String),
    ToolCall(ToolCall),
    ToolResult(ToolResult),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ToolCallId(pub String);

impl std::fmt::Display for ToolCallId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Which parsing path produced a `ToolCall` — kept for logs/diagnostics, the
/// agent loop treats calls from either source identically once constructed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolCallSource {
    Native,
    TextFallback,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: ToolCallId,
    pub name: String,
    pub arguments: serde_json::Value,
    pub source: ToolCallSource,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub call_id: ToolCallId,
    pub output: ToolOutput,
}

/// `Denied` is distinct from `Error` so the model can see *why* nothing
/// happened and adjust (e.g. ask the user directly) instead of retrying.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ToolOutput {
    Ok(String),
    Error(String),
    Denied(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters_schema: serde_json::Value,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_content_concatenates_text_blocks_only() {
        let message = Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Text("hello ".to_string()),
                ContentBlock::ToolCall(ToolCall {
                    id: ToolCallId("1".to_string()),
                    name: "read_file".to_string(),
                    arguments: serde_json::json!({}),
                    source: ToolCallSource::Native,
                }),
                ContentBlock::Text("world".to_string()),
            ],
            tool_call_id: None,
        };
        assert_eq!(message.text_content(), "hello world");
    }
}
