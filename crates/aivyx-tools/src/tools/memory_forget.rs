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

#[derive(Deserialize, JsonSchema)]
struct MemoryForgetArgs {
    /// Exact topic to forget entirely, e.g. "project:flaky-tests". Must
    /// start with "global:" or "project:".
    topic: String,
}

pub struct MemoryForgetTool {
    recall: Arc<dyn Recall>,
}

impl MemoryForgetTool {
    pub fn new(recall: Arc<dyn Recall>) -> Self {
        Self { recall }
    }
}

#[async_trait]
impl Tool for MemoryForgetTool {
    fn name(&self) -> &str {
        "memory_forget"
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name().to_string(),
            description: "Permanently delete every entry saved with memory_write under an \
                exact topic. Returns how many entries were deleted (0 if the topic was never \
                written). Topic must start with \"global:\" or \"project:\"."
                .to_string(),
            parameters_schema: serde_json::Value::from(schemars::schema_for!(MemoryForgetArgs)),
        }
    }

    fn permission_request(
        &self,
        arguments: &serde_json::Value,
        cwd: &Path,
    ) -> Result<PermissionRequest, ToolError> {
        let args: MemoryForgetArgs = serde_json::from_value(arguments.clone())
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;
        let resolved = resolve_topic(cwd, &args.topic)?;

        Ok(PermissionRequest {
            tool_name: self.name().to_string(),
            action: ActionKind::PersistentMemory,
            target: PermissionTarget::Other(resolved),
            arguments_preview: json!({ "topic": args.topic }),
            preview: None,
            diff: None,
        })
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        ctx: &ToolExecutionContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: MemoryForgetArgs = serde_json::from_value(arguments)
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;
        let resolved = resolve_topic(&ctx.cwd, &args.topic)?;

        let count = self
            .recall
            .forget(&resolved)
            .await
            .map_err(|err| ToolError::ExecutionFailed(err.to_string()))?;

        Ok(ToolOutput::Ok(format!(
            "deleted {count} entries under {:?}",
            args.topic
        )))
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

    #[test]
    fn permission_request_uses_persistent_memory_action_and_resolved_topic() {
        let tool = MemoryForgetTool::new(Arc::new(aivyx_recall::InMemoryRecall::new()));
        let request = tool
            .permission_request(&json!({"topic": "global:editor"}), Path::new("/irrelevant"))
            .unwrap();

        assert_eq!(request.action, ActionKind::PersistentMemory);
        assert_eq!(request.target, PermissionTarget::Other("global:editor".to_string()));
    }

    #[tokio::test]
    async fn execute_deletes_every_entry_and_reports_the_count() {
        let recall = Arc::new(aivyx_recall::InMemoryRecall::new());
        recall.put("global:notes", "one").await.unwrap();
        recall.put("global:notes", "two").await.unwrap();
        let tool = MemoryForgetTool::new(Arc::clone(&recall) as Arc<dyn Recall>);
        let dir = tempfile::tempdir().unwrap();

        let output = tool
            .execute(json!({"topic": "global:notes"}), &ctx(dir.path()))
            .await
            .unwrap();

        match output {
            ToolOutput::Ok(text) => assert!(text.contains("deleted 2 entries")),
            other => panic!("expected Ok, got {other:?}"),
        }
        assert_eq!(recall.get_recent("global:notes", 10).await.unwrap(), vec![]);
    }

    #[tokio::test]
    async fn execute_on_an_unwritten_topic_reports_zero_and_does_not_error() {
        let recall = Arc::new(aivyx_recall::InMemoryRecall::new());
        let tool = MemoryForgetTool::new(recall);
        let dir = tempfile::tempdir().unwrap();

        let output = tool
            .execute(json!({"topic": "global:never-written"}), &ctx(dir.path()))
            .await
            .unwrap();

        match output {
            ToolOutput::Ok(text) => assert!(text.contains("deleted 0 entries")),
            other => panic!("expected Ok, got {other:?}"),
        }
    }
}
