# Headless `--auto` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `aivyx-coder --auto` runs without the TUI when stdout isn't a terminal (or with
`--headless`): it prints progress to stderr and the stop notice to stdout, writes an extended
`--report`, and exits with a code that says how the run ended.

**Architecture:** the autonomous driver loop moves out of `aivyx-tui`'s `run()` into a new
module, `aivyx_core::autonomous`, with one entry point, `drive()`. The TUI and a new headless
frontend (`crates/aivyx/src/headless.rs`) both call it. `drive()` takes a run-level
`CancellationToken`, which headless mode cancels on `SIGINT`/`SIGTERM`. The `--report` JSON
gains version, model, token, checkpoint and read-only fields, fed by two small new counters
(usage on `Agent`, checkpoint attempts on `ToolExecutor`).

**Tech Stack:** Rust 2024 workspace; tokio, tokio-util `CancellationToken`, serde, clap,
wiremock (test only).

**Spec:** `aivyx-ecosystem/docs/superpowers/specs/2026-10-25-coder-bench-design.md`, Part 1.

## Global Constraints

- All new code is Rust: no Python, Node or shell programs. (`scripts/gen-*.py` are not touched:
  no config field changes in this plan.)
- Never run `cargo fmt` on this repo. Match the formatting of the code around each edit.
- Commits use `git commit -s` and end with
  `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
- Work on a branch `feat/headless-auto` off `main`. Merge with `--ff-only`.
- After every task: `cargo test --workspace` and `cargo clippy --workspace --all-targets` pass,
  with no new warnings.
- Behaviour of the TUI's `--auto` must not change: the same notices, at the same points.
- Exit codes (spec, verbatim): 0 goal achieved and last verification passed; 1 goal achieved
  but the last verification or a final check failed; 2 budget used up; 3 failed turns or an
  error; 4 injection detected; 130 cancelled by a signal.
- `--report` changes are additive: every existing field keeps its name and meaning.
- Every new CLI flag needs a `### \`--flag\`` heading in
  `docs/manual/reference/01-command-line.md` (a test enforces it).

## File Structure

| File | Change | Responsibility |
|---|---|---|
| `crates/aivyx-core/src/autonomous/mod.rs` | create | `AutonomousRun`, `RunModel`, `Outcome`, `RunEnd`, `drive()`, the stop rules and notices (moved from the TUI) |
| `crates/aivyx-core/src/autonomous/tests.rs` | create | the moved TUI tests plus new `drive()` tests with a scripted backend |
| `crates/aivyx-core/src/lib.rs` | modify | `pub mod autonomous;` |
| `crates/aivyx-core/src/run_facts.rs` | modify | `UsageTotals`; `RunReport` v1 fields |
| `crates/aivyx-core/src/agent/mod.rs` | modify | usage counter, checkpoint baseline, accessors |
| `crates/aivyx-core/src/agent/tests.rs` | modify | usage test |
| `crates/aivyx-tools/src/lib.rs` | modify | checkpoint attempt/failure counts |
| `crates/aivyx-tui/src/app.rs` | modify | calls `drive()`; the moved code and tests are deleted |
| `crates/aivyx-tui/src/lib.rs` | modify | stop re-exporting `AutonomousRun` |
| `crates/aivyx/src/headless.rs` | create | `NoHumanPrompter`, `Printer`, `exit_code`, `run()` |
| `crates/aivyx/src/main.rs` | modify | `--headless`/`--verbose`, mode choice, signal handling, `RunModel` |
| `crates/aivyx/Cargo.toml` | modify | `tokio-util` dependency |
| `crates/aivyx/tests/headless_e2e.rs` | create | the binary against a wiremock SSE server |
| `docs/manual/reference/01-command-line.md` | modify | `--headless`, `--verbose`, `--report` fields, exit codes |
| `docs/manual/guide/10-autonomous-and-advanced-modes.md` | modify | a "Running without the TUI" section |

---

### Task 1: Move the autonomous loop into `aivyx_core::autonomous`

Behaviour doesn't change. The loop body becomes `drive()`, which also takes a run-level stop
token and an `on_iteration` callback. Both are unused by the TUI (it passes a fresh token
and a no-op), but headless mode needs them.

**Files:**
- Create: `crates/aivyx-core/src/autonomous/mod.rs`, `crates/aivyx-core/src/autonomous/tests.rs`
- Modify: `crates/aivyx-core/src/lib.rs`, `crates/aivyx-tui/src/app.rs`,
  `crates/aivyx-tui/src/lib.rs`, `crates/aivyx/src/main.rs:909`

**Interfaces:**
- Produces:
  - `aivyx_core::autonomous::AutonomousRun`: the same fields as today's
    `aivyx_tui::AutonomousRun`.
  - `pub enum Outcome { GoalAchieved, BudgetExhausted, Cancelled, Failed, InjectionDetected }`
    with `pub fn as_str(self) -> &'static str`.
  - `pub struct RunEnd { outcome: Outcome, notice: String, iterations: u32,
    last_verification_passed: Option<bool>, final_checks_passed: bool }` (all fields `pub`).
  - `pub async fn drive(agent: &mut Agent, run: &AutonomousRun, cwd: &Path,
    stop: &CancellationToken, active_cancellation: &Mutex<Option<CancellationToken>>,
    on_iteration: &mut (dyn FnMut(u32) + Send)) -> RunEnd`.

- [ ] **Step 1: Branch**

```bash
cd ~/Projects/Rust/aivyx-coder && git switch -c feat/headless-auto main
```

- [ ] **Step 2: Create the module with the moved code**

Create `crates/aivyx-core/src/autonomous/mod.rs`. Start it with this header:

```rust
//! The `--auto` driver loop, shared by every frontend that runs one (the
//! TUI and headless mode): it sends the goal, keeps the run going turn by
//! turn, decides when and why it stops, and ends it with the final checks,
//! the run facts and `--report`. Moved out of `aivyx-tui` (2026-10-25) so
//! headless mode runs exactly the same loop.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aivyx_sandbox::{InjectionFinding, InjectionTaint};
use aivyx_types::MissionPlan;
use tokio_util::sync::CancellationToken;

use crate::run_facts::{RunFacts, RunReport};
use crate::{Agent, SpecialistSessionPool, SpecialistSessionSummary, Task, TaskStatus};

#[cfg(test)]
mod tests;
```

Then move these items **verbatim, with their doc comments**, out of
`crates/aivyx-tui/src/app.rs`. Line numbers are as of `main` at `b783043`.
- lines 36–177:
  - `goal_achieved`, `mission_in_use`;
  - `NO_TASK_LIST_MESSAGE`, `next_autonomous_message`;
  - `budget_exhausted_notice`, `cancel_at_deadline`;
  - `MAX_FAILED_TURNS_IN_A_ROW`, `failed_turns_notice`, `cancelled_notice`.
- lines 236–282:
  - the doc comment at 236–239 belongs to `goal_achieved_notice`, so move it onto that
    function;
  - `FINAL_REVIEW_MESSAGE`, `after_goal`, `notice_with_facts`, `write_report`.
- lines 329–348: `goal_achieved_notice` and `injection_detected_notice`.
- lines 420–447: `AutonomousRun`, which becomes `pub` in its new home.

Keep the moved functions private (`fn`), as they were. Delete `finish_autonomous_run`
(lines 284–327); Step 3 replaces it. Change every `aivyx_core::` path inside the moved code
to `crate::`.

- [ ] **Step 3: Add `Outcome`, `RunEnd`, `drive` and `finish` to the module**

Append to `crates/aivyx-core/src/autonomous/mod.rs`:

```rust
/// Why an autonomous run stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    GoalAchieved,
    BudgetExhausted,
    Cancelled,
    Failed,
    InjectionDetected,
}

impl Outcome {
    /// The `--report` file's `outcome` value.
    pub fn as_str(self) -> &'static str {
        match self {
            Outcome::GoalAchieved => "goal_achieved",
            Outcome::BudgetExhausted => "budget_exhausted",
            Outcome::Cancelled => "cancelled",
            Outcome::Failed => "failed",
            Outcome::InjectionDetected => "injection_detected",
        }
    }
}

/// How an autonomous run ended, for the frontend to show.
#[derive(Debug, Clone)]
pub struct RunEnd {
    pub outcome: Outcome,
    /// The stop notice with the facts aivyx-coder checked indented under it.
    pub notice: String,
    pub iterations: u32,
    pub last_verification_passed: Option<bool>,
    /// Whether every `[verification] final_checks` command passed (`true`
    /// when none ran).
    pub final_checks_passed: bool,
}

/// Runs an `--auto` session to its end. `stop` ends the run the way Ctrl+C
/// does (each turn runs under a child of it); `active_cancellation` holds the
/// running turn's token for a frontend that cancels turns itself (the TUI's
/// Ctrl+C); `on_iteration` is told each iteration's number as it starts.
pub async fn drive(
    agent: &mut Agent,
    run: &AutonomousRun,
    cwd: &Path,
    stop: &CancellationToken,
    active_cancellation: &Mutex<Option<CancellationToken>>,
    on_iteration: &mut (dyn FnMut(u32) + Send),
) -> RunEnd {
    let started = Instant::now();
    let deadline = started + run.max_duration;
    agent.mark_run_start().await;
    let mut reviewed = false;
    let mut iterations_used = 0u32;
    let mut failed_in_a_row = 0u32;
    let mut next_message = Some(run.goal.clone());
    // Kept by the agent too: compaction can drop the turn that carried it,
    // and there's no one to ask to restate it.
    agent.set_autonomous_goal(run.goal.clone());
    let (outcome, notice) = loop {
        let Some(message) = next_message.take() else {
            let notice = goal_achieved_notice(iterations_used, agent.last_verification_passed());
            break (Outcome::GoalAchieved, notice);
        };
        if stop.is_cancelled() {
            break (Outcome::Cancelled, cancelled_notice(iterations_used));
        }
        if iterations_used >= run.max_iterations || Instant::now() >= deadline {
            let tasks_snapshot = run.tasks.lock().unwrap().clone();
            break (Outcome::BudgetExhausted, budget_exhausted_notice(iterations_used, &tasks_snapshot));
        }
        iterations_used += 1;
        on_iteration(iterations_used);
        let cancellation = stop.child_token();
        *active_cancellation.lock().unwrap() = Some(cancellation.clone());
        let (deadline_hit, timer) = cancel_at_deadline(deadline, cancellation.clone());
        let outcome = agent.run_turn(message, cwd, cancellation.clone()).await;
        timer.abort();
        *active_cancellation.lock().unwrap() = None;

        if deadline_hit.load(Ordering::SeqCst) {
            let tasks_snapshot = run.tasks.lock().unwrap().clone();
            break (Outcome::BudgetExhausted, budget_exhausted_notice(iterations_used, &tasks_snapshot));
        }
        if cancellation.is_cancelled() {
            // Ctrl+C or a stop signal: don't send another message.
            break (Outcome::Cancelled, cancelled_notice(iterations_used));
        }
        match &outcome {
            Ok(()) => failed_in_a_row = 0,
            Err(error) => {
                failed_in_a_row += 1;
                if let Some(notice) = failed_turns_notice(failed_in_a_row, iterations_used, error) {
                    break (Outcome::Failed, notice);
                }
            }
        }
        // `run_turn`'s own mid-turn taint check only ends the turn that just
        // ran; this one decides whether the run sends another message.
        // `.take()`, since consuming the taint here is what decides that.
        if let Some(finding) = run.injection_taint.take() {
            break (Outcome::InjectionDetected, injection_detected_notice(iterations_used, &finding));
        }
        let tasks_snapshot = run.tasks.lock().unwrap().clone();
        let mission_plan_snapshot = run.mission_plan.as_ref().map(|p| p.lock().unwrap().clone());
        let open_specialist_sessions = run
            .specialist_session_pool
            .as_ref()
            .map(|pool| pool.open_sessions())
            .unwrap_or_default();
        next_message = next_autonomous_message(
            agent.last_turn_paused(),
            &tasks_snapshot,
            mission_plan_snapshot.as_ref(),
            &open_specialist_sessions,
        );
        // Not on the last allowed iteration: the review would end the run as
        // "budget exhausted" with the goal reached.
        if next_message.is_none() && iterations_used < run.max_iterations {
            next_message = after_goal(run.final_review, &mut reviewed);
        }
    };
    finish(agent, run, cwd, outcome, notice, iterations_used, started, stop, active_cancellation).await
}

/// Ends a run: runs the final checks (only when the goal was reached),
/// collects the facts, writes `--report`, and returns the notice with the
/// facts under it.
#[allow(clippy::too_many_arguments)]
async fn finish(
    agent: &mut Agent,
    run: &AutonomousRun,
    cwd: &Path,
    outcome: Outcome,
    notice: String,
    iterations: u32,
    started: Instant,
    stop: &CancellationToken,
    active_cancellation: &Mutex<Option<CancellationToken>>,
) -> RunEnd {
    let mut facts = RunFacts {
        verification: agent
            .verification_command()
            .map(|name| (name.to_string(), agent.last_verification_passed())),
        ..Default::default()
    };
    if outcome == Outcome::GoalAchieved && !run.final_checks.is_empty() {
        let cancellation = stop.child_token();
        *active_cancellation.lock().unwrap() = Some(cancellation.clone());
        facts.final_checks = agent.run_final_checks(&run.final_checks, cwd, &cancellation).await;
        *active_cancellation.lock().unwrap() = None;
    }
    facts.changed = agent.changed_since_run_start(&run.read_only_paths).await;
    let notice = notice_with_facts(&notice, &facts);
    if let Some(path) = &run.report {
        let report = RunReport {
            goal: &run.goal,
            outcome: outcome.as_str(),
            notice: &notice,
            iterations,
            duration_secs: started.elapsed().as_secs(),
            last_verification_passed: agent.last_verification_passed(),
            final_checks: &facts.final_checks,
            changed_files: facts.changed.as_deref().unwrap_or_default(),
        };
        if let Err(err) = write_report(path, &report.to_json()) {
            agent.notify(format!("couldn't write the --report file {}: {err}", path.display()));
        }
    }
    RunEnd {
        outcome,
        notice,
        iterations,
        last_verification_passed: agent.last_verification_passed(),
        final_checks_passed: facts.final_checks.iter().all(|check| check.passed),
    }
}
```

Two orderings change, without changing behaviour:
- The goal-achieved notice is now built at the top of the next pass rather than at the end of
  the pass that decided it. Nothing runs between those two points.
- The stop check comes first. The TUI's token is never cancelled outside a turn, so the TUI
  can't tell the difference.

Add `pub mod autonomous;` to `crates/aivyx-core/src/lib.rs`, in alphabetical order after
`pub mod architect;`.

- [ ] **Step 4: Point the TUI at `drive`**

In `crates/aivyx-tui/src/app.rs`, replace the whole `if let Some(autonomous) = autonomous { … }`
arm inside `run()`'s `tokio::spawn` (lines 563–674) with:

```rust
        if let Some(autonomous) = autonomous {
            // Autonomous mode drives itself; it never waits on input_rx.
            // Ctrl+C cancels the running turn through `background_cancellation`.
            let stop = CancellationToken::new();
            let end = aivyx_core::autonomous::drive(
                &mut agent,
                &autonomous,
                &cwd,
                &stop,
                &background_cancellation,
                &mut |_| {},
            )
            .await;
            agent.notify(end.notice);
```

The `} else {` interactive arm that follows is unchanged. Change `run()`'s parameter to
`autonomous: Option<aivyx_core::autonomous::AutonomousRun>`. In
`crates/aivyx-tui/src/lib.rs`, change `pub use app::{AutonomousRun, run};` to
`pub use app::run;`. In `crates/aivyx/src/main.rs:909`, change `aivyx_tui::AutonomousRun` to
`aivyx_core::autonomous::AutonomousRun`. Remove the imports in `app.rs` that are now unused;
clippy names them.

- [ ] **Step 5: Move the TUI's autonomous tests**

Create `crates/aivyx-core/src/autonomous/tests.rs`:

```rust
use super::*;
use std::collections::VecDeque;

use aivyx_llm::{ChatRequest, FinishReason, LlmBackend, LlmError, StreamEvent};
use aivyx_sandbox::{
    AutonomousMode, NoopConfiner, PermissionDecision, PermissionGate, PermissionRequest, PlanMode,
};
use aivyx_tools::{ToolExecutor, ToolRegistry};
use futures::StreamExt;
use futures::stream::BoxStream;
```

Then move these test functions and helpers, verbatim, from `app.rs`'s `mod tests` into it,
deleting them from `app.rs`:
- the helpers `done_task`, `pending_task`, `mission_with_summary`, `open_session`;
- the tests:
  - `goal_achieved_requires_at_least_one_task_and_all_done`;
  - `next_autonomous_message_chooses_correctly`;
  - `no_task_list_message_says_how_to_finish`;
  - `a_used_mission_without_tasks_keeps_the_generic_continue`;
  - `goal_achieved_requires_synthesize_results_for_a_mission_only_run`;
  - `goal_achieved_requires_both_tasks_and_mission_when_both_are_used`;
  - `goal_achieved_blocks_on_any_open_specialist_session_regardless_of_other_signals`;
  - `goal_achieved_team_disabled_matches_original_behavior_exactly`;
  - `goal_achieved_ignores_a_never_decomposed_pristine_mission_plan`;
  - `budget_exhausted_notice_reports_iterations_and_task_progress`;
  - `failed_turns_notice_waits_for_three_failures_in_a_row`;
  - `budget_exhausted_notice_handles_no_tasks_ever_set`;
  - `the_deadline_cancels_a_turn_still_running_when_it_passes`;
  - `a_user_cancel_before_the_deadline_is_not_reported_as_the_deadline`;
  - `an_already_passed_deadline_cancels_at_once`;
  - `cancelled_notice_reports_iterations`;
  - `a_final_review_runs_once_when_the_goal_is_first_reached`;
  - `the_stop_notice_carries_the_facts_indented_under_it`;
  - `the_report_file_is_private`;
  - `goal_achieved_notice_reports_iterations`;
  - `goal_achieved_notice_says_when_the_last_verification_failed`;
  - `injection_detected_notice_reports_iterations_source_and_pattern`.

Also delete the comment block above `budget_exhausted_notice_reports_iterations_and_task_progress`
in `app.rs` (it begins "`tokio::spawn`ed closure in `run()`" and explains why the loop wasn't
testable), since the loop is now tested directly. In the moved tests, replace
`aivyx_core::run_facts::` with `crate::run_facts::`. `mission_with_summary` builds a
`MissionPlan`; add a `use aivyx_types::{…};` line naming exactly the `aivyx_types` items
the moved helpers use (for example `MissionStep`, `StepStatus`), and nothing more, so clippy
stays clean.

`interactive_injection_notice` and its tests stay in the TUI.

- [ ] **Step 6: Run the moved tests**

Run: `cargo test -p aivyx-core autonomous && cargo test -p aivyx-tui`
Expected: PASS. 22 tests under `autonomous::tests`, and the TUI's remaining tests.

- [ ] **Step 7: Write the failing `drive` tests**

Append to `crates/aivyx-core/src/autonomous/tests.rs`:

```rust
/// Streams one scripted response per request; an exhausted queue streams
/// nothing, which ends the turn.
struct ScriptedBackend {
    responses: Mutex<VecDeque<Vec<StreamEvent>>>,
}

#[async_trait::async_trait]
impl LlmBackend for ScriptedBackend {
    fn model_id(&self) -> &str {
        "scripted"
    }

    async fn stream_chat(
        &self,
        _request: ChatRequest,
    ) -> Result<BoxStream<'static, Result<StreamEvent, LlmError>>, LlmError> {
        let events = self.responses.lock().unwrap().pop_front().unwrap_or_default();
        Ok(futures::stream::iter(events.into_iter().map(Ok::<StreamEvent, LlmError>)).boxed())
    }
}

/// Every request fails, as when the model server is down.
struct DownBackend;

#[async_trait::async_trait]
impl LlmBackend for DownBackend {
    fn model_id(&self) -> &str {
        "down"
    }

    async fn stream_chat(
        &self,
        _request: ChatRequest,
    ) -> Result<BoxStream<'static, Result<StreamEvent, LlmError>>, LlmError> {
        Err(LlmError::Timeout)
    }
}

struct AllowAll;

#[async_trait::async_trait]
impl PermissionGate for AllowAll {
    async fn check(&self, _request: &PermissionRequest) -> PermissionDecision {
        PermissionDecision::Allow
    }
}

fn text(reply: &str) -> Vec<StreamEvent> {
    vec![
        StreamEvent::TextDelta(reply.to_string()),
        StreamEvent::Done { finish_reason: FinishReason::Stop },
    ]
}

fn scripted(replies: &[&str]) -> Arc<dyn LlmBackend> {
    Arc::new(ScriptedBackend { responses: Mutex::new(replies.iter().map(|r| text(r)).collect()) })
}

fn run_with(tasks: Vec<Task>, max_iterations: u32) -> AutonomousRun {
    AutonomousRun {
        goal: "Do the task.".to_string(),
        max_iterations,
        max_duration: Duration::from_secs(60),
        tasks: Arc::new(Mutex::new(tasks)),
        injection_taint: InjectionTaint::new(),
        mission_plan: None,
        specialist_session_pool: None,
        final_checks: Vec::new(),
        read_only_paths: Vec::new(),
        report: None,
        final_review: false,
    }
}

/// Drives `run` against `llm` and returns how it ended plus the iteration
/// numbers `on_iteration` saw.
async fn drive_with(llm: Arc<dyn LlmBackend>, run: &AutonomousRun, stop: &CancellationToken) -> (RunEnd, Vec<u32>) {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let executor = ToolExecutor::new(ToolRegistry::new(), Arc::new(AllowAll), Arc::new(NoopConfiner));
    let mut agent = Agent::new(
        llm,
        executor,
        "system",
        crate::AgentConfig::default(),
        Arc::clone(&run.tasks),
        PlanMode::new(),
        AutonomousMode::new(),
        tx,
    );
    let mut seen = Vec::new();
    let active = Mutex::new(None);
    let end = drive(&mut agent, run, Path::new("."), stop, &active, &mut |n| seen.push(n)).await;
    (end, seen)
}

#[tokio::test]
async fn drive_stops_with_the_goal_achieved_once_every_task_is_done() {
    let run = run_with(vec![done_task(1)], 5);
    let (end, seen) = drive_with(scripted(&["Done."]), &run, &CancellationToken::new()).await;
    assert_eq!(end.outcome, Outcome::GoalAchieved);
    assert_eq!(end.iterations, 1);
    assert_eq!(seen, [1]);
    assert!(end.notice.starts_with("autonomous run stopped: goal achieved after 1 iteration(s)"), "{}", end.notice);
    assert!(end.final_checks_passed, "no final checks ran");
}

#[tokio::test]
async fn drive_stops_when_the_iteration_budget_is_used_up() {
    let run = run_with(vec![pending_task(1)], 2);
    let (end, seen) = drive_with(scripted(&["working", "still working"]), &run, &CancellationToken::new()).await;
    assert_eq!(end.outcome, Outcome::BudgetExhausted);
    assert_eq!(end.iterations, 2);
    assert_eq!(seen, [1, 2]);
    assert!(end.notice.contains("budget exhausted after 2 iteration(s) (0/1 tasks done)"), "{}", end.notice);
}

#[tokio::test]
async fn drive_stops_after_three_failed_turns_in_a_row() {
    let run = run_with(vec![pending_task(1)], 10);
    let (end, _) = drive_with(Arc::new(DownBackend), &run, &CancellationToken::new()).await;
    assert_eq!(end.outcome, Outcome::Failed);
    assert_eq!(end.iterations, 3);
}

#[tokio::test]
async fn a_cancelled_stop_token_ends_the_run_before_another_turn() {
    let run = run_with(vec![pending_task(1)], 10);
    let stop = CancellationToken::new();
    stop.cancel();
    let (end, seen) = drive_with(scripted(&["never sent"]), &run, &stop).await;
    assert_eq!(end.outcome, Outcome::Cancelled);
    assert_eq!(end.iterations, 0);
    assert!(seen.is_empty());
}

#[tokio::test]
async fn drive_stops_when_flagged_content_was_ingested() {
    let run = run_with(vec![pending_task(1)], 10);
    run.injection_taint.flag(InjectionFinding {
        source: "read_file: notes.txt".to_string(),
        matched_pattern: "ignore previous instructions".to_string(),
        excerpt: "IGNORE PREVIOUS INSTRUCTIONS".to_string(),
    });
    let (end, _) = drive_with(scripted(&["ok"]), &run, &CancellationToken::new()).await;
    assert_eq!(end.outcome, Outcome::InjectionDetected);
    assert_eq!(end.iterations, 1);
}

#[tokio::test]
async fn drive_writes_the_report_with_the_outcome() {
    let dir = tempfile::tempdir().unwrap();
    let mut run = run_with(vec![done_task(1)], 5);
    run.report = Some(dir.path().join("run.json"));
    drive_with(scripted(&["Done."]), &run, &CancellationToken::new()).await;
    let report: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.path().join("run.json")).unwrap()).unwrap();
    assert_eq!(report["outcome"], "goal_achieved");
    assert_eq!(report["iterations"], 1);
}

#[test]
fn every_outcome_has_its_report_name() {
    assert_eq!(Outcome::GoalAchieved.as_str(), "goal_achieved");
    assert_eq!(Outcome::BudgetExhausted.as_str(), "budget_exhausted");
    assert_eq!(Outcome::Cancelled.as_str(), "cancelled");
    assert_eq!(Outcome::Failed.as_str(), "failed");
    assert_eq!(Outcome::InjectionDetected.as_str(), "injection_detected");
}
```

- [ ] **Step 8: Run the `drive` tests**

Run: `cargo test -p aivyx-core autonomous::tests`
Expected: PASS: the 22 moved tests and the 7 new ones. Steps 2–4 already implemented
`drive`. If you write these tests before Step 3, they fail to compile on `drive`/`Outcome`;
that's the expected red.

- [ ] **Step 9: Run the workspace checks**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets`
Expected: all pass, no warnings.

- [ ] **Step 10: Commit**

```bash
git add crates/aivyx-core/src/autonomous crates/aivyx-core/src/lib.rs crates/aivyx-tui/src crates/aivyx/src/main.rs
git commit -s -m "refactor: the --auto loop moves to aivyx_core::autonomous

drive() runs it for any frontend; the TUI calls it unchanged. It also takes
a run-level stop token and an iteration callback, for headless mode.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 2: Count tokens and checkpoints for the run

**Files:**
- Modify: `crates/aivyx-tools/src/lib.rs` (`CheckpointHealth` at :261, dispatch at :635–656,
  the test at :1588)
- Modify: `crates/aivyx-core/src/run_facts.rs`, `crates/aivyx-core/src/agent/mod.rs`
  (fields near :551 and :657, `mark_run_start` at :874, the `StreamEvent::Usage` arm at :3270),
  `crates/aivyx-core/src/agent/tests.rs`

**Interfaces:**
- Produces:
  - `ToolExecutor::checkpoint_counts(&self) -> (u32, u32)`: `(attempts, failures)`, not counting
    cancelled attempts.
  - `aivyx_core::run_facts::UsageTotals { requests: u32, prompt_tokens: u64,
    completion_tokens: u64, peak_prompt_tokens: u32 }` (Copy, Default, PartialEq) with
    `record(&mut self, prompt: u32, completion: u32)`.
  - `Agent::usage_totals(&self) -> UsageTotals`, which `mark_run_start` resets.
  - `Agent::checkpoints_since_run_start(&self) -> (u32, u32)`.

- [ ] **Step 1: Failing test for the executor counts**

At the end of `a_failing_checkpoint_is_reported_once_per_run_of_failures` in
`crates/aivyx-tools/src/lib.rs` (after line 1648), add:

```rust
        // Five checkpointed calls; those for writes 2, 3 and 5 failed.
        assert_eq!(executor.checkpoint_counts(), (5, 3));
```

Run: `cargo test -p aivyx-tools a_failing_checkpoint_is_reported_once_per_run_of_failures`
Expected: FAIL to compile, with "no method named `checkpoint_counts`".

- [ ] **Step 2: Count attempts and failures**

In `CheckpointHealth`, after `unreported`, add:

```rust
    /// Checkpoint attempts that weren't cancelled, and how many of those
    /// failed, for the run report.
    attempts: u32,
    failures: u32,
```

In `dispatch`, change the `match outcome` to:

```rust
            match outcome {
                Ok(()) => {
                    health.failing = false;
                    health.attempts += 1;
                }
                // A cancelled snapshot isn't a broken one.
                Err(_) if cancellation.is_cancelled() => {}
                Err(err) => {
                    health.attempts += 1;
                    health.failures += 1;
                    tracing::warn!(tool = %call.name, error = %err, "checkpoint failed (tool call proceeds)");
                    if !health.failing {
                        health.failing = true;
                        health.unreported = Some(err);
                    }
                }
            }
```

After `last_checkpoint_ok`, add:

```rust
    /// Tool-call checkpoint attempts so far and how many failed; cancelled
    /// attempts count as neither.
    pub fn checkpoint_counts(&self) -> (u32, u32) {
        let health = self
            .checkpoint_health
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        (health.attempts, health.failures)
    }
```

Run: `cargo test -p aivyx-tools a_failing_checkpoint_is_reported_once_per_run_of_failures`
Expected: PASS.

- [ ] **Step 3: Failing test for `UsageTotals`**

In `crates/aivyx-core/src/run_facts.rs`'s `mod tests`, add:

```rust
    #[test]
    fn usage_totals_add_up_and_keep_the_peak() {
        let mut usage = UsageTotals::default();
        usage.record(100, 20);
        usage.record(300, 5);
        usage.record(200, 10);
        assert_eq!(
            usage,
            UsageTotals { requests: 3, prompt_tokens: 600, completion_tokens: 35, peak_prompt_tokens: 300 }
        );
    }
```

Run: `cargo test -p aivyx-core usage_totals_add_up`
Expected: FAIL to compile (`UsageTotals` not found).

- [ ] **Step 4: Add `UsageTotals`**

In `crates/aivyx-core/src/run_facts.rs`, after `RunFacts`:

```rust
/// Token use over a run's main-loop model requests, as the server reported
/// it. Sub-agents and specialists aren't counted.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UsageTotals {
    pub requests: u32,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    /// The largest single prompt: how full the context got.
    pub peak_prompt_tokens: u32,
}

impl UsageTotals {
    pub fn record(&mut self, prompt_tokens: u32, completion_tokens: u32) {
        self.requests += 1;
        self.prompt_tokens += u64::from(prompt_tokens);
        self.completion_tokens += u64::from(completion_tokens);
        self.peak_prompt_tokens = self.peak_prompt_tokens.max(prompt_tokens);
    }
}
```

Run: `cargo test -p aivyx-core usage_totals_add_up`
Expected: PASS.

- [ ] **Step 5: Failing test for the agent's counters**

In `crates/aivyx-core/src/agent/tests.rs`, add:

```rust
#[tokio::test]
async fn usage_is_totalled_per_run_and_reset_when_a_run_starts() {
    let reply = || {
        vec![
            StreamEvent::TextDelta("ok".to_string()),
            StreamEvent::Done { finish_reason: FinishReason::Stop },
            StreamEvent::Usage { prompt_tokens: 100, completion_tokens: 20 },
        ]
    };
    let (mut agent, _rx, _) = build_agent(vec![reply(), reply()], ToolRegistry::new(), 10);
    for message in ["one", "two"] {
        agent.run_turn(message.to_string(), Path::new("."), CancellationToken::new()).await.unwrap();
    }
    assert_eq!(
        agent.usage_totals(),
        crate::run_facts::UsageTotals {
            requests: 2,
            prompt_tokens: 200,
            completion_tokens: 40,
            peak_prompt_tokens: 100,
        }
    );
    assert_eq!(agent.checkpoints_since_run_start(), (0, 0));
    agent.mark_run_start().await;
    assert_eq!(agent.usage_totals(), crate::run_facts::UsageTotals::default());
}
```

Run: `cargo test -p aivyx-core usage_is_totalled_per_run`
Expected: FAIL to compile (`usage_totals` not found).

- [ ] **Step 6: Add the agent counters**

In `crates/aivyx-core/src/agent/mod.rs`, add two fields after `run_start_oid` in the `Agent`
struct:

```rust
    /// Token use since the run started (`mark_run_start`), for `--report`.
    usage: crate::run_facts::UsageTotals,
    /// `ToolExecutor::checkpoint_counts` when the run started.
    run_start_checkpoints: (u32, u32),
```

Initialize both in `Agent::new`, after `run_start_oid: None,`:

```rust
            usage: crate::run_facts::UsageTotals::default(),
            run_start_checkpoints: (0, 0),
```

Make `mark_run_start` read:

```rust
    pub async fn mark_run_start(&mut self) {
        self.usage = crate::run_facts::UsageTotals::default();
        self.run_start_checkpoints = self.executor.checkpoint_counts();
        self.run_start_oid = self
            .undo_snapshot("autonomous run start")
            .await
            .map(|(_, oid)| oid);
    }
```

After `mark_run_start`, add:

```rust
    /// Token use over the main loop's model requests since the run started.
    pub fn usage_totals(&self) -> crate::run_facts::UsageTotals {
        self.usage
    }

    /// Tool-call checkpoints since the run started, and how many failed.
    pub fn checkpoints_since_run_start(&self) -> (u32, u32) {
        let (attempts, failures) = self.executor.checkpoint_counts();
        (
            attempts.saturating_sub(self.run_start_checkpoints.0),
            failures.saturating_sub(self.run_start_checkpoints.1),
        )
    }
```

In the stream loop (around line 3270), change the arm to bind both counts and record them:

```rust
                    Ok(StreamEvent::Usage { prompt_tokens, completion_tokens }) => {
                        self.usage.record(prompt_tokens, completion_tokens);
```

The rest of the arm (calibration and the `ContextUsage` event) is unchanged.

Run: `cargo test -p aivyx-core usage_is_totalled_per_run`
Expected: PASS.

- [ ] **Step 7: Workspace checks and commit**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets`
Expected: all pass.

```bash
git add crates/aivyx-tools/src/lib.rs crates/aivyx-core/src
git commit -s -m "feat: count a run's tokens and checkpoints

For --report: UsageTotals over the main loop's requests, and checkpoint
attempts and failures since the run started.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 3: Report version 1

**Files:**
- Modify: `crates/aivyx-core/src/run_facts.rs` (`RunReport`, its test),
  `crates/aivyx-core/src/autonomous/mod.rs` (`RunModel`, the `AutonomousRun.model` field,
  `finish`), `crates/aivyx-core/src/autonomous/tests.rs` (`run_with`, the report test),
  `crates/aivyx/src/main.rs:909-921`, `docs/manual/reference/01-command-line.md:83-97`

**Interfaces:**
- Consumes: `Agent::usage_totals`, `Agent::checkpoints_since_run_start` (Task 2).
- Produces:
  - `pub struct RunModel { pub model: String, pub base_url: String,
    pub reasoning_effort: Option<String> }` (Debug, Clone, Default) in
    `aivyx_core::autonomous`;
  - `AutonomousRun.model: RunModel`;
  - `run_facts::REPORT_VERSION: u32 = 1`.

- [ ] **Step 1: Failing test: the report carries the new fields**

Replace `the_report_serializes_every_field` in `crates/aivyx-core/src/run_facts.rs` with:

```rust
    #[test]
    fn the_report_serializes_every_field() {
        let checks = [FinalCheck { name: "clippy".into(), passed: true, warnings: 3 }];
        let files = [
            file("src/main.rs", "modified", 10, 2, false),
            file("BRIEF.md", "modified", 1, 1, true),
        ];
        let report = RunReport {
            report_version: REPORT_VERSION,
            coder_version: "0.6.3",
            goal: "Build it.",
            outcome: "goal_achieved",
            notice: "autonomous run stopped: goal achieved after 5 iteration(s)",
            iterations: 5,
            duration_secs: 1200,
            last_verification_passed: Some(true),
            final_checks: &checks,
            changed_files: &files,
            model: "Qwen3.8-27B",
            base_url: "http://llama:8080/v1",
            reasoning_effort: Some("medium"),
            usage: UsageTotals { requests: 9, prompt_tokens: 90_000, completion_tokens: 12_000, peak_prompt_tokens: 41_000 },
            checkpoints: 30,
            checkpoint_failures: 1,
        };
        let json: serde_json::Value = serde_json::from_str(&report.to_json()).unwrap();
        assert_eq!(json["report_version"], 1);
        assert_eq!(json["coder_version"], "0.6.3");
        assert_eq!(json["outcome"], "goal_achieved");
        assert_eq!(json["iterations"], 5);
        assert_eq!(json["final_checks"][0]["warnings"], 3);
        assert_eq!(json["changed_files"][0]["status"], "modified");
        assert_eq!(json["changed_files"][0]["read_only"], false);
        assert_eq!(json["last_verification_passed"], true);
        assert_eq!(json["model"], "Qwen3.8-27B");
        assert_eq!(json["base_url"], "http://llama:8080/v1");
        assert_eq!(json["reasoning_effort"], "medium");
        assert_eq!(json["model_requests"], 9);
        assert_eq!(json["prompt_tokens"], 90_000);
        assert_eq!(json["completion_tokens"], 12_000);
        assert_eq!(json["peak_context_tokens"], 41_000);
        assert_eq!(json["checkpoints"], 30);
        assert_eq!(json["checkpoint_failures"], 1);
        assert_eq!(json["read_only_touched"], serde_json::json!(["BRIEF.md"]));
    }
```

Run: `cargo test -p aivyx-core the_report_serializes_every_field`
Expected: FAIL to compile (unknown fields).

- [ ] **Step 2: Extend `RunReport`**

Replace `RunReport` and its `impl` in `crates/aivyx-core/src/run_facts.rs` with:

```rust
/// The `--report` format's version: bumped when a field changes meaning
/// or goes away, not when one is added.
pub const REPORT_VERSION: u32 = 1;

/// The `--report` file's contents.
#[derive(Debug, Clone)]
pub struct RunReport<'a> {
    pub report_version: u32,
    pub coder_version: &'a str,
    pub goal: &'a str,
    /// `goal_achieved`, `budget_exhausted`, `cancelled`, `failed` or
    /// `injection_detected`.
    pub outcome: &'a str,
    pub notice: &'a str,
    pub iterations: u32,
    pub duration_secs: u64,
    pub last_verification_passed: Option<bool>,
    pub final_checks: &'a [FinalCheck],
    pub changed_files: &'a [ChangedFile],
    /// `[backend]` model and address, and `reasoning_effort` if set.
    pub model: &'a str,
    pub base_url: &'a str,
    pub reasoning_effort: Option<&'a str>,
    pub usage: UsageTotals,
    /// Tool-call checkpoints during the run, and how many failed.
    pub checkpoints: u32,
    pub checkpoint_failures: u32,
}

/// The JSON shape: `RunReport` flattened, with the read-only files that
/// changed listed on their own.
#[derive(Serialize)]
struct ReportJson<'a> {
    report_version: u32,
    coder_version: &'a str,
    goal: &'a str,
    outcome: &'a str,
    notice: &'a str,
    iterations: u32,
    duration_secs: u64,
    last_verification_passed: Option<bool>,
    final_checks: &'a [FinalCheck],
    changed_files: &'a [ChangedFile],
    model: &'a str,
    base_url: &'a str,
    reasoning_effort: Option<&'a str>,
    model_requests: u32,
    prompt_tokens: u64,
    completion_tokens: u64,
    peak_context_tokens: u32,
    checkpoints: u32,
    checkpoint_failures: u32,
    /// Files in `read_only_paths` that changed anyway (a command can change
    /// what the file tools may not).
    read_only_touched: Vec<&'a str>,
}

impl RunReport<'_> {
    /// Pretty-printed JSON, as written to the `--report` file.
    pub fn to_json(&self) -> String {
        let json = ReportJson {
            report_version: self.report_version,
            coder_version: self.coder_version,
            goal: self.goal,
            outcome: self.outcome,
            notice: self.notice,
            iterations: self.iterations,
            duration_secs: self.duration_secs,
            last_verification_passed: self.last_verification_passed,
            final_checks: self.final_checks,
            changed_files: self.changed_files,
            model: self.model,
            base_url: self.base_url,
            reasoning_effort: self.reasoning_effort,
            model_requests: self.usage.requests,
            prompt_tokens: self.usage.prompt_tokens,
            completion_tokens: self.usage.completion_tokens,
            peak_context_tokens: self.usage.peak_prompt_tokens,
            checkpoints: self.checkpoints,
            checkpoint_failures: self.checkpoint_failures,
            read_only_touched: self
                .changed_files
                .iter()
                .filter(|f| f.read_only)
                .map(|f| f.path.as_str())
                .collect(),
        };
        serde_json::to_string_pretty(&json).expect("a RunReport always serializes")
    }
}
```

The crate doesn't compile yet: `finish` still builds the old `RunReport`. Step 3 fixes that,
then the test runs.

- [ ] **Step 3: Fill the report in `finish`**

In `crates/aivyx-core/src/autonomous/mod.rs`, add after `RunEnd`:

```rust
/// The model a run talks to, as configured, for `--report`.
#[derive(Debug, Clone, Default)]
pub struct RunModel {
    pub model: String,
    pub base_url: String,
    pub reasoning_effort: Option<String>,
}
```

Add a field to `AutonomousRun`, after `final_review`:

```rust
    /// `[backend]` model, address and reasoning effort, for `--report`.
    pub model: RunModel,
```

In `finish`, replace the `let report = RunReport { … };` with:

```rust
        let (checkpoints, checkpoint_failures) = agent.checkpoints_since_run_start();
        let report = RunReport {
            report_version: crate::run_facts::REPORT_VERSION,
            coder_version: env!("CARGO_PKG_VERSION"),
            goal: &run.goal,
            outcome: outcome.as_str(),
            notice: &notice,
            iterations,
            duration_secs: started.elapsed().as_secs(),
            last_verification_passed: agent.last_verification_passed(),
            final_checks: &facts.final_checks,
            changed_files: facts.changed.as_deref().unwrap_or_default(),
            model: &run.model.model,
            base_url: &run.model.base_url,
            reasoning_effort: run.model.reasoning_effort.as_deref(),
            usage: agent.usage_totals(),
            checkpoints,
            checkpoint_failures,
        };
```

`aivyx-core`'s version is the workspace's, since every crate is bumped together at release.
In `autonomous/tests.rs`, add `model: RunModel::default(),` to `run_with`. In
`drive_writes_the_report_with_the_outcome`, add:

```rust
    assert_eq!(report["report_version"], 1);
    // The scripted reply carries no usage event, and only those are counted.
    assert_eq!(report["model_requests"], 0);
    assert_eq!(report["checkpoints"], 0);
```

In `crates/aivyx/src/main.rs`, add to the `AutonomousRun { … }` literal (after
`final_review`):

```rust
        model: aivyx_core::autonomous::RunModel {
            model: settings.backend.model.clone(),
            base_url: settings.backend.base_url.clone(),
            reasoning_effort: settings.backend.reasoning_effort.clone(),
        },
```

Run: `cargo test -p aivyx-core run_facts && cargo test -p aivyx-core autonomous`
Expected: PASS. The end-to-end test in Task 6 checks non-zero token counts against a server
that does report usage.

- [ ] **Step 4: Document the new fields**

In `docs/manual/reference/01-command-line.md`, replace the `--report` list (lines 89–97) with:

```markdown
With `--auto` only. When the run stops, for any reason, write what
aivyx-coder checked itself to this file as JSON:
- the outcome and the stop notice;
- iterations and duration;
- the last verification result;
- the `[verification] final_checks` results with their warning counts;
- every file the run changed, marking any in `read_only_paths`, and
  `read_only_touched`, the read-only files that changed anyway;
- the `[backend]` model, address and `reasoning_effort`;
- `model_requests`, `prompt_tokens`, `completion_tokens` and
  `peak_context_tokens` over the main loop's model requests (as the server
  reported them; sub-agents aren't counted);
- `checkpoints` and `checkpoint_failures` during the run;
- `report_version` (1) and `coder_version`. The version goes up only when a
  field changes meaning or is removed, not when one is added.

The file is private to you (mode 600).
```

- [ ] **Step 5: Workspace checks and commit**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets`
Expected: all pass.

```bash
git add crates/aivyx-core/src crates/aivyx/src/main.rs docs/manual/reference/01-command-line.md
git commit -s -m "feat: --report version 1: model, tokens, checkpoints, read-only

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 4: The headless frontend

**Files:**
- Create: `crates/aivyx/src/headless.rs`
- Modify: `crates/aivyx/src/main.rs` (`mod headless;` next to the other `mod` lines),
  `crates/aivyx/Cargo.toml`

**Interfaces:**
- Consumes: `aivyx_core::autonomous::{drive, AutonomousRun, Outcome, RunEnd}` (Task 1).
- Produces:
  - `crate::headless::NoHumanPrompter`, a `PermissionPrompter` that always denies;
  - `crate::headless::exit_code(&RunEnd) -> i32`;
  - `crate::headless::run(agent: Agent, events_rx: UnboundedReceiver<AgentEvent>,
    cwd: PathBuf, run: AutonomousRun, stop: CancellationToken, verbose: bool) -> i32`.

- [ ] **Step 1: Add the dependency**

In `crates/aivyx/Cargo.toml` `[dependencies]`, after the `tokio = …` line, add:

```toml
tokio-util = "0.7.18"
```

- [ ] **Step 2: Write the module with failing tests**

Create `crates/aivyx/src/headless.rs`:

```rust
//! `--auto` without the TUI: for scripts, CI and the benchmark. Progress
//! goes to stderr, one line per event, prefixed with the time since the run
//! started; the stop notice goes to stdout; the exit code says how the run
//! ended. Runs the same loop as the TUI (`aivyx_core::autonomous::drive`).

use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aivyx_core::autonomous::{AutonomousRun, Outcome, RunEnd};
use aivyx_core::{Agent, AgentEvent, TaskStatus};
use aivyx_sandbox::{PermissionPrompter, PermissionRequest, UserResponse};
use aivyx_types::{ToolCall, ToolOutput};
use tokio::sync::mpsc::UnboundedReceiver;
use tokio_util::sync::CancellationToken;

/// Denies anything that would need a person. Autonomous mode's gate settles
/// every request itself, so this is only a backstop.
pub struct NoHumanPrompter;

#[async_trait::async_trait]
impl PermissionPrompter for NoHumanPrompter {
    async fn prompt(&self, request: &PermissionRequest) -> UserResponse {
        tracing::warn!(tool = %request.tool_name, "headless: a permission prompt was denied (no one to ask)");
        UserResponse::Deny
    }
}

/// The process exit code for how the run ended.
pub fn exit_code(end: &RunEnd) -> i32 {
    match end.outcome {
        Outcome::GoalAchieved
            if end.last_verification_passed != Some(false) && end.final_checks_passed =>
        {
            0
        }
        Outcome::GoalAchieved => 1,
        Outcome::BudgetExhausted => 2,
        Outcome::Failed => 3,
        Outcome::InjectionDetected => 4,
        Outcome::Cancelled => 130,
    }
}

/// Longest a one-line summary gets before it's cut.
const MAX_SUMMARY_CHARS: usize = 160;

/// The first line of `text`, cut to `MAX_SUMMARY_CHARS`.
fn first_line(text: &str) -> String {
    let line = text.lines().next().unwrap_or_default();
    if line.chars().count() > MAX_SUMMARY_CHARS {
        let cut: String = line.chars().take(MAX_SUMMARY_CHARS).collect();
        format!("{cut}…")
    } else {
        line.to_string()
    }
}

/// What a tool call acts on, from its usual argument names.
fn tool_target(call: &ToolCall) -> String {
    ["path", "command", "from", "url", "query", "pattern"]
        .iter()
        .find_map(|key| call.arguments.get(*key).and_then(|v| v.as_str()))
        .map(|target| format!(" {}", first_line(target)))
        .unwrap_or_default()
}

/// The progress line for one agent event, if it gets one. Streamed text is
/// handled by `Printer` (only with `--verbose`).
fn progress_line(event: &AgentEvent) -> Option<String> {
    match event {
        AgentEvent::ToolCallDetected(call) => Some(format!("tool {}{}", call.name, tool_target(call))),
        AgentEvent::ToolResult(result) => match &result.output {
            ToolOutput::Ok(text) if text.starts_with("exit status:") => Some(format!("  {}", first_line(text))),
            ToolOutput::Ok(_) => None,
            ToolOutput::Error(text) => Some(format!("  error: {}", first_line(text))),
            ToolOutput::Denied(text) => Some(format!("  denied: {}", first_line(text))),
        },
        AgentEvent::TasksUpdated(tasks) => {
            let done = tasks.iter().filter(|t| t.status == TaskStatus::Done).count();
            Some(format!("tasks: {done}/{} done", tasks.len()))
        }
        AgentEvent::TurnPaused(text) => Some(format!("paused: {}", first_line(text))),
        AgentEvent::Error(text) => Some(format!("error: {text}")),
        AgentEvent::Warning(text) => Some(format!("warning: {text}")),
        AgentEvent::Info(text) => Some(text.clone()),
        AgentEvent::ModelRouted { model, reason } => Some(format!("model {model}: {reason}")),
        AgentEvent::SubAgentActivity(inner) => progress_line(inner).map(|line| format!("sub-agent: {line}")),
        _ => None,
    }
}

/// Writes progress lines with the time since `started` in front:
/// `[0:12:31] tool run_command test`. With `verbose`, the model's streamed
/// text goes out as it arrives, and the next progress line starts on a new line.
struct Printer {
    started: Instant,
    verbose: bool,
    mid_text: bool,
}

impl Printer {
    fn new(verbose: bool) -> Self {
        Self { started: Instant::now(), verbose, mid_text: false }
    }

    fn stamp(&self) -> String {
        let secs = self.started.elapsed().as_secs();
        format!("[{}:{:02}:{:02}]", secs / 3600, secs / 60 % 60, secs % 60)
    }

    fn line(&mut self, text: &str, out: &mut impl Write) {
        if self.mid_text {
            let _ = writeln!(out);
            self.mid_text = false;
        }
        let stamp = self.stamp();
        for (i, part) in text.lines().enumerate() {
            let _ = if i == 0 {
                writeln!(out, "{stamp} {part}")
            } else {
                writeln!(out, "{} {part}", " ".repeat(stamp.len()))
            };
        }
    }

    fn event(&mut self, event: &AgentEvent, out: &mut impl Write) {
        if let AgentEvent::TextDelta(text) = event
            && self.verbose
        {
            let _ = write!(out, "{text}");
            self.mid_text = true;
            return;
        }
        if let Some(line) = progress_line(event) {
            self.line(&line, out);
        }
    }
}

/// Runs an `--auto` session without the TUI and returns the exit code.
pub async fn run(
    mut agent: Agent,
    mut events_rx: UnboundedReceiver<AgentEvent>,
    cwd: PathBuf,
    run: AutonomousRun,
    stop: CancellationToken,
    verbose: bool,
) -> i32 {
    let printer = Arc::new(Mutex::new(Printer::new(verbose)));
    printer.lock().unwrap().line(&format!("goal: {}", run.goal), &mut std::io::stderr().lock());
    if let Some(tests) = agent.tests() {
        printer.lock().unwrap().line(&tests.auto_line(), &mut std::io::stderr().lock());
    }
    let events_printer = Arc::clone(&printer);
    let printing = tokio::spawn(async move {
        while let Some(event) = events_rx.recv().await {
            events_printer.lock().unwrap().event(&event, &mut std::io::stderr().lock());
        }
    });
    let iteration_printer = Arc::clone(&printer);
    let mut on_iteration = move |n: u32| {
        iteration_printer.lock().unwrap().line(&format!("iteration {n}"), &mut std::io::stderr().lock());
    };
    let active = Mutex::new(None);
    let end = aivyx_core::autonomous::drive(&mut agent, &run, &cwd, &stop, &active, &mut on_iteration).await;
    // Dropping the agent closes the event channel unless a tool kept a
    // sender; give the printer a moment to drain either way.
    drop(agent);
    if tokio::time::timeout(Duration::from_secs(2), printing).await.is_err() {
        tracing::warn!("headless: progress printer didn't finish draining");
    }
    let mut stdout = std::io::stdout().lock();
    let _ = writeln!(stdout, "{}", end.notice);
    let _ = stdout.flush();
    exit_code(&end)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aivyx_types::{ToolCallId, ToolCallSource, ToolResult};

    fn end(outcome: Outcome, verified: Option<bool>, checks: bool) -> RunEnd {
        RunEnd {
            outcome,
            notice: String::new(),
            iterations: 1,
            last_verification_passed: verified,
            final_checks_passed: checks,
        }
    }

    #[test]
    fn exit_codes_follow_the_outcome() {
        assert_eq!(exit_code(&end(Outcome::GoalAchieved, Some(true), true)), 0);
        assert_eq!(exit_code(&end(Outcome::GoalAchieved, None, true)), 0, "verification never ran");
        assert_eq!(exit_code(&end(Outcome::GoalAchieved, Some(false), true)), 1);
        assert_eq!(exit_code(&end(Outcome::GoalAchieved, Some(true), false)), 1, "a final check failed");
        assert_eq!(exit_code(&end(Outcome::BudgetExhausted, None, true)), 2);
        assert_eq!(exit_code(&end(Outcome::Failed, None, true)), 3);
        assert_eq!(exit_code(&end(Outcome::InjectionDetected, None, true)), 4);
        assert_eq!(exit_code(&end(Outcome::Cancelled, None, true)), 130);
    }

    fn call(name: &str, arguments: serde_json::Value) -> AgentEvent {
        AgentEvent::ToolCallDetected(ToolCall {
            id: ToolCallId("c1".into()),
            name: name.into(),
            arguments,
            source: ToolCallSource::Native,
        })
    }

    fn result(output: ToolOutput) -> AgentEvent {
        AgentEvent::ToolResult(ToolResult { call_id: ToolCallId("c1".into()), output })
    }

    #[test]
    fn events_become_short_progress_lines() {
        let line = |e: &AgentEvent| progress_line(e);
        assert_eq!(line(&call("run_command", serde_json::json!({"command": "test"}))).unwrap(), "tool run_command test");
        assert_eq!(line(&call("edit_file", serde_json::json!({"path": "src/lib.rs"}))).unwrap(), "tool edit_file src/lib.rs");
        assert_eq!(line(&call("set_tasks", serde_json::json!({"tasks": []}))).unwrap(), "tool set_tasks");
        assert_eq!(
            line(&result(ToolOutput::Ok("exit status: 101 (failed)\n--- stdout ---\nlots".into()))).unwrap(),
            "  exit status: 101 (failed)"
        );
        assert_eq!(line(&result(ToolOutput::Ok("file contents".into()))), None);
        assert_eq!(line(&result(ToolOutput::Denied("outside the worktree".into()))).unwrap(), "  denied: outside the worktree");
        assert_eq!(line(&AgentEvent::TextDelta("hi".into())), None);
        assert_eq!(line(&AgentEvent::Warning("Checkpoint failed".into())).unwrap(), "warning: Checkpoint failed");
        let sub = AgentEvent::SubAgentActivity(Box::new(call("grep", serde_json::json!({"pattern": "fn main"}))));
        assert_eq!(line(&sub).unwrap(), "sub-agent: tool grep fn main");
    }

    #[test]
    fn long_targets_are_cut() {
        let long = "x".repeat(500);
        let line = progress_line(&call("read_file", serde_json::json!({"path": long}))).unwrap();
        assert!(line.chars().count() < 200, "{line}");
        assert!(line.ends_with('…'));
    }

    #[test]
    fn the_printer_stamps_lines_and_keeps_streamed_text_apart() {
        let mut out = Vec::new();
        let mut printer = Printer::new(true);
        printer.event(&AgentEvent::TextDelta("thinking".into()), &mut out);
        printer.line("iteration 2", &mut out);
        printer.line("error: one\ntwo", &mut out);
        let text = String::from_utf8(out).unwrap();
        assert_eq!(text, "thinking\n[0:00:00] iteration 2\n[0:00:00] error: one\n          two\n");
    }

    #[test]
    fn without_verbose_streamed_text_is_not_printed() {
        let mut out = Vec::new();
        let mut printer = Printer::new(false);
        printer.event(&AgentEvent::TextDelta("thinking".into()), &mut out);
        assert!(out.is_empty());
    }
}
```

Add `mod headless;` to `crates/aivyx/src/main.rs` beside the existing `mod agent_builder;`
lines.

Run: `cargo test -p aivyx headless`
Expected: PASS for the 5 tests. If `Task.status`, `ToolResult` or `tests.auto_line()` don't
compile as written, fix them against the real definitions: `aivyx_types::ToolResult` has
`call_id` and `output`; `Task` is `aivyx_core::Task`; `auto_line` is on
`aivyx_core::test_detect::EffectiveTests`. Then run again.

- [ ] **Step 3: Clippy and commit**

`run` and `NoHumanPrompter` are unused until Task 5. Add
`#![allow(dead_code)] // wired up in the next commit` as the first line of `headless.rs`;
Task 5 removes it.

Run: `cargo clippy --workspace --all-targets && cargo test -p aivyx`
Expected: pass.

```bash
git add crates/aivyx/src/headless.rs crates/aivyx/src/main.rs crates/aivyx/Cargo.toml Cargo.lock
git commit -s -m "feat: headless frontend for --auto (not wired up yet)

Progress lines on stderr, the stop notice on stdout, an exit code per
outcome, and a prompter that denies.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 5: Wire up `--headless`, `--verbose` and the signals

**Files:**
- Modify: `crates/aivyx/src/main.rs` (the `Cli` struct near `report` at :501,
  `spawn_exit_signal_handler` at :691, `main` at :710–942),
  `crates/aivyx/src/headless.rs` (drop the `allow`),
  `docs/manual/reference/01-command-line.md`,
  `docs/manual/guide/10-autonomous-and-advanced-modes.md`

**Interfaces:**
- Consumes: `crate::headless::{run, NoHumanPrompter}` (Task 4).
- Produces: `Cli.headless: bool`, `Cli.verbose: bool`; and
  `fn spawn_exit_signal_handler(run_stop: Option<CancellationToken>)`.

- [ ] **Step 1: Failing test: the flags parse and need `--auto`**

In `crates/aivyx/src/main.rs`'s `mod tests`, add:

```rust
    #[test]
    fn headless_and_verbose_need_auto() {
        let cli = Cli::try_parse_from(["aivyx-coder", "--auto", "go", "--headless", "-v"]).unwrap();
        assert!(cli.headless && cli.verbose);
        assert!(Cli::try_parse_from(["aivyx-coder", "--headless"]).is_err());
        assert!(Cli::try_parse_from(["aivyx-coder", "--verbose"]).is_err());
    }
```

Run: `cargo test -p aivyx headless_and_verbose_need_auto`
Expected: FAIL to compile (no field `headless`).

- [ ] **Step 2: Add the flags**

In `Cli`, after the `report` field:

```rust
    /// With --auto: run without the terminal UI. Progress goes to stderr,
    /// the stop notice to stdout, and the exit code says how the run ended
    /// (0 goal achieved, 1 achieved but a check failed, 2 budget used up,
    /// 3 failed, 4 injection detected, 130 stopped by a signal). The
    /// default when stdout isn't a terminal.
    #[arg(long, requires = "auto")]
    headless: bool,

    /// With --auto in headless mode: also print the model's text as it
    /// streams.
    #[arg(long, short = 'v', requires = "auto")]
    verbose: bool,
```

- [ ] **Step 3: Document the flags**

The existing test `every_flag_has_a_command_line_reference_heading` fails until this is done.
In `docs/manual/reference/01-command-line.md`, after the `--report` section, add:

````markdown
### `--headless`

```
aivyx-coder --auto "build the app in BRIEF.md" --headless --report run.json
```

With `--auto` only. Run without the terminal UI: for scripts, CI and
benchmarks. It's the default when stdout isn't a terminal, so piping or
redirecting the output is enough.

- **Progress** goes to stderr, one line per step, with the time since the
  run started: `[0:04:12] tool run_command test`, then
  `[0:04:31]   exit status: 101 (failed)`. Warnings and notices are printed
  in full; the model's own text only with `--verbose`.
- **The stop notice** and its facts go to stdout when the run ends.
- **The exit code** says how it ended:

  | Code | Meaning |
  |---|---|
  | 0 | goal achieved, and the last verification passed (or never ran) |
  | 1 | goal achieved, but the last verification or a final check failed |
  | 2 | the `[autonomous]` budget ran out |
  | 3 | turns kept failing (for example, the model server is down) |
  | 4 | possible prompt injection detected |
  | 130 | stopped by `SIGINT` or `SIGTERM` |

- **Stopping it:** `SIGINT` or `SIGTERM` ends the run the way Ctrl+C does in
  the TUI: running commands are killed and `--report` is still written. A
  second signal exits at once.
- **Permissions:** nothing can be asked without a terminal, so anything that
  would need a person is refused. `--auto`'s gate already works this way.

### `--verbose`

```
aivyx-coder --auto "fix the failing test" --headless --verbose
```

With `--auto` in headless mode: also print the model's text to stderr as it
streams. `-v` for short.
````

In `docs/manual/guide/10-autonomous-and-advanced-modes.md`, after the paragraph ending
"for scripts and for looking back.", add:

```markdown
**Running without the TUI.** When stdout isn't a terminal, or with
`--headless`, `--auto` runs without the TUI: progress goes to stderr, the
stop notice to stdout, and the exit code says how the run ended (see
[`--headless`](../reference/01-command-line.md#--headless)). `SIGINT` or
`SIGTERM` stops it cleanly, with the report written. This is how the
benchmark and CI scripts run aivyx-coder.
```

Run: `cargo test -p aivyx headless_and_verbose_need_auto every_flag_has_a_command_line_reference_heading`
Expected: PASS.

- [ ] **Step 4: The signal handler**

Replace `spawn_exit_signal_handler` with:

```rust
/// On `SIGTERM`, or `SIGHUP` when the terminal goes away, kills every
/// command still running and exits. The process ends without unwinding, so
/// no `ProcessGroup` gets dropped: without this a running `cargo test` — a
/// test binary stuck in a loop — outlived aivyx-coder indefinitely
/// (live-test finding, 2026-10-20). `SIGKILL` can't be caught; it still
/// orphans whatever was running.
///
/// In headless `--auto` (`run_stop` set) the first `SIGINT` or `SIGTERM`
/// cancels the run instead, as Ctrl+C does in the TUI, so the report is
/// still written; a second one exits at once. `SIGINT` is only caught then:
/// in the TUI, Ctrl+C is a key, not a signal.
fn spawn_exit_signal_handler(run_stop: Option<CancellationToken>) {
    use tokio::signal::unix::{SignalKind, signal};
    let (Ok(mut terminate), Ok(mut hangup)) =
        (signal(SignalKind::terminate()), signal(SignalKind::hangup()))
    else {
        return;
    };
    let mut interrupt = match run_stop {
        Some(_) => match signal(SignalKind::interrupt()) {
            Ok(interrupt) => Some(interrupt),
            Err(_) => return,
        },
        None => None,
    };
    tokio::spawn(async move {
        loop {
            // The shell's convention for "ended by signal N": 128 + N.
            let code = tokio::select! {
                _ = terminate.recv() => 128 + 15,
                _ = hangup.recv() => 128 + 1,
                _ = async {
                    match interrupt.as_mut() {
                        Some(interrupt) => {
                            interrupt.recv().await;
                        }
                        None => std::future::pending::<()>().await,
                    }
                } => 128 + 2,
            };
            if code != 128 + 1
                && let Some(stop) = run_stop.as_ref().filter(|stop| !stop.is_cancelled())
            {
                stop.cancel();
                continue;
            }
            aivyx_tools::kill_all_process_groups();
            std::process::exit(code);
        }
    });
}
```

Add `use tokio_util::sync::CancellationToken;` to `main.rs`'s imports.

- [ ] **Step 5: Choose the mode in `main`**

In `main`, replace `spawn_exit_signal_handler();` (line 719) with:

```rust
    // Headless: --auto without a terminal on stdout, or asked for.
    let headless = cli.auto.is_some() && (cli.headless || !std::io::IsTerminal::is_terminal(&std::io::stdout()));
    let run_stop = headless.then(CancellationToken::new);
    spawn_exit_signal_handler(run_stop.clone());
```

Replace the prompter lines (849–850) with:

```rust
    let (tui_prompter, tui_permission_rx) = aivyx_tui::permission_channel();
    let prompter: Arc<dyn PermissionPrompter> = if headless {
        Arc::new(crate::headless::NoHumanPrompter)
    } else {
        Arc::new(tui_prompter)
    };
```

Right after the `let autonomous_run = …;` statement (ending at line 921), add:

```rust
    if let (Some(run), Some(stop)) = (autonomous_run.as_ref().filter(|_| headless), run_stop) {
        let run = run.clone();
        let code =
            crate::headless::run(built.agent, built.events_rx, built.cwd, run, stop, cli.verbose).await;
        aivyx_tools::kill_all_process_groups();
        drop(_tracing_guard);
        std::process::exit(code);
    }
```

This needs `AutonomousRun: Clone`. Add `#[derive(Clone)]` to `AutonomousRun` in
`crates/aivyx-core/src/autonomous/mod.rs`. Every field is already cloned in `main` when the
struct is built (`built.injection_taint.clone()`, `built.specialist_session_pool.clone()`),
and `RunModel` derives `Clone`.

Remove the `#![allow(dead_code)]` line from `headless.rs`.

- [ ] **Step 6: Build and run the unit tests**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets`
Expected: all pass.

- [ ] **Step 7: Commit**

```bash
git add crates/aivyx/src crates/aivyx-core/src/autonomous/mod.rs docs/manual
git commit -s -m "feat: --auto runs headless without a terminal (--headless, -v)

Exit codes per outcome; SIGINT/SIGTERM end the run cleanly with the report
written, a second signal exits at once.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 6: End-to-end tests against a fake model server

**Files:**
- Create: `crates/aivyx/tests/headless_e2e.rs`

**Interfaces:**
- Consumes: the `aivyx-coder` binary (`env!("CARGO_BIN_EXE_aivyx-coder")`) and wiremock
  (already a dev-dependency).

- [ ] **Step 1: Write the tests**

Create `crates/aivyx/tests/headless_e2e.rs`:

```rust
//! The binary in headless `--auto` mode against a wiremock server speaking
//! the OpenAI streaming format: exit codes, stderr progress, stdout notice,
//! and the `--report` fields.

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// One SSE body: `chunks` as `data:` lines, then `[DONE]`.
fn sse(chunks: &[serde_json::Value]) -> String {
    let mut body = String::new();
    for chunk in chunks {
        body.push_str(&format!("data: {chunk}\n\n"));
    }
    body.push_str("data: [DONE]\n\n");
    body
}

fn usage(prompt: u32, completion: u32) -> serde_json::Value {
    serde_json::json!({"choices": [], "usage": {"prompt_tokens": prompt, "completion_tokens": completion}})
}

fn text_reply(text: &str) -> String {
    sse(&[
        serde_json::json!({"choices": [{"delta": {"content": text}, "finish_reason": null}]}),
        serde_json::json!({"choices": [{"delta": {}, "finish_reason": "stop"}]}),
        usage(100, 10),
    ])
}

fn tool_reply(name: &str, arguments: serde_json::Value) -> String {
    sse(&[
        serde_json::json!({"choices": [{"delta": {"tool_calls": [{
            "index": 0, "id": "call_1",
            "function": {"name": name, "arguments": arguments.to_string()}
        }]}, "finish_reason": null}]}),
        serde_json::json!({"choices": [{"delta": {}, "finish_reason": "tool_calls"}]}),
        usage(100, 10),
    ])
}

/// Serves `first` once, then `rest` for every later chat request.
async fn model_server(first: String, rest: String) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(first, "text/event-stream"))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(rest, "text/event-stream"))
        .with_priority(2)
        .mount(&server)
        .await;
    server
}

/// A config dir and project dir for one run against `server`.
fn setup(server: &MockServer, max_iterations: u32) -> (tempfile::TempDir, tempfile::TempDir) {
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let config_dir = home.path().join("config/aivyx-coder");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(
        config_dir.join("config.toml"),
        format!(
            r#"[backend]
base_url = "{}/v1"
model = "fake-model"
context_tokens = 32768

[[permissions.allowed_commands]]
name = "test"
program = "true"
args = []

[[permissions.allowed_commands]]
name = "slow"
program = "sleep"
args = ["60"]

[sandbox]
require_enforcement = false

[verification]
command = "test"

[autonomous]
max_iterations = {max_iterations}
max_duration_secs = 120
"#,
            server.uri()
        ),
    )
    .unwrap();
    (home, project)
}

fn coder(home: &Path, project: &Path, report: &Path, extra: &[&str]) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_aivyx-coder"));
    cmd.current_dir(project)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_DATA_HOME", home.join("data"))
        .env("XDG_STATE_HOME", home.join("state"))
        .env("XDG_CACHE_HOME", home.join("cache"))
        .args(["--auto", "Do the task.", "--report"])
        .arg(report)
        .args(extra)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd
}

fn read_report(report: &Path) -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(report).unwrap()).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_finished_run_exits_0_with_the_report() {
    let done = serde_json::json!({"tasks": [{"text": "do the task", "status": "done"}]});
    let server = model_server(tool_reply("set_tasks", done), text_reply("Done.")).await;
    let (home, project) = setup(&server, 5);
    let report = home.path().join("run.json");
    // Not --headless: piped stdout alone selects headless mode.
    let output = coder(home.path(), project.path(), &report, &[]).output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(0), "stdout: {stdout}\nstderr: {stderr}");
    assert!(stdout.contains("autonomous run stopped: goal achieved after 1 iteration(s)"), "{stdout}");
    assert!(stderr.contains("] iteration 1"), "{stderr}");
    assert!(stderr.contains("] tool set_tasks"), "{stderr}");
    assert!(stderr.contains("] tasks: 1/1 done"), "{stderr}");
    let json = read_report(&report);
    assert_eq!(json["report_version"], 1);
    assert_eq!(json["outcome"], "goal_achieved");
    assert_eq!(json["model"], "fake-model");
    assert_eq!(json["model_requests"], 2);
    assert_eq!(json["prompt_tokens"], 200);
    assert_eq!(json["coder_version"], env!("CARGO_PKG_VERSION"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_run_that_never_finishes_exits_2() {
    let server = model_server(text_reply("Working on it."), text_reply("Still working.")).await;
    let (home, project) = setup(&server, 2);
    let report = home.path().join("run.json");
    let output = coder(home.path(), project.path(), &report, &["--headless"]).output().unwrap();
    assert_eq!(output.status.code(), Some(2), "{}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(read_report(&report)["outcome"], "budget_exhausted");
}

#[tokio::test(flavor = "multi_thread")]
async fn sigterm_stops_the_run_and_still_writes_the_report() {
    let server = model_server(
        tool_reply("run_command", serde_json::json!({"command": "slow"})),
        text_reply("ok"),
    )
    .await;
    let (home, project) = setup(&server, 5);
    let report = home.path().join("run.json");
    let mut child = coder(home.path(), project.path(), &report, &["--headless"]).spawn().unwrap();
    // Wait until the slow command is running, then send SIGTERM.
    let mut stderr = BufReader::new(child.stderr.take().unwrap());
    let mut line = String::new();
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        line.clear();
        assert!(stderr.read_line(&mut line).unwrap() > 0, "stderr closed before the command ran");
        if line.contains("tool run_command slow") {
            break;
        }
        assert!(Instant::now() < deadline, "the command never started");
    }
    std::thread::sleep(Duration::from_millis(500));
    let status = Command::new("kill").args(["-TERM", &child.id().to_string()]).status().unwrap();
    assert!(status.success());
    let started = Instant::now();
    let status = child.wait().unwrap();
    assert!(started.elapsed() < Duration::from_secs(20), "the run should stop well before sleep 60 ends");
    assert_eq!(status.code(), Some(130));
    assert_eq!(read_report(&report)["outcome"], "cancelled");
}
```

- [ ] **Step 2: Run them**

Run: `cargo test -p aivyx --test headless_e2e`
Expected: 3 tests PASS. If a test fails, read its stderr in the assertion message first.
- **A probe or a missing config field:** read `docs/manual/reference/config-example.toml` and
  fix the config in `setup`.
- **`model_requests` differs from 2:** check whether the agent sent another request, for
  example a summary or a verification nudge. If so, assert `>= 2` and leave a comment saying
  why.
- **Don't weaken the exit-code or outcome assertions.** Those are the contract.

- [ ] **Step 3: Workspace checks and commit**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets`
Expected: all pass.

```bash
git add crates/aivyx/tests/headless_e2e.rs
git commit -s -m "test: headless --auto end to end against a fake model server

Exit codes 0, 2 and 130, the progress lines, the stdout notice and the
report fields.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 7: Live check on the 27B, then release

**Files:** none new. This is the release routine.

- [ ] **Step 1: Merge**

```bash
cd ~/Projects/Rust/aivyx-coder && git switch main && git merge --ff-only feat/headless-auto && git push origin main
gh run watch "$(gh run list --branch main --limit 1 --json databaseId --jq '.[0].databaseId')" --exit-status
```

Expected: CI green.

- [ ] **Step 2: Live recipe run on Qwen3.8-27B, headless in Docker**

```bash
D=~/Projects/Rust/docker; K=$D/fltplan-test
cd $D && docker compose -f compose.yml -f compose.qwen38.yml up -d llama      # the 27B
docker build -t aivyx-coder:headless ~/Projects/Rust/aivyx-coder
docker build -t aivyx-coder:headless-rust --build-arg BASE=aivyx-coder:headless $K/img
RUN=$D/headless-live; rm -rf $RUN; mkdir -p $RUN/app $RUN/config/aivyx-coder $RUN/home
cp $D/bench-seed/recipe/BRIEF.md $RUN/app/ && git -C $RUN/app init -q
sed 's/^# fltplan build test (27b)./# headless live check (27b)./' \
  $K/config/27b/aivyx-coder/config.toml > $RUN/config/aivyx-coder/config.toml
cp -r $K/home/.cargo $RUN/home/ 2>/dev/null || true   # pre-fetched crates, if the kit has them
docker run --rm --network aivyx_default --user 1000:1000 --memory 12g -e HOME=/k/home \
  -e XDG_CONFIG_HOME=/k/config -e XDG_DATA_HOME=/k/data -e XDG_STATE_HOME=/k/state \
  -v $RUN:/k -w /k/app aivyx-coder:headless-rust \
  aivyx-coder --auto "$(cat $D/bench-seed/recipe/p.txt)" --report /k/run.json \
  > $RUN/stdout.log 2> $RUN/stderr.log; echo "exit=$?"
```

The kit's config runs `cargo test --offline`. If the recipe needs a crate that isn't in the
copied cache, drop `--offline` from the copied config and give the container network access
to crates.io (the `aivyx_default` network has it).

Expected:
- the exit code matches `run.json`'s `outcome`;
- `stderr.log` shows timestamped progress lines;
- `stdout.log` holds the stop notice;
- `run.json` has `report_version` 1, non-zero `prompt_tokens`, and `checkpoints` > 0.

Score it with the hidden tests:

```bash
mkdir -p $RUN/score && cp -r $RUN/app/. $RUN/score/ && mkdir -p $RUN/score/tests
cp $D/bench-seed/recipe/acceptance.rs $RUN/score/tests/
docker run --rm --user 1000:1000 -e HOME=/k/home -v $RUN:/k -w /k/score aivyx-coder:headless-rust \
  cargo test -q --test acceptance 2>&1 | tail -3
```

13/13 is the 27B's reference result (2026-10-20); record whatever it gets. Then switch the
stack back to the 9B: `cd $D && docker compose up -d llama aivyx-pa`.

- [ ] **Step 3: Release v0.6.3**

Follow the routine used for v0.6.2:
1. Bump the version from 0.6.2 to 0.6.3 in `Cargo.toml` and every `crates/*/Cargo.toml` that
   names it, then `cargo check --workspace` so `Cargo.lock` follows.
2. `git commit -qs -m "Release v0.6.3"` with the Co-Authored-By line, then
   `git tag -a v0.6.3 -m v0.6.3`, and push the commit and the tag.
3. Watch the Release, Docker Publish and CI workflows to green.
4. Download the darwin-aarch64 and x86_64-linux-musl archives, check them against their
   `.sha256` files, and run the Linux binary's `--version`.
5. Update `docs/registry-submission/aivyx-coder/agent.json`: the version, both archive URLs,
   both sha256 values and both `cmd` paths. Commit `registry: aivyx-coder v0.6.3` and push.
6. Update the registry fork's `aivyx-coder/agent.json` on branch `add-aivyx-coder` with
   `gh api -X PUT repos/Aivyx-Agent/registry/contents/aivyx-coder/agent.json`, as for v0.6.2.
   That updates PR 620.
7. Rebuild `aivyx-coder:local` from the release.

- [ ] **Step 4: Record it**

Update the memory note `aivyx_coder_bench_chapter.md`: Part 1 released, the live-run result,
and the registry fork commit. Then add a line to `aivyx-ecosystem/ROADMAP.md`'s current status
(coder at v0.6.3, with headless `--auto`).
