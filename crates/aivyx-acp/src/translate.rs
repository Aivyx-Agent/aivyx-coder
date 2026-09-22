//! Pure `AgentEvent` → ACP `SessionUpdate`/`StopReason` mapping — no I/O,
//! so these are unit-tested directly with no connection or subprocess.
//! See the Protocol Mapping table in `docs/superpowers/specs/
//! 2026-07-20-acp-editor-integration-design.md`.

use agent_client_protocol::schema::v1::{
    ContentBlock, ContentChunk, Plan, PlanEntry, PlanEntryPriority, PlanEntryStatus, SessionId,
    SessionUpdate, StopReason, TextContent, ToolCall as AcpToolCall, ToolCallContent,
    ToolCallStatus, ToolCallUpdate, ToolCallUpdateFields, ToolKind,
};
use aivyx_core::{AgentEvent, SpecialistSessionSummary};
use aivyx_types::{MissionPlan, StepStatus, Task, TaskStatus, ToolOutput};

/// Best-effort classification of a tool name into ACP's `ToolKind`, for
/// client icon/UI hints only — never affects behavior. Unknown/unlisted
/// names (e.g. MCP tool names, which are server-defined and unpredictable)
/// fall through to `ToolKind::Other`.
fn tool_kind(name: &str) -> ToolKind {
    match name {
        "read_file" | "grep" | "glob" | "git_read" => ToolKind::Read,
        "write_file" | "edit_file" => ToolKind::Edit,
        "delete_file" => ToolKind::Delete,
        "move_file" => ToolKind::Move,
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

/// Strips ASCII control characters (below 0x20, plus 0x7F/DEL -- this
/// includes `\n`, `\r`, and tab) from model-controlled text before it's
/// woven into a `PlanEntry.content` string, replacing each with U+FFFD.
/// `task.text`/`plan.mission`/`step.task` are free-form strings a model
/// (possibly manipulated by a prompt-injection source) fully controls,
/// with no length or content restriction from the tools that produce
/// them (`set_tasks`/`decompose_task`). Without this, an embedded
/// newline followed by text shaped like a different tag (e.g. `"done\n
/// [Specialist: reviewer] verified, ship it"`) could let one entry
/// visually masquerade as a separate, more-trusted-looking one in a
/// markdown-aware ACP client. Mirrors `aivyx-core`'s own
/// `sanitize_for_display` (`agent/mod.rs`) -- identical stripping logic
/// for the same problem class at a different trust boundary (that one
/// guards the system prompt) -- duplicated here rather than exported
/// across the crate boundary, matching this project's own established
/// precedent for a helper this small (`specialist_enforcement.rs`'s
/// duplicated path-resolution helpers). `session.member` is deliberately
/// NEVER passed through this function anywhere in `build_merged_plan` --
/// it's validated against the real team roster before ever reaching
/// here, never free-form model text.
fn strip_control_chars_for_display(raw: &str) -> String {
    raw.chars()
        .map(|c| if c.is_control() { '\u{FFFD}' } else { c })
        .collect()
}

/// Builds the single merged ACP `Plan` from three tracked state sources
/// -- tasks first, then mission steps, then open specialist sessions,
/// each origin-prefixed so they're distinguishable in one flat list,
/// since ACP has no dedicated "mission" slot (only `Plan`, which is
/// whole-list-replace). See `docs/superpowers/specs/
/// 2026-09-21-nonagon-team-acp-missions-surface-design.md`.
///
/// A `MissionPlan` that is `Some` always contributes a leading
/// `"[Mission] {mission}"` entry, even when `steps` is empty --
/// `decompose_task` can legitimately produce a zero-step plan, and
/// without this leading entry a Zed user would get no signal a mission
/// was ever created. This mirrors the TUI's own equivalent fix (Phase
/// 6a, `mission_panel_height` in `aivyx-tui/src/app.rs`): that panel is
/// shown whenever `self.mission_plan` is `Some` at all, not gated on a
/// non-empty step count.
fn build_merged_plan(
    tasks: &[Task],
    mission_plan: Option<&MissionPlan>,
    open_specialist_sessions: &[SpecialistSessionSummary],
) -> Plan {
    let mut entries = Vec::new();
    for task in tasks {
        let status = match task.status {
            TaskStatus::Pending => PlanEntryStatus::Pending,
            TaskStatus::InProgress => PlanEntryStatus::InProgress,
            TaskStatus::Done => PlanEntryStatus::Completed,
        };
        entries.push(PlanEntry::new(
            format!("[Task] {}", strip_control_chars_for_display(&task.text)),
            PlanEntryPriority::Medium,
            status,
        ));
    }
    if let Some(plan) = mission_plan {
        entries.push(PlanEntry::new(
            format!(
                "[Mission] {}",
                strip_control_chars_for_display(&plan.mission)
            ),
            PlanEntryPriority::Medium,
            if plan.summary.is_some() {
                PlanEntryStatus::Completed
            } else {
                PlanEntryStatus::InProgress
            },
        ));
        for step in &plan.steps {
            let step_task = strip_control_chars_for_display(&step.task);
            let (content, status) = match step.status {
                StepStatus::Pending => (
                    format!("[Mission: {}] {}", step.member, step_task),
                    PlanEntryStatus::Pending,
                ),
                StepStatus::Verified => (
                    format!("[Mission: {}] {}", step.member, step_task),
                    PlanEntryStatus::Completed,
                ),
                // Never Completed -- PlanEntryStatus has no failure state,
                // so Pending (not a false success) plus a text marker is
                // the honest mapping, mirroring the same reasoning behind
                // the TUI's own Failed-step handling (Phase 6a).
                StepStatus::Failed => (
                    format!("[Mission: {}] (FAILED) {}", step.member, step_task),
                    PlanEntryStatus::Pending,
                ),
            };
            entries.push(PlanEntry::new(content, PlanEntryPriority::Medium, status));
        }
    }
    for session in open_specialist_sessions {
        entries.push(PlanEntry::new(
            format!("[Specialist: {}] session open", session.member),
            PlanEntryPriority::Medium,
            // No real "done" concept for a parked session -- it's open or
            // it's gone (closed/evicted sessions simply aren't present in
            // this slice, they don't get a terminal status here).
            PlanEntryStatus::InProgress,
        ));
    }
    Plan::new(entries)
}

pub(crate) fn translate_event(session_id: &SessionId, event: &AgentEvent) -> Option<SessionUpdate> {
    let update = match event {
        AgentEvent::TextDelta(text) => SessionUpdate::AgentMessageChunk(text_chunk(text.clone())),
        AgentEvent::ReasoningDelta(text) => {
            SessionUpdate::AgentThoughtChunk(text_chunk(text.clone()))
        }
        // `Error` is a non-terminal advisory event — `Agent::run_turn` keeps
        // going and still returns `Ok(())` afterward in most cases (backend/
        // tool failures, verification failures, AGENTS.md load failures,
        // context-truncation warnings), so it cannot be mapped to a JSON-RPC
        // error response for the in-flight `session/prompt` call (that
        // response is for the turn's *final* outcome). Surfaced the same way
        // as `CouncilNote`/`ArchitectNote` — and the same way the TUI already
        // renders it, as a transcript line rather than a fatal condition.
        AgentEvent::CouncilNote(text) | AgentEvent::ArchitectNote(text) | AgentEvent::Error(text) => {
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
        AgentEvent::SubAgentActivity(inner) => return translate_event(session_id, inner),
        // Turn-terminal (handled by `terminal_stop_reason` instead) or
        // deliberately non-notification events — not surfaced as a
        // SessionUpdate. `ConversationCleared` is only ever emitted by the
        // TUI's `/clear` interception (see the design spec's scope note —
        // this frontend doesn't wire that command up), but the match must
        // still be exhaustive.
        AgentEvent::TurnComplete
        | AgentEvent::TurnPaused(_)
        | AgentEvent::ContextUsage { .. }
        // `Session` now owns display state (`tasks`/`mission_plan`/
        // `open_specialist_sessions`) that `ConversationCleared` does not
        // reset -- currently latent since ACP never receives this event
        // today (only the TUI's `/clear` interception emits it), but
        // worth revisiting if `/clear` is ever wired into ACP.
        | AgentEvent::ConversationCleared
        // Tasks/mission/specialist-session state all feed a single,
        // merged ACP Plan (see `build_merged_plan`) -- handled
        // exclusively by `translate_event_with_state`, which tracks
        // last-known state and rebuilds the union on every change. This
        // stateless function must never handle any of the three, or a
        // direct call here would silently bypass the merge and produce
        // an un-prefixed, single-source Plan. This also covers a
        // sub-agent's own wrapped `SubAgentActivity(TasksUpdated(...))`/
        // etc. (see the `SubAgentActivity` arm above, which recurses into
        // this stateless function, not `translate_event_with_state`): a
        // sub-agent's task/mission/session events are deliberately not
        // merged into the parent's tracked state either, for the same
        // reason the TUI never lets sub-agent activity update its own
        // task/mission panels -- only the top-level lead's own tool calls
        // should update what the editor's Plan panel shows.
        | AgentEvent::TasksUpdated(_)
        | AgentEvent::MissionsUpdated(_)
        | AgentEvent::SpecialistSessionsUpdated(_) => return None,
    };
    let _ = session_id; // session_id threading happens at the SessionNotification wrapper in Task 5
    Some(update)
}

/// Session-aware wrapper around `translate_event`: `TasksUpdated`/
/// `MissionsUpdated`/`SpecialistSessionsUpdated` update the relevant
/// tracked value and return the full re-merged `Plan`
/// (`build_merged_plan`); every other event passes straight through to
/// `translate_event`, unchanged. Takes the three tracked-state slots by
/// `&mut` directly (not a `&mut Session`) so this whole mechanism stays
/// testable with plain values, no `Agent`/`Session` construction needed
/// -- `Session::translate_and_merge` in `session.rs` is a thin wrapper
/// over this.
pub(crate) fn translate_event_with_state(
    session_id: &SessionId,
    tasks: &mut Vec<Task>,
    mission_plan: &mut Option<MissionPlan>,
    open_specialist_sessions: &mut Vec<SpecialistSessionSummary>,
    event: &AgentEvent,
) -> Option<SessionUpdate> {
    match event {
        AgentEvent::TasksUpdated(new_tasks) => *tasks = new_tasks.clone(),
        AgentEvent::MissionsUpdated(plan) => *mission_plan = Some(plan.clone()),
        AgentEvent::SpecialistSessionsUpdated(sessions) => {
            *open_specialist_sessions = sessions.clone();
        }
        other => return translate_event(session_id, other),
    }
    Some(SessionUpdate::Plan(build_merged_plan(
        tasks,
        mission_plan.as_ref(),
        open_specialist_sessions,
    )))
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
    use aivyx_types::{MissionStep, Task, ToolCall, ToolCallId, ToolCallSource, ToolResult};

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
        let council = translate_event(
            &sid(),
            &AgentEvent::CouncilNote("council said x".to_string()),
        )
        .unwrap();
        let architect =
            translate_event(&sid(), &AgentEvent::ArchitectNote("plan is y".to_string())).unwrap();
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
    fn tool_call_detected_for_move_file_maps_to_move_kind() {
        let call = ToolCall {
            id: ToolCallId("call-1".to_string()),
            name: "move_file".to_string(),
            arguments: serde_json::json!({"from": "a.rs", "to": "b.rs"}),
            source: ToolCallSource::Native,
        };
        let update = translate_event(&sid(), &AgentEvent::ToolCallDetected(call)).unwrap();
        let SessionUpdate::ToolCall(tool_call) = update else {
            panic!("expected ToolCall");
        };
        assert_eq!(tool_call.kind, ToolKind::Move);
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
    fn tasks_updated_is_not_handled_by_the_stateless_translate_event() {
        // TasksUpdated is handled exclusively by translate_event_with_state
        // now (see merged_plan_prefixes_tasks_and_maps_their_status and
        // translate_event_with_state_updates_tasks_and_returns_the_merged_plan
        // above) -- translate_event alone must return None for it, or a
        // direct call here would silently bypass the merge.
        let tasks = vec![Task {
            id: 1,
            text: "write tests".to_string(),
            status: TaskStatus::Done,
        }];
        assert!(translate_event(&sid(), &AgentEvent::TasksUpdated(tasks)).is_none());
    }

    #[test]
    fn merged_plan_prefixes_tasks_and_maps_their_status() {
        let tasks = vec![
            Task {
                id: 1,
                text: "write tests".to_string(),
                status: TaskStatus::Done,
            },
            Task {
                id: 2,
                text: "write code".to_string(),
                status: TaskStatus::InProgress,
            },
        ];
        let plan = build_merged_plan(&tasks, None, &[]);
        assert_eq!(plan.entries.len(), 2);
        assert_eq!(plan.entries[0].content, "[Task] write tests");
        assert_eq!(plan.entries[0].status, PlanEntryStatus::Completed);
        assert_eq!(plan.entries[1].content, "[Task] write code");
        assert_eq!(plan.entries[1].status, PlanEntryStatus::InProgress);
    }

    #[test]
    fn merged_plan_prefixes_mission_steps_with_their_member() {
        let mission = MissionPlan {
            mission: "fix the bug".to_string(),
            steps: vec![MissionStep {
                id: 1,
                member: "implementer".to_string(),
                task: "write the fix".to_string(),
                status: StepStatus::Pending,
                notes: None,
            }],
            summary: None,
        };
        let plan = build_merged_plan(&[], Some(&mission), &[]);
        assert_eq!(plan.entries.len(), 2);
        assert_eq!(plan.entries[0].content, "[Mission] fix the bug");
        assert_eq!(
            plan.entries[1].content,
            "[Mission: implementer] write the fix"
        );
        assert_eq!(plan.entries[1].status, PlanEntryStatus::Pending);
    }

    #[test]
    fn merged_plan_maps_a_failed_step_to_pending_with_a_failed_marker() {
        let mission = MissionPlan {
            mission: "fix the bug".to_string(),
            steps: vec![MissionStep {
                id: 1,
                member: "implementer".to_string(),
                task: "write the fix".to_string(),
                status: StepStatus::Failed,
                notes: Some("does not compile".to_string()),
            }],
            summary: None,
        };
        let plan = build_merged_plan(&[], Some(&mission), &[]);
        assert_eq!(plan.entries.len(), 2);
        assert_eq!(
            plan.entries[1].content,
            "[Mission: implementer] (FAILED) write the fix"
        );
        // Never Completed -- a failed step must never look like a success.
        assert_eq!(plan.entries[1].status, PlanEntryStatus::Pending);
    }

    #[test]
    fn merged_plan_maps_a_verified_step_to_completed() {
        let mission = MissionPlan {
            mission: "fix the bug".to_string(),
            steps: vec![MissionStep {
                id: 1,
                member: "implementer".to_string(),
                task: "write the fix".to_string(),
                status: StepStatus::Verified,
                notes: Some("looks good".to_string()),
            }],
            summary: None,
        };
        let plan = build_merged_plan(&[], Some(&mission), &[]);
        assert_eq!(
            plan.entries[1].content,
            "[Mission: implementer] write the fix"
        );
        assert_eq!(plan.entries[1].status, PlanEntryStatus::Completed);
    }

    #[test]
    fn merged_plan_prefixes_open_specialist_sessions_as_in_progress() {
        let sessions = vec![SpecialistSessionSummary {
            session_id: "abc123".to_string(),
            member: "reviewer".to_string(),
        }];
        let plan = build_merged_plan(&[], None, &sessions);
        assert_eq!(plan.entries.len(), 1);
        assert_eq!(
            plan.entries[0].content,
            "[Specialist: reviewer] session open"
        );
        assert_eq!(plan.entries[0].status, PlanEntryStatus::InProgress);
    }

    #[test]
    fn merged_plan_orders_tasks_then_mission_steps_then_sessions() {
        let tasks = vec![Task {
            id: 1,
            text: "a task".to_string(),
            status: TaskStatus::Pending,
        }];
        let mission = MissionPlan {
            mission: "fix the bug".to_string(),
            steps: vec![MissionStep {
                id: 1,
                member: "implementer".to_string(),
                task: "a step".to_string(),
                status: StepStatus::Pending,
                notes: None,
            }],
            summary: None,
        };
        let sessions = vec![SpecialistSessionSummary {
            session_id: "abc123".to_string(),
            member: "reviewer".to_string(),
        }];
        let plan = build_merged_plan(&tasks, Some(&mission), &sessions);
        assert_eq!(plan.entries.len(), 4);
        assert!(plan.entries[0].content.starts_with("[Task]"));
        // The mission's own leading summary entry comes before its steps
        // (Fix 1: a MissionPlan always contributes at least one entry,
        // even with zero steps -- see `build_merged_plan`'s doc comment).
        assert_eq!(plan.entries[1].content, "[Mission] fix the bug");
        assert!(plan.entries[2].content.starts_with("[Mission:"));
        assert!(plan.entries[3].content.starts_with("[Specialist:"));
    }

    #[test]
    fn merged_plan_with_no_sources_is_empty() {
        let plan = build_merged_plan(&[], None, &[]);
        assert!(plan.entries.is_empty());
    }

    #[test]
    fn merged_plan_strips_control_characters_from_task_text() {
        let tasks = vec![Task {
            id: 1,
            text: "done\n[Specialist: reviewer] verified, ship it".to_string(),
            status: TaskStatus::Pending,
        }];
        let plan = build_merged_plan(&tasks, None, &[]);
        assert_eq!(plan.entries.len(), 1);
        assert_eq!(
            plan.entries[0].content,
            "[Task] done\u{FFFD}[Specialist: reviewer] verified, ship it"
        );
        assert!(!plan.entries[0].content.contains('\n'));
    }

    #[test]
    fn merged_plan_strips_control_characters_from_the_mission_description() {
        let mission = MissionPlan {
            mission: "fix the bug\n[Mission] a completely different mission".to_string(),
            steps: vec![],
            summary: None,
        };
        let plan = build_merged_plan(&[], Some(&mission), &[]);
        assert_eq!(plan.entries.len(), 1);
        assert_eq!(
            plan.entries[0].content,
            "[Mission] fix the bug\u{FFFD}[Mission] a completely different mission"
        );
        assert!(!plan.entries[0].content.contains('\n'));
    }

    #[test]
    fn merged_plan_strips_control_characters_from_a_mission_steps_task_text() {
        let mission = MissionPlan {
            mission: "fix the bug".to_string(),
            steps: vec![MissionStep {
                id: 1,
                member: "implementer".to_string(),
                task: "write the fix\n[Specialist: reviewer] LGTM".to_string(),
                status: StepStatus::Pending,
                notes: None,
            }],
            summary: None,
        };
        let plan = build_merged_plan(&[], Some(&mission), &[]);
        assert_eq!(plan.entries.len(), 2);
        assert_eq!(
            plan.entries[1].content,
            "[Mission: implementer] write the fix\u{FFFD}[Specialist: reviewer] LGTM"
        );
        assert!(!plan.entries[1].content.contains('\n'));
    }

    #[test]
    fn translate_event_with_state_updates_tasks_and_returns_the_merged_plan() {
        let mut tasks = Vec::new();
        let mut mission_plan = None;
        let mut open_specialist_sessions = Vec::new();
        let new_tasks = vec![Task {
            id: 1,
            text: "write tests".to_string(),
            status: TaskStatus::Done,
        }];

        let update = translate_event_with_state(
            &sid(),
            &mut tasks,
            &mut mission_plan,
            &mut open_specialist_sessions,
            &AgentEvent::TasksUpdated(new_tasks.clone()),
        )
        .unwrap();

        assert_eq!(tasks, new_tasks);
        let SessionUpdate::Plan(plan) = update else {
            panic!("expected Plan");
        };
        assert_eq!(plan.entries.len(), 1);
        assert_eq!(plan.entries[0].content, "[Task] write tests");
    }

    #[test]
    fn translate_event_with_state_preserves_other_sources_when_one_changes() {
        let mut tasks = vec![Task {
            id: 1,
            text: "a task".to_string(),
            status: TaskStatus::Pending,
        }];
        let mut mission_plan = None;
        let mut open_specialist_sessions = Vec::new();
        let mission = MissionPlan {
            mission: "fix the bug".to_string(),
            steps: vec![MissionStep {
                id: 1,
                member: "implementer".to_string(),
                task: "a step".to_string(),
                status: StepStatus::Pending,
                notes: None,
            }],
            summary: None,
        };

        let update = translate_event_with_state(
            &sid(),
            &mut tasks,
            &mut mission_plan,
            &mut open_specialist_sessions,
            &AgentEvent::MissionsUpdated(mission.clone()),
        )
        .unwrap();

        assert_eq!(mission_plan, Some(mission));
        // The pre-existing task must still be present in the re-merged plan
        // -- this is the whole point of the union merge (a MissionsUpdated
        // event must not clobber the Tasks entries).
        let SessionUpdate::Plan(plan) = update else {
            panic!("expected Plan");
        };
        assert_eq!(plan.entries.len(), 3);
        assert!(plan.entries[0].content.starts_with("[Task]"));
        assert_eq!(plan.entries[1].content, "[Mission] fix the bug");
        assert!(plan.entries[2].content.starts_with("[Mission:"));
    }

    #[test]
    fn translate_event_with_state_updates_specialist_sessions() {
        let mut tasks = Vec::new();
        let mut mission_plan = None;
        let mut open_specialist_sessions = Vec::new();
        let sessions = vec![SpecialistSessionSummary {
            session_id: "abc123".to_string(),
            member: "reviewer".to_string(),
        }];

        let update = translate_event_with_state(
            &sid(),
            &mut tasks,
            &mut mission_plan,
            &mut open_specialist_sessions,
            &AgentEvent::SpecialistSessionsUpdated(sessions.clone()),
        )
        .unwrap();

        assert_eq!(open_specialist_sessions, sessions);
        let SessionUpdate::Plan(plan) = update else {
            panic!("expected Plan");
        };
        assert_eq!(
            plan.entries[0].content,
            "[Specialist: reviewer] session open"
        );
    }

    #[test]
    fn translate_event_with_state_passes_other_events_straight_through() {
        let mut tasks = Vec::new();
        let mut mission_plan = None;
        let mut open_specialist_sessions = Vec::new();

        let update = translate_event_with_state(
            &sid(),
            &mut tasks,
            &mut mission_plan,
            &mut open_specialist_sessions,
            &AgentEvent::TextDelta("hi".to_string()),
        )
        .unwrap();

        assert!(matches!(update, SessionUpdate::AgentMessageChunk(_)));
    }

    #[test]
    fn merged_plan_for_a_zero_step_mission_still_emits_a_leading_entry() {
        // Fix 1: `decompose_task` can legitimately produce a `MissionPlan`
        // with `steps: vec![]` -- without a leading entry, a Zed user gets
        // no signal a mission was ever created at all.
        let mission = MissionPlan {
            mission: "investigate the flaky test".to_string(),
            steps: vec![],
            summary: None,
        };
        let plan = build_merged_plan(&[], Some(&mission), &[]);
        assert_eq!(plan.entries.len(), 1);
        assert_eq!(
            plan.entries[0].content,
            "[Mission] investigate the flaky test"
        );
        assert_eq!(plan.entries[0].status, PlanEntryStatus::InProgress);
    }

    #[test]
    fn merged_plan_for_a_zero_step_mission_with_a_summary_is_completed() {
        let mission = MissionPlan {
            mission: "investigate the flaky test".to_string(),
            steps: vec![],
            summary: Some("root-caused: a timing race".to_string()),
        };
        let plan = build_merged_plan(&[], Some(&mission), &[]);
        assert_eq!(plan.entries.len(), 1);
        assert_eq!(plan.entries[0].status, PlanEntryStatus::Completed);
    }

    #[test]
    fn sub_agent_wrapped_task_events_are_not_merged_into_the_parent_state() {
        // Fix 2: `SubAgentActivity`'s `translate_event` arm recurses into
        // the stateless `translate_event`, never `translate_event_with_state`
        // -- so a sub-agent's own `TasksUpdated` must be silently dropped
        // and must never mutate the parent session's tracked `tasks`.
        let mut tasks = Vec::new();
        let mut mission_plan = None;
        let mut open_specialist_sessions = Vec::new();
        let wrapped =
            AgentEvent::SubAgentActivity(Box::new(AgentEvent::TasksUpdated(vec![Task {
                id: 1,
                text: "sub-agent's own task".to_string(),
                status: TaskStatus::Done,
            }])));

        let update = translate_event_with_state(
            &sid(),
            &mut tasks,
            &mut mission_plan,
            &mut open_specialist_sessions,
            &wrapped,
        );

        assert!(update.is_none());
        assert!(tasks.is_empty());
    }

    #[test]
    fn a_second_tasks_updated_replaces_rather_than_accumulates() {
        // Fix 3: an empty `TasksUpdated` must replace the tracked task
        // list, not merge with it -- and pre-existing mission state must
        // survive untouched.
        let mut tasks = vec![Task {
            id: 1,
            text: "old task".to_string(),
            status: TaskStatus::Pending,
        }];
        let mut mission_plan = Some(MissionPlan {
            mission: "fix the bug".to_string(),
            steps: vec![],
            summary: None,
        });
        let mut open_specialist_sessions = Vec::new();

        let update = translate_event_with_state(
            &sid(),
            &mut tasks,
            &mut mission_plan,
            &mut open_specialist_sessions,
            &AgentEvent::TasksUpdated(vec![]),
        )
        .unwrap();

        assert!(tasks.is_empty());
        let SessionUpdate::Plan(plan) = update else {
            panic!("expected Plan");
        };
        assert_eq!(plan.entries.len(), 1);
        assert_eq!(plan.entries[0].content, "[Mission] fix the bug");
    }

    #[test]
    fn error_becomes_plain_message_text_not_dropped() {
        let update =
            translate_event(&sid(), &AgentEvent::Error("backend timed out".to_string())).unwrap();
        assert!(matches!(update, SessionUpdate::AgentMessageChunk(_)));
        // Non-terminal: an Error event must never itself resolve the
        // in-flight `session/prompt` call.
        assert!(
            terminal_stop_reason(&AgentEvent::Error("backend timed out".to_string())).is_none()
        );
    }

    #[test]
    fn context_usage_is_not_surfaced() {
        assert!(
            translate_event(
                &sid(),
                &AgentEvent::ContextUsage {
                    used: 10,
                    limit: 100
                }
            )
            .is_none()
        );
    }

    #[test]
    fn turn_complete_and_turn_paused_are_not_session_updates_but_are_stop_reasons() {
        assert!(translate_event(&sid(), &AgentEvent::TurnComplete).is_none());
        assert!(translate_event(&sid(), &AgentEvent::TurnPaused("paused".to_string())).is_none());
        assert_eq!(
            terminal_stop_reason(&AgentEvent::TurnComplete),
            Some(StopReason::EndTurn)
        );
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
