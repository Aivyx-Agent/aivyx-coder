# Autonomous `goal_achieved()` Team-Awareness Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Autonomous mode's stop signal (`goal_achieved()`) recognizes mission-only and specialist-session-based work, not just the `set_tasks` list, so a team-only autonomous run stops cleanly instead of running to its iteration budget.

**Architecture:** `agent_builder.rs` surfaces two already-constructed-but-never-exposed pieces of shared state (`Arc<Mutex<MissionPlan>>`, `SpecialistSessionPool`) on `BuiltAgent`; `main.rs` threads them into `AutonomousRun` exactly like the existing `tasks` field; `aivyx-tui/src/app.rs`'s `goal_achieved()` becomes 3-argument, combining independent per-source signals.

**Tech Stack:** Rust, `tokio`, existing `aivyx-core`/`aivyx-types`/`aivyx-tui` crates.

## Global Constraints

- `[team] enabled = false` behavior must be byte-identical to today — the 4 existing `goal_achieved`/`next_autonomous_message` unit tests' assertions must pass unmodified (only their call-site arguments change).
- An open specialist session always blocks completion, regardless of task/mission state.
- No change to `AgentEvent::MissionsUpdated`/`SpecialistSessionsUpdated`, the TUI Mission panel, or ACP's merged-Plan translation — this reads shared state directly, not through the event/TUI-mirror path.
- No change to `verify_output`'s `StepStatus` semantics — mission completion is `mission_plan.summary.is_some()` only, not step-status-dependent.
- ACP/MCP-server frontends are untouched — `--auto` isn't supported there today.
- File-scoped `rustfmt --edition 2024 --check <path>` / `rustfmt --edition 2024 <path>` only — **never** a package-scoped `cargo fmt -p <crate>` command with no file argument, per this project's own repeated, documented incident history.

---

### Task 1: Surface mission/specialist state through `BuiltAgent` and `AutonomousRun`, teach `goal_achieved()` about both

**Files:**
- Modify: `crates/aivyx/src/agent_builder.rs` (`BuiltAgent` struct, its construction, the `if settings.team.enabled` block)
- Modify: `crates/aivyx/src/main.rs` (`AutonomousRun` construction)
- Modify: `crates/aivyx-tui/src/app.rs` (`AutonomousRun` struct, imports, `goal_achieved`/`next_autonomous_message`, the driver loop call site, tests)

**Interfaces:** none — this is the whole deliverable, no later task consumes it.

- [ ] **Step 1: Add the two new fields to `BuiltAgent`**

In `crates/aivyx/src/agent_builder.rs`, find this exact block (the end of the `BuiltAgent` struct definition):

```rust
    pub(crate) mcp_registry: ToolRegistry,
    /// The resolved `deny_paths` this function computed and used
    /// throughout its own body (sandbox confiner, checkpointer, repo map,
    /// tool constructors) — exposed so a frontend never has to recompute
    /// `settings.effective_deny_paths()` a second time independently,
    /// which could silently diverge if this function's own local
    /// `deny_paths` is ever augmented further.
    pub(crate) deny_paths: Vec<PathBuf>,
}
```

Replace it with:

```rust
    pub(crate) mcp_registry: ToolRegistry,
    /// The resolved `deny_paths` this function computed and used
    /// throughout its own body (sandbox confiner, checkpointer, repo map,
    /// tool constructors) — exposed so a frontend never has to recompute
    /// `settings.effective_deny_paths()` a second time independently,
    /// which could silently diverge if this function's own local
    /// `deny_paths` is ever augmented further.
    pub(crate) deny_paths: Vec<PathBuf>,
    /// The live, shared mission-plan state `decompose_task`/`verify_output`/
    /// `synthesize_results` write into, when `[team] enabled = true` --
    /// `None` when the team feature is off (mirrors `tasks` above, but
    /// `Option`-wrapped since this state doesn't exist at all in that
    /// configuration). Lets a frontend's autonomous driver loop poll
    /// mission-completion state directly, without going through the
    /// `AgentEvent::MissionsUpdated`/TUI-mirror path.
    pub(crate) mission_plan: Option<Arc<std::sync::Mutex<aivyx_types::MissionPlan>>>,
    /// The live, shared specialist-session pool `spawn_specialist`/
    /// `query_specialist`/`close_specialist` operate on, when `[team]
    /// enabled = true` -- `None` when the team feature is off. Cheap to
    /// clone (internally `Arc`-wrapped) and query via `.open_sessions()`.
    pub(crate) specialist_session_pool: Option<aivyx_core::SpecialistSessionPool>,
}
```

- [ ] **Step 2: Declare the two local variables, defaulted to `None`, alongside `tasks`**

In `crates/aivyx/src/agent_builder.rs`, find this exact block:

```rust
    // One handle shared between the `set_tasks` tool (the model-facing
    // mutator) and the agent (which renders and persists the list).
    let tasks: Arc<std::sync::Mutex<Vec<session::Task>>> = Arc::default();
```

Replace it with:

```rust
    // One handle shared between the `set_tasks` tool (the model-facing
    // mutator) and the agent (which renders and persists the list).
    let tasks: Arc<std::sync::Mutex<Vec<session::Task>>> = Arc::default();

    // Populated below inside `if settings.team.enabled`, stay `None`
    // otherwise -- surfaced on `BuiltAgent` so a frontend's autonomous
    // driver loop can poll real mission/specialist-session state (see
    // `goal_achieved` in `aivyx-tui/src/app.rs`).
    let mut mission_plan: Option<Arc<std::sync::Mutex<aivyx_types::MissionPlan>>> = None;
    let mut specialist_session_pool: Option<aivyx_core::SpecialistSessionPool> = None;
```

- [ ] **Step 3: Clone the mission-plan `Arc` and the specialist pool before each is moved into its tool config**

In `crates/aivyx/src/agent_builder.rs`, find this exact block:

```rust
        let mission_plan = Arc::new(std::sync::Mutex::new(aivyx_types::MissionPlan {
            mission: String::new(),
            steps: vec![],
            summary: None,
        }));
        let mission_tools_config = aivyx_core::MissionToolsConfig {
            team: team.clone(),
            plan: mission_plan,
            events_tx: events_tx.clone(),
        };
```

Replace it with:

```rust
        let mission_plan_handle = Arc::new(std::sync::Mutex::new(aivyx_types::MissionPlan {
            mission: String::new(),
            steps: vec![],
            summary: None,
        }));
        mission_plan = Some(Arc::clone(&mission_plan_handle));
        let mission_tools_config = aivyx_core::MissionToolsConfig {
            team: team.clone(),
            plan: mission_plan_handle,
            events_tx: events_tx.clone(),
        };
```

Then, in the same file, find this exact block:

```rust
        let specialist_session_pool = aivyx_core::SpecialistSessionPool::new(
            settings.team.max_concurrent_specialist_sessions,
            std::time::Duration::from_secs(settings.team.specialist_session_idle_timeout_secs),
        );
        let specialist_sessions_config = aivyx_core::SpecialistSessionsConfig {
```

Replace it with:

```rust
        let specialist_session_pool_handle = aivyx_core::SpecialistSessionPool::new(
            settings.team.max_concurrent_specialist_sessions,
            std::time::Duration::from_secs(settings.team.specialist_session_idle_timeout_secs),
        );
        specialist_session_pool = Some(specialist_session_pool_handle.clone());
        let specialist_sessions_config = aivyx_core::SpecialistSessionsConfig {
```

Then, still in the same file, find this exact block (the `pool:` field of `specialist_sessions_config`, near the end of the `if settings.team.enabled` block):

```rust
            max_iterations: settings.sub_agent.max_iterations,
            broker_mode,
            pool: specialist_session_pool,
        };
```

Replace it with:

```rust
            max_iterations: settings.sub_agent.max_iterations,
            broker_mode,
            pool: specialist_session_pool_handle,
        };
```

- [ ] **Step 4: Add the two fields to the final `BuiltAgent` construction**

In `crates/aivyx/src/agent_builder.rs`, find this exact block:

```rust
    Ok(BuiltAgent {
        agent,
        events_rx,
        cwd,
        plan_mode,
        restored,
        tasks,
        injection_taint,
        repl_resize,
        llm,
        confiner,
        checkpointer,
        repo_map,
        kv_cache_handles,
        mcp_registry,
        deny_paths: deny_paths.clone(),
    })
```

Replace it with:

```rust
    Ok(BuiltAgent {
        agent,
        events_rx,
        cwd,
        plan_mode,
        restored,
        tasks,
        injection_taint,
        repl_resize,
        llm,
        confiner,
        checkpointer,
        repo_map,
        kv_cache_handles,
        mcp_registry,
        deny_paths: deny_paths.clone(),
        mission_plan,
        specialist_session_pool,
    })
```

- [ ] **Step 5: Verify `agent_builder.rs` compiles in isolation**

Run: `cargo check -p aivyx`
Expected: compiles cleanly (warnings about unused `mission_plan`/`specialist_session_pool` fields on `BuiltAgent` are expected and fine at this point — Step 7 below consumes them, resolving the warning).

- [ ] **Step 6: Add the two new fields to `aivyx-tui`'s `AutonomousRun` struct**

In `crates/aivyx-tui/src/app.rs`, find this exact block:

```rust
pub struct AutonomousRun {
    pub goal: String,
    pub max_iterations: u32,
    pub max_duration: Duration,
    pub tasks: Arc<Mutex<Vec<Task>>>,
    pub injection_taint: InjectionTaint,
}
```

Replace it with:

```rust
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
    pub specialist_session_pool: Option<aivyx_core::SpecialistSessionPool>,
}
```

- [ ] **Step 7: Thread the two fields through in `main.rs`**

In `crates/aivyx/src/main.rs`, find this exact block:

```rust
    let autonomous_run = cli.auto.map(|goal| aivyx_tui::AutonomousRun {
        goal,
        max_iterations: settings.autonomous.max_iterations,
        max_duration: Duration::from_secs(settings.autonomous.max_duration_secs),
        tasks: Arc::clone(&built.tasks),
        injection_taint: built.injection_taint.clone(),
    });
```

Replace it with:

```rust
    let autonomous_run = cli.auto.map(|goal| aivyx_tui::AutonomousRun {
        goal,
        max_iterations: settings.autonomous.max_iterations,
        max_duration: Duration::from_secs(settings.autonomous.max_duration_secs),
        tasks: Arc::clone(&built.tasks),
        injection_taint: built.injection_taint.clone(),
        mission_plan: built.mission_plan.clone(),
        specialist_session_pool: built.specialist_session_pool.clone(),
    });
```

Note: this clones the `Option` before `built` is otherwise consumed further down in the same function (`built.agent`, `built.events_rx`, etc. are moved into `aivyx_tui::run(...)` afterward) — cloning an `Option<Arc<..>>`/`Option<SpecialistSessionPool>` is cheap (both are reference-counted handles), so this does not need to be the last read of `built`.

- [ ] **Step 8: Rewrite `goal_achieved()` as 3-argument, update `next_autonomous_message()`**

In `crates/aivyx-tui/src/app.rs`, find this exact block:

```rust
/// The autonomous driver's goal-achieved signal: every task in the list is
/// `Done`, and there is at least one task — an empty list means the model
/// never called `set_tasks` at all, which must not be misread as "nothing
/// to do, stop immediately." See ROADMAP.md Phase 11c.
fn goal_achieved(tasks: &[Task]) -> bool {
    !tasks.is_empty() && tasks.iter().all(|t| t.status == TaskStatus::Done)
}

/// What the autonomous driver sends next, given whether the turn that just
/// finished paused (Phase 12A) and the current task list. `None` means
/// stop the loop (goal achieved) — the caller is responsible for the
/// separate budget-exhaustion and cancellation stop conditions, which this
/// function doesn't know about.
fn next_autonomous_message(last_turn_paused: bool, tasks: &[Task]) -> Option<String> {
    if last_turn_paused {
        return Some("continue".to_string());
    }
    if goal_achieved(tasks) {
        return None;
    }
    Some("continue working toward the goal".to_string())
}
```

Replace it with:

```rust
/// The autonomous driver's goal-achieved signal, combining three
/// independent sources: the `set_tasks` list, `MissionPlan` (Nonagon team
/// missions), and open specialist sessions. Each of the first two
/// contributes a signal only if it was ever *used* — an empty task list or
/// a `None` mission plan means that surface was never engaged, so it must
/// not count as "nothing to do, stop immediately" (an unused signal is a
/// no-op, not a blocker). An open specialist session always blocks
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
    let mission_signal = mission_plan.map(|p| p.summary.is_some());
    match (tasks_signal, mission_signal) {
        (None, None) => false,
        _ => tasks_signal.unwrap_or(true) && mission_signal.unwrap_or(true),
    }
}

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
    Some("continue working toward the goal".to_string())
}
```

- [ ] **Step 9: Update the driver loop's call site to pass the new snapshots**

In `crates/aivyx-tui/src/app.rs`, find this exact block (inside the autonomous driver loop in `run()`):

```rust
                let tasks_snapshot = autonomous.tasks.lock().unwrap().clone();
                next_message = next_autonomous_message(agent.last_turn_paused(), &tasks_snapshot);
                if next_message.is_none() {
                    agent.notify(goal_achieved_notice(iterations_used));
                }
```

Replace it with:

```rust
                let tasks_snapshot = autonomous.tasks.lock().unwrap().clone();
                let mission_plan_snapshot = autonomous
                    .mission_plan
                    .as_ref()
                    .map(|p| p.lock().unwrap().clone());
                let open_specialist_sessions = autonomous
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
                if next_message.is_none() {
                    agent.notify(goal_achieved_notice(iterations_used));
                }
```

- [ ] **Step 10: Update the 2 existing tests' call sites (assertions stay unchanged)**

In `crates/aivyx-tui/src/app.rs`, find this exact block:

```rust
    #[test]
    fn goal_achieved_requires_at_least_one_task_and_all_done() {
        assert!(!goal_achieved(&[]), "no tasks ever set means never done");
        assert!(!goal_achieved(&[done_task(1), pending_task(2)]));
        assert!(goal_achieved(&[done_task(1), done_task(2)]));
    }

    #[test]
    fn next_autonomous_message_chooses_correctly() {
        assert_eq!(
            next_autonomous_message(true, &[]),
            Some("continue".to_string()),
            "a paused turn always continues, regardless of task state"
        );
        assert_eq!(
            next_autonomous_message(false, &[done_task(1)]),
            None,
            "goal achieved -> stop"
        );
        assert_eq!(
            next_autonomous_message(false, &[pending_task(1)]),
            Some("continue working toward the goal".to_string())
        );
        assert_eq!(
            next_autonomous_message(false, &[]),
            Some("continue working toward the goal".to_string()),
            "no tasks ever set -> keep going until budget exhausts, not stuck forever"
        );
    }
```

Replace it with:

```rust
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
            Some("continue working toward the goal".to_string()),
            "no tasks ever set -> keep going until budget exhausts, not stuck forever"
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
```

- [ ] **Step 11: Run the affected tests**

Run: `cargo test -p aivyx-tui goal_achieved -- --nocapture` and `cargo test -p aivyx-tui next_autonomous_message -- --nocapture`
Expected: all tests pass — the 2 original tests (assertions unchanged), plus the 4 new tests covering mission-only completion, combined task+mission requirements, the specialist-session hard block, and the team-disabled equivalence check.

- [ ] **Step 12: Format the exact files touched, and build/test/lint the full workspace**

Run:
```bash
rustfmt --edition 2024 crates/aivyx/src/agent_builder.rs
rustfmt --edition 2024 crates/aivyx/src/main.rs
rustfmt --edition 2024 crates/aivyx-tui/src/app.rs
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets
```
Expected: `rustfmt` reports no diff needed (or applies only whitespace consistent with the code above); `cargo build`/`cargo test` succeed with zero failures; `cargo clippy` reports zero warnings (confirming the two new `BuiltAgent` fields are no longer unused, resolving Step 5's expected warning).

- [ ] **Step 13: Commit**

```bash
git add crates/aivyx/src/agent_builder.rs crates/aivyx/src/main.rs crates/aivyx-tui/src/app.rs
git commit -m "fix: autonomous goal_achieved() recognizes mission and specialist-session state"
```

---

## Self-Review Notes

**Spec coverage:** Decision 1 (`BuiltAgent`'s two new `Option` fields, populated inside the existing `if settings.team.enabled` block via clone-before-move) → Steps 1-4. Decision 2 (`AutonomousRun`'s identical two fields, threaded from `main.rs`) → Steps 6-7. Decision 3 (the 3-argument `goal_achieved()`, the specialist-session hard block, the per-source no-op-when-unused semantics) → Step 8. Decision 4 (exact backward compatibility for `[team] enabled = false`, proven by the unchanged original 4 assertions plus a new dedicated equivalence test) → Steps 10 (unchanged assertions) and the new `goal_achieved_team_disabled_matches_original_behavior_exactly` test. "What this spec does not decide" items are all genuinely untouched: no `AgentEvent`/TUI-panel/ACP changes, no `StepStatus` semantic change, no ACP/MCP-server autonomous-mode work.

**Global Constraints deviation:** none — this plan implements the spec's decisions directly, uses only file-scoped `rustfmt`, and touches no frontend beyond `aivyx-tui`/`main.rs`/`agent_builder.rs`.

**Placeholder scan:** no TBD/TODO; every step shows complete, real code (full struct definitions, full function bodies, full test code); no "similar to Task N" references (single-task plan).

**Type/interface consistency check:** `MissionPlan`/`MissionStep`/`StepStatus`/`SpecialistSessionSummary` are already imported in `aivyx-tui/src/app.rs` (confirmed via the file's existing `use aivyx_core::{...}`/`use aivyx_types::{...}` blocks) — no new `use` lines are needed for the test code in Step 10. `aivyx_core::SpecialistSessionPool` and `aivyx_types::MissionPlan` are referenced via their full paths in `agent_builder.rs` (matching that file's existing style, e.g. `aivyx_types::MissionPlan { .. }` at the mission-plan construction site), consistent with Steps 1-4. The `goal_achieved`/`next_autonomous_message` signatures used in Step 9's driver-loop call site match exactly what Step 8 defines (3 and 4 arguments respectively, same parameter order, same types).
