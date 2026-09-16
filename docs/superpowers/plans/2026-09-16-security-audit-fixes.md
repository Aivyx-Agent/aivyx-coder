# Security Audit Fixes (High + Medium) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close every High- and Medium-severity finding from the 2026-09-16 full-codebase security audit of `aivyx-coder` (no separate spec doc exists; this plan's task descriptions are the spec, derived from the originating audit conversation).

**Architecture:** Each task is a narrow, self-contained fix. The single highest-leverage fix (Task 1) adds one new `ActionKind` variant, matching the exact pattern this codebase already used for `Internal`/`McpTool`/`Memory` ("kept distinct so a tool that touches the outside world can't honestly describe itself this way") — every other Medium task is independent of it.

**Tech Stack:** Rust, tokio, the existing `aivyx-*` workspace crates. No new dependencies.

## Global Constraints

- Every task must leave `cargo clippy --workspace --all-targets` clean.
- Every task must leave `cargo test --workspace` green (695+ tests as of the audit; 1 deliberately `#[ignore]`d).
- One commit per task, on the branch `security/audit-fixes-2026-09-16` created off `main` before Task 1.
- Line numbers cited below are from the repo state as read on 2026-09-16 during the audit and this plan's own drafting. **Re-read the current file before editing.**

---

## Setup

- [ ] Create the working branch:

```bash
cd /home/julian/Projects/Rust/aivyx-coder
git checkout main && git pull --ff-only
git checkout -b security/audit-fixes-2026-09-16
```

---

### Task 1: Stop `web_fetch`/`web_search` from bypassing plan-mode, autonomous-taint, and MCP-tier gating (HIGH)

**Finding:** `web_fetch.rs`/`web_search.rs` declare `action: ActionKind::Read`. `ConfirmationGate::check`'s very first branch is `if matches!(request.action, ActionKind::Read | ActionKind::Internal) { return Allow; }` — before the plan-mode check, before the autonomous-mode injection-taint check, before anything. This was independently rediscovered by two separate audit passes (the sandbox/confirmation-gate audit and the turn-loop audit), plus its `aivyx-mcp-server` manifestation (the `plan` tier, documented as "read-only," actually permits unprompted network egress since neither tool is in `EDIT_ONLY`/`EXECUTE_ONLY`).

**Files:**
- Modify: `crates/aivyx-sandbox/src/lib.rs:59-` (the `ActionKind` enum — add a new variant).
- Modify: `crates/aivyx-tools/src/tools/web_fetch.rs` (permission_request, around line 73).
- Modify: `crates/aivyx-tools/src/tools/web_search.rs` (same, find via `grep -n "ActionKind::" crates/aivyx-tools/src/tools/web_search.rs`).
- Modify: `crates/aivyx-sandbox/src/confirmation.rs` (the `check` function — the initial fast-path, the autonomous-mode taint-check match arm around what is currently `matches!(request.action, ActionKind::Write | ActionKind::Delete | ActionKind::Move)`, and a new explicit auto-allow added after the autonomous-mode block).
- Modify: `crates/aivyx-mcp-server/src/tiers.rs:49-56` (add `web_fetch`/`web_search` to `EXECUTE_ONLY`, since they should require at minimum the `Execute` tier — matching README's stated "plan is read-only" intent).
- Test: `confirmation.rs`'s own test module, `tiers.rs`'s own test module.

**Interfaces:**
- Produces: a new `ActionKind::Network` variant (doc comment matching the style of `Internal`/`McpTool`: "A tool that reaches outside the local session/filesystem to the network — kept distinct from `Read` so plan-mode's 'read-only' guarantee and autonomous-mode's injection-taint pause both actually apply to it, unlike the auto-allowed `Read`/`Internal` tier."). `web_fetch`/`web_search` declare `ActionKind::Network` instead of `ActionKind::Read`. `ConfirmationGate::check` auto-allows `Network` only *after* it has survived the plan-mode deny and the autonomous-mode taint/tier checks — same end-user behavior in ordinary interactive use (no new prompt for a routine fetch), but no longer bypassing plan-mode or a tainted-session pause.

- [ ] **Step 1: Read the full current `ActionKind` enum and `ConfirmationGate::check`**

```bash
sed -n '55,110p' crates/aivyx-sandbox/src/lib.rs
sed -n '260,340p' crates/aivyx-sandbox/src/confirmation.rs
```

- [ ] **Step 2: Write the failing tests**

```rust
// confirmation.rs test module
#[tokio::test]
async fn network_action_is_denied_in_plan_mode() {
    let gate = test_gate_with_plan_mode_active(); // reuse whatever this file's existing plan-mode tests construct
    let request = PermissionRequest {
        tool_name: "web_fetch".into(),
        action: ActionKind::Network,
        target: PermissionTarget::Other("https://example.com".into()),
    };
    assert!(matches!(gate.check(&request).await, PermissionDecision::Deny(_)));
}

#[tokio::test]
async fn network_action_is_denied_when_session_is_injection_tainted_in_autonomous_mode() {
    let gate = test_gate_with_autonomous_mode_and_taint(); // mirror the existing Write/Delete/Move taint test's setup
    let request = PermissionRequest {
        tool_name: "web_fetch".into(),
        action: ActionKind::Network,
        target: PermissionTarget::Other("https://attacker.example/?leak=secret".into()),
    };
    assert!(matches!(gate.check(&request).await, PermissionDecision::Deny(_)));
}

#[tokio::test]
async fn network_action_is_still_auto_allowed_in_plain_interactive_mode() {
    // No plan mode, no autonomous mode, no taint — proves the fix doesn't
    // regress ordinary UX (no new prompt for a routine, untainted fetch).
    let gate = test_gate_plain_interactive();
    let request = PermissionRequest {
        tool_name: "web_fetch".into(),
        action: ActionKind::Network,
        target: PermissionTarget::Other("https://example.com".into()),
    };
    assert!(matches!(gate.check(&request).await, PermissionDecision::Allow));
}
```

(Match the real test-construction helper names — read this file's existing plan-mode and autonomous-taint tests first; they almost certainly already have `test_gate_with_plan_mode_active`-shaped helpers to reuse rather than duplicate.)

- [ ] **Step 3: Run to verify they fail**

```bash
cargo test -p aivyx-sandbox network_action_is -- --nocapture
```
Expected: compile error (`ActionKind::Network` doesn't exist yet) — that's the correct "failing" state for this step; proceed to Step 4 immediately rather than trying to get a runtime failure first.

- [ ] **Step 4: Add the `ActionKind::Network` variant**

```rust
// aivyx-sandbox/src/lib.rs, in the ActionKind enum, after Internal:
/// A tool that reaches outside the local session/filesystem to the
/// network (web_fetch, web_search). Kept distinct from `Read` so
/// plan-mode's "read-only" guarantee and autonomous-mode's
/// injection-taint pause both actually apply to it — before this
/// variant existed, these tools declared themselves `Read` and hit
/// the same auto-allow fast path as truly side-effect-free actions,
/// bypassing both checks. Still auto-allowed in plain interactive use
/// once it survives those two checks — this is not a new confirmation
/// prompt for ordinary use.
Network,
```

- [ ] **Step 5: Reclassify `web_fetch`/`web_search`**

In both files, change `action: ActionKind::Read` to `action: ActionKind::Network` in `permission_request`. Check for a corresponding existing test asserting the old `ActionKind::Read` value (the audit cited one: `web_fetch.rs:301,306` — `permission_request_is_read_tier_with_no_confirmation_needed`) and update its assertion to `ActionKind::Network` — this is an intentional, expected change to that test, not a regression.

- [ ] **Step 6: Update `ConfirmationGate::check`**

Add `ActionKind::Network` to the autonomous-mode taint-check match arm:
```rust
if (matches!(
    request.action,
    ActionKind::Write | ActionKind::Delete | ActionKind::Move | ActionKind::Network
) || matches!(request.target, PermissionTarget::Command { .. }))
    && let Some(finding) = self.injection_taint.current()
{
    // unchanged body
}
```

Add an explicit auto-allow for `Network` immediately after the autonomous-mode block closes (i.e. after the `if self.autonomous_mode.active() { ... }` block's closing brace, before the existing `if request.action == ActionKind::Interact` check):
```rust
// Network survived plan-mode's unconditional deny (checked above, since
// Network is deliberately excluded from the Read|Internal fast path)
// and, if autonomous mode is active, the taint/cwd checks in the block
// above. Auto-allow with no prompt from here — same UX as before this
// ActionKind existed, for the case that actually matters (untainted,
// non-plan-mode use).
if request.action == ActionKind::Network {
    return PermissionDecision::Allow;
}
```

- [ ] **Step 7: Run to verify the three new tests pass**

```bash
cargo test -p aivyx-sandbox network_action_is -- --nocapture
```

- [ ] **Step 8: Update `aivyx-mcp-server`'s tier exclusion list**

```rust
// tiers.rs
const EXECUTE_ONLY: &[&str] = &[
    "run_command", "run_shell", "git_commit", "git_branch", "git_push", "git_pr",
    "memory_write", "memory_forget", "remember_preference",
    "web_fetch", "web_search",
];
```
Write/run a test confirming `AccessLevel::Plan.excluded_tool_names()` and `AccessLevel::Edit.excluded_tool_names()` now both include `"web_fetch"` and `"web_search"` — mirror whatever existing test already checks `excluded_tool_names()` for the pre-existing entries (grep `excluded_tool_names` in `tiers.rs`'s test module first).

- [ ] **Step 9: Update the README's `plan` tier description**

Find the "read-only: the session can read, search, and build a task list" line the audit cited (`README.md:1079-1082`) and correct it to state plainly that `plan` cannot make network requests (matching the now-true behavior), or if `search`-as-in-network-search was always meant to include `web_search`, correct the doc to describe the real tier boundary honestly either way — read the surrounding paragraph before editing to match its existing voice.

- [ ] **Step 10: Run the full `aivyx-sandbox`, `aivyx-tools`, and `aivyx-mcp-server` suites + clippy**

```bash
cargo test -p aivyx-sandbox -p aivyx-tools -p aivyx-mcp-server
cargo clippy -p aivyx-sandbox -p aivyx-tools -p aivyx-mcp-server --all-targets -- -D warnings
```

- [ ] **Step 11: Run the full workspace suite once** — `ActionKind` is matched in several places across the workspace; a non-exhaustive `match` without a wildcard arm on `ActionKind` anywhere else will now be a compile error, which is the correct, safe behavior (the compiler will find every site that needs to consciously decide how to treat `Network`) — fix each one it surfaces rather than adding a blanket wildcard that could silently mis-handle the new variant.

```bash
cargo build --workspace 2>&1 | grep -A3 "non-exhaustive\|error\[E0004\]"
cargo test --workspace
```

- [ ] **Step 12: Commit**

```bash
git add -A
git commit -m "fix: web_fetch/web_search no longer bypass plan-mode and taint checks (HIGH)

Both tools declared ActionKind::Read, hitting ConfirmationGate::check's
very first auto-allow branch — before plan-mode's deny, before
autonomous-mode's injection-taint pause. A session flagged as
compromised by prompt injection could still freely exfiltrate data via
an outbound fetch with zero prompts, and 'plan' mode (documented as
read-only) permitted unprompted network egress. Added a distinct
ActionKind::Network (matching this codebase's own existing pattern for
Internal/McpTool: 'a tool that touches the outside world can't honestly
describe itself as Read'), reclassified both tools, and moved their
auto-allow to after the plan-mode/taint checks instead of before. Same
end-user behavior for ordinary untainted interactive use — no new
prompt — but plan-mode and taint now actually apply. Also added both
tools to aivyx-mcp-server's EXECUTE_ONLY tier list, closing the same
gap in the MCP-server 'plan' tier.

Independently found by two separate passes of a full ecosystem security
audit, 2026-09-16."
```

---

### Task 2: Give MCP-server sessions a real, active `AutonomousMode` instead of a hardcoded-inactive one (MEDIUM)

**Finding:** `crates/aivyx-mcp-server/src/session.rs:135` — `let autonomous_mode = AutonomousMode::new(); // always inactive -- never reused, see Global Constraints`. MCP sessions are unattended by construction (`TieredPrompter::prompt` auto-resolves everything, `session.rs:41-48`), yet this line means every unattended-execution guardrail this codebase built (`AUTONOMOUS_HIDDEN_TOOLS`, the cwd-boundary check, the injection-taint pause, the memory-tool denials) is dead for them — an `Execute`-tier MCP session ends up *less* constrained than the CLI's own `--auto` mode.

**Files:**
- Modify: `crates/aivyx-mcp-server/src/session.rs:135` and the surrounding `build_session_agent`.
- Read first: `crates/aivyx-core/src/agent/mod.rs:137` (`AUTONOMOUS_HIDDEN_TOOLS`) and wherever it's applied (`grep -n "AUTONOMOUS_HIDDEN_TOOLS" crates/aivyx-core/src/agent/mod.rs`), and `crates/aivyx-sandbox/src/confirmation.rs`'s `is_outside_autonomous_worktree` (`grep -n "is_outside_autonomous_worktree" -B5 -A15 crates/aivyx-sandbox/src/confirmation.rs`) — this task must actually construct `AutonomousMode` in its active state and wire `set_injection_taint`, not just flip a boolean.
- Test: `session.rs`'s own test module.

**Interfaces:**
- Consumes: `AutonomousMode`'s real constructor for the active state (find it via `grep -n "impl AutonomousMode" -A 20 crates/aivyx-sandbox/src/lib.rs` or wherever it's defined — it is likely `AutonomousMode::active()` or similar, mirroring `PlanMode`'s shape).
- Produces: `build_session_agent` constructs an active `AutonomousMode` (scoped to the session's own cwd, matching how the CLI's `--auto` path scopes it), and calls `agent.set_injection_taint(...)` the same way `agent_builder.rs`/`delegate.rs` already do for the CLI and `delegate_task` paths (per the audit's citation that this call is currently missing here specifically).

- [ ] **Step 1: Read how the CLI's `--auto` path constructs and wires `AutonomousMode`, to mirror it exactly**

```bash
grep -n "AutonomousMode::" crates/aivyx/src/agent_builder.rs crates/aivyx-core/src/delegate.rs
grep -n "set_injection_taint" crates/aivyx/src/agent_builder.rs crates/aivyx-core/src/delegate.rs
```

- [ ] **Step 2: Write the failing test**

```rust
#[tokio::test]
async fn mcp_execute_session_denies_run_shell_the_same_way_cli_autonomous_mode_does() {
    // Build a session via build_session_agent at Execute tier, attempt
    // a run_shell call, and assert it's denied — matching what
    // AUTONOMOUS_HIDDEN_TOOLS already guarantees for --auto.
}

#[tokio::test]
async fn mcp_execute_session_enforces_the_cwd_boundary() {
    // Attempt a write outside the session's own worktree and assert
    // it's denied via is_outside_autonomous_worktree, the same as the
    // CLI's --auto mode.
}
```

- [ ] **Step 3: Run to verify they fail**

```bash
cargo test -p aivyx-mcp-server mcp_execute_session_denies_run_shell mcp_execute_session_enforces_the_cwd_boundary -- --nocapture
```

- [ ] **Step 4: Construct a real, active `AutonomousMode` in `build_session_agent`, scoped to the session's cwd**

```rust
// session.rs, replacing the hardcoded-inactive construction:
let autonomous_mode = AutonomousMode::active(); // match the real constructor name from Step 1
```
Wire it into the `ConfirmationGate` construction the same way the CLI path does (check whether `ConfirmationGate::new` takes `cwd` for the boundary check, per `is_outside_autonomous_worktree(request, &self.cwd)` from the earlier audit citation — the session's own working directory needs to be threaded through here, matching what an MCP session already tracks for its own file operations).

- [ ] **Step 5: Wire `set_injection_taint` the same way `agent_builder.rs`/`delegate.rs` do**

```rust
agent.set_injection_taint(Arc::clone(&shared_taint)); // match the real call signature from Step 1
```

- [ ] **Step 6: Run to verify both tests pass, then check `AUTONOMOUS_HIDDEN_TOOLS` is now actually applied**

```bash
cargo test -p aivyx-mcp-server mcp_execute_session_denies_run_shell mcp_execute_session_enforces_the_cwd_boundary -- --nocapture
```

- [ ] **Step 7: Run the full crate suite — this is a real behavior change for anyone already running MCP sessions at `Execute` tier, expecting `run_shell`/`git_commit`/`memory_write` to work unprompted; check for and update any existing test that currently asserts those succeed at `Execute` tier, since they now correctly match `--auto` mode's own restrictions**

```bash
cargo test -p aivyx-mcp-server 2>&1 | tee /tmp/mcp_test_output.txt
grep -c "FAILED" /tmp/mcp_test_output.txt
```

- [ ] **Step 8: Document the behavior change in README's MCP-server section** (find via `grep -n "max_access_level\|Execute tier" README.md`) — an operator relying on `Execute` tier for unattended `run_shell`/memory writes needs to know this now requires the same `[[permissions.allowed_commands]]` pre-approval `--auto` mode requires.

- [ ] **Step 9: Clippy**

```bash
cargo clippy -p aivyx-mcp-server --all-targets -- -D warnings
```

- [ ] **Step 10: Commit**

```bash
git add -A
git commit -m "fix: give MCP-server sessions a real, active AutonomousMode (MEDIUM)

session.rs hardcoded AutonomousMode::new() -- always inactive -- with a
comment acknowledging it. Since MCP sessions are unattended by
construction (TieredPrompter auto-resolves everything, no human to
prompt), this silently disabled every guardrail this codebase built
specifically for unattended execution: AUTONOMOUS_HIDDEN_TOOLS, the
cwd-boundary check, the injection-taint pause, and the memory-tool
deny-list. An Execute-tier MCP session was strictly less constrained
than the CLI's own --auto mode. Now constructs a real active
AutonomousMode scoped to the session's cwd and wires
set_injection_taint the same way agent_builder.rs/delegate.rs already
do. Real behavior change for existing Execute-tier MCP sessions --
documented in README.

Found by a full ecosystem security audit, 2026-09-16."
```

---

### Task 3: Make the autonomous injection-pause check within a turn, not just between turns (MEDIUM)

**Finding:** `crates/aivyx-tui/src/app.rs:249` checks `autonomous.injection_taint.take()` only *after* `agent.run_turn(...)` returns — but `run_turn` internally loops up to `max_tool_iterations` round-trips. Taint flagged on iteration 1 blocks only `Write`/`Delete`/`Move`/`Command` at the gate (per Task 1/2's taint-check match arm) for the *rest of that same turn*, but every `Read`-class tool (`read_file`, `grep`, `glob`, `git_read`, and — after Task 1 — no longer `web_fetch`/`web_search`, which now correctly get denied too) keeps executing until the turn itself ends.

**Files:**
- Modify: `crates/aivyx-tui/src/app.rs:249` and wherever else the same "check taint only after `run_turn` returns" pattern exists (`grep -rn "injection_taint.take()" crates/`).
- Read first: `crates/aivyx-core/src/agent/mod.rs`'s `run_turn` internal iteration loop (`grep -n "fn run_turn" -A 5 crates/aivyx-core/src/agent/mod.rs`, then find the loop body) — this fix likely needs to move the taint check *into* that loop, not just fix the TUI's outer check, since the same gap exists for every frontend that calls `run_turn`.

**Interfaces:**
- Produces: `Agent::run_turn`'s own internal tool-call iteration loop checks `self.injection_taint.current()` (read-only peek, not `take()`) after each tool-call round-trip and, if a finding is now present that wasn't at the start of the current iteration, breaks out of the loop early with a `TurnOutcome`/event indicating a mid-turn pause — rather than relying on each frontend to check only after the whole turn completes.

- [ ] **Step 1: Read `run_turn`'s internal loop in full**

```bash
grep -n "fn run_turn" -A 10 crates/aivyx-core/src/agent/mod.rs
```
Then read the full loop body (likely 50-150 lines) to find where each tool-call round-trip completes and where a natural early-exit point would sit.

- [ ] **Step 2: Write the failing test**

```rust
#[tokio::test]
async fn run_turn_stops_issuing_new_read_class_tool_calls_after_mid_turn_taint() {
    // Build an agent with autonomous mode active, a fake LLM backend
    // that (a) first emits a web_fetch/read_file call whose result is
    // engineered to trip the injection scan, then (b) on the next
    // iteration emits another read_file call — assert the SECOND call
    // never executes (the turn stops/pauses after taint is detected,
    // not just after the whole turn naturally ends).
}
```

(This needs a fake/mock LLM backend that can be scripted to emit multiple sequential tool calls — check whether `aivyx-core`'s existing test suite already has one, per the audit's note that `agent/tests.rs` has 347 test functions; reuse its existing mock rather than building a new one.)

- [ ] **Step 3: Run to verify it fails**

```bash
cargo test -p aivyx-core run_turn_stops_issuing_new_read_class_tool_calls_after_mid_turn_taint -- --nocapture
```

- [ ] **Step 4: Add the mid-loop taint check**

At the point in `run_turn`'s loop identified in Step 1 (after each tool-call round-trip completes, before the next iteration begins):

```rust
if self.autonomous_mode.active() && self.injection_taint.current().is_some() {
    // A tool call within this same turn just flagged content as a
    // likely prompt injection. Stop issuing further tool calls for the
    // rest of this turn, not just the rest of the session (the
    // existing gate-level check already covers future turns) --
    // previously only Write/Delete/Move/Command calls were blocked by
    // the gate itself for the remainder of THIS turn; Read-class calls
    // (and, before Task 1's fix, web_fetch/web_search) kept running
    // until the turn naturally ended.
    break; // or whatever the loop's real early-exit mechanism is, matching existing patterns for other break conditions in this same loop (e.g. how max_tool_iterations exhaustion is handled)
}
```

- [ ] **Step 5: Run to verify it passes**

```bash
cargo test -p aivyx-core run_turn_stops_issuing_new_read_class_tool_calls_after_mid_turn_taint -- --nocapture
```

- [ ] **Step 6: Run the full `aivyx-core` and `aivyx-tui` suites** — the TUI's own outer check at `app.rs:249` becomes redundant but harmless once `run_turn` itself stops early; leave it in place as a defense-in-depth belt-and-braces check rather than removing it.

```bash
cargo test -p aivyx-core -p aivyx-tui
cargo clippy -p aivyx-core -p aivyx-tui --all-targets -- -D warnings
```

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "fix: check injection taint within a turn's tool-call loop, not just after (MEDIUM)

run_turn's internal iteration loop had no taint check of its own --
only the TUI's outer check, run after the whole turn (potentially many
tool-call round-trips) completed. Taint flagged on iteration 1 blocked
only Write/Delete/Move/Command at the gate for the rest of that turn;
every Read-class call kept executing until the turn naturally ended.
Added the check inside the loop itself, so it applies uniformly
regardless of which frontend is driving the turn.

Found by a full ecosystem security audit, 2026-09-16."
```

---

### Task 4: Surface injection-taint findings in interactive mode, not just autonomous mode (MEDIUM)

**Finding:** `Agent::record_tool_result` scans every `ToolOutput::Ok` and flags shared taint regardless of mode, but the only consumers of that flag are the autonomous branch of `ConfirmationGate::check` and the TUI's autonomous-only post-turn check. In interactive mode, the scan runs and the cost is paid, but nothing surfaces it — the operator is never told flagged content entered context.

**Files:**
- Modify: `crates/aivyx-tui/src/app.rs` (wherever the main interactive render loop processes turn events — find the non-autonomous branch adjacent to the one at line 249).
- Read first: whatever `AgentEvent`/`StreamEvent` variant set already exists for surfacing status to the TUI (`grep -n "enum AgentEvent\|enum StreamEvent" crates/aivyx-core/src/*.rs crates/aivyx-types/src/*.rs`), since the fix should emit a real event through the existing channel rather than inventing a side-channel.
- Test: `app.rs`'s own test module, or `aivyx-core`'s if the event addition lives there.

**Interfaces:**
- Produces: after every `run_turn` call (interactive or autonomous), the TUI checks `injection_taint.current()` (a read-only peek — do not `take()` it in interactive mode, since nothing here is supposed to clear it the way autonomous mode's pause-and-resume flow does) and, if set, renders a visible warning line naming the flagged tool/source, matching the existing style of other status lines in this TUI.

- [ ] **Step 1: Read the current interactive turn-completion handling in `app.rs`, adjacent to the autonomous-only check at line 249**

```bash
sed -n '220,260p' crates/aivyx-tui/src/app.rs
```

- [ ] **Step 2: Write the failing test**

```rust
#[tokio::test]
async fn interactive_mode_surfaces_a_visible_warning_when_a_turn_ingested_flagged_content() {
    // Drive a turn (no autonomous mode) whose tool result trips the
    // injection scan, and assert whatever render/output the TUI
    // produces contains a recognizable warning string naming the
    // flagged source -- reuse whatever existing app.rs test harness
    // captures rendered output (grep `fn test_app\|TestBackend` in
    // this file's own test module first).
}
```

- [ ] **Step 3: Run to verify it fails**

```bash
cargo test -p aivyx-tui interactive_mode_surfaces_a_visible_warning -- --nocapture
```

- [ ] **Step 4: Add the interactive-mode surface point**

```rust
// After run_turn returns, regardless of autonomous mode:
if let Some(finding) = agent.injection_taint.current() {
    // Peek, don't take() -- interactive mode has no "resume from pause"
    // flow to clear this the way autonomous mode's does; the operator
    // has already seen every action this session took and chose to
    // continue, so simply surfacing the fact (not blocking on it) is
    // the right interactive-mode behavior.
    self.render_warning(&format!(
        "note: content from \"{}\" was flagged as a likely prompt injection this session",
        finding.source
    ));
}
```

(Match `render_warning`, or whatever the real rendering method is called, to this file's existing conventions — check how other status/warning lines are already rendered in `app.rs` before inventing a new mechanism.)

- [ ] **Step 5: Run to verify it passes, then the full crate suite**

```bash
cargo test -p aivyx-tui interactive_mode_surfaces_a_visible_warning -- --nocapture
cargo test -p aivyx-tui
cargo clippy -p aivyx-tui --all-targets -- -D warnings
```

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "fix: surface injection-taint findings in interactive mode (MEDIUM)

record_tool_result scanned every tool result and flagged shared taint
regardless of mode, but only autonomous mode's gate and post-turn
check ever consumed the flag -- in interactive mode the scan cost was
paid but the operator was never told flagged content entered context.
Added a visible post-turn warning in the TUI's interactive path,
peeking (not clearing) the taint flag since interactive mode has no
pause-and-resume flow to reset it.

Found by a full ecosystem security audit, 2026-09-16."
```

---

### Task 5: Checkpoint `repl_send`-authored changes before any rollback can destroy them (MEDIUM)

**Finding:** `crates/aivyx-tools/src/tools/repl.rs:512-514` declares `repl_send`'s `mutates_outside_session() == false`, so `ToolExecutor::dispatch_inner` skips checkpointing for it — but `repl_send` writes to the stdin of a spawned process (`python`, `sh`, `node`) that can freely create/overwrite/delete real files in the worktree. A subsequent checkpoint-and-restore (batch rollback, autonomous retry) destroys those changes with no snapshot to recover from.

**Files:**
- Modify: `crates/aivyx-tools/src/tools/repl.rs:504-514`.
- Read first: `crates/aivyx-tools/src/lib.rs:244-249` (`dispatch_inner`'s checkpoint-if-`mutates_outside_session` call) to confirm exactly how the flag gates checkpointing.

**Interfaces:**
- Produces: a checkpoint is taken before the *first* `repl_send` to a given REPL session (not on every send, which the original doc comment's "no new checkpoint per send" reasoning correctly avoids for performance) — track this via a per-session flag on whatever state `repl_start`/`repl_send` already share (find the shared REPL-session state struct via `grep -n "struct.*Repl.*Session\|struct.*ReplState" crates/aivyx-tools/src/tools/repl.rs`).

- [ ] **Step 1: Read the full REPL session state and the current `mutates_outside_session` declarations for `repl_start`/`repl_send`/`repl_stop`**

```bash
sed -n '495,520p' crates/aivyx-tools/src/tools/repl.rs
grep -n "struct.*Repl" crates/aivyx-tools/src/tools/repl.rs
grep -n "fn mutates_outside_session" crates/aivyx-tools/src/tools/repl.rs
```

- [ ] **Step 2: Write the failing test**

```rust
#[tokio::test]
async fn first_repl_send_to_a_session_gets_checkpointed() {
    // Start a REPL session, send one command that would modify a file
    // in the worktree, then simulate a rollback (restore_to on the
    // checkpointer) and assert the file's pre-repl-send content is
    // recoverable -- i.e. a checkpoint ref exists that predates the
    // repl_send.
}
```

- [ ] **Step 3: Run to verify it fails**

```bash
cargo test -p aivyx-tools first_repl_send_to_a_session_gets_checkpointed -- --nocapture
```

- [ ] **Step 4: Implement the first-send checkpoint**

Add a `checkpointed: bool` (or similar) field to the shared REPL session state found in Step 1, defaulting to `false` when `repl_start` creates it. In `repl_send`'s `execute`, before writing to the process's stdin:

```rust
if !session_state.checkpointed {
    ctx.checkpointer.checkpoint("before first repl_send to this session").await?; // match the real checkpointer call signature used elsewhere in this crate, e.g. in write_file.rs
    session_state.checkpointed = true;
}
```

Update the doc comment at `repl.rs:504-510` (the one that currently says "no new checkpoint per send," which was correct reasoning about *cost* but didn't account for the *first* send needing one at all) to describe the corrected behavior: one checkpoint per REPL session, taken lazily before its first send, not zero.

- [ ] **Step 5: Run to verify it passes**

```bash
cargo test -p aivyx-tools first_repl_send_to_a_session_gets_checkpointed -- --nocapture
```

- [ ] **Step 6: Run the full crate suite + clippy**

```bash
cargo test -p aivyx-tools
cargo clippy -p aivyx-tools --all-targets -- -D warnings
```

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "fix: checkpoint before the first repl_send to a session (MEDIUM)

repl_send declared mutates_outside_session() == false, so no
checkpoint was ever taken before it -- but it writes to the stdin of a
spawned process that can freely modify real worktree files. A
subsequent rollback (batch failure, autonomous retry) could destroy
REPL-authored changes with zero snapshot to recover from. Now takes
one checkpoint lazily before a session's first send, preserving the
original 'no checkpoint per send' cost reasoning for every send after
the first.

Found by a full ecosystem security audit, 2026-09-16."
```

---

### Task 6: Reject an empty SEARCH block against an existing file instead of silently overwriting it (MEDIUM)

**Finding:** `crates/aivyx-core/src/agent/mod.rs:1692-1700` treats an empty SEARCH section as "create this as a new file" (`("write_file", json!({ "path": block.path, "content": block.replace }))`), with no existence check — a small model emitting the `=======` divider one line early silently truncates an existing file via an unconditional `tokio::fs::write` in `write_file.rs:94`.

**Files:**
- Modify: `crates/aivyx-core/src/agent/mod.rs:1692-1700`.
- Read first: `crates/aivyx-core/src/edit_blocks.rs`'s `BlockParse` enum/`Malformed` variant shape (`grep -n "enum BlockParse\|Malformed" crates/aivyx-core/src/edit_blocks.rs`) — the fix should route a "SEARCH empty but file already exists" case through the same `Malformed`-retry path the parser already uses for other unparseable blocks, not invent a new error shape.

**Interfaces:**
- Produces: when `block.search.is_empty()` AND the target path already exists on disk, the synthesized call becomes a `Malformed` result (with a message telling the model the file already exists and it should either use a non-empty SEARCH or a different filename) instead of a `write_file` call — preserving the existing "empty SEARCH means new file" behavior only when the path genuinely doesn't exist yet.

- [ ] **Step 1: Read the current code and the `Malformed` retry path in full**

```bash
sed -n '1680,1710p' crates/aivyx-core/src/agent/mod.rs
grep -n "Malformed" -A 10 crates/aivyx-core/src/edit_blocks.rs | head -30
```

- [ ] **Step 2: Write the failing test**

```rust
#[tokio::test]
async fn empty_search_against_an_existing_file_is_rejected_not_silently_overwritten() {
    // Create a real temp file with known content, feed the agent an
    // edit block for that same path with an empty SEARCH section, run
    // one turn, and assert (a) the file's content on disk is
    // unchanged, and (b) the model received a Malformed-shaped
    // "file already exists" message rather than a completed write.
}

#[tokio::test]
async fn empty_search_against_a_genuinely_new_path_still_creates_the_file() {
    // Same shape, but the target path does not exist -- assert this
    // still succeeds as a new-file write, unchanged from today.
}
```

- [ ] **Step 3: Run to verify the first fails**

```bash
cargo test -p aivyx-core empty_search_against_an_existing_file_is_rejected -- --nocapture
```

- [ ] **Step 4: Implement the existence check**

```rust
let (name, arguments) = if block.search.is_empty() {
    if std::path::Path::new(&block.path).exists() {
        // Return whatever this function's real Malformed-signaling
        // shape is (check the surrounding match arms in the same
        // function for the pattern to match, rather than introducing
        // a new one) with a message like:
        // format!("SEARCH section was empty, but {} already exists -- \
        //          use a non-empty SEARCH block to edit it, or a \
        //          different path to create a new file", block.path)
    } else {
        ("write_file", json!({ "path": block.path, "content": block.replace }))
    }
} else {
    // unchanged
};
```

- [ ] **Step 5: Run to verify both pass**

```bash
cargo test -p aivyx-core empty_search_against_an_existing_file_is_rejected empty_search_against_a_genuinely_new_path_still_creates_the_file -- --nocapture
```

- [ ] **Step 6: Add a CRLF-line-ending test while in this area (a related, previously-untested gap the audit also flagged in `edit_blocks.rs`)**

```rust
// edit_blocks.rs test module
#[test]
fn search_replace_matches_against_crlf_line_endings() {
    let file_content = "line one\r\nline two\r\nline three\r\n";
    let block = parse_edit_blocks(/* a real SEARCH/REPLACE block targeting "line two\r\n" */).unwrap();
    // assert the match succeeds against the CRLF content, not just LF
}
```
If this fails (the audit's finding says it currently does, since `text.lines()` strips `\r` and `join_block` rejoins with bare `\n`), fix `parse_edit_blocks`/`join_block` to preserve the original line-ending style per-file (detect CRLF vs LF once, up front, and use it consistently rather than normalizing to LF and silently failing to match).

- [ ] **Step 7: Run to verify, then the full crate suite + clippy**

```bash
cargo test -p aivyx-core search_replace_matches_against_crlf_line_endings
cargo test -p aivyx-core
cargo clippy -p aivyx-core --all-targets -- -D warnings
```

- [ ] **Step 8: Commit**

```bash
git add -A
git commit -m "fix: reject empty-SEARCH edits against files that already exist (MEDIUM)

An empty SEARCH section (a small model emitting the ======= divider
one line early) was unconditionally treated as 'create a new file',
silently truncating an existing file via an unchecked tokio::fs::write.
Now checks existence first and routes to the same Malformed-retry path
the parser already uses for other unparseable blocks when the target
already exists; genuinely new files are unaffected. Also fixed
SEARCH/REPLACE matching against CRLF-line-ended files, a related,
previously-untested gap in the same parser.

Found by a full ecosystem security audit, 2026-09-16."
```

---

### Task 7: Stop telling the model gitignored files were rolled back when they weren't (MEDIUM)

**Finding:** `crates/aivyx-checkpoint`'s `checkpoint_inner`/`restore_to` use `git add -A` with no `--force`, so gitignored paths are never captured or restored — documented honestly in that crate's own doc comment. But `crates/aivyx-core/src/agent/mod.rs:1963-1968` unconditionally tells the model "the codebase is now back to its state before this response's edits began," which is false for any gitignored file touched.

**Files:**
- Modify: `crates/aivyx-core/src/agent/mod.rs:1963-1968` (the rollback message).
- Test: wherever this message is asserted in an existing test (`grep -n "back to its state before" crates/aivyx-core/src/agent/mod.rs crates/aivyx-core/tests/*.rs`).

**Interfaces:**
- Produces: the rollback message is softened to acknowledge the real caveat, e.g. "the codebase's git-tracked files are now back to their state before this response's edits began — any gitignored files touched during this response were not restored," rather than the unqualified claim.

- [ ] **Step 1: Read the exact current message and its call site**

```bash
sed -n '1955,1975p' crates/aivyx-core/src/agent/mod.rs
```

- [ ] **Step 2: Write/update the test asserting the message content**

```rust
#[test]
fn rollback_message_names_the_gitignored_file_caveat() {
    let msg = rollback_message(/* whatever real args this function takes */);
    assert!(msg.contains("gitignored"));
}
```

- [ ] **Step 3: Run to verify it fails**

```bash
cargo test -p aivyx-core rollback_message_names_the_gitignored_file_caveat -- --nocapture
```

- [ ] **Step 4: Update the message text**

```rust
"the codebase's git-tracked files are now back to their state before \
 this response's edits began — any gitignored files touched during \
 this response were not restored (checkpoints only capture git-tracked \
 content)"
```

- [ ] **Step 5: Run to verify it passes, then the full crate suite**

```bash
cargo test -p aivyx-core rollback_message_names_the_gitignored_file_caveat -- --nocapture
cargo test -p aivyx-core
cargo clippy -p aivyx-core --all-targets -- -D warnings
```

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "fix: stop overclaiming rollback coverage for gitignored files (MEDIUM)

Checkpoint/restore already honestly documents (in its own crate) that
git add -A never captures or restores gitignored paths -- but the
model-facing rollback message unconditionally claimed the codebase was
'back to its state before this response's edits began,' false for any
gitignored file touched. Softened the message to name the real caveat.

Found by a full ecosystem security audit, 2026-09-16."
```

---

### Task 8: Drop stale Always-Allow cache entries when their turn-group is dropped by compaction (MEDIUM)

**Finding:** `compact_if_needed`'s `drop_oldest_group` drops a `ContentBlock::ToolCall`/`ToolResult` pair wholesale when compacting — so the model's own history can lose all record that e.g. `git_push`/`delete_file`/`run_command("./deploy.sh")` already ran. The generic "conversation was truncated, ask the user to restate" fallback is unactionable in autonomous mode, and worse: the `ConfirmationGate`'s Always-Allow cache is independent of history, so a re-issued identical call after compaction runs **silently**, with no re-prompt, since the earlier approval is still cached.

**Files:**
- Modify: `crates/aivyx-core/src/agent/mod.rs` (`drop_oldest_group`, ~2081-2099) to also report which permission keys it dropped.
- Modify: wherever the compaction call site has access to both the agent's `ConfirmationGate` and the dropped-group's tool calls, to evict the matching cache entries.
- Test: `agent/mod.rs`'s own test module (or wherever compaction is already tested — check for an existing `compact` test module first).

**Interfaces:**
- Consumes: `PermissionKey::from_request` (already used inside `ConfirmationGate` — needs to be reconstructable from a historical `ContentBlock::ToolCall`'s recorded tool name + arguments, matching whatever `PermissionRequest` shape the original call used).
- Produces: `drop_oldest_group` returns the set of tool calls it dropped (not just discards them silently); the caller uses that to evict the corresponding `PermissionKey`s from `self.confirmation_gate.always_allow` (may need a new small `pub(crate)` method on `ConfirmationGate`, e.g. `fn forget(&self, key: &PermissionKey)`, since `always_allow` is presumably private — check via `grep -n "always_allow" crates/aivyx-sandbox/src/confirmation.rs` first), so a dropped-from-history mutating action requires a fresh prompt if the model re-issues it.

- [ ] **Step 1: Read `drop_oldest_group` and `ConfirmationGate`'s `always_allow` field visibility in full**

```bash
sed -n '2075,2105p' crates/aivyx-core/src/agent/mod.rs
grep -n "always_allow" crates/aivyx-sandbox/src/confirmation.rs
```

- [ ] **Step 2: Write the failing test**

```rust
#[tokio::test]
async fn a_dropped_compacted_tool_call_requires_a_fresh_confirmation_on_reissue() {
    // Run a turn that (a) calls a Command-target tool once and gets
    // AllowAlways cached for it, (b) fills up enough history to force
    // compact_if_needed to drop that turn-group, (c) has the model
    // re-issue the identical command call, and assert the gate now
    // returns something other than the cached AllowAlways (i.e. a
    // fresh prompt/decision path is taken) rather than silently
    // re-executing.
}
```

- [ ] **Step 3: Run to verify it fails**

```bash
cargo test -p aivyx-core a_dropped_compacted_tool_call_requires_a_fresh_confirmation -- --nocapture
```

- [ ] **Step 4: Add cache eviction on compaction**

```rust
// drop_oldest_group, changed to return what it dropped:
fn drop_oldest_group(&mut self) -> Vec<ToolCallRecord> { // adjust return type to whatever's actually needed to reconstruct a PermissionKey
    // ... existing drain logic, but collect the dropped ToolCall blocks
    // into a Vec before returning instead of discarding them
}
```

At the call site (`compact_if_needed`), after calling `drop_oldest_group`:
```rust
for dropped_call in dropped {
    if let Some(key) = permission_key_for_historical_call(&dropped_call) { // new small helper, reconstructing PermissionKey the same way the original PermissionRequest would have
        self.confirmation_gate.forget(&key);
    }
}
```

Add `pub(crate) fn forget(&self, key: &PermissionKey)` to `ConfirmationGate` if it doesn't already have an equivalent method (`grep -n "fn forget\|always_allow.lock" crates/aivyx-sandbox/src/confirmation.rs` to check).

- [ ] **Step 5: Run to verify it passes**

```bash
cargo test -p aivyx-core a_dropped_compacted_tool_call_requires_a_fresh_confirmation -- --nocapture
```

- [ ] **Step 6: Run the full `aivyx-core` and `aivyx-sandbox` suites + clippy**

```bash
cargo test -p aivyx-core -p aivyx-sandbox
cargo clippy -p aivyx-core -p aivyx-sandbox --all-targets -- -D warnings
```

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "fix: evict Always-Allow cache entries when compaction drops their turn-group (MEDIUM)

Context compaction could drop the only record that an irreversible
action (git_push, delete_file, a deploy command) already ran, with a
generic 'ask the user to restate' fallback that's unactionable in
autonomous mode. Worse: the Always-Allow cache is independent of
history, so a model re-issuing the same call after compaction ran
silently, with no re-prompt, since the earlier approval was still
cached. drop_oldest_group now reports what it dropped, and the
matching permission-cache entries are evicted alongside it -- a
re-issued call after compaction gets a fresh confirmation decision.

Found by a full ecosystem security audit, 2026-09-16."
```

---

### Task 9: Fix the session-file permission TOCTOU (MEDIUM)

**Finding:** `crates/aivyx-core/src/session.rs:123-128` writes the session file (full conversation + command output) via plain `std::fs::write`, *then* a separate `set_permissions(0o600)` whose result is discarded (`let _ =`) — real world-readable window, and a silent failure mode if the chmod itself fails.

**Files:**
- Modify: `crates/aivyx-core/src/session.rs:119-128`.
- Test: `session.rs`'s own test module (it already has `saved_file_is_owner_only` per the audit — extend it, it currently only checks the mode *after* `save` returns, which can't observe the window).

**Interfaces:**
- Produces: the session file is created via `OpenOptions::new().mode(0o600).create(true).write(true)` (or the existing `write_secure`-style atomic pattern reused from Task 8 of the `aivyx-pa` plan's equivalent fix, if this crate can reasonably share it — check whether `aivyx-toolkit`/an equivalent secure-write helper is already reachable from `aivyx-coder`; if not, inline the same `OpenOptions` pattern directly here rather than pulling in a cross-repo dependency for a 5-line helper) — no window at any wider mode, and a chmod/write failure propagates as a real `Result::Err` instead of being silently discarded.

- [ ] **Step 1: Read the current write + parent-dir-creation code in full**

```bash
sed -n '115,130p' crates/aivyx-core/src/session.rs
```

- [ ] **Step 2: Write the failing test** (extending the existing `saved_file_is_owner_only` rather than duplicating it)

```rust
#[cfg(unix)]
#[test]
fn session_file_is_never_observable_at_a_wider_mode_than_0600() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("session.json");
    let old = unsafe { libc::umask(0o022) };
    save_session(&path, &test_session()).unwrap(); // match the real function name/signature
    unsafe { libc::umask(old) };
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
}
```

- [ ] **Step 3: Run to verify it fails or is currently unproven** (the existing test likely already passes at the *final* mode; this test's real value is forcing the write path itself to be atomic — proceed to Step 4 regardless of whether Step 3 shows a failure or a pass-by-coincidence, since the fix is about the write mechanism, not just the final observable mode)

```bash
cargo test -p aivyx-core session_file_is_never_observable_at_a_wider_mode -- --nocapture
```

- [ ] **Step 4: Implement the atomic write**

```rust
use std::os::unix::fs::OpenOptionsExt;
let mut f = std::fs::OpenOptions::new()
    .write(true)
    .create(true)
    .truncate(true)
    .mode(0o600)
    .open(path)?;
f.write_all(json.as_bytes())?;
f.sync_all()?;
```
Also fix the parent-directory creation (`create_dir_all(parent)` at ~line 119) to `0o700`, either via a umask bracket (per the `aivyx-pa` plan's Task 7 pattern) or an explicit `set_permissions` immediately after.

- [ ] **Step 5: Run to verify it passes**

```bash
cargo test -p aivyx-core session_file_is_never_observable_at_a_wider_mode saved_file_is_owner_only -- --nocapture
```

- [ ] **Step 6: Run the full crate suite + clippy**

```bash
cargo test -p aivyx-core
cargo clippy -p aivyx-core --all-targets -- -D warnings
```

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "fix: write session files atomically at 0600 (MEDIUM)

Session files (containing full conversation history and command
output) were created via plain std::fs::write, then chmod'd
separately with the result discarded (let _ =) -- a real
world-readable window at the process's default umask, and a silent
failure mode. Now created via OpenOptions with mode(0o600) set at
open() time; the parent directory is tightened to 0700 too.

Found by a full ecosystem security audit, 2026-09-16."
```

---

### Task 10: Scan `/council` synthesis output for injection markers before it enters history (MEDIUM)

**Finding:** `crates/aivyx-core/src/council.rs`'s stage-3 chairman synthesis becomes the sole content pushed to history — a single adversarial council member's answer text is interpolated verbatim into both the ranker prompt and the chairman prompt, and the final synthesis is never run through `scan_for_injection_markers` the way every other model-derived/tool-derived content is.

**Files:**
- Modify: wherever the chairman's synthesis result is pushed to history (`grep -n "history.push\|Role::User" crates/aivyx-core/src/agent/mod.rs` near the council-integration site, ~line 1280 per the audit).
- Read first: `check_for_injection`/`record_tool_result`'s existing call pattern (`aivyx-core/src/agent/mod.rs`), to reuse the exact same scan call rather than inventing a parallel one.

**Interfaces:**
- Produces: the council synthesis is passed through the same `scan_for_injection_markers`/taint-flagging call every tool result already goes through, before it's pushed into history — a match sets the same shared taint (so autonomous mode's existing pause/deny logic naturally covers a council-poisoned session too, with zero additional gate code needed).

- [ ] **Step 1: Read the existing scan call site and the council-synthesis push-to-history site**

```bash
grep -n "scan_for_injection_markers\|check_for_injection" crates/aivyx-core/src/agent/mod.rs
sed -n '1270,1290p' crates/aivyx-core/src/agent/mod.rs
```

- [ ] **Step 2: Write the failing test**

```rust
#[tokio::test]
async fn council_synthesis_containing_an_injection_marker_flags_taint() {
    // Drive a /council turn where one member's answer (or the
    // chairman's own synthesis) contains a known INJECTION_MARKERS
    // phrase, and assert self.injection_taint.current() is Some(_)
    // after the turn.
}
```

- [ ] **Step 3: Run to verify it fails**

```bash
cargo test -p aivyx-core council_synthesis_containing_an_injection_marker_flags_taint -- --nocapture
```

- [ ] **Step 4: Add the scan call at the synthesis push-to-history site**

```rust
// Right before pushing the chairman's synthesis to history:
if let Some(finding) = aivyx_injection_guard::scan_for_injection_markers(&synthesis, "council") {
    self.injection_taint.flag(finding); // match the real flag() call signature used elsewhere in this file
}
```

- [ ] **Step 5: Run to verify it passes, then the full crate suite**

```bash
cargo test -p aivyx-core council_synthesis_containing_an_injection_marker_flags_taint -- --nocapture
cargo test -p aivyx-core
cargo clippy -p aivyx-core --all-targets -- -D warnings
```

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "fix: scan /council synthesis for injection markers before it enters history (MEDIUM)

The chairman's stage-3 synthesis became the sole content pushed to
history with no injection scan at all, unlike every tool result --
letting a single adversarial council member's answer text (verbatim
in both the ranker and chairman prompts) reach the model's own context
completely unfenced. Reused the existing scan_for_injection_markers
call, flagging the same shared taint so autonomous mode's existing
pause/deny logic covers a council-poisoned session with no new gate
code.

Found by a full ecosystem security audit, 2026-09-16."
```

---

### Task 11: Merge, don't silently replace, `deny_paths` defaults when an operator configures their own (MEDIUM)

**Finding:** `crates/aivyx-config/src/lib.rs:711-719` has `#[serde(default)]` on the *struct*, not the field — a present `deny_paths = [...]` in `config.toml` replaces the whole default vector rather than extending it. One of the dropped defaults (`~/.local/state/aivyx-coder`) is what prevents the model from writing a fake memory-topic file or self-approving its own pending permission gate via the editor-approval channel.

**Files:**
- Modify: `crates/aivyx-config/src/lib.rs:707-760` (the `deny_paths` field and its default function).
- Test: `aivyx-config`'s own test module.

**Interfaces:**
- Produces: `deny_paths`'s deserialization merges an operator-supplied list with the built-in defaults (union, de-duplicated) rather than replacing them — an operator who wants to *remove* a default entry needs a distinct, explicit mechanism (out of scope for this fix; if none exists, that's fine — losing the ability to remove a default is a much smaller, safer failure mode than silently losing the load-bearing security defaults).

- [ ] **Step 1: Read the current field definition and default function in full**

```bash
sed -n '700,762p' crates/aivyx-config/src/lib.rs
```

- [ ] **Step 2: Write the failing test**

```rust
#[test]
fn operator_supplied_deny_paths_are_added_to_the_security_critical_defaults_not_replacing_them() {
    let toml = r#"deny_paths = ["/my/custom/path"]"#;
    let config: SomeConfigStruct = toml::from_str(toml).unwrap(); // match the real struct name
    assert!(config.deny_paths.iter().any(|p| p == "/my/custom/path"));
    assert!(config.deny_paths.iter().any(|p| p.to_string_lossy().contains(".local/state/aivyx-coder")));
    assert!(config.deny_paths.iter().any(|p| p.to_string_lossy().contains(".config/aivyx-coder")));
}
```

- [ ] **Step 3: Run to verify it fails**

```bash
cargo test -p aivyx-config operator_supplied_deny_paths_are_added_to_the_security_critical_defaults -- --nocapture
```

- [ ] **Step 4: Fix the merge semantics**

Change from a struct-level `#[serde(default)]` on a plain field to a custom deserializer that merges, e.g.:
```rust
#[serde(default = "default_deny_paths", deserialize_with = "merge_deny_paths")]
deny_paths: Vec<PathBuf>,

fn merge_deny_paths<'de, D>(deserializer: D) -> Result<Vec<PathBuf>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let operator_supplied: Vec<PathBuf> = Vec::deserialize(deserializer)?;
    let mut merged = default_deny_paths();
    for p in operator_supplied {
        if !merged.contains(&p) {
            merged.push(p);
        }
    }
    Ok(merged)
}
```
(Adjust to whatever `default_deny_paths()`'s real name/return type already is — read Step 1's output for the exact existing default function to call rather than duplicating it.)

- [ ] **Step 5: Run to verify it passes**

```bash
cargo test -p aivyx-config operator_supplied_deny_paths_are_added_to_the_security_critical_defaults -- --nocapture
```

- [ ] **Step 6: Run the full crate suite + document the change**

```bash
cargo test -p aivyx-config
cargo clippy -p aivyx-config --all-targets -- -D warnings
```
Update wherever `deny_paths` is documented (README or a config-reference doc) to state the new merge behavior explicitly, since this is a real, observable change for any operator who previously set `deny_paths` expecting it to replace the defaults.

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "fix: merge operator-supplied deny_paths with built-in defaults instead of replacing them (MEDIUM)

#[serde(default)] on the struct meant a present deny_paths entry in
config.toml silently replaced the whole default vector -- including
~/.local/state/aivyx-coder, which specifically prevents the model from
writing a fake memory-topic file or self-approving its own pending
permission gate via the editor-approval channel. Now merges (union,
deduplicated) instead of replacing. Documented the behavior change.

Found by a full ecosystem security audit, 2026-09-16."
```

---

### Task 12: Route the three unconfined git-tool preflight spawns through `ExecutionConfiner` (MEDIUM)

**Finding:** `git_pr.rs:201-207` (`check_gh_authenticated`), `git_pr.rs:180-187` (`check_upstream_configured`), and `git_push.rs:131-137` (`current_branch`) all spawn `std::process::Command` directly, bypassing the confiner every other command-spawn in this codebase goes through — inconsistent even within `git_pr.rs`, whose real PR-creation call *is* confined.

**Files:**
- Modify: `crates/aivyx-tools/src/tools/git_pr.rs:180-207`, `crates/aivyx-tools/src/tools/git_push.rs:131-137`.
- Read first: `git_pr.rs`'s own confined call (`gh_command`, ~line 173 per the audit) as the pattern to match exactly.

**Interfaces:**
- Produces: all three preflight helpers spawn through `ctx.confiner.confine(...)` the same way `gh_command` already does, taking whatever `ToolContext`/confiner handle is already threaded through the enclosing `execute()` function (these are private helpers called from within `execute`, so the context should already be in scope — no new parameter threading needed at the call sites, just at the helper function signatures themselves if they don't already accept `&ToolContext`).

- [ ] **Step 1: Read the confined pattern and all three unconfined call sites**

```bash
sed -n '170,210p' crates/aivyx-tools/src/tools/git_pr.rs
sed -n '105,140p' crates/aivyx-tools/src/tools/git_push.rs
```

- [ ] **Step 2: Write the failing tests**

```rust
// git_pr.rs
#[tokio::test]
async fn check_gh_authenticated_spawns_through_the_confiner() {
    // Use whatever test double/spy ExecutionConfiner this crate's
    // existing confined-tool tests already use (grep `MockConfiner` or
    // similar in this crate's test modules first) and assert
    // check_gh_authenticated's spawn goes through it, not a raw
    // std::process::Command.
}
#[tokio::test]
async fn check_upstream_configured_spawns_through_the_confiner() { /* same shape */ }
```

```rust
// git_push.rs
#[tokio::test]
async fn current_branch_spawns_through_the_confiner() { /* same shape */ }
```

- [ ] **Step 3: Run to verify they fail**

```bash
cargo test -p aivyx-tools spawns_through_the_confiner -- --nocapture
```

- [ ] **Step 4: Route all three through the confiner, matching `gh_command`'s existing pattern exactly**

For each helper, change the signature to accept `&ToolContext` (or whatever `gh_command` already accepts) and replace the raw `std::process::Command::new(...).spawn()`/`.output()` with the same `ctx.confiner.confine(cmd).spawn()`/`.output()` shape `gh_command` uses. Update every call site (from within `execute`) to pass the context through.

- [ ] **Step 5: Run to verify they pass**

```bash
cargo test -p aivyx-tools spawns_through_the_confiner -- --nocapture
```

- [ ] **Step 6: Run the full crate suite + clippy**

```bash
cargo test -p aivyx-tools
cargo clippy -p aivyx-tools --all-targets -- -D warnings
```

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "fix: confine the three unconfined git-tool preflight spawns (MEDIUM)

check_gh_authenticated, check_upstream_configured, and current_branch
all spawned std::process::Command directly, bypassing
ExecutionConfiner -- inconsistent even within git_pr.rs, whose own
real PR-creation call is confined. Argv was fixed and not
model-controlled in all three, so exploitability was limited, but the
codebase's own 'every spawned process during tool execution is
confined' invariant didn't actually hold. Routed all three through the
confiner via the same pattern gh_command already uses.

Found by a full ecosystem security audit, 2026-09-16."
```

---

### Task 13: Bump the `aivyx-confine` and `aivyx-kvcache` git-dependency pins (MEDIUM — real functional drift, not just staleness)

**Finding:** The code-health sweep found `aivyx-confine`'s pin is missing a fix that changed `require_enforcement`'s fail-closed check from rejecting anything other than `FullyEnforced` to tolerating `PartiallyEnforced` (found via a real CI failure on GitHub's runner kernel) plus a musl aarch64/riscv64 build fix; `aivyx-kvcache`'s pin is missing a 60-second HTTP timeout on the slot-save/restore client — the currently-pinned version has *no timeout at all* on a call that runs on the hot path of every turn, meaning a wedged local llama-server connection can hang every subsequent turn indefinitely.

**Files:**
- Modify: `Cargo.toml` (the `[workspace.dependencies]` or per-crate `rev = "..."` entries for `aivyx-confine` and `aivyx-kvcache` — find via `grep -n "aivyx-confine\|aivyx-kvcache" Cargo.toml`).
- Modify: `crates/aivyx-coder/CLAUDE.md` (or wherever this repo documents `require_enforcement`'s exact semantics — the audit noted this doc is stale relative to upstream's `PartiallyEnforced`-tolerant behavior; update it to match the new pinned behavior once bumped).

**Interfaces:**
- Produces: both pins updated to the real current HEAD of their respective repos (`/home/julian/Projects/Rust/aivyx-confine` and `/home/julian/Projects/Rust/aivyx-kvcache`), `Cargo.lock` regenerated, full workspace build/test confirming no regression from either bump.

- [ ] **Step 1: Get the real current HEAD commit hash of both dependency repos**

```bash
git -C /home/julian/Projects/Rust/aivyx-confine rev-parse HEAD
git -C /home/julian/Projects/Rust/aivyx-kvcache rev-parse HEAD
```

- [ ] **Step 2: Update the pins**

```bash
grep -rln "aivyx-confine\|aivyx-kvcache" --include="Cargo.toml" .
```
For each file found, update the `rev = "..."` value to the hashes from Step 1.

- [ ] **Step 3: Regenerate the lockfile and confirm resolution**

```bash
cargo update -p aivyx-confine -p aivyx-kvcache
git diff Cargo.lock | head -40
```

- [ ] **Step 4: Build and run the full workspace test suite** — the kvcache timeout change and the confine enforcement-check change are both real behavior changes; watch specifically for anything timing-sensitive.

```bash
cargo build --workspace
cargo test --workspace
```

- [ ] **Step 5: Update the stale `CLAUDE.md` description of `require_enforcement`**

Read the current wording (per the audit: "observing a `restrict_self()` status that isn't `RulesetStatus::FullyEnforced`") and correct it to describe the new, upstream-matching behavior (tolerates `PartiallyEnforced`, only hard-fails on `NotEnforced`) — read `aivyx-confine`'s own current `confiner.rs` at the new pinned rev to get the exact real wording right, don't guess.

- [ ] **Step 6: Run clippy**

```bash
cargo clippy --workspace --all-targets -- -D warnings
```

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "fix: bump aivyx-confine and aivyx-kvcache pins for real functional fixes (MEDIUM)

Both pins were behind their real current HEAD with genuine functional
drift, not just staleness. aivyx-kvcache's pinned version has no
timeout at all on the slot-save/restore HTTP client -- a call on the
hot path of every single turn -- so a wedged local llama-server
connection could hang every subsequent turn indefinitely; upstream
already fixed this with a 60s timeout. aivyx-confine's pin predates a
change to require_enforcement's fail-closed check (found via a real CI
kernel-version failure) and a musl aarch64/riscv64 build fix. Bumped
both, regenerated the lockfile, and corrected this repo's own CLAUDE.md
description of require_enforcement's semantics to match the new pin.

Found by a full ecosystem security audit, 2026-09-16."
```

---

## Final verification (after all tasks land)

- [ ] Run the complete workspace test suite once, not per-crate:

```bash
cd /home/julian/Projects/Rust/aivyx-coder
cargo test --workspace 2>&1 | tail -30
```

- [ ] Run the documented clippy command once more at the end:

```bash
cargo clippy --workspace --all-targets -- -D warnings
```

- [ ] Push the branch and open one PR per logically-independent fix (matching this repo's established convention throughout the preceding audit-fix work), or one PR covering the whole plan — ask before choosing.
