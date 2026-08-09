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
struct MemoryWriteArgs {
    /// Topic to file this under, e.g. "project:flaky-tests" or
    /// "global:editor-preference". Must start with "global:" or
    /// "project:".
    topic: String,
    /// The fact or note to remember.
    body: String,
}

pub struct MemoryWriteTool {
    recall: Arc<dyn Recall>,
}

impl MemoryWriteTool {
    pub fn new(recall: Arc<dyn Recall>) -> Self {
        Self { recall }
    }
}

#[async_trait]
impl Tool for MemoryWriteTool {
    fn name(&self) -> &str {
        "memory_write"
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name().to_string(),
            description: "Persist a small fact or note for future recall via memory_read — \
                not shown to you automatically. Use \"project:<name>\" for something specific \
                to this project (e.g. \"project:flaky-tests\"), or \"global:<name>\" for \
                something true across every project (e.g. \"global:editor-preference\"). For \
                standing instructions that should always be active, use remember_preference \
                instead."
                .to_string(),
            parameters_schema: serde_json::Value::from(schemars::schema_for!(MemoryWriteArgs)),
        }
    }

    fn permission_request(
        &self,
        arguments: &serde_json::Value,
        cwd: &Path,
    ) -> Result<PermissionRequest, ToolError> {
        let args: MemoryWriteArgs = serde_json::from_value(arguments.clone())
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;
        let resolved = resolve_topic(cwd, &args.topic)?;

        Ok(PermissionRequest {
            tool_name: self.name().to_string(),
            action: ActionKind::PersistentMemory,
            // Tool-qualified, not the bare resolved topic: `memory_forget`
            // uses the same ActionKind::PersistentMemory + resolved-topic
            // shape for the same topic, and PermissionKey::from_request
            // keys an `Other` target on `{action, description}` only — a
            // bare topic here would let an Always-Allow granted for a
            // memory_write on this topic silently satisfy a later
            // memory_forget on the same topic from the cache, with no
            // prompt for the irreversible delete.
            target: PermissionTarget::Other(format!("memory_write {resolved}")),
            arguments_preview: json!({ "topic": args.topic }),
            preview: Some(args.body.clone()),
            diff: None,
        })
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        ctx: &ToolExecutionContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: MemoryWriteArgs = serde_json::from_value(arguments)
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;
        let resolved = resolve_topic(&ctx.cwd, &args.topic)?;

        self.recall
            .put(&resolved, &args.body)
            .await
            .map_err(|err| ToolError::ExecutionFailed(err.to_string()))?;

        Ok(ToolOutput::Ok(format!("remembered under {:?}", args.topic)))
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
        let tool = MemoryWriteTool::new(Arc::new(aivyx_recall::InMemoryRecall::new()));
        let request = tool
            .permission_request(
                &json!({"topic": "global:editor", "body": "prefers tabs"}),
                Path::new("/irrelevant"),
            )
            .unwrap();

        assert_eq!(request.action, ActionKind::PersistentMemory);
        assert_eq!(
            request.target,
            PermissionTarget::Other("memory_write global:editor".to_string())
        );
        assert_eq!(request.preview, Some("prefers tabs".to_string()));
    }

    #[test]
    fn permission_request_rejects_a_bare_topic() {
        let tool = MemoryWriteTool::new(Arc::new(aivyx_recall::InMemoryRecall::new()));
        let err = tool
            .permission_request(
                &json!({"topic": "editor", "body": "x"}),
                Path::new("/irrelevant"),
            )
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArguments(_)));
    }

    #[tokio::test]
    async fn execute_persists_the_entry_so_it_can_be_read_back() {
        let recall = Arc::new(aivyx_recall::InMemoryRecall::new());
        let tool = MemoryWriteTool::new(Arc::clone(&recall) as Arc<dyn Recall>);
        let dir = tempfile::tempdir().unwrap();

        tool.execute(
            json!({"topic": "global:editor", "body": "prefers tabs"}),
            &ctx(dir.path()),
        )
        .await
        .unwrap();

        let entries = recall.get_recent("global:editor", 10).await.unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].body, "prefers tabs");
    }

    #[tokio::test]
    async fn execute_scopes_a_project_topic_under_the_cwd_hash() {
        let recall = Arc::new(aivyx_recall::InMemoryRecall::new());
        let tool = MemoryWriteTool::new(Arc::clone(&recall) as Arc<dyn Recall>);
        let dir = tempfile::tempdir().unwrap();

        tool.execute(
            json!({"topic": "project:flaky-tests", "body": "cargo test -p foo is flaky"}),
            &ctx(dir.path()),
        )
        .await
        .unwrap();

        // The literal, unscoped topic was never written.
        assert_eq!(
            recall.get_recent("project:flaky-tests", 10).await.unwrap(),
            vec![]
        );
    }
}
