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

/// The autonomous driver's goal-achieved signal, combining three
/// independent sources: the `set_tasks` list, `MissionPlan` (Nonagon team
/// missions), and open specialist sessions. Each of the first two
/// contributes a signal only if it was ever *used* — an empty task list, a
/// `None` mission plan, or a `Some(MissionPlan)` whose `mission` and `steps`
/// are both empty (the eager placeholder `agent_builder.rs` constructs
/// whenever `[team] enabled = true`, regardless of whether `decompose_task`
/// was ever called this run) all mean that surface was never engaged, so it
/// must not count as "nothing to do, stop immediately" (an unused signal is
/// a no-op, not a blocker). An open specialist session always blocks
/// completion outright, regardless of the other two — a live, un-closed
/// specialist session is inherently evidence of unfinished business. When
/// neither tasks nor a mission were ever used, this is `false` — matching
/// the original single-signal behavior exactly. See ROADMAP.md Phase 11c
/// and `docs/superpowers/specs/2026-09-21-autonomous-goal-achieved-team-awareness-design.md`.
fn goal_achieved(
    tasks: &[Task],
    mission_plan: Option<&MissionPlan>,
    open_specialist_sessions: &[SpecialistSessionSummary],
) -> bool {
    if !open_specialist_sessions.is_empty() {
        return false;
    }
    let tasks_signal =
        (!tasks.is_empty()).then(|| tasks.iter().all(|t| t.status == TaskStatus::Done));
    let mission_signal = mission_plan
        .filter(|p| mission_in_use(p))
        .map(|p| p.summary.is_some());
    match (tasks_signal, mission_signal) {
        (None, None) => false,
        _ => tasks_signal.unwrap_or(true) && mission_signal.unwrap_or(true),
    }
}

/// Whether a mission plan was ever engaged — `agent_builder.rs` constructs
/// an empty placeholder whenever `[team] enabled = true`, which doesn't count.
fn mission_in_use(plan: &MissionPlan) -> bool {
    !plan.mission.is_empty() || !plan.steps.is_empty()
}

/// Sent instead of the generic "continue" when the model has neither a task
/// list nor a mission. A task list marked done is the only way an
/// autonomous run can finish, so a model that did the work without one
/// would otherwise be told to keep going until the budget ran out — and a
/// small model then invents work, undoing what it got right.
const NO_TASK_LIST_MESSAGE: &str = "You have no task list, and a task list is how this run \
ends. If the goal is already achieved, call set_tasks with the steps you completed, each \
marked done, and stop. Otherwise call set_tasks with your plan, then carry it out. Don't \
start work the goal didn't ask for.";

/// What the autonomous driver sends next, given whether the turn that just
/// finished paused (Phase 12A), the current task list, the current mission
/// plan (if any), and any open specialist sessions. `None` means stop the
/// loop (goal achieved) — the caller is responsible for the separate
/// budget-exhaustion and cancellation stop conditions, which this function
/// doesn't know about.
fn next_autonomous_message(
    last_turn_paused: bool,
    tasks: &[Task],
    mission_plan: Option<&MissionPlan>,
    open_specialist_sessions: &[SpecialistSessionSummary],
) -> Option<String> {
    if last_turn_paused {
        return Some("continue".to_string());
    }
    if goal_achieved(tasks, mission_plan, open_specialist_sessions) {
        return None;
    }
    if tasks.is_empty()
        && !mission_plan.is_some_and(mission_in_use)
        && open_specialist_sessions.is_empty()
    {
        return Some(NO_TASK_LIST_MESSAGE.to_string());
    }
    Some("continue working toward the goal".to_string())
}

/// The driver stop reasons each get their own message-building function
/// (rather than inlining `format!` at each `agent.notify(...)` call site)
/// so the message *content* can be unit-tested on its own; the loop that
/// uses them, `drive()`, is also tested directly (see `tests.rs`).
fn budget_exhausted_notice(iterations_used: u32, tasks: &[Task]) -> String {
    let done_count = tasks
        .iter()
        .filter(|t| t.status == TaskStatus::Done)
        .count();
    format!(
        "autonomous run stopped: budget exhausted after {iterations_used} iteration(s) \
         ({done_count}/{} tasks done)",
        tasks.len()
    )
}

/// Cancels `cancellation` once `deadline` passes, so a single long turn
/// (up to its tool-call cap, each verification retry allowed its own
/// command timeout) can't run past `[autonomous] max_duration_secs` — the
/// driver loop only checks the deadline between turns. The flag is true
/// only if the deadline did the cancelling, so the stop is reported as
/// the budget running out, not as the user's Ctrl+C. Abort the returned
/// task once the turn ends.
fn cancel_at_deadline(
    deadline: Instant,
    cancellation: CancellationToken,
) -> (Arc<AtomicBool>, tokio::task::JoinHandle<()>) {
    let deadline_hit = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&deadline_hit);
    let timer = tokio::spawn(async move {
        tokio::time::sleep_until(deadline.into()).await;
        if !cancellation.is_cancelled() {
            flag.store(true, Ordering::SeqCst);
            cancellation.cancel();
        }
    });
    (deadline_hit, timer)
}

/// Failed turns in a row that end an autonomous run. More than one, so a
/// model server that is restarting doesn't end it; without a limit, a
/// request the server always rejects (a `[backend] reasoning_effort` the
/// model's template doesn't accept, say) used up every iteration in
/// seconds and was reported as the budget running out.
const MAX_FAILED_TURNS_IN_A_ROW: u32 = 3;

/// `Some(notice)` once `failed_in_a_row` reaches
/// `MAX_FAILED_TURNS_IN_A_ROW`.
fn failed_turns_notice(
    failed_in_a_row: u32,
    iterations_used: u32,
    error: &crate::AgentError,
) -> Option<String> {
    (failed_in_a_row >= MAX_FAILED_TURNS_IN_A_ROW).then(|| {
        format!(
            "autonomous run stopped: {failed_in_a_row} turns in a row failed, after \
             {iterations_used} iteration(s) — {error}"
        )
    })
}

fn cancelled_notice(iterations_used: u32) -> String {
    format!("autonomous run stopped: cancelled by user after {iterations_used} iteration(s)")
}

/// The one extra turn `[autonomous] final_review` sends when the goal is
/// first reached. In the build tests the 27B twice left out fields the
/// brief says are `null` while its final answer said they were `null`.
const FINAL_REVIEW_MESSAGE: &str = "Before this run ends, check your work against the task's \
spec. For each request and response the spec describes, read the code that handles it (don't \
rely on memory) and compare field by field: names, types, status codes, and whether a missing \
value is `null` or left out. Fix any mismatch, with a test. If everything matches, say so and \
change nothing.";

/// What the driver sends when the goal is reached: the review message the
/// first time, when `final_review` is on; otherwise `None` (stop).
fn after_goal(final_review: bool, reviewed: &mut bool) -> Option<String> {
    if final_review && !*reviewed {
        *reviewed = true;
        return Some(FINAL_REVIEW_MESSAGE.to_string());
    }
    None
}

/// The stop notice with the facts aivyx-coder checked itself indented
/// under it, one per line (`RunFacts::notice_lines`).
fn notice_with_facts(notice: &str, facts: &RunFacts) -> String {
    let mut text = notice.to_string();
    for line in facts.notice_lines() {
        text.push_str("\n  ");
        text.push_str(&line);
    }
    text
}

/// Writes the `--report` JSON, private to the user (mode 600), replacing
/// any earlier report at `path`.
fn write_report(path: &std::path::Path, json: &str) -> std::io::Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(json.as_bytes())
}

/// `last_verification_passed` is `Agent::last_verification_passed`: the
/// model decides the goal is achieved (its tasks are all done), not the
/// tests, so a run can end that way with verification still failing --
/// which the notice then says rather than reading as plain success.
fn goal_achieved_notice(iterations_used: u32, last_verification_passed: Option<bool>) -> String {
    let notice =
        format!("autonomous run stopped: goal achieved after {iterations_used} iteration(s)");
    if last_verification_passed == Some(false) {
        format!(
            "{notice} -- but the last verification run failed, so check the result before \
             relying on it"
        )
    } else {
        notice
    }
}

fn injection_detected_notice(iterations_used: u32, finding: &InjectionFinding) -> String {
    format!(
        "autonomous run stopped: possible prompt injection detected after {iterations_used} \
         iteration(s) — flagged content from {} matched \"{}\": \"{}\"",
        finding.source, finding.matched_pattern, finding.excerpt
    )
}

/// Configures an unattended `--auto` session (ROADMAP.md Phase 11c). `tasks`
/// is a clone of the same `Arc<Mutex<Vec<Task>>>` handle already shared
/// between the `set_tasks` tool and the `Agent` — the driver reads it
/// directly rather than waiting on an `AgentEvent::TasksUpdated` event, so
/// its goal-achieved check always sees the current state.
#[derive(Clone)]
pub struct AutonomousRun {
    pub goal: String,
    pub max_iterations: u32,
    pub max_duration: Duration,
    pub tasks: Arc<Mutex<Vec<Task>>>,
    pub injection_taint: InjectionTaint,
    /// `None` when `[team] enabled = false` (see `agent_builder.rs`'s
    /// `BuiltAgent.mission_plan`, which this is threaded from directly).
    pub mission_plan: Option<Arc<Mutex<MissionPlan>>>,
    /// `None` when `[team] enabled = false` (see `agent_builder.rs`'s
    /// `BuiltAgent.specialist_session_pool`, threaded from directly).
    pub specialist_session_pool: Option<SpecialistSessionPool>,
    /// `[verification] final_checks`: run by aivyx-coder when the goal is
    /// reached, reported in the stop notice.
    pub final_checks: Vec<String>,
    /// Resolved `[permissions] read_only_paths`, to mark changed files.
    pub read_only_paths: Vec<PathBuf>,
    /// `--report`: where to write the run's facts as JSON when it stops.
    pub report: Option<PathBuf>,
    /// `[autonomous] final_review`: one review turn against the spec before
    /// the run stops with its goal reached.
    pub final_review: bool,
    /// `[backend]` model, address and reasoning effort, for `--report`.
    pub model: RunModel,
}

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

/// The model a run talks to, as configured, for `--report`.
#[derive(Debug, Clone, Default)]
pub struct RunModel {
    pub model: String,
    pub base_url: String,
    pub reasoning_effort: Option<String>,
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
    mut outcome: Outcome,
    mut notice: String,
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
        // A signal during the checks cancelled them (they count as failed);
        // report the run as cancelled, not as a goal reached with failing
        // checks.
        if stop.is_cancelled() {
            outcome = Outcome::Cancelled;
            notice = format!(
                "{} (goal reached; cancelled during the final checks)",
                cancelled_notice(iterations)
            );
        }
    }
    facts.changed = agent.changed_since_run_start(&run.read_only_paths).await;
    let notice = notice_with_facts(&notice, &facts);
    if let Some(path) = &run.report {
        let (checkpoints, checkpoint_failures) = agent.checkpoints_since_run_start();
        let report = RunReport {
            report_version: crate::run_facts::REPORT_VERSION,
            // Every crate inherits the workspace version, so this is the
            // coder's version.
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
