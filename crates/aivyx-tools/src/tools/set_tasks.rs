use std::path::Path;
use std::sync::{Arc, Mutex};

use aivyx_sandbox::{ActionKind, PermissionRequest, PermissionTarget, PlanMode};
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
    /// Read on every call (B2): while plan mode is active, nothing has
    /// actually been done yet (the mode is read-only), so a task marked
    /// anything but `pending` here would be the model falsely reporting
    /// work as complete. The gate itself auto-allows this tool's
    /// `ActionKind::Internal` even in plan mode (see this struct's own doc
    /// comment) — this check is the actual enforcement.
    plan_mode: PlanMode,
}

impl SetTasksTool {
    pub fn new(tasks: Arc<Mutex<Vec<Task>>>, plan_mode: PlanMode) -> Self {
        Self { tasks, plan_mode }
    }
}

#[async_trait]
impl Tool for SetTasksTool {
    fn name(&self) -> &str {
        "set_tasks"
    }

    // Session-internal only — deliberately available in plan mode, where
    // the task list is exactly how the model records the plan for review.
    fn mutates_outside_session(&self) -> bool {
        false
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
            diff: None,
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

        if self.plan_mode.active()
            && args
                .tasks
                .iter()
                .any(|item| !matches!(item.status, TaskItemStatus::Pending))
        {
            return Err(ToolError::ExecutionFailed(
                "Plan mode is read-only: nothing can be done yet, so every task must stay \
                 pending until the user approves the plan (Ctrl+P)."
                    .to_string(),
            ));
        }

        let new_tasks: Vec<Task> = args
            .tasks
            .into_iter()
            .enumerate()
            .map(|(i, item)| Task {
                id: (i + 1) as u32,
                text: strip_leading_number(&item.text),
                status: item.status.into(),
            })
            .collect();

        let summary = summarize(&new_tasks);
        *self.tasks.lock().unwrap() = new_tasks;
        Ok(ToolOutput::Ok(summary))
    }
}

/// Strips a model-supplied leading "1. "/"2) " ordinal from task text (U1):
/// models often number their own list items even though `TaskItem`s are
/// already ordered and rendered with their own `id` — left alone, that
/// shows up doubled as "1. 1. do the thing". Strips at most one such
/// prefix, so a task whose real text legitimately starts with a number
/// (e.g. "2024 tax prep") is untouched unless it's immediately followed by
/// `.`/`)` and whitespace.
fn strip_leading_number(text: &str) -> String {
    let trimmed = text.trim_start();
    let digits_end = trimmed.find(|c: char| !c.is_ascii_digit()).unwrap_or(0);
    if digits_end == 0 {
        return text.to_string();
    }
    let rest = &trimmed[digits_end..];
    let Some(after_marker) = rest
        .strip_prefix('.')
        .or_else(|| rest.strip_prefix(')'))
    else {
        return text.to_string();
    };
    let after_ws = after_marker.trim_start_matches(char::is_whitespace);
    if after_ws.len() == after_marker.len() {
        // No whitespace followed the marker (e.g. "1.5x speed") — not a
        // numbering prefix, leave it alone.
        return text.to_string();
    }
    after_ws.to_string()
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
        let tool = SetTasksTool::new(Arc::clone(&shared), PlanMode::new());

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
        let tool = SetTasksTool::new(Arc::clone(&shared), PlanMode::new());

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
        let tool = SetTasksTool::new(Arc::clone(&shared), PlanMode::new());

        let huge: Vec<_> = (0..MAX_TASKS + 1)
            .map(|i| json!({ "text": format!("t{i}"), "status": "pending" }))
            .collect();
        let result = tool.execute(json!({ "tasks": huge }), &ctx()).await;

        assert!(matches!(result, Err(ToolError::InvalidArguments(_))));
        assert_eq!(shared.lock().unwrap()[0].text, "keep me");
    }

    #[tokio::test]
    async fn an_unknown_status_is_an_invalid_arguments_error() {
        let tool = SetTasksTool::new(Arc::default(), PlanMode::new());
        let result = tool
            .execute(
                json!({ "tasks": [{ "text": "x", "status": "doing" }] }),
                &ctx(),
            )
            .await;
        assert!(matches!(result, Err(ToolError::InvalidArguments(_))));
    }

    #[tokio::test]
    async fn plan_mode_rejects_a_non_pending_task_and_leaves_the_list_untouched() {
        let shared = Arc::new(Mutex::new(vec![Task {
            id: 1,
            text: "keep me".to_string(),
            status: TaskStatus::Pending,
        }]));
        let plan_mode = PlanMode::new();
        plan_mode.set_active(true);
        let tool = SetTasksTool::new(Arc::clone(&shared), plan_mode);

        let result = tool
            .execute(
                json!({ "tasks": [
                    { "text": "first", "status": "done" },
                    { "text": "second", "status": "pending" },
                ]}),
                &ctx(),
            )
            .await;

        let Err(ToolError::ExecutionFailed(message)) = result else {
            panic!("expected ExecutionFailed, got {result:?}");
        };
        assert!(message.contains("Plan mode is read-only"));
        assert!(message.contains("Ctrl+P"));
        // Rejected outright — the existing list is untouched.
        assert_eq!(shared.lock().unwrap()[0].text, "keep me");
    }

    #[tokio::test]
    async fn plan_mode_still_allows_an_all_pending_list() {
        let plan_mode = PlanMode::new();
        plan_mode.set_active(true);
        let tool = SetTasksTool::new(Arc::default(), plan_mode);

        let result = tool
            .execute(
                json!({ "tasks": [
                    { "text": "first", "status": "pending" },
                    { "text": "second", "status": "pending" },
                ]}),
                &ctx(),
            )
            .await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn act_mode_still_allows_marking_tasks_done() {
        // Outside plan mode, today's behaviour (any status, any turn) is
        // unchanged.
        let tool = SetTasksTool::new(Arc::default(), PlanMode::new());

        let result = tool
            .execute(
                json!({ "tasks": [{ "text": "first", "status": "done" }] }),
                &ctx(),
            )
            .await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn a_leading_model_supplied_ordinal_is_stripped_from_task_text() {
        let shared: Arc<Mutex<Vec<Task>>> = Arc::default();
        let tool = SetTasksTool::new(Arc::clone(&shared), PlanMode::new());

        tool.execute(
            json!({ "tasks": [
                { "text": "1. do the thing", "status": "pending" },
                { "text": "2) another one", "status": "pending" },
                { "text": "no number here", "status": "pending" },
                { "text": "2024 tax prep", "status": "pending" },
            ]}),
            &ctx(),
        )
        .await
        .unwrap();

        let tasks = shared.lock().unwrap().clone();
        assert_eq!(tasks[0].text, "do the thing");
        assert_eq!(tasks[1].text, "another one");
        assert_eq!(tasks[2].text, "no number here");
        // Not an ordinal prefix (no "." / ")" + whitespace after the
        // digits) — left alone.
        assert_eq!(tasks[3].text, "2024 tax prep");
    }

    #[test]
    fn strip_leading_number_treats_any_unicode_whitespace_as_the_separator() {
        // Fix round 1 (Minor): the brief says `\s+`, not just ASCII space/
        // tab -- a non-breaking space (U+00A0, which IS in Unicode's
        // White_Space property) and a newline must both work as the
        // separator after the marker.
        assert_eq!(strip_leading_number("1.\u{a0}do the thing"), "do the thing");
        assert_eq!(strip_leading_number("2.\ndo the thing"), "do the thing");
        // Still untouched: no "." / ")" + whitespace after the digits.
        assert_eq!(strip_leading_number("2024 roadmap"), "2024 roadmap");
        assert_eq!(strip_leading_number("1.5x speedup"), "1.5x speedup");
    }

    #[test]
    fn permission_request_is_internal_and_never_touches_a_path_or_command() {
        let tool = SetTasksTool::new(Arc::default(), PlanMode::new());
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
