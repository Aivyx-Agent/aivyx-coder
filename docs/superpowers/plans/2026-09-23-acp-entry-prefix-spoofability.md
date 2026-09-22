# ACP Entry-Prefix Spoofability Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A model-controlled string (`task.text`, a mission's `mission` description, or a mission step's `task` description) can no longer forge a fake `[Specialist: ...]`/`[Mission: ...]`/`[Task]`-prefixed entry inside a *different* ACP Plan entry by embedding a newline.

**Architecture:** A small, local `strip_control_chars_for_display` helper (mirroring `aivyx-core`'s existing `sanitize_for_display`, duplicated rather than cross-crate-exported per this project's established small-helper precedent) replaces every control character (`\n`/`\r`/tab/etc.) in the three free-form fields `build_merged_plan` interpolates into `PlanEntry.content` with U+FFFD, before formatting.

**Tech Stack:** Rust, `aivyx-acp` crate only.

## Global Constraints

- `session.member` is never passed through the new sanitization helper anywhere — it's already roster-validated, never free-form model text.
- No change to `set_tasks`/`decompose_task`'s own input validation — this fix is scoped to what gets rendered into the ACP Plan panel, not to what the tools accept.
- No TUI change — confirmed unnecessary by direct empirical test during design (ratatui silently drops embedded `\n` when building a `Line` from a `String`, and neither the task panel nor the mission panel wraps, so there's no equivalent attack surface there).
- No length truncation added — this fix addresses the structure-forging vector (control characters), not general entry-length UX.
- File-scoped `rustfmt --edition 2024 <path>` only.

---

### Task 1: Sanitize model-controlled text in `build_merged_plan`

**Files:**
- Modify: `crates/aivyx-acp/src/translate.rs`

**Interfaces:**
- Produces: `strip_control_chars_for_display(raw: &str) -> String` (private to this file) — used only within `build_merged_plan`.

- [ ] **Step 1: Write the failing tests**

In `crates/aivyx-acp/src/translate.rs`'s `#[cfg(test)] mod tests` block, find the existing test `merged_plan_with_no_sources_is_empty` (search for it) and add these three tests immediately after it:

```rust
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
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p aivyx-acp merged_plan_strips_control_characters -- --nocapture`
Expected: all three FAIL — the assertions expect a U+FFFD-substituted string, but `build_merged_plan` currently interpolates `task.text`/`plan.mission`/`step.task` verbatim, so the actual content still contains a literal `\n`.

- [ ] **Step 3: Add the sanitization helper**

In `crates/aivyx-acp/src/translate.rs`, find this exact block:

```rust
fn text_chunk(text: String) -> ContentChunk {
    ContentChunk::new(ContentBlock::Text(TextContent::new(text)))
}
```

Replace it with:

```rust
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
```

- [ ] **Step 4: Apply the helper inside `build_merged_plan`**

Find this exact block:

```rust
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
            format!("[Task] {}", task.text),
            PlanEntryPriority::Medium,
            status,
        ));
    }
    if let Some(plan) = mission_plan {
        entries.push(PlanEntry::new(
            format!("[Mission] {}", plan.mission),
            PlanEntryPriority::Medium,
            if plan.summary.is_some() {
                PlanEntryStatus::Completed
            } else {
                PlanEntryStatus::InProgress
            },
        ));
        for step in &plan.steps {
            let (content, status) = match step.status {
                StepStatus::Pending => (
                    format!("[Mission: {}] {}", step.member, step.task),
                    PlanEntryStatus::Pending,
                ),
                StepStatus::Verified => (
                    format!("[Mission: {}] {}", step.member, step.task),
                    PlanEntryStatus::Completed,
                ),
                // Never Completed -- PlanEntryStatus has no failure state,
                // so Pending (not a false success) plus a text marker is
                // the honest mapping, mirroring the same reasoning behind
                // the TUI's own Failed-step handling (Phase 6a).
                StepStatus::Failed => (
                    format!("[Mission: {}] (FAILED) {}", step.member, step.task),
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
```

Replace it with:

```rust
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
```

(`session.member` in the final loop is deliberately unchanged — see Global Constraints.)

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p aivyx-acp merged_plan -- --nocapture`
Expected: all pass, including the three new tests AND every pre-existing `merged_plan_*` test (none of their fixture strings contain control characters, so their expected output is unaffected by this change).

- [ ] **Step 6: Run the whole crate's tests**

Run: `cargo test -p aivyx-acp`
Expected: all pass, 0 failures.

- [ ] **Step 7: Format and commit**

```bash
rustfmt --edition 2024 crates/aivyx-acp/src/translate.rs
git add crates/aivyx-acp/src/translate.rs
git commit -m "fix: sanitize model-controlled text in ACP Plan entries against prefix spoofing"
```

- [ ] **Step 8: Build/test/lint the full workspace**

Run:
```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```
Expected: all clean, zero failures, zero warnings.

---

## Self-Review Notes

**Spec coverage:** Decision 1 (local `strip_control_chars_for_display` helper, duplicated not exported) → Step 3. Decision 2 (applied to exactly `task.text`/`plan.mission`/`step.task`, `session.member` untouched) → Step 4. Decision 3 (no TUI change) → nothing to implement, confirmed unnecessary during design. Decision 4 (no length truncation) → the helper has no clamp, matching the spec exactly. "What this spec does not decide" items are all genuinely untouched: no change to `set_tasks`/`decompose_task`, no TUI change, no change to `Session`'s own merge/state-tracking logic, no length limits added.

**Global Constraints deviation:** none — `session.member` is never passed to the new helper (Step 4's replacement block leaves that one `format!` call completely unchanged), no tool-input-validation change, no TUI file touched, no truncation logic added, only file-scoped `rustfmt`.

**Placeholder scan:** no TBD/TODO; every step shows complete, real code — this is a small, fully-specified single-task plan with no investigative/judgment-call steps needed (unlike several earlier plans in this project's history, every detail here was already pinned down during design, including exact existing test-module conventions read directly from the file).

**Type/interface consistency check:** `strip_control_chars_for_display(raw: &str) -> String` (Step 3) is called with matching signature at all three Step 4 call sites (`&task.text`, `&plan.mission`, `&step.task`) and in all three Step 1 tests' implicit expectations (each test's fixture text contains a literal `\n` that the assertion expects replaced with `\u{FFFD}`, exactly matching the helper's specified behavior).
