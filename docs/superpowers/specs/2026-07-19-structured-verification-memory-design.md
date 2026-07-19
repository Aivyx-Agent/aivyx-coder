# Structured Verification Memory — Design

**Status:** Approved by user 2026-07-19. Third of 4 sub-projects closing the
capability gaps identified in a fresh audit of aivyx-coder's actual
code-writing ability (the first two — multi-file edit atomicity, reasoning
visibility — are merged; repo-map multi-language support remains).

## Context

**Problem this spec solves:** the existing enforced-verification loop
(ROADMAP.md Phase 12 Part B, already shipped) feeds the model raw,
tail-capped text output from whatever `[verification] command` is
configured — there is no structured understanding of which parts of a
failure are genuinely new versus already present before this turn's edits.
A model fixing a failure has no way to tell "I introduced this regression"
apart from "this was already broken, not my problem" without re-deriving
that context from raw text each time, which a small local model may not do
reliably. This is a real, previously-identified gap
(`docs/HISTORY.md`'s capability audit), not a hypothetical.

Facts confirmed against the current codebase before this design was written:

- `Agent::run_auto_verification` (`crates/aivyx-core/src/agent/mod.rs:636-669`)
  synthesizes a `run_command` `ToolCall` (source `ToolCallSource::
  AutoVerification`), dispatches it, computes `passed = matches!(&result.output,
  ToolOutput::Ok(text) if command_reported_success(text))`, and pushes both
  the synthetic call and its result into `self.history` — the exact
  established "narrate an agent-internal action as a normal tool round-trip"
  idiom this project already uses (confirmed as the only safe pattern for
  this during the reasoning-visibility and multi-file-edit-atomicity specs).
- `command_reported_success` (`agent/mod.rs:1604-1607`) is `output.contains
  ("exit status:") && output.contains("(success)")` — a purely binary
  signal against `aivyx_tools::process::format_output`'s own fixed string
  template (`"exit status: {code} ({verdict})\n--- stdout ---\n{}\n---
  stderr ---\n{}"`, `crates/aivyx-tools/src/process.rs:178-193`). A failing
  test run is `ToolOutput::Ok` (not `Error`) at the tool level — a
  non-zero exit is expected, informative verification data, not a tool
  failure — so `result.output` is `Ok(text)` in both the pass and fail case,
  only the embedded verdict marker differs.
- Both `stdout`/`stderr` are already tail-capped independently at collection
  time (`drain_capped_tail`, `process.rs:140-176`) before `format_output`
  ever sees them — the "raw tail-capped output" the audit refers to.
- The turn loop (`agent/mod.rs:1224-1250`) calls `run_auto_verification`
  only when `self.unverified_edits` is true and a `VerificationConfig` is
  set, bounded by `self.verify_retries < verification.max_retries`
  (`VerificationConfig`, `agent/types.rs:77-82`) — a real, separate cap from
  the generic per-response iteration cap. On success, `unverified_edits`,
  `verify_retries`, and (autonomous mode's own, separate,
  multi-file-edit-atomicity-untouched) `pre_experiment_ref` are all reset.
- `Agent`'s existing scalar state fields relevant here (`unverified_edits:
  bool`, `verify_retries: u32`, both `agent/mod.rs:200-205`, initialized at
  `agent/mod.rs:262-263`) establish the precedent for where this spec's own
  new field belongs.
- `elide(text: &str, cap: usize) -> String` (`agent/mod.rs:1541-1553`) is
  the existing head+tail truncation helper, already used by
  `elide_oversized_tool_results` for a different, dynamic
  context-budget-driven cap — this spec's own truncation need is smaller
  and fixed, warranting its own constant rather than reusing that dynamic
  cap.
- No `use std::collections::HashSet;` currently exists in `agent/mod.rs` —
  this spec's line-comparison needs one, a new, trivial, well-justified
  import.

## Decisions (confirmed with user via one-at-a-time questions before this doc was written)

1. **Structure depth: coarse and framework-agnostic.** No test-runner-specific
   output parsing (cargo test's format alone differs from pytest's, jest's,
   go test's, and a custom script could print anything — the configured
   command is genuinely user-chosen, not always `cargo test`). Instead: a
   **line-set difference** against a stored baseline's raw output — treats
   the text as opaque diffable lines, never needing to understand what a
   "test" is.
2. **Baseline scope, corrected during brainstorming (not the original
   framing): compare against the immediately-preceding verification run,
   regardless of whether that run passed or failed** — updated after
   *every* run, not gated on success. The original "last successful run
   only" framing was found to be a real gap while writing this spec: it
   can never help in either of the two scenarios the audit actually cares
   about most — a codebase that starts with an already-failing test
   (verification would never have passed yet, so there'd be no baseline at
   all under the success-only framing) or a fix-and-retry loop where the
   model needs "is this the same failure as my last attempt, or did I just
   cause a new one" (comparing against a much-earlier pass tells you
   nothing about the last attempt specifically — the run that actually
   matters for that question). Comparing against the immediately-preceding
   run, whatever its outcome, answers both: a persistently-failing or
   starts-broken codebase still gets a comparison baseline from its very
   first verification attempt (compared against on the *second* attempt
   onward), and a fix-and-retry loop always compares against exactly the
   attempt the model just made. No proactive extra verification run is
   added anywhere — the first-ever verification call in a session still
   has nothing to compare against (there is no "immediately preceding run"
   yet), gracefully degrading to today's raw-output-only behavior for
   that one, unavoidable case.
3. **Persistence: in-memory only.** A plain `Agent` field, never written to
   the session JSON. Matches the audit's own framing ("cross-turn memory,"
   not cross-session) and needs zero session-format changes. A resumed
   session has no prior-run reference until the next verification
   re-establishes one — the same graceful degradation as Decision 2's
   first-call case.
4. **Feedback mechanism: enrich the existing message, don't invent a new
   one.** Mirrors the multi-file-edit-atomicity precedent directly: when a
   failure has new lines relative to the baseline, append a short note to
   the *same* `ToolOutput::Ok(text)` string `run_auto_verification` already
   produces and pushes to history — no new message shape, no new
   `ToolCallSource`, no wire-protocol novelty. Capped via the existing
   `elide` helper with a fixed cap, matching this file's established
   truncation convention.
5. **Honest, accepted limitation of the coarse approach**: a line that
   differs only by a timestamp, duration, memory address, or similar
   non-deterministic content will look "new" even if it's really the same
   recurring failure. This is a real tradeoff of staying framework-agnostic
   (avoiding it would require framework-specific knowledge of which lines
   are noisy, contradicting Decision 1) — the feature is a heuristic
   signal-strengthener over today's raw-only feedback, not a precise
   classifier, and must not be documented as more precise than it is.

## Changes

### 1. New `Agent` field: `last_verification_output`

`crates/aivyx-core/src/agent/mod.rs`, alongside the existing
`unverified_edits`/`verify_retries` fields:

```rust
    /// The raw `run_auto_verification` output text from the immediately
    /// preceding verification run in this session, *regardless of whether
    /// that run passed or failed* — `None` until the first verification
    /// call ever happens, then updated after every subsequent call (pass
    /// or fail alike). Used to distinguish a genuinely new failure line
    /// from one that was already present in the last attempt, whatever its
    /// outcome — see docs/superpowers/specs/
    /// 2026-07-19-structured-verification-memory-design.md's Decision 2
    /// for why this compares against the last run rather than only the
    /// last *successful* one. In-memory only; never persisted to the
    /// session JSON (Decision 3) — a resumed session starts with nothing
    /// to compare against, same as before the first verification call in
    /// a fresh session.
    last_verification_output: Option<String>,
```

Initialized to `None` alongside the existing `unverified_edits: false,
verify_retries: 0,` in `Agent::new`.

### 2. `new_lines_note` — the line-set-difference helper

New free function, alongside `elide`/`command_reported_success`:

```rust
/// Bound on the rendered new-lines note appended to a failing verification
/// result — small and fixed, unlike `elide_oversized_tool_results`'s own
/// dynamic, context-budget-driven cap, since this note is a bounded
/// addition to an already-capped tool result, not the whole history.
const NEW_LINES_NOTE_CAP: usize = 2000;

/// Compares `current` (this verification run's raw output) against
/// `previous` (the immediately preceding run's raw output, whatever its
/// outcome — see Decision 2) line-by-line, and returns a short note
/// listing lines present in `current` but absent from `previous` — a
/// rough "what's new since the last attempt" signal. `None` if every line
/// in `current` already appeared in `previous` (nothing new to report).
/// Deliberately coarse: this has no notion of what a "test" is, so a line
/// that only differs by e.g. a timestamp will still look new — see
/// docs/superpowers/specs/
/// 2026-07-19-structured-verification-memory-design.md's Decision 5.
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

`use std::collections::HashSet;` is used fully-qualified inline above
rather than added as a top-level `use` — a deliberate, minimal choice
consistent with keeping this addition self-contained; the plan may choose
either at implementation time, whichever it verifies compiles cleanly.

### 3. Wire it into `run_auto_verification`

`crates/aivyx-core/src/agent/mod.rs`, modify:

```rust
    async fn run_auto_verification(
        &mut self,
        command_name: &str,
        cwd: &Path,
        cancellation: &CancellationToken,
    ) -> bool {
        self.synthetic_seq += 1;
        let call = ToolCall {
            id: ToolCallId(format!("auto-verify-{}", self.synthetic_seq)),
            name: "run_command".to_string(),
            arguments: serde_json::json!({ "command": command_name }),
            source: ToolCallSource::AutoVerification,
        };
        self.emit(AgentEvent::ToolCallDetected(call.clone()));
        self.history.push(Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolCall(call.clone())],
            tool_call_id: None,
        });

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
        // output. See Decision 2 for why this compares against the last
        // run rather than only the last *successful* one.
        if let ToolOutput::Ok(text) = &mut result.output {
            let current_text = text.clone();
            if let Some(previous) = &self.last_verification_output
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
ToolResult(...))` moves from immediately-after-dispatch to
after the enrichment block, so the TUI-facing event and the history entry
both carry the same, possibly-enriched text — not the raw text for one and
enriched for the other.

## Out of scope for this spec

- Any real test-framework-specific output parsing (Decision 1).
- A proactive baseline-establishing verification run (Decision 2).
- Persisting the baseline to the session JSON (Decision 3).
- Any change to `command_reported_success`'s own pass/fail detection logic,
  `VerificationConfig`, the retry-cap mechanism, or the autonomous-mode
  `pre_experiment_ref` discard/rewind path — all untouched, this spec only
  enriches the *text* of an already-computed result.
- Filtering or suppressing known-noisy lines (timestamps, durations,
  etc.) — would require framework-specific knowledge, contradicting
  Decision 1; accepted as a known limitation (Decision 5) instead.
- Any new config surface — `last_verification_output` requires no
  configuration; it activates automatically the first time
  `[verification]` is already configured and a verification run happens
  at all (pass or fail).

## Testing / verification

- Unit test for `new_lines_note`: a `current` output containing a line not
  in `previous` returns `Some` with that line's content and the correct
  count; a `current` output whose every line already appears in `previous`
  (even if `previous` has additional lines `current` lacks) returns `None`.
- Unit test confirming `new_lines_note`'s output respects
  `NEW_LINES_NOTE_CAP` via `elide` when the new-lines list is large.
- Unit test on `Agent`'s turn loop, the **fix-and-retry scenario** (the
  primary case this spec exists for): a scripted verification sequence
  where the *first* attempt already fails (establishing a reference — no
  prior success required), and the *second* attempt (a retry after the
  model's fix) fails again with a mix of lines shared with the first
  attempt and genuinely new ones — assert the second failure's text (as it
  appears in `self.history`) contains the new-lines note listing only the
  lines that weren't in the first attempt's output, proving the comparison
  is against the immediately preceding attempt, not requiring any success
  to have happened first.
- Unit test on `Agent`'s turn loop, the **starts-broken-then-fixed
  scenario**: a first failing attempt, then a second attempt that passes
  cleanly — assert the second (passing) result's text is unmodified (no
  note needed on success in this case, since `command_reported_success`
  already communicates that plainly) and that `last_verification_output`
  is updated to the passing run's own text, ready to serve as the
  reference for whatever comes next.
- Unit test confirming the very first verification call ever in a fresh
  session (no prior run to compare against, `last_verification_output`
  still `None`) produces the exact same raw-output-only text as before
  this spec — the graceful-degradation case (Decision 2/3) must not
  silently break or alter existing behavior.
- Unit test confirming `last_verification_output` updates after **every**
  call regardless of outcome (pass-then-pass, fail-then-pass,
  pass-then-fail — not just "on success" as the earlier, corrected framing
  of Decision 2 would have required).
- Live E2E (through the real binary, PTY harness, per this project's
  established method): configure `[verification]` against a real toy
  project seeded with one already-failing test, have the model attempt a
  fix that resolves that failure but introduces a new one in a different
  test — confirm via the persisted session JSON that the second attempt's
  tool-result text contains the new-lines note, and that it correctly
  reflects "new since the first attempt" (the newly-broken test's output),
  not the original pre-existing failure (which was already present in the
  very first comparison point and thus wouldn't be re-flagged as "new" on
  the second attempt if it happened to still be present — construct the
  toy project so this distinction is unambiguous either way).

## Sequencing

Written now, at the user's request, as the third of four gap-closing
sub-projects (multi-file edit atomicity and reasoning visibility already
shipped; repo-map multi-language support remains). The eventual bare-metal
test-rig trial remains the motivating context, not something this spec
itself designs.
