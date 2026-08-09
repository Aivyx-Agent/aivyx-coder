use std::path::Path;
use std::sync::Arc;

use aivyx_recall::Recall;
use aivyx_sandbox::{ActionKind, PermissionRequest, PermissionTarget};
use aivyx_types::{ToolDefinition, ToolOutput};
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use crate::memory_topic::resolve_topic;
use crate::{Tool, ToolError, ToolExecutionContext};

fn default_limit() -> usize {
    10
}

#[derive(Deserialize, JsonSchema)]
struct MemoryReadArgs {
    /// Exact topic to recall, e.g. "project:flaky-tests" or
    /// "global:editor-preference". Must start with "global:" or
    /// "project:".
    topic: String,
    /// Maximum number of entries to return, newest first.
    #[serde(default = "default_limit")]
    limit: usize,
}

pub struct MemoryReadTool {
    recall: Arc<dyn Recall>,
}

impl MemoryReadTool {
    pub fn new(recall: Arc<dyn Recall>) -> Self {
        Self { recall }
    }
}

#[async_trait]
impl Tool for MemoryReadTool {
    fn name(&self) -> &str {
        "memory_read"
    }

    fn mutates_outside_session(&self) -> bool {
        false
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name().to_string(),
            description: "Recall entries previously saved with memory_write under an exact \
                topic, newest first. Returns an empty list if nothing has been saved under that \
                topic. Topic must start with \"global:\" (across every project) or \"project:\" \
                (this project only)."
                .to_string(),
            parameters_schema: serde_json::Value::from(schemars::schema_for!(MemoryReadArgs)),
        }
    }

    fn permission_request(
        &self,
        arguments: &serde_json::Value,
        cwd: &Path,
    ) -> Result<PermissionRequest, ToolError> {
        let args: MemoryReadArgs = serde_json::from_value(arguments.clone())
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;
        let resolved = resolve_topic(cwd, &args.topic)?;

        Ok(PermissionRequest {
            tool_name: self.name().to_string(),
            action: ActionKind::Read,
            target: PermissionTarget::Other(resolved),
            arguments_preview: json!({ "topic": args.topic, "limit": args.limit }),
            preview: None,
            diff: None,
        })
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        ctx: &ToolExecutionContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: MemoryReadArgs = serde_json::from_value(arguments)
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;
        if args.limit == 0 {
            return Err(ToolError::InvalidArguments("limit must be > 0".to_string()));
        }
        let resolved = resolve_topic(&ctx.cwd, &args.topic)?;

        let entries = self
            .recall
            .get_recent(&resolved, args.limit)
            .await
            .map_err(|err| ToolError::ExecutionFailed(err.to_string()))?;

        if entries.is_empty() {
            return Ok(ToolOutput::Ok(format!("no memory saved under {:?}", args.topic)));
        }

        let rendered = entries
            .iter()
            .map(|e| format!("- {}", e.body))
            .collect::<Vec<_>>()
            .join("\n");
        Ok(ToolOutput::Ok(rendered))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // No shared ToolExecutionContext test helper exists in this crate —
    // every tool's own test module defines a local one. Mirrors
    // remember_preference.rs's `fn ctx(dir: &Path)` exactly.
    fn ctx(dir: &Path) -> ToolExecutionContext {
        ToolExecutionContext {
            cwd: dir.to_path_buf(),
            confiner: Arc::new(aivyx_sandbox::NoopConfiner),
            cancellation: tokio_util::sync::CancellationToken::new(),
        }
    }

    #[test]
    fn permission_request_targets_read_action_and_resolved_topic() {
        let tool = MemoryReadTool::new(Arc::new(aivyx_recall::InMemoryRecall::new()));
        let request = tool
            .permission_request(&json!({"topic": "global:editor"}), Path::new("/irrelevant"))
            .unwrap();

        assert_eq!(request.action, ActionKind::Read);
        assert_eq!(request.target, PermissionTarget::Other("global:editor".to_string()));
    }

    #[test]
    fn permission_request_rejects_a_bare_topic() {
        let tool = MemoryReadTool::new(Arc::new(aivyx_recall::InMemoryRecall::new()));
        let err = tool
            .permission_request(&json!({"topic": "editor"}), Path::new("/irrelevant"))
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArguments(_)));
    }

    #[tokio::test]
    async fn execute_returns_a_placeholder_message_for_an_unwritten_topic() {
        let tool = MemoryReadTool::new(Arc::new(aivyx_recall::InMemoryRecall::new()));
        let dir = tempfile::tempdir().unwrap();

        let output = tool
            .execute(json!({"topic": "global:never-written"}), &ctx(dir.path()))
            .await
            .unwrap();

        match output {
            ToolOutput::Ok(text) => assert!(text.contains("no memory saved")),
            other => panic!("expected Ok, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn execute_returns_previously_written_entries_newest_first() {
        let recall = Arc::new(aivyx_recall::InMemoryRecall::new());
        recall.put("global:notes", "first").await.unwrap();
        recall.put("global:notes", "second").await.unwrap();
        let tool = MemoryReadTool::new(recall);
        let dir = tempfile::tempdir().unwrap();

        let output = tool
            .execute(json!({"topic": "global:notes"}), &ctx(dir.path()))
            .await
            .unwrap();

        match output {
            ToolOutput::Ok(text) => {
                let second_pos = text.find("second").expect("should contain 'second'");
                let first_pos = text.find("first").expect("should contain 'first'");
                assert!(second_pos < first_pos, "newest entry should come first");
            }
            other => panic!("expected Ok, got {other:?}"),
        }
    }
}
