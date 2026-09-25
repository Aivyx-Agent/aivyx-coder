//! Three lightweight, state-recording tools for Nonagon-style team
//! missions: `decompose_task`, `verify_output`, `synthesize_results`.
//! None of them call an LLM or construct an `Agent` -- each validates
//! and records structured state against a shared `MissionPlan`, the
//! same complexity class as `aivyx-tools`' own `set_tasks` tool, not
//! `delegate_task`'s/`delegate_to_specialist`'s. See
//! `docs/superpowers/specs/2026-09-20-nonagon-team-mission-structure-design.md`.
//!
//! Deliberately loosely coupled to `delegate_to_specialist`: nothing
//! here automatically triggers a delegation call or automatically
//! advances a step's status when one returns -- the lead drives
//! everything itself, calling `verify_output` when it judges a step
//! done. No DAG, no auto-orchestration this phase.

use std::sync::{Arc, Mutex};

use aivyx_sandbox::{ActionKind, PermissionRequest, PermissionTarget};
use aivyx_team::TeamConfig;
use aivyx_tools::{Tool, ToolError, ToolExecutionContext};
use aivyx_types::{MissionPlan, MissionStep, StepStatus, ToolDefinition, ToolOutput};
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use tokio::sync::mpsc::UnboundedSender;

use crate::agent::AgentEvent;
use crate::delegate_to_specialist::{specialist_names, specialists};

/// Shared by all three tools -- unlike `DelegateToSpecialistConfig`,
/// none of these need an `LlmBackend`/`PermissionGate`/checkpointer/etc.,
/// since none of them construct a sub-agent.
#[derive(Clone)]
pub struct MissionToolsConfig {
    pub team: TeamConfig,
    pub plan: Arc<Mutex<MissionPlan>>,
    pub events_tx: UnboundedSender<AgentEvent>,
}

#[derive(Deserialize, JsonSchema)]
struct StepArg {
    /// A `TeamConfig` member's name to delegate this step to -- must not
    /// be the team's own lead.
    member: String,
    /// A complete, self-contained description of the step's task.
    task: String,
}

#[derive(Deserialize, JsonSchema)]
struct DecomposeTaskArgs {
    /// A short description of the overall mission being decomposed.
    mission: String,
    /// The ordered list of steps, each delegated to one team specialist.
    steps: Vec<StepArg>,
}

pub struct DecomposeTaskTool {
    config: MissionToolsConfig,
}

impl DecomposeTaskTool {
    pub fn new(config: MissionToolsConfig) -> Self {
        Self { config }
    }
}

#[async_trait]
impl Tool for DecomposeTaskTool {
    fn name(&self) -> &str {
        "decompose_task"
    }

    fn mutates_outside_session(&self) -> bool {
        false
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name().to_string(),
            description: format!(
                "Decompose a mission into steps, each delegated to one team specialist. Call \
                this once at the start of a team mission, before delegating anything. Returns \
                the stored plan with step numbers you'll use with verify_output. Available \
                specialists: {}. This is separate from set_tasks -- keep using set_tasks for \
                your own visible task list and progress tracking (including autonomous mode's \
                stop signal); decompose_task only records which specialist owns which step.",
                crate::delegate_to_specialist::specialist_roster_description(&self.config.team)
            ),
            parameters_schema: serde_json::Value::from(schemars::schema_for!(DecomposeTaskArgs)),
        }
    }

    fn permission_request(
        &self,
        _arguments: &serde_json::Value,
        _cwd: &std::path::Path,
    ) -> Result<PermissionRequest, ToolError> {
        Ok(PermissionRequest {
            tool_name: self.name().to_string(),
            action: ActionKind::Internal,
            target: PermissionTarget::Other("mission plan".to_string()),
            arguments_preview: serde_json::json!({}),
            preview: None,
            diff: None,
        })
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        _ctx: &ToolExecutionContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: DecomposeTaskArgs = serde_json::from_value(arguments)
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;

        for step in &args.steps {
            if step.member == self.config.team.lead {
                return Ok(ToolOutput::Error(format!(
                    "cannot delegate a step to the team's own lead ({:?}) -- delegate to one \
                    of the other specialists instead: {}",
                    step.member,
                    specialist_names(&self.config.team)
                )));
            }
            if !specialists(&self.config.team).any(|m| m.name == step.member) {
                return Ok(ToolOutput::Error(format!(
                    "unknown team member: {:?} -- valid specialists: {}",
                    step.member,
                    specialist_names(&self.config.team)
                )));
            }
        }

        let steps: Vec<MissionStep> = args
            .steps
            .into_iter()
            .enumerate()
            .map(|(i, s)| MissionStep {
                id: (i + 1) as u32,
                member: s.member,
                task: s.task,
                status: StepStatus::Pending,
                notes: None,
            })
            .collect();

        let summary = summarize_plan(&args.mission, &steps);
        let updated_plan = {
            let mut plan = self.config.plan.lock().unwrap();
            *plan = MissionPlan {
                mission: args.mission,
                steps,
                summary: None,
            };
            plan.clone()
        };
        let _ = self
            .config
            .events_tx
            .send(AgentEvent::MissionsUpdated(updated_plan));
        Ok(ToolOutput::Ok(summary))
    }
}

fn summarize_plan(mission: &str, steps: &[MissionStep]) -> String {
    let mut out = format!("mission plan stored: {mission:?}");
    for step in steps {
        out.push_str(&format!("\n{}. [{}] {}", step.id, step.member, step.task));
    }
    out
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum Verdict {
    Pass,
    Fail,
}

#[derive(Deserialize, JsonSchema)]
struct VerifyOutputArgs {
    /// The step number returned by `decompose_task`.
    step_id: u32,
    /// Whether the step's delegated output passes review.
    verdict: Verdict,
    /// Your reasoning for the verdict.
    notes: String,
}

pub struct VerifyOutputTool {
    config: MissionToolsConfig,
}

impl VerifyOutputTool {
    pub fn new(config: MissionToolsConfig) -> Self {
        Self { config }
    }
}

#[async_trait]
impl Tool for VerifyOutputTool {
    fn name(&self) -> &str {
        "verify_output"
    }

    fn mutates_outside_session(&self) -> bool {
        false
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name().to_string(),
            description: "Record your verification judgment (pass or fail, with notes) for one \
                step of the current mission plan, after reviewing a specialist's delegated \
                output. Use the step number from decompose_task's own response."
                .to_string(),
            parameters_schema: serde_json::Value::from(schemars::schema_for!(VerifyOutputArgs)),
        }
    }

    fn permission_request(
        &self,
        _arguments: &serde_json::Value,
        _cwd: &std::path::Path,
    ) -> Result<PermissionRequest, ToolError> {
        Ok(PermissionRequest {
            tool_name: self.name().to_string(),
            action: ActionKind::Internal,
            target: PermissionTarget::Other("mission plan".to_string()),
            arguments_preview: serde_json::json!({}),
            preview: None,
            diff: None,
        })
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        _ctx: &ToolExecutionContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: VerifyOutputArgs = serde_json::from_value(arguments)
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;

        let mut plan = self.config.plan.lock().unwrap();
        let Some(step) = plan.steps.iter_mut().find(|s| s.id == args.step_id) else {
            return Ok(ToolOutput::Error(format!(
                "unknown step_id: {} -- no such step in the current mission plan",
                args.step_id
            )));
        };
        step.status = match args.verdict {
            Verdict::Pass => StepStatus::Verified,
            Verdict::Fail => StepStatus::Failed,
        };
        step.notes = Some(args.notes.clone());
        let message = format!(
            "step {} ({}: {}) marked {:?}: {}",
            step.id, step.member, step.task, step.status, args.notes
        );
        let updated_plan = plan.clone();
        drop(plan);
        let _ = self
            .config
            .events_tx
            .send(AgentEvent::MissionsUpdated(updated_plan));
        Ok(ToolOutput::Ok(message))
    }
}

#[derive(Deserialize, JsonSchema)]
struct SynthesizeResultsArgs {
    /// The final synthesized deliverable for the whole mission.
    summary: String,
}

pub struct SynthesizeResultsTool {
    config: MissionToolsConfig,
}

impl SynthesizeResultsTool {
    pub fn new(config: MissionToolsConfig) -> Self {
        Self { config }
    }
}

#[async_trait]
impl Tool for SynthesizeResultsTool {
    fn name(&self) -> &str {
        "synthesize_results"
    }

    fn mutates_outside_session(&self) -> bool {
        false
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name().to_string(),
            description: "Record the final synthesized deliverable for the current mission, \
                once every step has been delegated and verified. This is a structured \
                checkpoint, not your final answer to the user -- still write your own summary \
                as your next response after calling this. This does not mark set_tasks' own \
                task list done -- that's tracked separately and still needs its own update."
                .to_string(),
            parameters_schema: serde_json::Value::from(schemars::schema_for!(
                SynthesizeResultsArgs
            )),
        }
    }

    fn permission_request(
        &self,
        _arguments: &serde_json::Value,
        _cwd: &std::path::Path,
    ) -> Result<PermissionRequest, ToolError> {
        Ok(PermissionRequest {
            tool_name: self.name().to_string(),
            action: ActionKind::Internal,
            target: PermissionTarget::Other("mission plan".to_string()),
            arguments_preview: serde_json::json!({}),
            preview: None,
            diff: None,
        })
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        _ctx: &ToolExecutionContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: SynthesizeResultsArgs = serde_json::from_value(arguments)
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;
        let char_count = args.summary.chars().count();
        let line_count = args.summary.lines().count();
        let updated_plan = {
            let mut plan = self.config.plan.lock().unwrap();
            plan.summary = Some(args.summary);
            plan.clone()
        };
        let _ = self
            .config
            .events_tx
            .send(AgentEvent::MissionsUpdated(updated_plan));
        Ok(ToolOutput::Ok(format!(
            "mission synthesis recorded ({char_count} chars, {line_count} line(s))"
        )))
    }
}

#[cfg(test)]
mod mission_tools_tests {
    use super::*;
    use aivyx_team::{TeamConfig, TeamMember};
    use aivyx_types::StepStatus;

    fn simple_team() -> TeamConfig {
        TeamConfig {
            lead: "coordinator".to_string(),
            members: vec![
                TeamMember {
                    task: None,
                    name: "coordinator".to_string(),
                    role: "Lead".to_string(),
                    persona: "You delegate.".to_string(),
                    tool_allowlist: vec![],
                    extra_deny_paths: vec![],
                },
                TeamMember {
                    task: None,
                    name: "implementer".to_string(),
                    role: "Implementer".to_string(),
                    persona: "You write code.".to_string(),
                    tool_allowlist: vec![],
                    extra_deny_paths: vec![],
                },
            ],
        }
    }

    fn config(events_tx: UnboundedSender<AgentEvent>, team: TeamConfig) -> MissionToolsConfig {
        MissionToolsConfig {
            team,
            plan: Arc::new(Mutex::new(MissionPlan {
                mission: String::new(),
                steps: vec![],
                summary: None,
            })),
            events_tx,
        }
    }

    fn ctx() -> aivyx_tools::ToolExecutionContext {
        aivyx_tools::ToolExecutionContext {
            cwd: std::path::PathBuf::from("."),
            confiner: Arc::new(aivyx_sandbox::NoopConfiner),
            cancellation: tokio_util::sync::CancellationToken::new(),
        }
    }

    #[tokio::test]
    async fn decompose_task_stores_a_valid_plan_and_echoes_it_back() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let cfg = config(tx, simple_team());
        let tool = DecomposeTaskTool::new(cfg.clone());
        let args = serde_json::json!({
            "mission": "fix the bug",
            "steps": [{ "member": "implementer", "task": "write the fix" }],
        });
        let result = tool.execute(args, &ctx()).await.unwrap();
        let aivyx_types::ToolOutput::Ok(text) = result else {
            panic!("expected Ok");
        };
        assert!(text.contains("implementer"));
        let stored = cfg.plan.lock().unwrap();
        assert_eq!(stored.steps.len(), 1);
        assert_eq!(stored.steps[0].status, StepStatus::Pending);
    }

    #[tokio::test]
    async fn decompose_task_rejects_the_lead_as_a_step_member() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let cfg = config(tx, simple_team());
        let tool = DecomposeTaskTool::new(cfg.clone());
        let args = serde_json::json!({
            "mission": "fix the bug",
            "steps": [{ "member": "coordinator", "task": "do it yourself" }],
        });
        let result = tool.execute(args, &ctx()).await.unwrap();
        let aivyx_types::ToolOutput::Error(msg) = result else {
            panic!("expected Error");
        };
        assert!(msg.contains("coordinator"));
        assert!(cfg.plan.lock().unwrap().steps.is_empty());
    }

    #[tokio::test]
    async fn decompose_task_rejects_an_unknown_member() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let cfg = config(tx, simple_team());
        let tool = DecomposeTaskTool::new(cfg.clone());
        let args = serde_json::json!({
            "mission": "fix the bug",
            "steps": [{ "member": "nonexistent", "task": "do it" }],
        });
        let result = tool.execute(args, &ctx()).await.unwrap();
        let aivyx_types::ToolOutput::Error(msg) = result else {
            panic!("expected Error");
        };
        assert!(msg.contains("nonexistent"));
        assert!(
            msg.contains("implementer"),
            "should list the valid specialist(s), got: {msg:?}"
        );
    }

    #[tokio::test]
    async fn decompose_task_replaces_the_whole_plan_including_the_summary() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let cfg = config(tx, simple_team());
        let tool = DecomposeTaskTool::new(cfg.clone());

        let first_args = serde_json::json!({
            "mission": "fix the bug",
            "steps": [{ "member": "implementer", "task": "write the fix" }],
        });
        tool.execute(first_args, &ctx()).await.unwrap();
        cfg.plan.lock().unwrap().summary = Some("old synthesis result".to_string());

        let second_args = serde_json::json!({
            "mission": "ship the feature",
            "steps": [
                { "member": "implementer", "task": "write the feature" },
                { "member": "implementer", "task": "write tests" },
            ],
        });
        tool.execute(second_args, &ctx()).await.unwrap();

        let stored = cfg.plan.lock().unwrap();
        assert_eq!(stored.mission, "ship the feature");
        assert_eq!(stored.steps.len(), 2);
        assert_eq!(stored.steps[0].id, 1);
        assert_eq!(stored.steps[0].task, "write the feature");
        assert_eq!(stored.steps[1].id, 2);
        assert_eq!(stored.steps[1].task, "write tests");
        assert_eq!(stored.summary, None);
    }

    #[tokio::test]
    async fn decompose_task_emits_missions_updated() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let cfg = config(tx, simple_team());
        let tool = DecomposeTaskTool::new(cfg);
        let args = serde_json::json!({
            "mission": "fix the bug",
            "steps": [{ "member": "implementer", "task": "write the fix" }],
        });
        tool.execute(args, &ctx()).await.unwrap();

        let mut found = false;
        while let Ok(event) = rx.try_recv() {
            if let AgentEvent::MissionsUpdated(plan) = event {
                assert_eq!(plan.mission, "fix the bug");
                assert_eq!(plan.steps.len(), 1);
                found = true;
            }
        }
        assert!(found, "expected a MissionsUpdated event");
    }

    #[tokio::test]
    async fn verify_output_updates_the_named_steps_status() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let cfg = config(tx, simple_team());
        cfg.plan
            .lock()
            .unwrap()
            .steps
            .push(aivyx_types::MissionStep {
                id: 1,
                member: "implementer".to_string(),
                task: "write the fix".to_string(),
                status: StepStatus::Pending,
                notes: None,
            });
        let tool = VerifyOutputTool::new(cfg.clone());
        let args = serde_json::json!({ "step_id": 1, "verdict": "pass", "notes": "looks good" });
        let result = tool.execute(args, &ctx()).await.unwrap();
        assert!(matches!(result, aivyx_types::ToolOutput::Ok(_)));
        let stored = cfg.plan.lock().unwrap();
        assert_eq!(stored.steps[0].status, StepStatus::Verified);
        assert_eq!(stored.steps[0].notes, Some("looks good".to_string()));
    }

    #[tokio::test]
    async fn verify_output_records_a_failed_verdict() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let cfg = config(tx, simple_team());
        cfg.plan
            .lock()
            .unwrap()
            .steps
            .push(aivyx_types::MissionStep {
                id: 1,
                member: "implementer".to_string(),
                task: "write the fix".to_string(),
                status: StepStatus::Pending,
                notes: None,
            });
        let tool = VerifyOutputTool::new(cfg.clone());
        let args =
            serde_json::json!({ "step_id": 1, "verdict": "fail", "notes": "does not compile" });
        let result = tool.execute(args, &ctx()).await.unwrap();
        assert!(matches!(result, aivyx_types::ToolOutput::Ok(_)));
        let stored = cfg.plan.lock().unwrap();
        assert_eq!(stored.steps[0].status, StepStatus::Failed);
        assert_eq!(stored.steps[0].notes, Some("does not compile".to_string()));
    }

    #[tokio::test]
    async fn verify_output_rejects_an_unknown_step_id() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let cfg = config(tx, simple_team());
        let tool = VerifyOutputTool::new(cfg.clone());
        let args = serde_json::json!({ "step_id": 99, "verdict": "pass", "notes": "n/a" });
        let result = tool.execute(args, &ctx()).await.unwrap();
        assert!(matches!(result, aivyx_types::ToolOutput::Error(_)));
    }

    #[tokio::test]
    async fn verify_output_emits_missions_updated() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let cfg = config(tx, simple_team());
        cfg.plan
            .lock()
            .unwrap()
            .steps
            .push(aivyx_types::MissionStep {
                id: 1,
                member: "implementer".to_string(),
                task: "write the fix".to_string(),
                status: StepStatus::Pending,
                notes: None,
            });
        let tool = VerifyOutputTool::new(cfg);
        let args = serde_json::json!({ "step_id": 1, "verdict": "pass", "notes": "looks good" });
        tool.execute(args, &ctx()).await.unwrap();

        let mut found = false;
        while let Ok(event) = rx.try_recv() {
            if let AgentEvent::MissionsUpdated(plan) = event {
                assert_eq!(plan.steps[0].status, StepStatus::Verified);
                found = true;
            }
        }
        assert!(found, "expected a MissionsUpdated event");
    }

    #[tokio::test]
    async fn synthesize_results_stores_the_summary() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let cfg = config(tx, simple_team());
        let tool = SynthesizeResultsTool::new(cfg.clone());
        let args = serde_json::json!({ "summary": "done, fix applied and verified" });
        let result = tool.execute(args, &ctx()).await.unwrap();
        assert!(matches!(result, aivyx_types::ToolOutput::Ok(_)));
        assert_eq!(
            cfg.plan.lock().unwrap().summary,
            Some("done, fix applied and verified".to_string())
        );
    }

    #[tokio::test]
    async fn synthesize_results_emits_missions_updated() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let cfg = config(tx, simple_team());
        let tool = SynthesizeResultsTool::new(cfg);
        let args = serde_json::json!({ "summary": "done, fix applied and verified" });
        tool.execute(args, &ctx()).await.unwrap();

        let mut found = false;
        while let Ok(event) = rx.try_recv() {
            if let AgentEvent::MissionsUpdated(plan) = event {
                assert_eq!(
                    plan.summary,
                    Some("done, fix applied and verified".to_string())
                );
                found = true;
            }
        }
        assert!(found, "expected a MissionsUpdated event");
    }
}
