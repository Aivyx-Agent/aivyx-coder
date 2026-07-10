use std::path::Path;
use std::sync::{Arc, Mutex};

use aivyx_sandbox::{ActionKind, PermissionRequest, PermissionTarget};
use aivyx_types::{Task, TaskStatus, ToolDefinition, ToolOutput};
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use crate::{Tool, ToolError, ToolExecutionContext};

/// A model that "plans" hundreds of tasks is looping, not planning — cap it
/// with an actionable error instead of letting the list (which is rendered
/// in the TUI and persisted every turn) grow without bound.
const MAX_TASKS: usize = 50;

/// Whole-list replacement (rather than incremental add/update/remove ops)
/// is deliberate: it needs no id bookkeeping across turns, is self-healing
/// after a malformed call, and is the shape small local models handle most
/// reliably — the same reasoning behind Claude Code's TodoWrite. Ids are
/// (re)assigned sequentially on every write and exist for display/
/// persistence, not as a protocol the model must track.
#[derive(Deserialize, JsonSchema)]
struct SetTasksArgs {
    /// The complete new task list, replacing the previous one. Include every task that is still relevant (not just changed ones), in order.
    tasks: Vec<TaskItem>,
}

#[derive(Deserialize, JsonSchema)]
struct TaskItem {
    /// Short description of the task.
    text: String,
    /// One of "pending", "in_progress", "done".
    status: TaskItemStatus,
}

/// Mirror of `aivyx_types::TaskStatus` so the schema derive lives here
/// instead of forcing a `schemars` dependency onto the types crate.
#[derive(Deserialize, JsonSchema, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum TaskItemStatus {
    Pending,
    InProgress,
    Done,
}

impl From<TaskItemStatus> for TaskStatus {
    fn from(status: TaskItemStatus) -> Self {
        match status {
            TaskItemStatus::Pending => TaskStatus::Pending,
            TaskItemStatus::InProgress => TaskStatus::InProgress,
            TaskItemStatus::Done => TaskStatus::Done,
        }
    }
}

/// Replaces the session task list. The list itself lives in the agent
/// (shared via `Arc` so the agent can render events from it and persist it
/// with the session); this tool is just the model-facing mutator.
pub struct SetTasksTool {
    tasks: Arc<Mutex<Vec<Task>>>,
}

impl SetTasksTool {
    pub fn new(tasks: Arc<Mutex<Vec<Task>>>) -> Self {
        Self { tasks }
    }
}

#[async_trait]
impl Tool for SetTasksTool {
    fn name(&self) -> &str {
        "set_tasks"
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name().to_string(),
            description: "Replace your task list with a new one. Use this to plan multi-step work \
                and track progress: write the full list of steps up front, then rewrite the list \
                as statuses change (pending, in_progress, done). Always send the complete list, \
                including unchanged tasks. The list is shown to the user and survives restarts. \
                Send an empty list to clear it."
                .to_string(),
            parameters_schema: serde_json::Value::from(schemars::schema_for!(SetTasksArgs)),
        }
    }

    fn permission_request(
        &self,
        arguments: &serde_json::Value,
        _cwd: &Path,
    ) -> Result<PermissionRequest, ToolError> {
        let args: SetTasksArgs = serde_json::from_value(arguments.clone())
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;

        Ok(PermissionRequest {
            tool_name: self.name().to_string(),
            action: ActionKind::Internal,
            target: PermissionTarget::Other("session task list".to_string()),
            arguments_preview: json!({ "tasks": args.tasks.len() }),
            preview: None,
        })
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        _ctx: &ToolExecutionContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: SetTasksArgs = serde_json::from_value(arguments)
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;

        if args.tasks.len() > MAX_TASKS {
            return Err(ToolError::InvalidArguments(format!(
                "{} tasks is too many (max {MAX_TASKS}) — plan at a coarser granularity",
                args.tasks.len()
            )));
        }

        let new_tasks: Vec<Task> = args
            .tasks
            .into_iter()
            .enumerate()
            .map(|(i, item)| Task {
                id: (i + 1) as u32,
                text: item.text,
                status: item.status.into(),
            })
            .collect();

        let summary = summarize(&new_tasks);
        *self.tasks.lock().unwrap() = new_tasks;
        Ok(ToolOutput::Ok(summary))
    }
}

/// Echo the accepted list back compactly so the model's context reflects
/// what the list now is without re-sending every description at length.
fn summarize(tasks: &[Task]) -> String {
    if tasks.is_empty() {
        return "task list cleared".to_string();
    }
    let done = tasks
        .iter()
        .filter(|t| t.status == TaskStatus::Done)
        .count();
    let mut out = format!("task list updated ({done}/{} done):", tasks.len());
    for task in tasks {
        let marker = match task.status {
            TaskStatus::Pending => "[ ]",
            TaskStatus::InProgress => "[~]",
            TaskStatus::Done => "[x]",
        };
        out.push_str(&format!("\n{marker} {}. {}", task.id, task.text));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> ToolExecutionContext {
        ToolExecutionContext {
            cwd: std::path::PathBuf::from("."),
            confiner: Arc::new(aivyx_sandbox::NoopConfiner),
            cancellation: tokio_util::sync::CancellationToken::new(),
        }
    }

    #[tokio::test]
    async fn replaces_the_shared_list_and_assigns_sequential_ids() {
        let shared = Arc::new(Mutex::new(vec![Task {
            id: 7,
            text: "stale".to_string(),
            status: TaskStatus::Done,
        }]));
        let tool = SetTasksTool::new(Arc::clone(&shared));

        let output = tool
            .execute(
                json!({ "tasks": [
                    { "text": "first", "status": "done" },
                    { "text": "second", "status": "in_progress" },
                    { "text": "third", "status": "pending" },
                ]}),
                &ctx(),
            )
            .await
            .unwrap();

        let tasks = shared.lock().unwrap().clone();
        assert_eq!(tasks.len(), 3);
        assert_eq!(tasks[0].id, 1);
        assert_eq!(tasks[1].status, TaskStatus::InProgress);
        assert_eq!(tasks[2].text, "third");

        let ToolOutput::Ok(text) = output else {
            panic!("expected Ok output")
        };
        assert!(text.contains("1/3 done"));
        assert!(text.contains("[~] 2. second"));
    }

    #[tokio::test]
    async fn an_empty_list_clears_it() {
        let shared = Arc::new(Mutex::new(vec![Task {
            id: 1,
            text: "old".to_string(),
            status: TaskStatus::Pending,
        }]));
        let tool = SetTasksTool::new(Arc::clone(&shared));

        let output = tool.execute(json!({ "tasks": [] }), &ctx()).await.unwrap();

        assert!(shared.lock().unwrap().is_empty());
        let ToolOutput::Ok(text) = output else {
            panic!("expected Ok output")
        };
        assert!(text.contains("cleared"));
    }

    #[tokio::test]
    async fn an_oversized_list_is_rejected_and_leaves_the_current_list_untouched() {
        let shared = Arc::new(Mutex::new(vec![Task {
            id: 1,
            text: "keep me".to_string(),
            status: TaskStatus::Pending,
        }]));
        let tool = SetTasksTool::new(Arc::clone(&shared));

        let huge: Vec<_> = (0..MAX_TASKS + 1)
            .map(|i| json!({ "text": format!("t{i}"), "status": "pending" }))
            .collect();
        let result = tool.execute(json!({ "tasks": huge }), &ctx()).await;

        assert!(matches!(result, Err(ToolError::InvalidArguments(_))));
        assert_eq!(shared.lock().unwrap()[0].text, "keep me");
    }

    #[tokio::test]
    async fn an_unknown_status_is_an_invalid_arguments_error() {
        let tool = SetTasksTool::new(Arc::default());
        let result = tool
            .execute(
                json!({ "tasks": [{ "text": "x", "status": "doing" }] }),
                &ctx(),
            )
            .await;
        assert!(matches!(result, Err(ToolError::InvalidArguments(_))));
    }

    #[test]
    fn permission_request_is_internal_and_never_touches_a_path_or_command() {
        let tool = SetTasksTool::new(Arc::default());
        let request = tool
            .permission_request(
                &json!({ "tasks": [{ "text": "x", "status": "pending" }] }),
                Path::new("."),
            )
            .unwrap();

        assert_eq!(request.action, ActionKind::Internal);
        assert!(matches!(request.target, PermissionTarget::Other(_)));
    }
}
