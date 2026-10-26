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

#[test]
fn a_final_review_runs_once_when_the_goal_is_first_reached() {
    let mut reviewed = false;
    assert_eq!(after_goal(false, &mut reviewed), None);
    assert!(!reviewed);
    assert_eq!(after_goal(true, &mut reviewed).as_deref(), Some(FINAL_REVIEW_MESSAGE));
    assert!(reviewed);
    assert_eq!(after_goal(true, &mut reviewed), None, "only once");
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
        model: RunModel::default(),
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
