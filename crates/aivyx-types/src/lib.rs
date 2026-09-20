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
    /// Synthesized by the agent itself, not the model — the enforced
    /// `[verification] command` auto-invoked after file edits, before a
    /// turn is allowed to end. See ROADMAP.md Phase 12 Part B.
    AutoVerification,
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

/// One item in the agent's session task list — an externalized scratchpad
/// of intent the model doesn't have to hold entirely in its context window.
/// Lives here (not in `aivyx-core`) because the `set_tasks` tool in
/// `aivyx-tools`, the session persistence in `aivyx-core`, and the task
/// panel in `aivyx-tui` all share it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Task {
    pub id: u32,
    pub text: String,
    pub status: TaskStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Pending,
    InProgress,
    Done,
}

/// A team mission's structured plan -- `decompose_task` creates one,
/// `verify_output` updates individual steps' status, `synthesize_results`
/// sets `summary`. Lives here (not `aivyx-core`) for the same reason
/// `Task` does -- shared, zero-logic data multiple crates need, no
/// `schemars`/`Tool` dependency required to define it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MissionPlan {
    pub mission: String,
    pub steps: Vec<MissionStep>,
    /// Set by `synthesize_results` -- `None` until the lead has
    /// explicitly synthesized a final deliverable.
    pub summary: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MissionStep {
    pub id: u32,
    /// A `TeamConfig` member's name (never the team's own lead --
    /// enforced at `decompose_task` construction time, not here).
    pub member: String,
    pub task: String,
    pub status: StepStatus,
    /// Set by `verify_output` alongside its verdict -- `None` until a
    /// step has been verified.
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    Pending,
    Verified,
    Failed,
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

#[cfg(test)]
mod mission_plan_tests {
    use super::*;

    #[test]
    fn mission_step_serializes_with_snake_case_status() {
        let step = MissionStep {
            id: 1,
            member: "implementer".to_string(),
            task: "write the fix".to_string(),
            status: StepStatus::Pending,
            notes: None,
        };
        let json = serde_json::to_value(&step).unwrap();
        assert_eq!(json["status"], "pending");
    }

    #[test]
    fn mission_plan_round_trips_through_json() {
        let plan = MissionPlan {
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
        let json = serde_json::to_string(&plan).unwrap();
        let back: MissionPlan = serde_json::from_str(&json).unwrap();
        assert_eq!(back.steps[0].status, StepStatus::Verified);
        assert_eq!(back.steps[0].notes, Some("looks good".to_string()));
    }
}
