//! Pure conversion between aivyx-coder's request/response types and
//! mistral.rs's. No async, no IO -- directly unit-testable without a
//! model load, mirroring aivyx's own convert.rs test pattern (construct
//! a bare `RequestBuilder::new()` and call the conversion functions
//! against it directly).

use aivyx_types::{ContentBlock, Message, Role, ToolCall, ToolCallId, ToolCallSource, ToolDefinition, ToolOutput, ToolResult};
use mistralrs::{RequestBuilder, TextMessageRole, ToolChoice as MistralRsToolChoice};

/// Add a single aivyx-coder `Message` to a mistral.rs `RequestBuilder`.
/// A message's `content` vec may contain plain text, an inline tool
/// call (on an `Assistant`-role message), or an inline tool result (on
/// a `Tool`-role message) -- aivyx-coder unifies these as `ContentBlock`
/// variants rather than separate per-role fields the way aivyx's own
/// `LlmMessage` enum does.
pub fn append_message_to_builder(mut builder: RequestBuilder, message: &Message) -> RequestBuilder {
    match message.role {
        Role::System => {
            let text = flatten_text_blocks(&message.content);
            builder.add_message(TextMessageRole::System, text)
        }
        Role::User => {
            let text = flatten_text_blocks(&message.content);
            builder.add_message(TextMessageRole::User, text)
        }
        Role::Assistant => {
            let text = flatten_text_blocks(&message.content);
            let tool_calls: Vec<_> = message
                .content
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::ToolCall(tc) => Some(tc),
                    _ => None,
                })
                .collect();
            if tool_calls.is_empty() {
                builder.add_message(TextMessageRole::Assistant, text)
            } else {
                let mistralrs_calls: Vec<_> = tool_calls
                    .iter()
                    .enumerate()
                    .map(|(idx, tc)| mistralrs::ToolCallResponse {
                        index: idx,
                        id: tc.id.0.clone(),
                        tp: mistralrs::ToolCallType::Function,
                        function: mistralrs::CalledFunction {
                            name: tc.name.clone(),
                            arguments: tc.arguments.to_string(),
                        },
                    })
                    .collect();
                builder.add_message_with_tool_call(TextMessageRole::Assistant, text, mistralrs_calls)
            }
        }
        Role::Tool => {
            // A Tool-role message's content holds exactly one
            // ContentBlock::ToolResult in every real call site today
            // (aivyx-coder's own agent loop constructs it that way) --
            // take the first one found, defensively ignoring anything
            // else rather than panicking on an unexpected shape.
            let result_text = message
                .content
                .iter()
                .find_map(|block| match block {
                    ContentBlock::ToolResult(tr) => Some(tool_output_to_text(&tr.output)),
                    _ => None,
                })
                .unwrap_or_default();
            let call_id = message
                .tool_call_id
                .as_ref()
                .map(|id| id.0.clone())
                .unwrap_or_default();
            builder = builder.add_tool_message(result_text, call_id);
            builder
        }
    }
}

fn flatten_text_blocks(content: &[ContentBlock]) -> String {
    content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(t) => Some(t.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn tool_output_to_text(output: &ToolOutput) -> String {
    match output {
        ToolOutput::Ok(s) => s.clone(),
        ToolOutput::Error(s) => format!("Error: {s}"),
        ToolOutput::Denied(s) => format!("Denied: {s}"),
    }
}

/// Map an aivyx-coder `ToolDefinition` to a mistral.rs `Tool`.
/// mistral.rs's `Function::parameters` is `HashMap<String, Value>` (the
/// top-level keys of the JSON-Schema object), not the wrapped object
/// aivyx-coder's own `parameters_schema` carries -- extract the keys.
pub fn to_mistralrs_tool(desc: &ToolDefinition) -> Result<mistralrs::Tool, String> {
    let parameters = match &desc.parameters_schema {
        serde_json::Value::Object(map) => {
            let hm: std::collections::HashMap<String, serde_json::Value> =
                map.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
            Some(hm)
        }
        _ => {
            return Err(format!(
                "tool `{}` has a non-object parameters_schema; mistralrs requires object",
                desc.name
            ));
        }
    };
    Ok(mistralrs::Tool {
        tp: mistralrs::ToolType::Function,
        function: mistralrs::Function {
            description: Some(desc.description.clone()),
            name: desc.name.clone(),
            parameters,
        },
    })
}

/// Apply aivyx-coder's tool catalog onto a mistral.rs builder. Empty
/// tool list leaves the builder unchanged.
pub fn apply_tools(
    builder: RequestBuilder,
    tools: &[ToolDefinition],
) -> Result<RequestBuilder, String> {
    if tools.is_empty() {
        return Ok(builder);
    }
    let converted: Result<Vec<_>, _> = tools.iter().map(to_mistralrs_tool).collect();
    Ok(builder.set_tools(converted?).set_tool_choice(MistralRsToolChoice::Auto))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn append_user_text_message() {
        let builder = RequestBuilder::new();
        let msg = Message {
            role: Role::User,
            content: vec![ContentBlock::Text("ping".to_string())],
            tool_call_id: None,
        };
        let _builder = append_message_to_builder(builder, &msg);
        // No public accessor on RequestBuilder to inspect state directly
        // (same caveat aivyx's own convert.rs tests document) -- this
        // proves the conversion compiles and runs on the happy path.
    }

    #[test]
    fn append_assistant_message_with_inline_tool_call_content_block() {
        let builder = RequestBuilder::new();
        let msg = Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Text("Let me check that.".to_string()),
                ContentBlock::ToolCall(ToolCall {
                    id: ToolCallId("call_abc".to_string()),
                    name: "fs.read".to_string(),
                    arguments: json!({"path": "/etc/hosts"}),
                    source: ToolCallSource::Native,
                }),
            ],
            tool_call_id: None,
        };
        let _builder = append_message_to_builder(builder, &msg);
    }

    #[test]
    fn append_tool_result_message() {
        let builder = RequestBuilder::new();
        let msg = Message {
            role: Role::Tool,
            content: vec![ContentBlock::ToolResult(ToolResult {
                call_id: ToolCallId("call_abc".to_string()),
                output: ToolOutput::Ok("127.0.0.1 localhost".to_string()),
            })],
            tool_call_id: Some(ToolCallId("call_abc".to_string())),
        };
        let _builder = append_message_to_builder(builder, &msg);
    }

    #[test]
    fn append_tool_result_message_with_denied_output_still_converts() {
        let builder = RequestBuilder::new();
        let msg = Message {
            role: Role::Tool,
            content: vec![ContentBlock::ToolResult(ToolResult {
                call_id: ToolCallId("call_xyz".to_string()),
                output: ToolOutput::Denied("operator declined".to_string()),
            })],
            tool_call_id: Some(ToolCallId("call_xyz".to_string())),
        };
        let _builder = append_message_to_builder(builder, &msg);
    }

    #[test]
    fn to_mistralrs_tool_forwards_schema_verbatim() {
        let desc = ToolDefinition {
            name: "fs.read".to_string(),
            description: "Read a file from disk".to_string(),
            parameters_schema: json!({
                "type": "object",
                "properties": {"path": {"type": "string"}},
                "required": ["path"],
            }),
        };
        let tool = to_mistralrs_tool(&desc).expect("ok");
        assert_eq!(tool.function.name, "fs.read");
        assert_eq!(tool.function.description.as_deref(), Some("Read a file from disk"));
        let params = tool.function.parameters.as_ref().expect("present");
        assert!(params.contains_key("type"));
        assert!(params.contains_key("properties"));
        assert!(params.contains_key("required"));
    }

    #[test]
    fn to_mistralrs_tool_rejects_non_object_schema() {
        let desc = ToolDefinition {
            name: "weird".to_string(),
            description: "schema isn't an object".to_string(),
            parameters_schema: json!("nope"),
        };
        let e = to_mistralrs_tool(&desc).expect_err("must error");
        assert!(e.contains("weird"), "{e}");
        assert!(e.contains("object"), "{e}");
    }

    #[test]
    fn apply_tools_empty_is_noop() {
        let builder = RequestBuilder::new();
        apply_tools(builder, &[]).expect("ok");
    }

    #[test]
    fn apply_tools_with_one_tool_succeeds() {
        let builder = RequestBuilder::new();
        let tools = vec![ToolDefinition {
            name: "fs.read".to_string(),
            description: "read".to_string(),
            parameters_schema: json!({"type": "object", "properties": {}}),
        }];
        apply_tools(builder, &tools).expect("ok");
    }
}
