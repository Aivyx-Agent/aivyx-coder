//! Pure `AgentEvent` → ACP `SessionUpdate`/`StopReason` mapping — no I/O,
//! so these are unit-tested directly with no connection or subprocess.
//! See the Protocol Mapping table in `docs/superpowers/specs/
//! 2026-07-20-acp-editor-integration-design.md`.

use agent_client_protocol::schema::v1::{
    ContentBlock, ContentChunk, Plan, PlanEntry, PlanEntryPriority, PlanEntryStatus, SessionId,
    SessionUpdate, StopReason, TextContent, ToolCall as AcpToolCall, ToolCallContent,
    ToolCallStatus, ToolCallUpdate, ToolCallUpdateFields, ToolKind,
};
use aivyx_core::AgentEvent;
use aivyx_types::{ToolOutput, TaskStatus};

/// Best-effort classification of a tool name into ACP's `ToolKind`, for
/// client icon/UI hints only — never affects behavior. Unknown/unlisted
/// names (e.g. MCP tool names, which are server-defined and unpredictable)
/// fall through to `ToolKind::Other`.
fn tool_kind(name: &str) -> ToolKind {
    match name {
        "read_file" | "grep" | "glob" | "git_read" => ToolKind::Read,
        "write_file" | "edit_file" => ToolKind::Edit,
        "delete_file" => ToolKind::Delete,
        "run_command" | "run_shell" | "git_commit" | "git_branch" | "git_push" | "git_pr" => {
            ToolKind::Execute
        }
        "web_fetch" => ToolKind::Fetch,
        "web_search" => ToolKind::Search,
        "set_tasks" => ToolKind::Think,
        _ => ToolKind::Other,
    }
}

fn text_chunk(text: String) -> ContentChunk {
    ContentChunk::new(ContentBlock::Text(TextContent::new(text)))
}

pub(crate) fn translate_event(session_id: &SessionId, event: &AgentEvent) -> Option<SessionUpdate> {
    let update = match event {
        AgentEvent::TextDelta(text) => SessionUpdate::AgentMessageChunk(text_chunk(text.clone())),
        AgentEvent::ReasoningDelta(text) => {
            SessionUpdate::AgentThoughtChunk(text_chunk(text.clone()))
        }
        AgentEvent::CouncilNote(text) | AgentEvent::ArchitectNote(text) => {
            SessionUpdate::AgentMessageChunk(text_chunk(text.clone()))
        }
        AgentEvent::ToolCallDetected(call) => SessionUpdate::ToolCall(
            AcpToolCall::new(call.id.0.clone(), call.name.clone())
                .kind(tool_kind(&call.name))
                .status(ToolCallStatus::InProgress)
                .raw_input(call.arguments.clone()),
        ),
        AgentEvent::ToolResult(result) => {
            let (status, text) = match &result.output {
                ToolOutput::Ok(text) => (ToolCallStatus::Completed, text.clone()),
                ToolOutput::Error(text) | ToolOutput::Denied(text) => {
                    (ToolCallStatus::Failed, text.clone())
                }
            };
            // `text.clone().into()` targets `ToolCallContent` directly (via its
            // blanket `From<T: Into<ContentBlock>>` impl) — wrapping it in an
            // explicit `ToolCallContent::Content(...)` call would instead ask
            // `.into()` to produce the inner (non-public-constructible) `Content`
            // struct, which has no `From<String>` impl.
            let content: ToolCallContent = text.clone().into();
            SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(
                result.call_id.0.clone(),
                ToolCallUpdateFields::new()
                    .status(status)
                    .content(vec![content])
                    .raw_output(serde_json::Value::String(text)),
            ))
        }
        AgentEvent::TasksUpdated(tasks) => SessionUpdate::Plan(Plan::new(
            tasks
                .iter()
                .map(|task| {
                    let status = match task.status {
                        TaskStatus::Pending => PlanEntryStatus::Pending,
                        TaskStatus::InProgress => PlanEntryStatus::InProgress,
                        TaskStatus::Done => PlanEntryStatus::Completed,
                    };
                    PlanEntry::new(task.text.clone(), PlanEntryPriority::Medium, status)
                })
                .collect(),
        )),
        AgentEvent::SubAgentActivity(inner) => return translate_event(session_id, inner),
        // Turn-terminal and non-notification events — handled by
        // `terminal_stop_reason` instead, not surfaced as a SessionUpdate.
        AgentEvent::TurnComplete
        | AgentEvent::TurnPaused(_)
        | AgentEvent::Error(_)
        | AgentEvent::ContextUsage { .. } => return None,
    };
    let _ = session_id; // session_id threading happens at the SessionNotification wrapper in Task 5
    Some(update)
}

pub(crate) fn terminal_stop_reason(event: &AgentEvent) -> Option<StopReason> {
    match event {
        AgentEvent::TurnComplete => Some(StopReason::EndTurn),
        AgentEvent::TurnPaused(_) => Some(StopReason::MaxTurnRequests),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aivyx_types::{Task, ToolCall, ToolCallId, ToolCallSource, ToolResult};

    fn sid() -> SessionId {
        SessionId::new("sess-1")
    }

    #[test]
    fn text_delta_becomes_agent_message_chunk() {
        let update = translate_event(&sid(), &AgentEvent::TextDelta("hi".to_string())).unwrap();
        match update {
            SessionUpdate::AgentMessageChunk(chunk) => {
                assert_eq!(chunk.content, ContentBlock::Text(TextContent::new("hi")));
            }
            other => panic!("expected AgentMessageChunk, got {other:?}"),
        }
    }

    #[test]
    fn reasoning_delta_becomes_agent_thought_chunk() {
        let update =
            translate_event(&sid(), &AgentEvent::ReasoningDelta("thinking".to_string())).unwrap();
        assert!(matches!(update, SessionUpdate::AgentThoughtChunk(_)));
    }

    #[test]
    fn council_and_architect_notes_become_plain_message_text() {
        let council = translate_event(&sid(), &AgentEvent::CouncilNote("council said x".to_string())).unwrap();
        let architect = translate_event(&sid(), &AgentEvent::ArchitectNote("plan is y".to_string())).unwrap();
        assert!(matches!(council, SessionUpdate::AgentMessageChunk(_)));
        assert!(matches!(architect, SessionUpdate::AgentMessageChunk(_)));
    }

    #[test]
    fn tool_call_detected_carries_no_diff_only_metadata() {
        let call = ToolCall {
            id: ToolCallId("call-1".to_string()),
            name: "write_file".to_string(),
            arguments: serde_json::json!({"path": "a.rs"}),
            source: ToolCallSource::Native,
        };
        let update = translate_event(&sid(), &AgentEvent::ToolCallDetected(call)).unwrap();
        let SessionUpdate::ToolCall(tool_call) = update else {
            panic!("expected ToolCall");
        };
        assert_eq!(tool_call.title, "write_file");
        assert_eq!(tool_call.kind, ToolKind::Edit);
        assert!(tool_call.content.is_empty());
    }

    #[test]
    fn tool_result_ok_maps_to_completed_status() {
        let result = ToolResult {
            call_id: ToolCallId("call-1".to_string()),
            output: ToolOutput::Ok("wrote 3 lines".to_string()),
        };
        let update = translate_event(&sid(), &AgentEvent::ToolResult(result)).unwrap();
        let SessionUpdate::ToolCallUpdate(update) = update else {
            panic!("expected ToolCallUpdate");
        };
        assert_eq!(update.fields.status, Some(ToolCallStatus::Completed));
    }

    #[test]
    fn tool_result_denied_maps_to_failed_status() {
        let result = ToolResult {
            call_id: ToolCallId("call-1".to_string()),
            output: ToolOutput::Denied("user said no".to_string()),
        };
        let update = translate_event(&sid(), &AgentEvent::ToolResult(result)).unwrap();
        let SessionUpdate::ToolCallUpdate(update) = update else {
            panic!("expected ToolCallUpdate");
        };
        assert_eq!(update.fields.status, Some(ToolCallStatus::Failed));
    }

    #[test]
    fn tasks_updated_becomes_a_plan() {
        let tasks = vec![
            Task { id: 1, text: "write tests".to_string(), status: TaskStatus::Done },
            Task { id: 2, text: "write code".to_string(), status: TaskStatus::InProgress },
        ];
        let update = translate_event(&sid(), &AgentEvent::TasksUpdated(tasks)).unwrap();
        let SessionUpdate::Plan(plan) = update else {
            panic!("expected Plan");
        };
        assert_eq!(plan.entries.len(), 2);
        assert_eq!(plan.entries[0].status, PlanEntryStatus::Completed);
        assert_eq!(plan.entries[1].status, PlanEntryStatus::InProgress);
    }

    #[test]
    fn context_usage_is_not_surfaced() {
        assert!(translate_event(&sid(), &AgentEvent::ContextUsage { used: 10, limit: 100 }).is_none());
    }

    #[test]
    fn turn_complete_and_turn_paused_are_not_session_updates_but_are_stop_reasons() {
        assert!(translate_event(&sid(), &AgentEvent::TurnComplete).is_none());
        assert!(translate_event(&sid(), &AgentEvent::TurnPaused("paused".to_string())).is_none());
        assert_eq!(terminal_stop_reason(&AgentEvent::TurnComplete), Some(StopReason::EndTurn));
        assert_eq!(
            terminal_stop_reason(&AgentEvent::TurnPaused("paused".to_string())),
            Some(StopReason::MaxTurnRequests)
        );
    }

    #[test]
    fn sub_agent_activity_unwraps_to_its_inner_event() {
        let inner = Box::new(AgentEvent::TextDelta("from sub-agent".to_string()));
        let update = translate_event(&sid(), &AgentEvent::SubAgentActivity(inner)).unwrap();
        assert!(matches!(update, SessionUpdate::AgentMessageChunk(_)));
    }
}
