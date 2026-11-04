use super::*;
use std::collections::VecDeque;

use aivyx_llm::{ChatRequest, FinishReason, LlmBackend, LlmError, StreamEvent};
use aivyx_sandbox::{
    AutonomousMode, NoopConfiner, PermissionDecision, PermissionGate, PermissionRequest, PlanMode,
};
use aivyx_tools::{ToolExecutor, ToolRegistry};
use aivyx_types::{MissionStep, StepStatus};
use futures::StreamExt;
use futures::stream::BoxStream;

fn done_task(id: u32) -> Task {
    Task {
        id,
        text: "x".to_string(),
        status: TaskStatus::Done,
    }
}

fn pending_task(id: u32) -> Task {
    Task {
        id,
        text: "x".to_string(),
        status: TaskStatus::Pending,
    }
}

#[test]
fn goal_achieved_requires_at_least_one_task_and_all_done() {
    assert!(
        !goal_achieved(&[], None, &[]),
        "no tasks ever set means never done"
    );
    assert!(!goal_achieved(&[done_task(1), pending_task(2)], None, &[]));
    assert!(goal_achieved(&[done_task(1), done_task(2)], None, &[]));
}

#[test]
fn next_autonomous_message_chooses_correctly() {
    assert_eq!(
        next_autonomous_message(true, &[], None, &[]),
        Some("continue".to_string()),
        "a paused turn always continues, regardless of task state"
    );
    assert_eq!(
        next_autonomous_message(false, &[done_task(1)], None, &[]),
        None,
        "goal achieved -> stop"
    );
    assert_eq!(
        next_autonomous_message(false, &[pending_task(1)], None, &[]),
        Some("continue working toward the goal".to_string())
    );
    assert_eq!(
        next_autonomous_message(false, &[], None, &[]),
        Some(NO_TASK_LIST_MESSAGE.to_string()),
        "no tasks ever set -> ask for a task list (the only way to finish), not more work"
    );
}

#[test]
fn no_task_list_message_says_how_to_finish() {
    assert!(NO_TASK_LIST_MESSAGE.contains("set_tasks"));
    assert!(NO_TASK_LIST_MESSAGE.contains("already achieved"));
    assert!(NO_TASK_LIST_MESSAGE.contains("marked done"));
}

#[test]
fn a_used_mission_without_tasks_keeps_the_generic_continue() {
    let unsynthesized = mission_with_summary(None);
    assert_eq!(
        next_autonomous_message(false, &[], Some(&unsynthesized), &[]),
        Some("continue working toward the goal".to_string())
    );
    let pristine = MissionPlan {
        mission: String::new(),
        steps: vec![],
        summary: None,
    };
    assert_eq!(
        next_autonomous_message(false, &[], Some(&pristine), &[]),
        Some(NO_TASK_LIST_MESSAGE.to_string()),
        "a never-decomposed placeholder plan counts as no plan"
    );
    assert_eq!(
        next_autonomous_message(false, &[], None, &[open_session("s1")]),
        Some("continue working toward the goal".to_string()),
        "an open specialist session blocks finishing, so set_tasks alone wouldn't end the run"
    );
}

fn mission_with_summary(summary: Option<&str>) -> MissionPlan {
    MissionPlan {
        mission: "test mission".to_string(),
        steps: vec![MissionStep {
            id: 1,
            member: "implementer".to_string(),
            task: "do the thing".to_string(),
            status: StepStatus::Verified,
            notes: None,
        }],
        summary: summary.map(|s| s.to_string()),
    }
}

fn open_session(id: &str) -> SpecialistSessionSummary {
    SpecialistSessionSummary {
        session_id: id.to_string(),
        member: "implementer".to_string(),
    }
}

#[test]
fn goal_achieved_requires_synthesize_results_for_a_mission_only_run() {
    let unsynthesized = mission_with_summary(None);
    assert!(
        !goal_achieved(&[], Some(&unsynthesized), &[]),
        "a decomposed mission with no summary yet is not done"
    );
    let synthesized = mission_with_summary(Some("final deliverable"));
    assert!(
        goal_achieved(&[], Some(&synthesized), &[]),
        "synthesize_results having been called is enough on its own, no set_tasks needed"
    );
}

#[test]
fn goal_achieved_requires_both_tasks_and_mission_when_both_are_used() {
    let synthesized = mission_with_summary(Some("final deliverable"));
    assert!(
        !goal_achieved(&[pending_task(1)], Some(&synthesized), &[]),
        "mission synthesized but a set_tasks task is still pending -> not done"
    );
    assert!(
        goal_achieved(&[done_task(1)], Some(&synthesized), &[]),
        "both signals satisfied -> done"
    );
}

#[test]
fn goal_achieved_blocks_on_any_open_specialist_session_regardless_of_other_signals() {
    let synthesized = mission_with_summary(Some("final deliverable"));
    assert!(
        !goal_achieved(&[done_task(1)], Some(&synthesized), &[open_session("s1")]),
        "an open specialist session always blocks completion"
    );
    assert!(
        !goal_achieved(&[], None, &[open_session("s1")]),
        "even with no tasks or mission ever used, an open session blocks"
    );
}

#[test]
fn goal_achieved_team_disabled_matches_original_behavior_exactly() {
    // [team] enabled = false means mission_plan is always None and
    // open_specialist_sessions is always empty -- confirms the 3-arg
    // function reduces to the original 1-arg behavior byte-for-byte.
    assert!(!goal_achieved(&[], None, &[]));
    assert!(!goal_achieved(&[done_task(1), pending_task(2)], None, &[]));
    assert!(goal_achieved(&[done_task(1), done_task(2)], None, &[]));
}

#[test]
fn goal_achieved_ignores_a_never_decomposed_pristine_mission_plan() {
    // [team] enabled = true constructs a placeholder MissionPlan eagerly,
    // even if decompose_task is never called this run -- must not be
    // mistaken for "mission used but incomplete."
    let pristine = MissionPlan {
        mission: String::new(),
        steps: vec![],
        summary: None,
    };
    assert!(
        goal_achieved(&[done_task(1)], Some(&pristine), &[]),
        "a never-decomposed MissionPlan must not block a tasks-only completion"
    );
    assert!(
        !goal_achieved(&[], Some(&pristine), &[]),
        "still correctly not done with no tasks and no real mission either"
    );
}

#[test]
fn budget_exhausted_notice_reports_iterations_and_task_progress() {
    let tasks = vec![done_task(1), done_task(2), pending_task(3)];
    let message = budget_exhausted_notice(5, &tasks);
    assert!(message.contains("budget exhausted"));
    assert!(message.contains('5'));
    assert!(message.contains("2/3 tasks done"));
}

#[test]
fn failed_turns_notice_waits_for_three_failures_in_a_row() {
    // Live-test finding (2026-10-20): with a reasoning_effort the
    // model's template rejected, every request failed and --auto used
    // all 30 iterations in four seconds, then reported "budget
    // exhausted".
    let error = crate::AgentError::ResponseTooLarge;
    assert_eq!(failed_turns_notice(1, 1, &error), None);
    assert_eq!(failed_turns_notice(2, 5, &error), None);
    let notice = failed_turns_notice(3, 6, &error).unwrap();
    assert!(notice.starts_with("autonomous run stopped: 3 turns in a row failed"), "{notice}");
    assert!(notice.contains("after 6 iteration(s)"), "{notice}");
    assert!(notice.contains(&error.to_string()), "{notice}");
}

#[test]
fn budget_exhausted_notice_handles_no_tasks_ever_set() {
    let message = budget_exhausted_notice(3, &[]);
    assert!(message.contains("0/0 tasks done"));
}

#[tokio::test]
async fn the_deadline_cancels_a_turn_still_running_when_it_passes() {
    // Live-test finding (2026-10-20): max_duration_secs was checked only
    // between turns, so one long turn ran past it until killed from outside.
    let cancellation = CancellationToken::new();
    let deadline = Instant::now() + Duration::from_millis(50);
    let (deadline_hit, _timer) = cancel_at_deadline(deadline, cancellation.clone());
    tokio::time::timeout(Duration::from_secs(5), cancellation.cancelled())
        .await
        .expect("the turn's token must be cancelled once the deadline passes");
    assert!(deadline_hit.load(Ordering::SeqCst));
}

#[tokio::test]
async fn a_user_cancel_before_the_deadline_is_not_reported_as_the_deadline() {
    let cancellation = CancellationToken::new();
    let deadline = Instant::now() + Duration::from_secs(60);
    let (deadline_hit, timer) = cancel_at_deadline(deadline, cancellation.clone());
    cancellation.cancel();
    timer.abort();
    assert!(!deadline_hit.load(Ordering::SeqCst));
}

#[tokio::test]
async fn an_already_passed_deadline_cancels_at_once() {
    let cancellation = CancellationToken::new();
    let (deadline_hit, _timer) = cancel_at_deadline(Instant::now(), cancellation.clone());
    tokio::time::timeout(Duration::from_secs(5), cancellation.cancelled())
        .await
        .expect("a passed deadline must cancel immediately");
    assert!(deadline_hit.load(Ordering::SeqCst));
}

#[test]
fn cancelled_notice_reports_iterations() {
    let message = cancelled_notice(2);
    assert!(message.contains("cancelled by user"));
    assert!(message.contains('2'));
}

const HOUR: Duration = Duration::from_secs(3600);

fn review(rounds: u32) -> FinalReview {
    FinalReview {
        enabled: true,
        rounds,
        min_budget: Duration::from_secs(600),
        fresh_context: false,
    }
}

#[test]
fn a_final_review_runs_once_when_the_goal_is_first_reached() {
    let plenty = Duration::from_secs(3600);
    let mut off = ReviewState::new(&FinalReview { enabled: false, ..review(1) }, HOUR);
    assert_eq!(after_goal(&mut off, plenty), None);
    assert_eq!(off.ran, 0);
    assert!(!off.skipped_for_budget);
    let mut state = ReviewState::new(&review(1), HOUR);
    assert_eq!(after_goal(&mut state, plenty).as_deref(), Some(FINAL_REVIEW_MESSAGE));
    assert_eq!(state.ran, 1);
    assert_eq!(after_goal(&mut state, plenty), None, "only once");
}

#[test]
fn final_review_rounds_send_the_review_that_many_times() {
    let plenty = Duration::from_secs(3600);
    let mut state = ReviewState::new(&review(2), HOUR);
    assert_eq!(after_goal(&mut state, plenty).as_deref(), Some(FINAL_REVIEW_MESSAGE));
    assert_eq!(after_goal(&mut state, plenty).as_deref(), Some(FINAL_REVIEW_MESSAGE));
    assert_eq!(after_goal(&mut state, plenty), None);
    assert_eq!(state.ran, 2);
    assert!(!state.skipped_for_budget);
    let mut none = ReviewState::new(&review(0), HOUR);
    assert_eq!(after_goal(&mut none, plenty), None, "zero rounds is off");
    assert!(!none.skipped_for_budget);
}

#[test]
fn a_review_is_skipped_when_too_little_time_is_left() {
    let mut state = ReviewState::new(&review(2), HOUR);
    assert_eq!(after_goal(&mut state, Duration::from_secs(599)), None);
    assert!(state.skipped_for_budget);
    assert_eq!(state.skip_reason, Some("budget"));
    assert_eq!(state.ran, 0);
    assert_eq!(after_goal(&mut state, Duration::from_secs(3600)), None, "a skip ends the review");
    // Enough time for the first round, not the second.
    let mut state = ReviewState::new(&review(2), HOUR);
    assert!(after_goal(&mut state, Duration::from_secs(600)).is_some());
    assert_eq!(after_goal(&mut state, Duration::from_secs(10)), None);
    assert_eq!(state.ran, 1);
    assert!(state.skipped_for_budget);
}

#[test]
fn the_review_message_is_a_general_checklist() {
    for item in [
        "field by field",
        "malformed or missing input",
        "boundary and impossible values",
        "documented defaults and limits",
        "content type",
        "status codes",
        "earlier parts of the task",
        "existing tests",
        "Fix each mismatch with a test",
        "change nothing",
    ] {
        assert!(FINAL_REVIEW_MESSAGE.contains(item), "missing: {item}");
    }
    // General engineering practice, never anything from one benchmark case.
    let lower = FINAL_REVIEW_MESSAGE.to_lowercase();
    for name in ["ckl", "fltplan", "vaops", "recipe", "bughunt", "bug hunt"] {
        assert!(!lower.contains(name), "names a benchmark case: {name}");
    }
}

#[test]
fn files_named_in_a_goal_are_found() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("BRIEF.md"), "x").unwrap();
    std::fs::create_dir(dir.path().join("docs")).unwrap();
    std::fs::write(dir.path().join("docs/api.md"), "x").unwrap();
    let goal = "Build the app in `BRIEF.md`, following docs/api.md (see BRIEF.md). \
                Use e.g. JSON; ignore missing.md and http://example.com/a.md.";
    assert_eq!(
        crate::agent::files_named_in(goal, dir.path()),
        ["BRIEF.md", "docs/api.md"]
    );
}

#[test]
fn the_stop_notice_carries_the_facts_indented_under_it() {
    let facts = crate::run_facts::RunFacts {
        verification: Some(("test".into(), Some(true))),
        changed: Some(vec![]),
        ..Default::default()
    };
    assert_eq!(
        notice_with_facts("autonomous run stopped: goal achieved after 2 iteration(s)", &facts),
        "autonomous run stopped: goal achieved after 2 iteration(s)\n  \
         verification: `test` passed\n  changed: no files"
    );
    let none = crate::run_facts::RunFacts::default();
    assert_eq!(notice_with_facts("stopped", &none), "stopped");
}

#[test]
fn the_report_file_is_private() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("run.json");
    write_report(&path, "{}").unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "{}");
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    // A second run overwrites it.
    write_report(&path, "{\"n\": 2}").unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\"n\": 2}");
}

#[test]
fn goal_achieved_notice_reports_iterations() {
    for verified in [None, Some(true)] {
        let message = goal_achieved_notice(4, verified);
        assert_eq!(message, "autonomous run stopped: goal achieved after 4 iteration(s)");
    }
}

#[test]
fn goal_achieved_notice_says_when_the_last_verification_failed() {
    let message = goal_achieved_notice(2, Some(false));
    assert!(message.starts_with("autonomous run stopped: goal achieved after 2"), "{message}");
    assert!(message.contains("the last verification run failed"), "{message}");
}

#[test]
fn injection_detected_notice_reports_iterations_source_and_pattern() {
    let finding = InjectionFinding {
        source: "read_file: notes.txt".to_string(),
        matched_pattern: "ignore previous instructions".to_string(),
        excerpt: "...IGNORE PREVIOUS INSTRUCTIONS...".to_string(),
    };
    let message = injection_detected_notice(3, &finding);
    assert!(message.contains("possible prompt injection"));
    assert!(message.contains('3'));
    assert!(message.contains("read_file: notes.txt"));
    assert!(message.contains("ignore previous instructions"));
}

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

/// A `run_command` that never finishes on its own: it returns an error
/// once its call is cancelled, as the real one does when killed.
struct HangingRunCommand;

#[async_trait::async_trait]
impl aivyx_tools::Tool for HangingRunCommand {
    fn name(&self) -> &str {
        "run_command"
    }

    fn definition(&self) -> aivyx_types::ToolDefinition {
        aivyx_types::ToolDefinition {
            name: "run_command".to_string(),
            description: "hangs".to_string(),
            parameters_schema: serde_json::json!({ "type": "object" }),
        }
    }

    fn permission_request(
        &self,
        _arguments: &serde_json::Value,
        _cwd: &Path,
    ) -> Result<PermissionRequest, aivyx_tools::ToolError> {
        Ok(PermissionRequest {
            tool_name: "run_command".to_string(),
            action: aivyx_sandbox::ActionKind::Internal,
            target: aivyx_sandbox::PermissionTarget::Other("hang".to_string()),
            arguments_preview: serde_json::json!({}),
            preview: None,
            diff: None,
        })
    }

    async fn execute(
        &self,
        _arguments: serde_json::Value,
        ctx: &aivyx_tools::ToolExecutionContext,
    ) -> Result<aivyx_types::ToolOutput, aivyx_tools::ToolError> {
        tokio::select! {
            _ = ctx.cancellation.cancelled() => {
                Ok(aivyx_types::ToolOutput::Error("cancelled".to_string()))
            }
            _ = tokio::time::sleep(Duration::from_secs(5)) => {
                Ok(aivyx_types::ToolOutput::Ok("exit code 0".to_string()))
            }
        }
    }
}

/// A `run_command` whose check fails its first `fails` runs, then passes.
struct CheckThatFails {
    fails: u32,
    runs: Arc<std::sync::atomic::AtomicU32>,
}

#[async_trait::async_trait]
impl aivyx_tools::Tool for CheckThatFails {
    fn name(&self) -> &str {
        "run_command"
    }

    fn definition(&self) -> aivyx_types::ToolDefinition {
        aivyx_types::ToolDefinition {
            name: "run_command".to_string(),
            description: "a check".to_string(),
            parameters_schema: serde_json::json!({ "type": "object" }),
        }
    }

    fn permission_request(
        &self,
        _arguments: &serde_json::Value,
        _cwd: &Path,
    ) -> Result<PermissionRequest, aivyx_tools::ToolError> {
        Ok(PermissionRequest {
            tool_name: "run_command".to_string(),
            action: aivyx_sandbox::ActionKind::Internal,
            target: aivyx_sandbox::PermissionTarget::Other("check".to_string()),
            arguments_preview: serde_json::json!({}),
            preview: None,
            diff: None,
        })
    }

    async fn execute(
        &self,
        _arguments: serde_json::Value,
        _ctx: &aivyx_tools::ToolExecutionContext,
    ) -> Result<aivyx_types::ToolOutput, aivyx_tools::ToolError> {
        let run = self.runs.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(aivyx_types::ToolOutput::Ok(if run < self.fails {
            "src/app.ts(3,1): error TS2345: wrong type\nexit status: 2 (failed)".to_string()
        } else {
            "exit status: 0 (success)".to_string()
        }))
    }
}

async fn drive_with_check(fails: u32, retries: u32, replies: &[&str]) -> (RunEnd, u32) {
    let mut run = run_with(vec![done_task(1)], 10);
    run.final_checks = vec!["typecheck".to_string()];
    run.final_check_retries = retries;
    let runs = Arc::new(std::sync::atomic::AtomicU32::new(0));
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(CheckThatFails { fails, runs: Arc::clone(&runs) }));
    let (end, _) = drive_with_tools(scripted(replies), registry, &run, &CancellationToken::new()).await;
    (end, runs.load(std::sync::atomic::Ordering::SeqCst))
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
        final_check_retries: 2,
        read_only_paths: Vec::new(),
        report: None,
        final_review: FinalReview::default(),
        model: RunModel::default(),
    }
}

/// Drives `run` against `llm` and returns how it ended plus the iteration
/// numbers `on_iteration` saw.
async fn drive_with(llm: Arc<dyn LlmBackend>, run: &AutonomousRun, stop: &CancellationToken) -> (RunEnd, Vec<u32>) {
    drive_with_tools(llm, ToolRegistry::new(), run, stop).await
}

async fn drive_with_tools(
    llm: Arc<dyn LlmBackend>,
    registry: ToolRegistry,
    run: &AutonomousRun,
    stop: &CancellationToken,
) -> (RunEnd, Vec<u32>) {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let executor = ToolExecutor::new(registry, Arc::new(AllowAll), Arc::new(NoopConfiner));
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
async fn a_signal_during_the_final_checks_ends_the_run_as_cancelled() {
    let dir = tempfile::tempdir().unwrap();
    let mut run = run_with(vec![done_task(1)], 5);
    run.final_checks = vec!["sleep 5".to_string()];
    run.report = Some(dir.path().join("run.json"));
    let stop = CancellationToken::new();
    let canceller = stop.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(500)).await;
        canceller.cancel();
    });
    let started = Instant::now();
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(HangingRunCommand));
    let (end, _) = drive_with_tools(scripted(&["Done."]), registry, &run, &stop).await;
    assert!(started.elapsed() < Duration::from_secs(4), "the check wasn't interrupted");
    assert_eq!(end.outcome, Outcome::Cancelled, "{}", end.notice);
    assert!(end.notice.contains("cancelled during the final checks"), "{}", end.notice);
    let report: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.path().join("run.json")).unwrap()).unwrap();
    assert_eq!(report["outcome"], "cancelled");
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
    assert_eq!(report["report_version"], 1);
    // The scripted reply carries no usage event, and only those are counted.
    assert_eq!(report["model_requests"], 0);
    assert_eq!(report["checkpoints"], 0);
}

#[test]
fn every_outcome_has_its_report_name() {
    assert_eq!(Outcome::GoalAchieved.as_str(), "goal_achieved");
    assert_eq!(Outcome::BudgetExhausted.as_str(), "budget_exhausted");
    assert_eq!(Outcome::Cancelled.as_str(), "cancelled");
    assert_eq!(Outcome::Failed.as_str(), "failed");
    assert_eq!(Outcome::InjectionDetected.as_str(), "injection_detected");
}

// ---- audit 2026-10-28 ----

/// C2: `--report` is written with `create + truncate` and no `O_NOFOLLOW`,
/// by the unconfined aivyx-coder process. A symlink the confined run left at
/// the report path makes it overwrite whatever the link points at.
#[cfg(unix)]
#[test]
fn audit_c2_report_through_a_planted_symlink_does_not_clobber_the_target() {
    let outside = tempfile::tempdir().unwrap();
    let victim = outside.path().join("authorized_keys");
    std::fs::write(&victim, "ssh-ed25519 AAAA-ORIGINAL user@host\n").unwrap();
    let project = tempfile::tempdir().unwrap();
    let report = project.path().join("run.json");
    std::os::unix::fs::symlink(&victim, &report).unwrap();

    let _ = write_report(&report, "{\"report_version\": 1}");

    assert_eq!(
        std::fs::read_to_string(&victim).unwrap(),
        "ssh-ed25519 AAAA-ORIGINAL user@host\n",
        "the report overwrote the file the symlink pointed at"
    );
}

// ---- the final review over a real repository ----

async fn init_git_repo(dir: &Path) {
    for argv in [
        vec!["init", "-q", "-b", "main"],
        vec!["config", "user.name", "test"],
        vec!["config", "user.email", "test@test.invalid"],
        vec!["add", "-A"],
        vec!["commit", "-q", "-m", "initial"],
    ] {
        tokio::process::Command::new("git").args(&argv).current_dir(dir).output().await.unwrap();
    }
}

fn write_call(id: &str, path: &str, content: &str) -> Vec<StreamEvent> {
    vec![
        StreamEvent::ToolCallComplete(aivyx_types::ToolCall {
            id: aivyx_types::ToolCallId(id.to_string()),
            name: "write_file".to_string(),
            arguments: serde_json::json!({ "path": path, "content": content }),
            source: aivyx_types::ToolCallSource::Native,
        }),
        StreamEvent::Done { finish_reason: FinishReason::ToolCalls },
    ]
}

/// Denies reads of any path ending in `secret.txt`, as `deny_paths` does.
struct DenySecrets;

#[async_trait::async_trait]
impl PermissionGate for DenySecrets {
    async fn check(&self, request: &PermissionRequest) -> PermissionDecision {
        match &request.target {
            aivyx_sandbox::PermissionTarget::Path(path) if path.ends_with("secret.txt") => {
                PermissionDecision::Deny(Some("denied by deny_paths".to_string()))
            }
            _ => PermissionDecision::Allow,
        }
    }
}

/// An agent over the git repository at `dir` with read/write tools and a
/// passing `[verification] command` that prints `VERIFY-MARKER`, driven
/// through `run` with `responses`. Returns the agent for inspection.
async fn drive_in_repo(dir: &Path, responses: Vec<Vec<StreamEvent>>, run: &AutonomousRun) -> (Agent, RunEnd) {
    drive_in_repo_with(dir, responses, run, RepoOptions::default()).await
}

struct RepoOptions {
    context_tokens: u32,
    /// The `[verification] command`'s shell script.
    verify: &'static str,
    session: Option<PathBuf>,
}

impl Default for RepoOptions {
    fn default() -> Self {
        Self { context_tokens: 8192, verify: "echo VERIFY-MARKER", session: None }
    }
}

async fn drive_in_repo_with(
    dir: &Path,
    responses: Vec<Vec<StreamEvent>>,
    run: &AutonomousRun,
    options: RepoOptions,
) -> (Agent, RunEnd) {
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(aivyx_tools::ReadFileTool));
    registry.register(Arc::new(aivyx_tools::WriteFileTool));
    registry.register(Arc::new(aivyx_tools::RunCommandTool::new(vec![aivyx_tools::CommandSpec {
        name: "verify".to_string(),
        program: "sh".to_string(),
        args: vec!["-c".to_string(), options.verify.to_string()],
        timeout: Duration::from_secs(5),
    }])));
    let mut executor = ToolExecutor::new(registry, Arc::new(DenySecrets), Arc::new(NoopConfiner));
    executor.set_checkpointer_at(
        dir.to_path_buf(),
        vec![],
        Arc::new(aivyx_tools::GitCheckpointer::detect(dir, vec![]).await.unwrap()),
    );
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let mut agent = Agent::new(
        Arc::new(ScriptedBackend { responses: Mutex::new(responses.into()) }),
        executor,
        "system",
        crate::AgentConfig {
            max_tool_iterations: 10,
            context_tokens: options.context_tokens,
            ..Default::default()
        },
        Arc::clone(&run.tasks),
        PlanMode::new(),
        AutonomousMode::new(),
        tx,
    );
    agent.set_verification("verify".to_string(), 1);
    if let Some(path) = options.session {
        agent.set_session_path(path);
    }
    let active = Mutex::new(None);
    let end = drive(&mut agent, run, dir, &CancellationToken::new(), &active, &mut |_| {}).await;
    (agent, end)
}

#[tokio::test]
async fn a_fresh_context_review_starts_from_the_goal_files_diff_and_verification() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().canonicalize().unwrap();
    std::fs::write(cwd.join("BRIEF.md"), "SPEC-MARKER: the API returns JSON\n").unwrap();
    std::fs::write(cwd.join("secret.txt"), "SECRET-MARKER\n").unwrap();
    init_git_repo(&cwd).await;
    let mut run = run_with(vec![done_task(1)], 5);
    run.goal = "Build the app in BRIEF.md; the key is in secret.txt.".to_string();
    run.max_duration = Duration::from_secs(3600);
    let out = tempfile::tempdir().unwrap();
    run.report = Some(out.path().join("run.json"));
    run.final_review = FinalReview { fresh_context: true, ..review(1) };
    let (agent, end) = drive_in_repo(
        &cwd,
        vec![
            write_call("c1", "app.rs", "DIFF-MARKER\n"),
            text("OLD-CONVERSATION-MARKER done"),
            text("Reviewed; everything matches."),
        ],
        &run,
    )
    .await;
    assert_eq!(end.outcome, Outcome::GoalAchieved, "{}", end.notice);
    assert_eq!(end.iterations, 2);

    let history = agent.history_snapshot();
    let all: String = history.iter().map(|m| m.text_content()).collect::<Vec<_>>().join("\n");
    assert!(!all.contains("OLD-CONVERSATION-MARKER"), "the earlier conversation was kept: {all}");
    assert_eq!(history.len(), 2, "one user message and the reply: {history:#?}");
    assert_eq!(history[0].role, aivyx_types::Role::User);
    let opening = history[0].text_content();
    for marker in [
        run.goal.as_str(),
        "SPEC-MARKER: the API returns JSON",
        "DIFF-MARKER",
        "app.rs",
        "VERIFY-MARKER",
        FINAL_REVIEW_MESSAGE,
    ] {
        assert!(opening.contains(marker), "missing {marker:?} in:\n{opening}");
    }
    assert!(!opening.contains("SECRET-MARKER"), "a denied file was read: {opening}");
    // Nothing else about the run was reset.
    assert_eq!(run.tasks.lock().unwrap().len(), 1);

    let report_path = run.report.as_ref().unwrap();
    let report: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(report_path).unwrap()).unwrap();
    assert_eq!(report["final_review"]["ran"], 1);
    assert_eq!(report["final_review"]["skipped_for_budget"], false);
    assert_eq!(report["final_review"]["changed_files"], serde_json::json!([]));
}

#[tokio::test]
async fn the_report_lists_the_files_the_review_changed() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().canonicalize().unwrap();
    std::fs::write(cwd.join("README.md"), "x\n").unwrap();
    init_git_repo(&cwd).await;
    let mut run = run_with(vec![done_task(1)], 5);
    run.max_duration = Duration::from_secs(3600);
    let out = tempfile::tempdir().unwrap();
    run.report = Some(out.path().join("run.json"));
    run.final_review = review(1);
    let (agent, end) = drive_in_repo(
        &cwd,
        vec![
            write_call("c1", "app.rs", "first\n"),
            text("done"),
            write_call("c2", "fix.rs", "fixed\n"),
            text("fixed a mismatch"),
        ],
        &run,
    )
    .await;
    assert_eq!(end.outcome, Outcome::GoalAchieved, "{}", end.notice);
    assert!(
        agent.history_snapshot().iter().any(|m| m.text_content().contains("done")),
        "without fresh_context the conversation is kept"
    );
    let report_path = run.report.as_ref().unwrap();
    let report: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(report_path).unwrap()).unwrap();
    assert_eq!(report["final_review"]["ran"], 1);
    assert_eq!(report["final_review"]["changed_files"], serde_json::json!(["fix.rs"]));
}

#[test]
fn the_budget_guard_is_at_most_a_quarter_of_the_run() {
    assert_eq!(ReviewState::new(&review(1), HOUR).min_budget, Duration::from_secs(600));
    assert_eq!(
        ReviewState::new(&review(1), Duration::from_secs(1200)).min_budget,
        Duration::from_secs(300)
    );
    let mut short = ReviewState::new(&review(1), Duration::from_secs(120));
    assert_eq!(short.min_budget, Duration::from_secs(30));
    assert!(after_goal(&mut short, Duration::from_secs(100)).is_some(), "a short run still reviews");
}

#[tokio::test]
async fn a_short_run_still_reviews_and_reports_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut run = run_with(vec![done_task(1)], 5);
    run.report = Some(dir.path().join("run.json"));
    // run_with gives the run 60 s: under the 600 s minimum, but the guard
    // is then a quarter of the run (15 s).
    run.final_review = review(1);
    let (end, _) = drive_with(scripted(&["Done.", "Reviewed."]), &run, &CancellationToken::new()).await;
    assert_eq!(end.iterations, 2);
    let report: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.path().join("run.json")).unwrap()).unwrap();
    assert_eq!(report["final_review"]["ran"], 1);
    assert_eq!(report["final_review"]["skipped_for_budget"], false);
}

#[tokio::test]
async fn a_fresh_review_message_fits_a_quarter_of_the_context_window() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let executor = ToolExecutor::new(ToolRegistry::new(), Arc::new(AllowAll), Arc::new(NoopConfiner));
    let mut agent = Agent::new(
        scripted(&[]),
        executor,
        "system",
        crate::AgentConfig { context_tokens: 8192, ..Default::default() },
        Arc::default(),
        PlanMode::new(),
        AutonomousMode::new(),
        tx,
    );
    let goal = "Build the app described in BRIEF.md.";
    let context = crate::agent::ReviewContext {
        goal: goal.to_string(),
        files: (0..4).map(|i| (format!("f{i}.md"), "spec line\n".repeat(6_500))).collect(),
        diff: Some("+code\n".repeat(11_000)),
        last_verification: Some("test output\n".repeat(1_400)),
    };
    let message = agent.replace_history_for_review(context, FINAL_REVIEW_MESSAGE);
    assert!(agent.estimate_tokens(&message) < 2048, "{} tokens", agent.estimate_tokens(&message));
    assert!(message.contains(goal));
    assert!(message.contains(FINAL_REVIEW_MESSAGE));
    assert!(message.contains("bytes cut"), "the cut is marked");
    assert!(agent.history_snapshot().is_empty());
}

#[tokio::test]
async fn a_fresh_review_keeps_the_whole_spec_and_leaves_lockfile_bodies_out() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().canonicalize().unwrap();
    let brief: String =
        (0..320).map(|i| format!("SPEC line {i:03}: the field is documented here.\n")).collect();
    assert!(brief.len() > 13 * 1024);
    std::fs::write(cwd.join("BRIEF.md"), &brief).unwrap();
    init_git_repo(&cwd).await;
    let lock = "LOCKBODY checksum = \"abc\"\n".repeat(20 * 1024 / 26);
    let src = "fn generated() -> u32 { 42 } // SRC\n".repeat(60 * 1024 / 36);
    let mut run = run_with(vec![done_task(1)], 5);
    run.goal = "Build the app in BRIEF.md.".to_string();
    run.max_duration = Duration::from_secs(3600);
    run.final_review = FinalReview { fresh_context: true, ..review(1) };
    let (agent, end) = drive_in_repo_with(
        &cwd,
        vec![
            write_call("c1", "src/lib.rs", &src),
            write_call("c2", "Cargo.lock", &lock),
            text("done"),
            text("Reviewed."),
        ],
        &run,
        RepoOptions { context_tokens: 65536, ..RepoOptions::default() },
    )
    .await;
    assert_eq!(end.outcome, Outcome::GoalAchieved, "{}", end.notice);
    let opening = agent.history_snapshot()[0].text_content();
    assert!(opening.contains(&brief), "the spec was cut");
    assert!(!opening.contains("LOCKBODY"), "a lockfile body was included");
    assert!(opening.contains("Cargo.lock"), "the lockfile is listed in the summary");
    assert!(opening.contains("src/lib.rs"));
    assert!(opening.contains(FINAL_REVIEW_MESSAGE));
    assert!(agent.estimate_tokens(&opening) <= 65536 / 4);
}

#[tokio::test]
async fn no_review_runs_while_the_last_verification_failed() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().canonicalize().unwrap();
    std::fs::write(cwd.join("README.md"), "x\n").unwrap();
    init_git_repo(&cwd).await;
    let mut run = run_with(vec![done_task(1)], 5);
    run.max_duration = Duration::from_secs(3600);
    let out = tempfile::tempdir().unwrap();
    run.report = Some(out.path().join("run.json"));
    run.final_review = review(1);
    let (_, end) = drive_in_repo_with(
        &cwd,
        vec![write_call("c1", "app.rs", "x\n"), text("done"), text("never")],
        &run,
        RepoOptions { verify: "echo failing; exit 1", ..RepoOptions::default() },
    )
    .await;
    assert_eq!(end.last_verification_passed, Some(false));
    assert_eq!(end.iterations, 1, "{}", end.notice);
    let report: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(out.path().join("run.json")).unwrap()).unwrap();
    assert_eq!(report["final_review"]["ran"], 0);
    assert_eq!(report["final_review"]["skipped_for_budget"], false);
    assert_eq!(report["final_review"]["skip_reason"], "verification_failed");
}

#[tokio::test]
async fn a_fresh_review_keeps_the_saved_session_whole() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().canonicalize().unwrap();
    std::fs::write(cwd.join("README.md"), "x\n").unwrap();
    init_git_repo(&cwd).await;
    let state_dir = tempfile::tempdir().unwrap();
    let session = state_dir.path().join("session.json");
    let mut run = run_with(vec![done_task(1)], 5);
    run.max_duration = Duration::from_secs(3600);
    run.final_review = FinalReview { fresh_context: true, ..review(1) };
    let (agent, _) = drive_in_repo_with(
        &cwd,
        vec![text("OLD-CONVERSATION-MARKER done"), text("REVIEW-REPLY-MARKER")],
        &run,
        RepoOptions { session: Some(session.clone()), ..RepoOptions::default() },
    )
    .await;
    assert_eq!(agent.history_snapshot().len(), 2, "the live conversation is the review");
    let saved = crate::session::load(&session).expect("the session was saved");
    let all: String = saved.history.iter().map(|m| m.text_content()).collect::<Vec<_>>().join("\n");
    assert!(all.contains("OLD-CONVERSATION-MARKER"), "the run's conversation was lost: {all}");
    assert!(all.contains("REVIEW-REPLY-MARKER"), "the review was not saved: {all}");
}

#[tokio::test]
async fn a_failing_final_check_goes_back_to_the_model_and_the_run_ends_when_it_passes() {
    let (end, check_runs) = drive_with_check(1, 2, &["Done.", "Fixed the type."]).await;
    assert_eq!(end.outcome, Outcome::GoalAchieved, "{}", end.notice);
    assert_eq!(end.iterations, 2, "one more turn for the failing check");
    assert_eq!(check_runs, 2, "checked again after the fix, and not a third time for the report");
    assert!(end.final_checks_passed, "{}", end.notice);
}

#[tokio::test]
async fn final_checks_go_back_at_most_final_check_retries_times() {
    let (end, check_runs) = drive_with_check(u32::MAX, 2, &["Done.", "Tried.", "Tried again."]).await;
    assert_eq!(end.outcome, Outcome::GoalAchieved, "{}", end.notice);
    assert_eq!(end.iterations, 3);
    assert_eq!(check_runs, 3);
    assert!(!end.final_checks_passed);
}

#[tokio::test]
async fn with_no_retries_final_checks_only_report() {
    let (end, check_runs) = drive_with_check(u32::MAX, 0, &["Done."]).await;
    assert_eq!(end.iterations, 1);
    assert_eq!(check_runs, 1);
    assert!(!end.final_checks_passed);
}

#[test]
fn the_failed_checks_message_names_each_failure_with_its_output() {
    let check = |name: &str, passed, output: &str| crate::run_facts::FinalCheck {
        name: name.into(),
        passed,
        warnings: 0,
        output: output.into(),
    };
    let message = final_checks_failed_message(&[
        check("typecheck", false, "src/a.ts(1,1): error TS1\n"),
        check("lint", true, "all good"),
        check("build", false, "error: boom"),
    ]);
    assert!(message.starts_with("The goal is not done yet"), "{message}");
    assert!(message.contains("`typecheck` failed") && message.contains("error TS1"), "{message}");
    assert!(message.contains("`build` failed") && message.contains("error: boom"), "{message}");
    assert!(!message.contains("lint") && !message.contains("all good"), "{message}");
}
