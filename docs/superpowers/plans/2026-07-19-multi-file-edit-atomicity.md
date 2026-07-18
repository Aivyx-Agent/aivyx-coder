# Multi-File Edit Atomicity Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** When a later call in the same model response fails, automatically
roll back every file-touching call that succeeded earlier in that response,
so a cross-file change either fully applies or leaves no partial trace.

**Architecture:** `run_turn_inner`'s existing per-response dispatch loop
(`crates/aivyx-core/src/agent/mod.rs`) gains local batch-tracking state:
remember the checkpoint ref from right before the batch's first mutating
call (detected by observing `ToolExecutor::latest_checkpoint_ref` change
across a dispatch, not by any new API), and on a later `ToolOutput::Error`
in the same batch, call the already-existing `restore_to_checkpoint`, fold
an explanation into that failing call's own error text, and skip the rest
of the batch.

**Tech Stack:** Rust, tokio, the existing `MockBackend` test harness in
`crates/aivyx-core/src/agent/tests.rs`.

## Global Constraints

- **Zero changes outside `crates/aivyx-core/src/agent/mod.rs` (plus its own
  test file, `tests.rs`).** No new dependencies, no `aivyx-tools`/
  `aivyx-sandbox`/`aivyx-types` changes — every primitive this plan needs
  (`ToolExecutor::latest_checkpoint_ref`, `restore_to_checkpoint`,
  `ToolOutput::{Ok,Error,Denied}`, `record_skipped_tool_result`) already
  exists exactly as-is.
- **Mechanism correction to the spec's Decision 5** (discovered during
  plan-writing, not a re-litigation of the user-approved *intent* — see
  rationale below): the rollback notice is **folded into the failing call's
  own `ToolOutput::Error` string**, not injected as a separate history
  entry. Confirmed with the user directly once this codebase's own
  established caution around synthetic history messages surfaced: the one
  existing precedent for "the agent narrates something to the model
  mid-turn" (`run_auto_verification`, `agent/mod.rs:636-669`) always fakes a
  *complete* call+result pair against an **already-registered real tool**
  (`run_command`) — it never injects a bare, unpaired note. A brand-new tool
  name risks stricter OpenAI-compatible backends rejecting an unrecognized
  `tool_calls` entry; a lone `Role::Tool` message needs a real
  `tool_call_id` it wouldn't have. Folding the explanation into the already
  real, already-paired failing call's own error text carries none of that
  risk and still satisfies the actual requirement (the model sees, on its
  very next request, exactly what got undone and why) — same information,
  same position in history, no new message shape.
- **Trigger is exactly `ToolOutput::Error(_)`** (`crates/aivyx-types/src/lib.rs:86-90`)
  — never `Denied` (a deliberate user choice, spec Decision 3), never a
  skipped call (cancellation/`MAX_TOOL_CALLS_PER_RESPONSE`).
- **The existing autonomous-mode verification-failure rollback
  (`pre_experiment_ref`, `agent/mod.rs:1252-1284`, `1342-1351`) is untouched.**
  This plan adds a second, parallel, independently-triggered mechanism; it
  must not read, write, or reset `self.pre_experiment_ref`/
  `self.unverified_edits`, and must not change the existing `is_edit_call`
  block's behavior in any way.
- **Batch state is local to the per-response dispatch loop**, never a
  struct field — it must not persist across responses or turns (spec
  Decision 1).
- Full test suite (`cargo test --workspace`) and `cargo clippy --workspace
  --all-targets` must stay clean (0 failures, 0 warnings) after every task.

---

### Task 1: Batch-tracking, rollback trigger, and notice

**Files:**
- Modify: `crates/aivyx-core/src/agent/mod.rs` (the per-response dispatch
  loop inside `run_turn_inner`, plus one new free function)
- Modify: `crates/aivyx-core/src/agent/tests.rs` (5 new tests)

**Interfaces:**
- Consumes: `ToolExecutor::latest_checkpoint_ref(&self, cancellation:
  &CancellationToken) -> Option<String>` and `ToolExecutor::
  restore_to_checkpoint(&self, ref_name: &str, cancellation:
  &CancellationToken) -> Result<(), String>` (`crates/aivyx-tools/src/lib.rs:159-177`,
  both already exist, unmodified by this plan). `ToolOutput::{Ok(String),
  Error(String), Denied(String)}` (`crates/aivyx-types/src/lib.rs:86-90`).
  `record_skipped_tool_result(&mut self, call: ToolCall, reason: &str)`
  (`agent/mod.rs:611-618`, already exists).
- Produces: a new free function `fn describe_tool_call_target(call:
  &ToolCall) -> String`, placed alongside the other bottom-of-file helper
  functions (`elide`, `command_reported_success`) in `agent/mod.rs`. No new
  public API — this task's whole effect is internal to `run_turn_inner`.

- [ ] **Step 1: Write the failing tests**

Read `crates/aivyx-core/src/agent/tests.rs`'s existing `init_git_repo`
helper (around line 1772) and the `autonomous_mode_discards_and_rewinds_on_exhausted_verification`
test (around line 1800) first — the new tests below follow that exact
fixture style (a real git repo via `init_git_repo`, real `WriteFileTool`/
`EditFileTool` registered, a `MockBackend` scripting the model's tool
calls, `ToolExecutor::set_checkpointer` wired to a real `GitCheckpointer`)
since this feature's whole point is real checkpoint/restore behavior, not
something a mock can stand in for.

Add near the bottom of `crates/aivyx-core/src/agent/tests.rs` (after the
existing `write_call` helper, around line 1893):

```rust
fn edit_call(id: &str, path: &str, old_string: &str, new_string: &str) -> Vec<StreamEvent> {
    vec![
        StreamEvent::ToolCallComplete(ToolCall {
            id: ToolCallId(id.to_string()),
            name: "edit_file".to_string(),
            arguments: serde_json::json!({
                "path": path,
                "old_string": old_string,
                "new_string": new_string,
            }),
            source: ToolCallSource::Native,
        }),
        StreamEvent::Done {
            finish_reason: FinishReason::ToolCalls,
        },
    ]
}

/// A single model response containing every call in `calls`, in order —
/// used to build the "one batch" scenarios this feature is about (a real
/// model response emits all its tool calls before any of them execute).
fn multi_call_response(calls: Vec<ToolCall>) -> Vec<StreamEvent> {
    let mut events: Vec<StreamEvent> = calls
        .into_iter()
        .map(StreamEvent::ToolCallComplete)
        .collect();
    events.push(StreamEvent::Done {
        finish_reason: FinishReason::ToolCalls,
    });
    events
}

fn write_call_in(path: &str, content: &str, id: &str) -> ToolCall {
    ToolCall {
        id: ToolCallId(id.to_string()),
        name: "write_file".to_string(),
        arguments: serde_json::json!({ "path": path, "content": content }),
        source: ToolCallSource::Native,
    }
}

fn edit_call_in(path: &str, old_string: &str, new_string: &str, id: &str) -> ToolCall {
    ToolCall {
        id: ToolCallId(id.to_string()),
        name: "edit_file".to_string(),
        arguments: serde_json::json!({
            "path": path,
            "old_string": old_string,
            "new_string": new_string,
        }),
        source: ToolCallSource::Native,
    }
}

async fn checkpointed_agent(
    dir: &Path,
    responses: Vec<Vec<StreamEvent>>,
    autonomous: bool,
) -> (Agent, UnboundedReceiver<AgentEvent>, Arc<MockBackend>) {
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(aivyx_tools::WriteFileTool));
    registry.register(Arc::new(aivyx_tools::EditFileTool));

    let (tx, rx) = unbounded_channel();
    let mock = Arc::new(MockBackend::new(responses));
    let llm: Arc<dyn LlmBackend> = mock.clone();
    let gate: Arc<dyn PermissionGate> = Arc::new(AllowAllGate);
    let confiner: Arc<dyn ExecutionConfiner> = Arc::new(NoopConfiner);
    let mut executor = ToolExecutor::new(registry, gate, confiner);
    executor.set_checkpointer(Arc::new(
        aivyx_tools::GitCheckpointer::detect(dir, vec![]).await.unwrap(),
    ));

    let autonomous_mode = AutonomousMode::new();
    autonomous_mode.set_active(autonomous);
    let agent = Agent::new(
        llm,
        executor,
        "system",
        AgentConfig {
            max_tool_iterations: 10,
            ..Default::default()
        },
        Arc::default(),
        PlanMode::new(),
        autonomous_mode,
        tx,
    );
    (agent, rx, mock)
}
```

Each test below is already `#[tokio::test]`, so calling `checkpointed_agent(...).await` is a normal async helper call, matching how `init_git_repo` is already awaited elsewhere in this file.

Now add the 5 tests:

```rust
#[tokio::test]
async fn batch_rollback_undoes_earlier_successful_edits_on_a_later_failure() {
    let dir = tempfile::tempdir().unwrap();
    init_git_repo(dir.path()).await;

    let response = multi_call_response(vec![
        write_call_in("a.txt", "A\n", "c1"),
        write_call_in("b.txt", "B\n", "c2"),
        edit_call_in("a.txt", "this text does not exist", "replacement", "c3"),
    ]);
    let (mut agent, _rx, _mock) =
        checkpointed_agent(dir.path(), vec![response, text_response("done")], false).await;

    agent
        .run_turn("go".to_string(), dir.path(), CancellationToken::new())
        .await
        .unwrap();

    assert!(
        !dir.path().join("a.txt").exists(),
        "a.txt was created in this same batch — must be rolled back"
    );
    assert!(
        !dir.path().join("b.txt").exists(),
        "b.txt was created in this same batch — must be rolled back too"
    );
}

#[tokio::test]
async fn batch_rollback_notice_lists_every_rolled_back_path() {
    let dir = tempfile::tempdir().unwrap();
    init_git_repo(dir.path()).await;

    let response = multi_call_response(vec![
        write_call_in("a.txt", "A\n", "c1"),
        write_call_in("b.txt", "B\n", "c2"),
        edit_call_in("a.txt", "does not exist", "x", "c3"),
    ]);
    let (mut agent, _rx, _mock) =
        checkpointed_agent(dir.path(), vec![response, text_response("done")], false).await;

    agent
        .run_turn("go".to_string(), dir.path(), CancellationToken::new())
        .await
        .unwrap();

    let error_text = agent
        .history
        .iter()
        .flat_map(|m| &m.content)
        .find_map(|b| match b {
            ContentBlock::ToolResult(ToolResult { call_id, output: ToolOutput::Error(text) })
                if call_id.0 == "c3" =>
            {
                Some(text.clone())
            }
            _ => None,
        })
        .expect("expected an Error result for the failing edit_file call");

    assert!(error_text.contains("a.txt"), "notice must name a.txt: {error_text}");
    assert!(error_text.contains("b.txt"), "notice must name b.txt: {error_text}");
    assert!(
        error_text.contains("rolled back") || error_text.contains("rollback"),
        "notice must explain what happened: {error_text}"
    );
}

#[tokio::test]
async fn remaining_calls_in_a_rolled_back_batch_are_skipped_not_executed() {
    let dir = tempfile::tempdir().unwrap();
    init_git_repo(dir.path()).await;

    let response = multi_call_response(vec![
        write_call_in("a.txt", "A\n", "c1"),
        edit_call_in("a.txt", "does not exist", "x", "c2"),
        write_call_in("never_created.txt", "should not exist\n", "c3"),
    ]);
    let (mut agent, _rx, _mock) =
        checkpointed_agent(dir.path(), vec![response, text_response("done")], false).await;

    agent
        .run_turn("go".to_string(), dir.path(), CancellationToken::new())
        .await
        .unwrap();

    assert!(
        !dir.path().join("never_created.txt").exists(),
        "the call after the failure must never have been dispatched"
    );

    let c3_output = agent
        .history
        .iter()
        .flat_map(|m| &m.content)
        .find_map(|b| match b {
            ContentBlock::ToolResult(ToolResult { call_id, output })
                if call_id.0 == "c3" =>
            {
                Some(output.clone())
            }
            _ => None,
        })
        .expect("c3 must still have a matching Role::Tool result (skipped, not dropped)");
    assert!(
        matches!(&c3_output, ToolOutput::Denied(reason) if reason.contains("rolled back")),
        "skipped call must say why: {c3_output:?}"
    );
}

#[tokio::test]
async fn a_deny_partway_through_a_batch_does_not_roll_back_earlier_approved_calls() {
    struct DenySecondCallGate;
    #[async_trait::async_trait]
    impl PermissionGate for DenySecondCallGate {
        async fn check(&self, request: &PermissionRequest) -> PermissionDecision {
            if let PermissionTarget::Path(path) = &request.target
                && path.ends_with("b.txt")
            {
                return PermissionDecision::Deny(Some("test denial".to_string()));
            }
            PermissionDecision::Allow
        }
    }

    let dir = tempfile::tempdir().unwrap();
    init_git_repo(dir.path()).await;

    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(aivyx_tools::WriteFileTool));
    registry.register(Arc::new(aivyx_tools::EditFileTool));

    let response = multi_call_response(vec![
        write_call_in("a.txt", "A\n", "c1"),
        write_call_in("b.txt", "B\n", "c2"), // denied by the gate above
    ]);
    let (tx, _rx) = unbounded_channel();
    let mock = Arc::new(MockBackend::new(vec![response, text_response("done")]));
    let llm: Arc<dyn LlmBackend> = mock.clone();
    let gate: Arc<dyn PermissionGate> = Arc::new(DenySecondCallGate);
    let confiner: Arc<dyn ExecutionConfiner> = Arc::new(NoopConfiner);
    let mut executor = ToolExecutor::new(registry, gate, confiner);
    executor.set_checkpointer(Arc::new(
        aivyx_tools::GitCheckpointer::detect(dir.path(), vec![])
            .await
            .unwrap(),
    ));
    let mut agent = Agent::new(
        llm,
        executor,
        "system",
        AgentConfig {
            max_tool_iterations: 10,
            ..Default::default()
        },
        Arc::default(),
        PlanMode::new(),
        AutonomousMode::new(),
        tx,
    );

    agent
        .run_turn("go".to_string(), dir.path(), CancellationToken::new())
        .await
        .unwrap();

    assert!(
        dir.path().join("a.txt").exists(),
        "a.txt was approved and written — a later Deny must not roll it back"
    );
}

#[tokio::test]
async fn a_solo_failing_call_with_no_earlier_success_behaves_as_before() {
    let dir = tempfile::tempdir().unwrap();
    init_git_repo(dir.path()).await;

    let response = multi_call_response(vec![edit_call_in(
        "tracked.txt",
        "text that is not in the file",
        "x",
        "c1",
    )]);
    let (mut agent, _rx, _mock) =
        checkpointed_agent(dir.path(), vec![response, text_response("done")], false).await;

    agent
        .run_turn("go".to_string(), dir.path(), CancellationToken::new())
        .await
        .unwrap();

    let error_text = agent
        .history
        .iter()
        .flat_map(|m| &m.content)
        .find_map(|b| match b {
            ContentBlock::ToolResult(ToolResult { call_id, output: ToolOutput::Error(text) })
                if call_id.0 == "c1" =>
            {
                Some(text.clone())
            }
            _ => None,
        })
        .expect("expected an Error result");
    assert!(
        !error_text.contains("rolled back") && !error_text.contains("rollback"),
        "a solo failing call has nothing to roll back — must not claim it did: {error_text}"
    );
}

#[tokio::test]
async fn batch_rollback_fires_in_autonomous_mode_too() {
    let dir = tempfile::tempdir().unwrap();
    init_git_repo(dir.path()).await;

    let response = multi_call_response(vec![
        write_call_in("a.txt", "A\n", "c1"),
        edit_call_in("a.txt", "does not exist", "x", "c2"),
    ]);
    // autonomous = true, and no [verification] command configured at all,
    // so the *existing* pre_experiment_ref mechanism (which only fires on
    // verification failure) can't be the thing producing this result —
    // proving this plan's mechanism is independent of it.
    let (mut agent, _rx, _mock) =
        checkpointed_agent(dir.path(), vec![response, text_response("done")], true).await;

    agent
        .run_turn("go".to_string(), dir.path(), CancellationToken::new())
        .await
        .unwrap();

    assert!(
        !dir.path().join("a.txt").exists(),
        "batch rollback must fire in autonomous mode too, independent of pre_experiment_ref"
    );
}
```

- [ ] **Step 2: Run the new tests to verify they fail**

Run: `cargo test -p aivyx-core batch_rollback -- --nocapture 2>&1 | tail -60`
Expected: compile errors (the helpers reference behavior that doesn't exist
yet) or, once compiling, assertion failures — the current code has no
rollback logic at all, so `a.txt`/`b.txt` will still exist and no error
text will mention a rollback.

- [ ] **Step 3: Add the `describe_tool_call_target` helper**

In `crates/aivyx-core/src/agent/mod.rs`, add near the other bottom-of-file
helper functions (alongside `elide`/`command_reported_success`, roughly
line 1510 as of this plan's writing — re-verify the exact location, it's
simply "near the other small free-function helpers"):

```rust
/// Best-effort human-readable description of what a tool call touched, for
/// the batch-rollback notice — most mutating tools (`write_file`,
/// `edit_file`, `delete_file`) take a `"path"` argument; anything else
/// falls back to just the tool's name.
fn describe_tool_call_target(call: &ToolCall) -> String {
    match call.arguments.get("path").and_then(|v| v.as_str()) {
        Some(path) => format!("{path} ({})", call.name),
        None => call.name.clone(),
    }
}
```

- [ ] **Step 4: Add batch-tracking state and the rollback trigger to the dispatch loop**

In `crates/aivyx-core/src/agent/mod.rs`, find the per-response dispatch
loop (currently, as of this plan's writing — re-verify exact surrounding
lines, they will have drifted):

```rust
            for (index, call) in tool_calls.into_iter().enumerate() {
                if index >= MAX_TOOL_CALLS_PER_RESPONSE {
                    self.record_skipped_tool_result(
                        call,
                        "skipped — too many tool calls in a single response",
                    );
                    continue;
                }
                if cancellation.is_cancelled() {
                    self.record_skipped_tool_result(
                        call,
                        "cancelled before this tool call was executed",
                    );
                    continue;
                }

                let is_edit_call = PROMPTED_EDIT_HIDDEN_TOOLS.contains(&call.name.as_str());
                let was_already_unverified = self.unverified_edits;
                let result = self
                    .executor
                    .dispatch(call, cwd, cancellation.clone())
                    .await;
                if is_edit_call && matches!(result.output, ToolOutput::Ok(_)) {
                    self.unverified_edits = true;
                    if self.autonomous_mode.active() && !was_already_unverified {
                        self.pre_experiment_ref =
                            self.executor.latest_checkpoint_ref(&cancellation).await;
                    }
                }
                self.emit(AgentEvent::ToolResult(result.clone()));
                self.history.push(Message {
                    role: Role::Tool,
                    tool_call_id: Some(result.call_id.clone()),
                    content: vec![ContentBlock::ToolResult(result)],
                });
            }
```

Replace with:

```rust
            let mut last_checkpoint_ref = self.executor.latest_checkpoint_ref(&cancellation).await;
            let mut batch_start_ref: Option<String> = None;
            let mut batch_touched_paths: Vec<String> = Vec::new();
            let mut batch_rolled_back = false;

            for (index, call) in tool_calls.into_iter().enumerate() {
                if index >= MAX_TOOL_CALLS_PER_RESPONSE {
                    self.record_skipped_tool_result(
                        call,
                        "skipped — too many tool calls in a single response",
                    );
                    continue;
                }
                if cancellation.is_cancelled() {
                    self.record_skipped_tool_result(
                        call,
                        "cancelled before this tool call was executed",
                    );
                    continue;
                }
                if batch_rolled_back {
                    self.record_skipped_tool_result(
                        call,
                        "skipped — a failure earlier in this response rolled back the batch of edits",
                    );
                    continue;
                }

                let is_edit_call = PROMPTED_EDIT_HIDDEN_TOOLS.contains(&call.name.as_str());
                let was_already_unverified = self.unverified_edits;
                let call_description = describe_tool_call_target(&call);
                let mut result = self
                    .executor
                    .dispatch(call, cwd, cancellation.clone())
                    .await;
                if is_edit_call && matches!(result.output, ToolOutput::Ok(_)) {
                    self.unverified_edits = true;
                    if self.autonomous_mode.active() && !was_already_unverified {
                        self.pre_experiment_ref =
                            self.executor.latest_checkpoint_ref(&cancellation).await;
                    }
                }

                // Batch-checkpoint tracking, independent of the
                // pre_experiment_ref bookkeeping above.
                let ref_after_this_call = self.executor.latest_checkpoint_ref(&cancellation).await;
                let minted_new_checkpoint = ref_after_this_call != last_checkpoint_ref;
                let ref_before_this_call =
                    std::mem::replace(&mut last_checkpoint_ref, ref_after_this_call);
                if matches!(result.output, ToolOutput::Ok(_)) && minted_new_checkpoint {
                    if batch_start_ref.is_none() {
                        batch_start_ref = ref_before_this_call;
                    }
                    batch_touched_paths.push(call_description);
                }
                if let ToolOutput::Error(original_error) = &result.output
                    && let Some(start_ref) = batch_start_ref.take()
                {
                    match self.executor.restore_to_checkpoint(&start_ref, &cancellation).await {
                        Ok(()) => {
                            result.output = ToolOutput::Error(format!(
                                "{original_error}\n\nThis failure automatically rolled back {} \
                                 earlier edit(s) in this same response to keep the codebase \
                                 consistent: {}. The codebase is now back to its state before \
                                 this response's edits began.",
                                batch_touched_paths.len(),
                                batch_touched_paths.join(", "),
                            ));
                        }
                        Err(restore_err) => {
                            result.output = ToolOutput::Error(format!(
                                "{original_error}\n\nAdditionally, an automatic rollback of {} \
                                 earlier edit(s) in this same response was attempted (to keep the \
                                 codebase consistent) but FAILED ({restore_err}) — the codebase \
                                 may now be in a partially-edited, inconsistent state. Affected \
                                 files: {}. Inspect manually via `git log \
                                 refs/aivyx/checkpoints/`.",
                                batch_touched_paths.len(),
                                batch_touched_paths.join(", "),
                            ));
                        }
                    }
                    batch_rolled_back = true;
                    batch_touched_paths.clear();
                }

                self.emit(AgentEvent::ToolResult(result.clone()));
                self.history.push(Message {
                    role: Role::Tool,
                    tool_call_id: Some(result.call_id.clone()),
                    content: vec![ContentBlock::ToolResult(result)],
                });
            }
```

- [ ] **Step 5: Run the new tests to verify they pass**

Run: `cargo test -p aivyx-core batch_rollback -- --nocapture 2>&1 | tail -60`
Also run: `cargo test -p aivyx-core a_deny_partway_through_a_batch a_solo_failing_call remaining_calls_in_a_rolled_back_batch -- --nocapture 2>&1 | tail -60`
Expected: all 6 new tests (5 named `batch_rollback_*`/etc. plus the deny
and solo-failure tests) pass.

- [ ] **Step 6: Run the full test suite and clippy**

Run: `cargo test --workspace 2>&1 | grep -E "^test result|FAILED"`
Expected: every line `ok`, 0 failed — in particular, confirm
`autonomous_mode_discards_and_rewinds_on_exhausted_verification` (the
existing, untouched autonomous-verification test) still passes unchanged,
proving this plan's new logic doesn't interfere with it.

Run: `cargo clippy --workspace --all-targets 2>&1 | tail -20`
Expected: 0 warnings.

- [ ] **Step 7: Commit**

```bash
git add crates/aivyx-core/src/agent/mod.rs crates/aivyx-core/src/agent/tests.rs
git commit -m "Roll back a response's whole batch of edits when a later call in it fails"
```

---

### Task 2: Live E2E through the real binary

**Files:**
- None modified — verification only, via this project's established
  live-E2E method (PTY + `python-pyte`, graded via the persisted session
  JSON).

**Interfaces:**
- Consumes: Task 1's fully-wired feature.

- [ ] **Step 1: Build the release binary**

```bash
cargo build --release -p aivyx 2>&1 | tail -10
```

Expected: succeeds.

- [ ] **Step 2: Set up a scratch project with a real cross-file dependency**

```bash
SCRATCH_PROJECT=$(mktemp -d)
cd "$SCRATCH_PROJECT"
git init -q -b main
git config user.name test
git config user.email test@test.invalid
```

Create two files where the second genuinely depends on content in the
first, so a real edit-and-rename-style cross-file change is natural to
prompt for:

```bash
cat > greeter.py << 'EOF'
def greet(name):
    return f"Hello, {name}!"
EOF

cat > main.py << 'EOF'
from greeter import greet

print(greet("World"))
EOF

git add -A
git commit -q -m initial
```

- [ ] **Step 3: Drive the real binary, engineer a genuine mid-batch failure**

Follow this project's established live-E2E harness pattern (`python-pyte`
for rendering, explicit `TIOCSWINSZ` window sizing, paced keystrokes
~15ms apart, wait for the "Type a message..." readiness marker, wait for
both the ready-status text and the input-placeholder text before
considering a turn complete). Send a message engineered to produce a
multi-file edit where the second edit is very likely to fail on the
model's first attempt against a small local model, e.g.:

`"Rename the function greet in greeter.py to say_hello, and update main.py to call the new name. Use edit_file for both changes, in a single response if you can."`

If the model's own edit genuinely succeeds cleanly both times on the first
try (small local models sometimes get simple two-file renames right), that
does not prove this feature — you need an *engineered* failure to verify
rollback specifically. If the first natural attempt succeeds cleanly,
re-run with a prompt that's more likely to trigger a stale second edit,
e.g.: `"In greeter.py, change the function name from greet to say_hello. In main.py, replace the text 'from old_greeter_name import greet' with 'from greeter import say_hello' using edit_file — old_greeter_name is deliberately wrong, I want to see what happens when that edit fails."` (this second phrasing deliberately hands the model a guaranteed-to-fail `old_string`
for the second file, giving you a reliable, reproducible mid-batch failure
without relying on the model's own mistake).

- [ ] **Step 4: Grade from the persisted session JSON**

Confirm via `~/.local/state/aivyx-coder/sessions/<hash>.json` (not screen
text):
- The first edit's tool result really did succeed (`ToolOutput::Ok`).
- The second edit's tool result is `ToolOutput::Error`, and its text
  contains a rollback explanation naming `greeter.py`.
- On disk, `greeter.py` is back to its **original** content (`greet`, not
  `say_hello`) — proving the first edit really was undone, not just that
  the second one failed.

- [ ] **Step 5: Clean up**

```bash
rm -f ~/.local/state/aivyx-coder/sessions/*"$(basename "$SCRATCH_PROJECT")"*.json 2>/dev/null || true
rm -rf "$SCRATCH_PROJECT"
```

(Adjust the session-file glob after inspecting what file actually got
created, per `session::session_file_path`'s own naming convention.)

- [ ] **Step 6: Report**

No commit for this task (verification only, no files modified). Report the
exact session JSON excerpt showing the Ok→Error sequence with the rollback
text, and confirmation `greeter.py`'s on-disk content matches its original,
pre-batch state.
