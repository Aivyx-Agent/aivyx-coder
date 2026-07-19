# Structured Verification Memory Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give the model a coarse, framework-agnostic "what's new since my
last verification attempt" signal, instead of raw tail-capped output alone.

**Architecture:** `Agent` gains one new field remembering the immediately
preceding verification run's raw output (whatever that run's own outcome
was), and `run_auto_verification` gains a line-set comparison against it —
appending a short note to a *failing* result listing which output lines are
genuinely new, then updating the stored reference regardless of outcome.

**Tech Stack:** Rust, `std::collections::HashSet`. No new dependencies.

## Global Constraints

- Entirely within `crates/aivyx-core/src/agent/mod.rs` (plus its own test
  file, `tests.rs`) — no other crate touched, no new dependencies, no new
  config surface.
- **Correction to the spec's own Change 3 code, found while writing this
  plan** (not a re-litigation of any Decision — this is a bug in the
  spec's literal code relative to its own Testing section's stated
  requirement): the spec's code appends the new-lines note unconditionally,
  comparing every run's output against the previous one regardless of
  whether the *current* run passed. But the spec's own Testing section
  requires "the second (passing) result's text is unmodified (no note
  needed on success ... since `command_reported_success` already
  communicates that plainly)" — and a passing run's output will almost
  always differ substantially from a preceding failing run's output (a
  clean `test result: ok. N passed` versus lines full of `FAILED`), so the
  unconditional version would append a large, unhelpful "what's new" note
  to results that are already unambiguous good news. **Fix: gate the
  note-appending (not the reference-updating) on `!passed`** — a failing
  result gets the comparison and possible note; a passing result never
  does, but `last_verification_output` is still updated on *both* outcomes
  exactly as Decision 2 requires. This task's own code below already has
  the fix applied — do not follow the spec document's literal Change 3
  code verbatim, follow this plan's version.
- The corrected, load-bearing semantic from the spec's own Decision 2
  (arrived at only after a brainstorming-time correction): compare against
  the **immediately preceding run, regardless of whether that run passed
  or failed** — never simplify this back to "compare against the last
  successful run," which would silently reintroduce the exact gap the
  brainstorming session found and fixed (no help for a codebase that
  starts broken, and no help distinguishing a fix-and-retry loop's own
  same-failure-as-last-attempt case).
- Full test suite (`cargo test --workspace`) and `cargo clippy --workspace
  --all-targets` must stay clean (0 failures, 0 warnings) after every task.

---

### Task 1: New field, comparison helper, and wiring

**Files:**
- Modify: `crates/aivyx-core/src/agent/mod.rs` (`Agent` struct + `Agent::new`,
  new free function, `run_auto_verification`)
- Modify: `crates/aivyx-core/src/agent/tests.rs` (6 new tests)

**Interfaces:**
- Produces: `Agent.last_verification_output: Option<String>` (new private
  field) and `fn new_lines_note(previous: &str, current: &str) ->
  Option<String>` (new free function) — both internal to this crate, no
  external API.

- [ ] **Step 1: Write the failing tests**

Read `crates/aivyx-core/src/agent/tests.rs`'s existing `verify_command_spec`
helper and its two neighboring tests
(`a_passing_verification_completes_the_turn_without_an_extra_round_trip`,
`a_failing_verification_feeds_back_and_retries_until_exhausted`, both
around line 2638-2770) first — the new tests below extend that exact
harness (`build_agent`, a `RunCommandTool` registered with a `CommandSpec`,
`agent.set_verification`, `agent.run_turn`), just with **stateful** shell
commands whose output differs between successive invocations (via a marker
file written into the test's own tempdir — `run_command` executes with
`ctx.cwd` as the working directory, confirmed at
`crates/aivyx-tools/src/tools/run_command.rs:113`, so a relative marker
filename resolves correctly inside each test's isolated tempdir).

Add near the bottom of the "enforced verification" test section (after
`auto_verify_calls`, before the two existing verification tests, or after
them — placement only needs to keep them grouped with the section's
existing tests):

```rust
#[test]
fn new_lines_note_reports_only_lines_absent_from_previous() {
    let previous = "test test_a ... FAILED\nfailures:\n    test_a\n";
    let current = "test test_a ... FAILED\ntest test_b ... FAILED\nfailures:\n    test_a\n    test_b\n";

    let note = new_lines_note(previous, current).expect("current has genuinely new lines");
    assert!(note.contains("test_b"));
    assert!(
        note.contains("2 line(s)"),
        "expected exactly 2 new lines ('test test_b ... FAILED' and '    test_b'): {note}"
    );

    // Every line in `current` already present in `previous` (even though
    // `previous` itself has an extra line `current` lacks) -> None.
    let previous_superset = "test test_a ... FAILED\nsome extra line only in previous\n";
    let current_subset = "test test_a ... FAILED\n";
    assert_eq!(new_lines_note(previous_superset, current_subset), None);
}

#[test]
fn new_lines_note_respects_its_cap() {
    let previous = "";
    let current: String = (0..5000).map(|i| format!("new line {i}\n")).collect();

    let note = new_lines_note(previous, &current).expect("all lines are new");
    // The rendered new-lines section itself must be capped, even though the
    // preamble text ("N line(s) ... attempt:") is uncapped and always present.
    let capped_section = note.split_once("attempt:\n").unwrap().1;
    assert!(
        capped_section.len() <= NEW_LINES_NOTE_CAP + 200,
        "capped section should stay close to NEW_LINES_NOTE_CAP, got {} bytes",
        capped_section.len()
    );
}

fn stateful_verify_command_spec(name: &str, script: &str) -> CommandSpec {
    CommandSpec {
        name: name.to_string(),
        program: "sh".to_string(),
        args: vec!["-c".to_string(), script.to_string()],
        timeout: Duration::from_secs(5),
    }
}

/// Extracts, in history order, the text of every `ToolResult::Ok` whose
/// `call_id` came from `run_auto_verification` (its synthetic IDs are
/// always `"auto-verify-{n}"`) — lets a test inspect what the model
/// actually saw for each verification attempt, not just how many happened.
fn auto_verify_result_texts(history: &[Message]) -> Vec<String> {
    history
        .iter()
        .flat_map(|m| &m.content)
        .filter_map(|b| match b {
            ContentBlock::ToolResult(ToolResult {
                call_id,
                output: ToolOutput::Ok(text),
            }) if call_id.0.starts_with("auto-verify-") => Some(text.clone()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn fix_and_retry_note_lists_only_lines_new_since_the_first_attempt() {
    let dir = tempfile::tempdir().unwrap();
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(aivyx_tools::WriteFileTool));
    registry.register(Arc::new(RunCommandTool::new(vec![
        stateful_verify_command_spec(
            "verify",
            "if [ -f marker ]; then \
                echo 'test test_a ... FAILED'; \
                echo 'test test_b ... FAILED'; \
             else \
                touch marker; \
                echo 'test test_a ... FAILED'; \
             fi; exit 1",
        ),
    ])));

    let write_call = vec![
        StreamEvent::ToolCallComplete(ToolCall {
            id: ToolCallId("c1".to_string()),
            name: "write_file".to_string(),
            arguments: serde_json::json!({ "path": "out.txt", "content": "hi\n" }),
            source: ToolCallSource::Native,
        }),
        StreamEvent::Done {
            finish_reason: FinishReason::ToolCalls,
        },
    ];
    let (mut agent, _rx, _mock) = build_agent(
        vec![
            write_call,
            text_response("done"),
            text_response("trying again"),
        ],
        registry,
        10,
    );
    agent.set_verification("verify".to_string(), 2);

    agent
        .run_turn("go".to_string(), dir.path(), CancellationToken::new())
        .await
        .unwrap();

    let results = auto_verify_result_texts(&agent.history);
    assert_eq!(results.len(), 2, "expected exactly 2 verification attempts");
    assert!(
        !results[0].contains("not present in the immediately preceding"),
        "the first attempt has nothing prior to compare against: {}",
        results[0]
    );
    assert!(
        results[1].contains("test_b"),
        "the second attempt's note must mention the newly-appeared failure: {}",
        results[1]
    );
    assert!(
        !results[1].contains("1 line(s)") || results[1].matches("test_a").count() <= 1,
        "the note must not re-flag test_a, which was already present in the first attempt: {}",
        results[1]
    );
}

#[tokio::test]
async fn starts_broken_then_fixed_leaves_the_passing_result_unmodified() {
    let dir = tempfile::tempdir().unwrap();
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(aivyx_tools::WriteFileTool));
    registry.register(Arc::new(RunCommandTool::new(vec![
        stateful_verify_command_spec(
            "verify",
            "if [ -f marker ]; then \
                exit 0; \
             else \
                touch marker; \
                echo 'test test_a ... FAILED'; \
                exit 1; \
             fi",
        ),
    ])));

    let write_call = vec![
        StreamEvent::ToolCallComplete(ToolCall {
            id: ToolCallId("c1".to_string()),
            name: "write_file".to_string(),
            arguments: serde_json::json!({ "path": "out.txt", "content": "hi\n" }),
            source: ToolCallSource::Native,
        }),
        StreamEvent::Done {
            finish_reason: FinishReason::ToolCalls,
        },
    ];
    let (mut agent, _rx, _mock) = build_agent(
        vec![write_call, text_response("done"), text_response("trying again")],
        registry,
        10,
    );
    agent.set_verification("verify".to_string(), 2);

    agent
        .run_turn("go".to_string(), dir.path(), CancellationToken::new())
        .await
        .unwrap();

    let results = auto_verify_result_texts(&agent.history);
    assert_eq!(results.len(), 2, "expected a failing attempt then a passing one");
    assert!(results[0].contains("(failed)"));
    assert!(results[1].contains("(success)"));
    assert!(
        !results[1].contains("not present in the immediately preceding"),
        "a passing result must never carry a new-lines note, even though its \
         output differs hugely from the prior failing attempt: {}",
        results[1]
    );
    assert_eq!(
        agent.last_verification_output.as_deref(),
        Some(results[1].as_str()),
        "the stored reference must be the passing run's own text"
    );
}

#[tokio::test]
async fn the_very_first_verification_call_ever_has_nothing_to_compare_against() {
    let dir = tempfile::tempdir().unwrap();
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(aivyx_tools::WriteFileTool));
    registry.register(Arc::new(RunCommandTool::new(vec![verify_command_spec(
        "verify", false,
    )])));

    let write_call = vec![
        StreamEvent::ToolCallComplete(ToolCall {
            id: ToolCallId("c1".to_string()),
            name: "write_file".to_string(),
            arguments: serde_json::json!({ "path": "out.txt", "content": "hi\n" }),
            source: ToolCallSource::Native,
        }),
        StreamEvent::Done {
            finish_reason: FinishReason::ToolCalls,
        },
    ];
    let (mut agent, _rx, _mock) = build_agent(
        vec![write_call, text_response("done"), text_response("still trying")],
        registry,
        10,
    );
    agent.set_verification("verify".to_string(), 1);

    agent
        .run_turn("go".to_string(), dir.path(), CancellationToken::new())
        .await
        .unwrap();

    let results = auto_verify_result_texts(&agent.history);
    assert_eq!(results.len(), 1);
    assert!(
        !results[0].contains("not present in the immediately preceding"),
        "the very first verification call has no prior run to compare against, \
         so its text must be exactly what command_reported_success/format_output \
         already produce, unmodified: {}",
        results[0]
    );
}

#[tokio::test]
async fn last_verification_output_updates_after_every_call_regardless_of_outcome() {
    let dir = tempfile::tempdir().unwrap();
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(aivyx_tools::WriteFileTool));
    registry.register(Arc::new(RunCommandTool::new(vec![
        stateful_verify_command_spec(
            "verify",
            "if [ -f marker ]; then \
                exit 0; \
             else \
                touch marker; \
                echo 'test test_a ... FAILED'; \
                exit 1; \
             fi",
        ),
    ])));

    let write_call = vec![
        StreamEvent::ToolCallComplete(ToolCall {
            id: ToolCallId("c1".to_string()),
            name: "write_file".to_string(),
            arguments: serde_json::json!({ "path": "out.txt", "content": "hi\n" }),
            source: ToolCallSource::Native,
        }),
        StreamEvent::Done {
            finish_reason: FinishReason::ToolCalls,
        },
    ];
    let (mut agent, _rx, _mock) = build_agent(
        vec![write_call, text_response("done"), text_response("trying again")],
        registry,
        10,
    );
    agent.set_verification("verify".to_string(), 2);

    assert!(agent.last_verification_output.is_none());

    agent
        .run_turn("go".to_string(), dir.path(), CancellationToken::new())
        .await
        .unwrap();

    let results = auto_verify_result_texts(&agent.history);
    assert_eq!(results.len(), 2);
    // Updated after the FIRST (failing) call already, not only on success.
    // We can't directly observe the intermediate value, but the final
    // stored value must equal the second (passing) call's own text —
    // proving it was overwritten again after the first failing call's own
    // update, not left stuck at whatever the first call set.
    assert_eq!(
        agent.last_verification_output.as_deref(),
        Some(results[1].as_str())
    );
}
```

- [ ] **Step 2: Run the new tests to verify they fail**

Run: `cargo test -p aivyx-core new_lines_note fix_and_retry starts_broken very_first last_verification_output_updates -- --nocapture`
Expected: compile errors (`new_lines_note`/`last_verification_output` don't
exist yet).

- [ ] **Step 3: Add the `new_lines_note` helper and its constant**

In `crates/aivyx-core/src/agent/mod.rs`, add near the other bottom-of-file
helper functions (alongside `elide`/`command_reported_success`):

```rust
/// Bound on the rendered new-lines note appended to a failing verification
/// result — small and fixed, unlike `elide_oversized_tool_results`'s own
/// dynamic, context-budget-driven cap, since this note is a bounded
/// addition to an already-capped tool result, not the whole history.
const NEW_LINES_NOTE_CAP: usize = 2000;

/// Compares `current` (this verification run's raw output) against
/// `previous` (the immediately preceding run's raw output, whatever its
/// outcome — see docs/superpowers/specs/
/// 2026-07-19-structured-verification-memory-design.md's Decision 2) line
/// by line, and returns a short note listing lines present in `current`
/// but absent from `previous` — a rough "what's new since the last
/// attempt" signal. `None` if every line in `current` already appeared in
/// `previous` (nothing new to report). Deliberately coarse: this has no
/// notion of what a "test" is, so a line that only differs by e.g. a
/// timestamp will still look new.
fn new_lines_note(previous: &str, current: &str) -> Option<String> {
    let previous_lines: std::collections::HashSet<&str> = previous.lines().collect();
    let new_lines: Vec<&str> = current
        .lines()
        .filter(|line| !previous_lines.contains(line))
        .collect();
    if new_lines.is_empty() {
        return None;
    }
    Some(format!(
        "\n\n{} line(s) of this output were not present in the immediately preceding \
         verification attempt — a rough signal for what's new since then, not a precise \
         test-level diff (some noise is possible, e.g. timestamps or other \
         non-deterministic content):\n{}",
        new_lines.len(),
        elide(&new_lines.join("\n"), NEW_LINES_NOTE_CAP)
    ))
}
```

- [ ] **Step 4: Add the `Agent` field**

In `crates/aivyx-core/src/agent/mod.rs`, find the struct fields
`unverified_edits: bool,` and `verify_retries: u32,` and add immediately
after `verify_retries`:

```rust
    /// The raw `run_auto_verification` output text from the immediately
    /// preceding verification run in this session, *regardless of whether
    /// that run passed or failed* — `None` until the first verification
    /// call ever happens, then updated after every subsequent call (pass
    /// or fail alike). Used to distinguish a genuinely new failure line
    /// from one that was already present in the last attempt, whatever its
    /// outcome. In-memory only; never persisted to the session JSON — a
    /// resumed session starts with nothing to compare against, same as
    /// before the first verification call in a fresh session.
    last_verification_output: Option<String>,
```

Find, in `Agent::new`, the initializers `unverified_edits: false,` and
`verify_retries: 0,`, and add immediately after:

```rust
            last_verification_output: None,
```

- [ ] **Step 5: Wire it into `run_auto_verification` (with the pass/fail gate fix)**

In `crates/aivyx-core/src/agent/mod.rs`, find `run_auto_verification`'s
current body:

```rust
        let result = self
            .executor
            .dispatch(call, cwd, cancellation.clone())
            .await;
        self.emit(AgentEvent::ToolResult(result.clone()));
        let passed =
            matches!(&result.output, ToolOutput::Ok(text) if command_reported_success(text));
        self.history.push(Message {
            role: Role::Tool,
            tool_call_id: Some(result.call_id.clone()),
            content: vec![ContentBlock::ToolResult(result)],
        });
        passed
    }
```

Replace with:

```rust
        let mut result = self
            .executor
            .dispatch(call, cwd, cancellation.clone())
            .await;
        let passed =
            matches!(&result.output, ToolOutput::Ok(text) if command_reported_success(text));

        // Compare against the immediately preceding run (whatever its own
        // outcome was) using the *original* text, then store that same
        // original (not the enriched) text as the new reference for next
        // time — the enrichment note is this turn's feedback only, not
        // something a future comparison should treat as real command
        // output. Only a FAILING result gets the note: a passing result's
        // output almost always differs hugely from a preceding failure
        // (clean output vs. lines full of FAILED), and `command_reported_
        // success` already communicates "this passed" plainly — appending
        // a large "what changed" note to already-unambiguous good news
        // would be pure noise. `last_verification_output` is still updated
        // unconditionally on both outcomes, though — comparing against
        // the last run rather than only the last *successful* one is the
        // whole point of this feature (see the design spec's Decision 2).
        if let ToolOutput::Ok(text) = &mut result.output {
            let current_text = text.clone();
            if !passed
                && let Some(previous) = &self.last_verification_output
                && let Some(note) = new_lines_note(previous, &current_text)
            {
                text.push_str(&note);
            }
            self.last_verification_output = Some(current_text);
        }

        self.emit(AgentEvent::ToolResult(result.clone()));
        self.history.push(Message {
            role: Role::Tool,
            tool_call_id: Some(result.call_id.clone()),
            content: vec![ContentBlock::ToolResult(result)],
        });
        passed
    }
```

Note the reordering relative to today's code: `self.emit(AgentEvent::
ToolResult(...))` moves from immediately-after-dispatch to after the
comparison/update block, so the TUI-facing event and the history entry
both carry the same, possibly-enriched text — not the raw text for one and
enriched for the other.

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test -p aivyx-core new_lines_note fix_and_retry starts_broken very_first last_verification_output_updates -- --nocapture`
Expected: all 6 new tests pass.

- [ ] **Step 7: Run the full aivyx-core suite and clippy**

Run: `cargo test -p aivyx-core 2>&1 | grep -E "^test result|FAILED"`
Expected: `ok`, 0 failed — in particular, confirm both pre-existing
verification tests
(`a_passing_verification_completes_the_turn_without_an_extra_round_trip`,
`a_failing_verification_feeds_back_and_retries_until_exhausted`) still
pass unchanged, proving this task's changes don't disturb the existing
retry-cap/reset mechanism.

Run: `cargo clippy -p aivyx-core --all-targets 2>&1 | tail -20`
Expected: 0 warnings.

- [ ] **Step 8: Run the full workspace suite and clippy**

Run: `cargo test --workspace 2>&1 | grep -E "^test result|FAILED"`
Expected: every crate `ok`, 0 failed.

Run: `cargo clippy --workspace --all-targets 2>&1 | tail -20`
Expected: 0 warnings.

- [ ] **Step 9: Commit**

```bash
git add crates/aivyx-core/src/agent/mod.rs crates/aivyx-core/src/agent/tests.rs
git commit -m "Compare each verification attempt against the immediately preceding one"
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

- [ ] **Step 2: Set up a scratch project seeded with one already-failing test**

```bash
SCRATCH_PROJECT=$(mktemp -d)
cd "$SCRATCH_PROJECT"
git init -q -b main
git config user.name test
git config user.email test@test.invalid
```

A minimal Python project (no build step needed, keeps the toy project
simple) with two tests, one already broken:

```bash
cat > calc.py << 'EOF'
def add(a, b):
    return a + b

def subtract(a, b):
    return a - b - 1  # deliberately wrong
EOF

cat > test_calc.py << 'EOF'
from calc import add, subtract

def test_add():
    assert add(2, 2) == 4

def test_subtract():
    assert subtract(5, 2) == 3
EOF

git add -A
git commit -q -m initial
```

Confirm the seeded failure is real and reproducible before continuing:
`python3 -m pytest test_calc.py -q` should show `test_add` passing and
`test_subtract` failing.

- [ ] **Step 3: Configure verification and drive the real binary**

In this scratch project's own `~/.config` equivalent or however this
project's `[[permissions.allowed_commands]]` + `[verification]` config is
normally set for a live E2E (check an existing live-E2E task from a prior
phase's plan for the exact config-file mechanics this project uses, since
none of this plan's earlier tasks needed to configure verification) —
register a `pytest` allowed command and set `[verification] command =
"pytest"` with a reasonable `max_retries` (e.g. 3).

Follow this project's established live-E2E harness pattern (`python-pyte`
for rendering, explicit `TIOCSWINSZ` window sizing, paced keystrokes
~15ms apart, wait for the "Type a message..." readiness marker, wait for
both the ready-status text and the input-placeholder text before
considering a turn complete). Send a message engineered to introduce a
**new, second** failure while leaving the pre-existing one alone, e.g.:

`"In calc.py, change the add function to return a + b + 1 instead — a deliberate off-by-one, I want to see what happens when this breaks a test. Use edit_file."`

This makes `test_add` newly fail while `test_subtract` remains failing
exactly as it was before any edits.

- [ ] **Step 4: Grade from the persisted session JSON**

Confirm via `~/.local/state/aivyx-coder/sessions/<hash>.json` (not screen
text): the auto-verification tool-result entries show (a) the first
attempt failing on `test_subtract` only, and (b) a second attempt (if the
model's edit landed in the same response as expected, this may be the
very first verification call of the turn — adjust the prompt if the model
doesn't reproduce a clean two-attempt sequence) whose text contains a
new-lines note mentioning `test_add`, and does **not** claim `test_subtract`
is new (since it was already present in the first comparison point).

- [ ] **Step 5: Clean up**

```bash
rm -f ~/.local/state/aivyx-coder/sessions/*"$(basename "$SCRATCH_PROJECT")"*.json 2>/dev/null || true
rm -rf "$SCRATCH_PROJECT"
```

(Adjust the session-file glob after inspecting what file actually got
created, per `session::session_file_path`'s own naming convention.)

- [ ] **Step 6: Report**

No commit for this task (verification only, no files modified). Report the
exact session JSON excerpt showing the new-lines note correctly
distinguishing the newly-broken `test_add` from the pre-existing
`test_subtract` failure.
