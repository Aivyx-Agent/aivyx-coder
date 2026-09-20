# Nonagon-Style Team — Phase 3 (Mission Structure) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give the lead three new, lightweight, state-recording tools —
`decompose_task`, `verify_output`, `synthesize_results` — matching
`aivyx-pa`'s own Nonagon tool set, none of which spin up a sub-agent or
call an LLM themselves (that stays `delegate_to_specialist`'s job).
Registered under the same `[team] enabled` gate, no new config knob.

**Architecture:** Three tasks. (1) `MissionPlan`/`MissionStep`/
`StepStatus` types in `aivyx-types`, mirroring `Task`/`TaskStatus`'s
own placement and shape. (2) The three tools in a new
`crates/aivyx-core/src/mission_tools.rs`, sharing an
`Arc<Mutex<MissionPlan>>` the same way `SetTasksTool` shares
`Arc<Mutex<Vec<Task>>>` — plus promoting `delegate_to_specialist.rs`'s
existing `specialists`/`specialist_names`/`specialist_roster_description`
helpers to `pub(crate)` so `decompose_task`'s member validation reuses
them instead of duplicating the same logic. (3) Wiring into
`agent_builder.rs`'s existing `if settings.team.enabled` block.

**Tech Stack:** Rust, reusing `set_tasks.rs`'s established
state-recording-tool pattern exactly.

## Global Constraints

- `cargo build --workspace`, `cargo test --workspace`,
  `cargo clippy --workspace --all-targets -- -D warnings` must stay
  clean. `cargo fmt --check` file-scoped only — **never** a
  package-scoped `cargo fmt -p <crate>` command (this exact mistake has
  happened once already in this initiative and had to be reverted; use
  `rustfmt --edition 2024 --check <path>` / `rustfmt --edition 2024
  <path>` on specific files only).
- **Deviation from the spec's literal wording, confirmed correct at
  plan time**: the design spec's Decision 1 sketches `StepStatus` as
  `Pending`/`InProgress`/`Done`/`Verified`/`Failed` (5 states). Per
  Decision 2, nothing in this phase ever transitions a step to
  `InProgress` or `Done` — there is no tool that marks a step "started"
  or "finished" other than `verify_output`'s own pass/fail judgment.
  `InProgress`/`Done` would therefore be permanently unreachable enum
  variants — dead code the moment they're written. This plan
  simplifies `StepStatus` to exactly three states: `Pending` (set by
  `decompose_task`), `Verified` (set by `verify_output` on a pass
  verdict), `Failed` (set by `verify_output` on a fail verdict). If a
  future phase adds real orchestration that tracks "in progress," that
  phase can add the variant then, when something will actually set it.
  Re-verify this reasoning still holds before Task 1 — if this plan's
  own research is stale and some other mechanism already sets an
  intermediate state, ask before deviating from the spec's original
  5-state sketch.
- None of the three new tools call an LLM or construct an `Agent` —
  each is a pure state-mutation-plus-validation tool, matching
  `set_tasks.rs`'s complexity class exactly, not `delegate_task`'s/
  `delegate_to_specialist`'s.
- `decompose_task`'s per-step `member` validation must reuse
  `delegate_to_specialist.rs`'s existing `specialists`/`specialist_names`
  helpers (promoted to `pub(crate)` in Task 2) — do not write a second,
  separate "is this a valid non-lead specialist" check.
- No `Agent`, session-persistence, or TUI changes this phase — the
  `MissionPlan`'s shared state is constructed once in
  `agent_builder.rs` and passed only to these three tools' configs.

---

## Task 1: `MissionPlan`/`MissionStep`/`StepStatus` in `aivyx-types`

**Files:**
- Modify: `/home/julian/Projects/Rust/aivyx-coder/crates/aivyx-types/src/lib.rs`

**Interfaces:**
- Produces: `pub struct MissionPlan { pub mission: String, pub steps: Vec<MissionStep>, pub summary: Option<String> }`,
  `pub struct MissionStep { pub id: u32, pub member: String, pub task: String, pub status: StepStatus, pub notes: Option<String> }`,
  `pub enum StepStatus { Pending, Verified, Failed }` (all
  `#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]`
  except `StepStatus` also derives `Copy`, matching `TaskStatus`'s own
  derive list exactly).

- [ ] **Step 1: Read `Task`/`TaskStatus`'s real current definition in full**

Read `crates/aivyx-types/src/lib.rs` around lines 92-110 (re-verify —
may have shifted) to match doc-comment style, derive-list style, and
placement exactly.

- [ ] **Step 2: Write the failing tests**

```rust
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
```

- [ ] **Step 3: Run to verify they fail**

```bash
cd /home/julian/Projects/Rust/aivyx-coder
cargo test -p aivyx-types mission_plan_tests
```

Expected: compile error — the types don't exist yet.

- [ ] **Step 4: Implement**

```rust
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
```

Place near `Task`/`TaskStatus` in the same file, matching their exact
style.

- [ ] **Step 5: Run to verify they pass**

```bash
cd /home/julian/Projects/Rust/aivyx-coder
cargo test -p aivyx-types mission_plan_tests
```

Expected: both tests pass.

- [ ] **Step 6: Run full crate check**

```bash
cd /home/julian/Projects/Rust/aivyx-coder
cargo test -p aivyx-types
cargo clippy -p aivyx-types --all-targets -- -D warnings
rustfmt --edition 2024 --check crates/aivyx-types/src/lib.rs
```

- [ ] **Step 7: Commit**

```bash
cd /home/julian/Projects/Rust/aivyx-coder
git add crates/aivyx-types/src/lib.rs
git commit -m "feat: add MissionPlan/MissionStep/StepStatus to aivyx-types

Mirrors Task/TaskStatus's own placement and shape exactly -- shared,
zero-logic types the three new mission tools (Task 2, a separate
dispatch) will mutate. StepStatus is 3 states (Pending/Verified/Failed),
not the 5 aivyx-pa's own design sketches -- InProgress/Done have no
tool that would ever set them this phase (see the plan's own Global
Constraints for the full reasoning)."
```

---

## Task 2: The three tools in `aivyx-core`

**Files:**
- Create: `/home/julian/Projects/Rust/aivyx-coder/crates/aivyx-core/src/mission_tools.rs`
- Modify: `/home/julian/Projects/Rust/aivyx-coder/crates/aivyx-core/src/delegate_to_specialist.rs`
  (promote `specialists`/`specialist_names`/`specialist_roster_description`
  to `pub(crate)`)
- Modify: `/home/julian/Projects/Rust/aivyx-coder/crates/aivyx-core/src/lib.rs`

**Interfaces:**
- Consumes: `MissionPlan`/`MissionStep`/`StepStatus` (Task 1),
  `delegate_to_specialist`'s promoted `specialists`/`specialist_names`
  helpers.
- Produces: `pub struct MissionToolsConfig { pub team: aivyx_team::TeamConfig, pub plan: Arc<Mutex<MissionPlan>> }`
  (shared by all three tools — simpler than three separate config
  structs, since none of them need `delegate_to_specialist`'s much
  larger collaborator set); `pub struct DecomposeTaskTool`,
  `pub struct VerifyOutputTool`, `pub struct SynthesizeResultsTool`,
  each `::new(config: MissionToolsConfig)` (or a shared `Arc<MissionToolsConfig>`
  if that reads cleaner — your call at implementation time, note which
  you chose).

- [ ] **Step 1: Promote the reusable validation helpers**

Read `crates/aivyx-core/src/delegate_to_specialist.rs`'s real current
`specialists`, `specialist_names`, `specialist_roster_description`
functions (private `fn`, currently). Change all three to `pub(crate) fn`
— no other change to their bodies. Confirm `delegate_to_specialist.rs`'s
own existing call sites still compile unchanged (visibility widening
never breaks existing same-crate callers).

- [ ] **Step 2: Write the failing tests**

```rust
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
                    name: "coordinator".to_string(),
                    role: "Lead".to_string(),
                    persona: "You delegate.".to_string(),
                    tool_allowlist: vec![],
                    extra_deny_paths: vec![],
                },
                TeamMember {
                    name: "implementer".to_string(),
                    role: "Implementer".to_string(),
                    persona: "You write code.".to_string(),
                    tool_allowlist: vec![],
                    extra_deny_paths: vec![],
                },
            ],
        }
    }

    fn config(team: TeamConfig) -> MissionToolsConfig {
        MissionToolsConfig {
            team,
            plan: Arc::new(Mutex::new(MissionPlan {
                mission: String::new(),
                steps: vec![],
                summary: None,
            })),
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
        let cfg = config(simple_team());
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
        let cfg = config(simple_team());
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
        let cfg = config(simple_team());
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
        assert!(msg.contains("implementer"), "should list the valid specialist(s), got: {msg:?}");
    }

    #[tokio::test]
    async fn verify_output_updates_the_named_steps_status() {
        let cfg = config(simple_team());
        cfg.plan.lock().unwrap().steps.push(aivyx_types::MissionStep {
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
    async fn verify_output_rejects_an_unknown_step_id() {
        let cfg = config(simple_team());
        let tool = VerifyOutputTool::new(cfg.clone());
        let args = serde_json::json!({ "step_id": 99, "verdict": "pass", "notes": "n/a" });
        let result = tool.execute(args, &ctx()).await.unwrap();
        assert!(matches!(result, aivyx_types::ToolOutput::Error(_)));
    }

    #[tokio::test]
    async fn synthesize_results_stores_the_summary() {
        let cfg = config(simple_team());
        let tool = SynthesizeResultsTool::new(cfg.clone());
        let args = serde_json::json!({ "summary": "done, fix applied and verified" });
        let result = tool.execute(args, &ctx()).await.unwrap();
        assert!(matches!(result, aivyx_types::ToolOutput::Ok(_)));
        assert_eq!(
            cfg.plan.lock().unwrap().summary,
            Some("done, fix applied and verified".to_string())
        );
    }
}
```

(The tests above call `cfg.clone()` because `MissionToolsConfig` derives
`Clone` in Step 4's real implementation below — `Arc<Mutex<MissionPlan>>`
and `TeamConfig` are both cheaply cloneable, so each tool gets its own
handle to the *same* shared plan. Confirm this still holds once you
write Step 4 for real.)

- [ ] **Step 3: Run to verify they fail**

```bash
cd /home/julian/Projects/Rust/aivyx-coder
cargo test -p aivyx-core mission_tools_tests
```

Expected: compile error — the file/types don't exist yet.

- [ ] **Step 4: Implement**

```rust
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

use crate::delegate_to_specialist::{specialist_names, specialists};

/// Shared by all three tools -- unlike `DelegateToSpecialistConfig`,
/// none of these need an `LlmBackend`/`PermissionGate`/checkpointer/etc.,
/// since none of them construct a sub-agent.
#[derive(Clone)]
pub struct MissionToolsConfig {
    pub team: TeamConfig,
    pub plan: Arc<Mutex<MissionPlan>>,
}

#[derive(Deserialize, JsonSchema)]
struct StepArg {
    member: String,
    task: String,
}

#[derive(Deserialize, JsonSchema)]
struct DecomposeTaskArgs {
    mission: String,
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
                specialists: {}.",
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
        *self.config.plan.lock().unwrap() = MissionPlan {
            mission: args.mission,
            steps,
            summary: None,
        };
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
    step_id: u32,
    verdict: Verdict,
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
        Ok(ToolOutput::Ok(format!(
            "step {} marked {:?}: {}",
            args.step_id, step.status, args.notes
        )))
    }
}

#[derive(Deserialize, JsonSchema)]
struct SynthesizeResultsArgs {
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
                as your next response after calling this."
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
        self.config.plan.lock().unwrap().summary = Some(args.summary.clone());
        Ok(ToolOutput::Ok(format!("mission synthesis recorded: {}", args.summary)))
    }
}
```

Before trusting this verbatim: re-verify `Tool`'s exact trait signature
(especially `permission_request`'s and `execute`'s exact parameter
types) against `crates/aivyx-tools/src/lib.rs`'s real current
definition, and re-verify `ToolExecutionContext`'s real fields, matching
`set_tasks.rs`'s/`delegate_to_specialist.rs`'s own real, current usage
— this plan's sketch is grounded in earlier research and may have
drifted.

- [ ] **Step 5: Wire the module and its exports**

Add `pub mod mission_tools;` to `crates/aivyx-core/src/lib.rs` near
`pub mod delegate_to_specialist;`. Add
`pub use mission_tools::{DecomposeTaskTool, MissionToolsConfig, SynthesizeResultsTool, VerifyOutputTool};`
near the existing `pub use delegate_to_specialist::{...}` line, matching
its exact style.

- [ ] **Step 6: Run to verify tests pass**

```bash
cd /home/julian/Projects/Rust/aivyx-coder
cargo test -p aivyx-core mission_tools_tests
cargo test -p aivyx-core delegate_to_specialist
```

Expected: all pass, including `delegate_to_specialist`'s own existing
tests (confirming the visibility promotion in Step 1 didn't break
anything).

- [ ] **Step 7: Run full crate check**

```bash
cd /home/julian/Projects/Rust/aivyx-coder
cargo build --workspace
cargo test -p aivyx-core
cargo clippy -p aivyx-core --all-targets -- -D warnings
rustfmt --edition 2024 --check crates/aivyx-core/src/mission_tools.rs
rustfmt --edition 2024 --check crates/aivyx-core/src/delegate_to_specialist.rs
```

- [ ] **Step 8: Commit**

```bash
cd /home/julian/Projects/Rust/aivyx-coder
git add crates/aivyx-core/src/mission_tools.rs crates/aivyx-core/src/delegate_to_specialist.rs crates/aivyx-core/src/lib.rs
git commit -m "feat: add decompose_task/verify_output/synthesize_results tools

Lightweight, state-recording tools sharing an Arc<Mutex<MissionPlan>> --
none construct a sub-agent or call an LLM, matching set_tasks' own
complexity class. decompose_task's member validation reuses
delegate_to_specialist's existing specialists/specialist_names helpers
(promoted to pub(crate)) rather than duplicating the same
non-lead-member check. Not yet registered anywhere -- Task 3."
```

---

## Task 3: Wire into `agent_builder.rs`

**Files:**
- Modify: `/home/julian/Projects/Rust/aivyx-coder/crates/aivyx/src/agent_builder.rs`

**Interfaces:**
- Consumes: `MissionToolsConfig`, `DecomposeTaskTool`, `VerifyOutputTool`,
  `SynthesizeResultsTool` (Task 2).

- [ ] **Step 1: Read the real current `if settings.team.enabled` block in full**

Read `crates/aivyx/src/agent_builder.rs` around the existing
`if settings.team.enabled { ... }` block (search for
`DelegateToSpecialistTool`) — re-verify the exact current shape,
including the inline `team: aivyx_team::default_coding_roster()` call
inside `DelegateToSpecialistConfig`'s construction.

- [ ] **Step 2: Bind `team` to a local variable, share it, add the three registrations**

Change the block so `aivyx_team::default_coding_roster()` is called
once and bound to a local (`let team = aivyx_team::default_coding_roster();`),
reused both by `DelegateToSpecialistConfig.team` (now `team.clone()`
instead of calling the function again) and by a new
`aivyx_core::MissionToolsConfig`. Construct one shared
`let mission_plan = Arc::new(std::sync::Mutex::new(aivyx_types::MissionPlan { mission: String::new(), steps: vec![], summary: None }));`
and register all three new tools:

```rust
let mission_tools_config = aivyx_core::MissionToolsConfig {
    team: team.clone(),
    plan: Arc::clone(&mission_plan),
};
registry.register(Arc::new(aivyx_core::DecomposeTaskTool::new(
    mission_tools_config.clone(),
)));
registry.register(Arc::new(aivyx_core::VerifyOutputTool::new(
    mission_tools_config.clone(),
)));
registry.register(Arc::new(aivyx_core::SynthesizeResultsTool::new(
    mission_tools_config,
)));
```

Place these registrations inside the same `if settings.team.enabled`
block, after `delegate_to_specialist`'s own registration (order among
these doesn't matter for recursion-prevention the way it does for the
delegation tools, since none of the three mission tools construct a
sub-agent or get included in any specialist's attenuated registry
question — but confirm this reasoning holds by checking whether
`compute_specialist_registry`/`effective_tool_allowlist` would ever
need these three tool names excluded the way REPL tools are; if a
specialist's own `tool_allowlist` could ever legitimately include
`decompose_task`/`verify_output`/`synthesize_results`, that's fine and
intentional — a specialist mid-mission is still just a normal `Agent`
with whatever tools its own `tool_allowlist` grants, and there's no
principled reason a specialist couldn't itself decompose a sub-task,
though the shipped `default_coding_roster()` never grants these to any
specialist today).

- [ ] **Step 3: Verify the workspace builds and existing tests still pass**

```bash
cd /home/julian/Projects/Rust/aivyx-coder
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
rustfmt --edition 2024 --check crates/aivyx/src/agent_builder.rs
```

If the fmt check reports drift, confirm it's not in this task's own new
lines before treating it as pre-existing (this file has known,
pre-existing drift elsewhere, confirmed in Phase 5's own build — but
re-verify your own new lines specifically, don't assume).

- [ ] **Step 4: Manual smoke check**

```bash
cd /home/julian/Projects/Rust/aivyx-coder
cargo build -p aivyx
./target/debug/aivyx-coder --help
```

If a real local LLM backend is reachable in this environment, go
further (temp config with `[team]\nenabled = true`, confirm no startup
error) — if not, state plainly in your report that this is the
practical ceiling, matching Phase 5's own precedent for this exact
limitation.

- [ ] **Step 5: Commit**

```bash
cd /home/julian/Projects/Rust/aivyx-coder
git add crates/aivyx/src/agent_builder.rs
git commit -m "feat: register decompose_task/verify_output/synthesize_results when [team] enabled

Shares the same TeamConfig (now bound once, reused, instead of calling
default_coding_roster() twice) and a new Arc<Mutex<MissionPlan>>
constructed alongside delegate_to_specialist's own config, inside the
same [team] enabled gate -- no new config knob."
```

---

## Final verification

- [ ] Run the complete workspace check once more, after all 3 tasks:

```bash
cd /home/julian/Projects/Rust/aivyx-coder
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

Expected: everything clean/passing.

## Explicitly out of scope for this plan

(Copied forward from the design spec's own "What this spec does not
decide" section.)

- Phase 4 (message bus) and any real DAG/concurrent-branch execution
  engine — separate, later, unscoped work.
- Phase 6 (TUI missions surface) — this phase's `MissionPlan` state is
  not rendered or persisted anywhere.
- Automating `verify_output`'s judgment to gate anything — nothing is
  automated this phase.
- Deny-paths attenuation for specialists — still deferred from Phase 2.
