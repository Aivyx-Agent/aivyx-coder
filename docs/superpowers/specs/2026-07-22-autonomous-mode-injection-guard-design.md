# Autonomous-Mode Injection Guard — Design

**Status:** Approved by user 2026-07-22.

## Context

Prompted by a capability audit of whether aivyx-coder is safe to run
alongside the unrelated sibling Aivyx Agent platform on the same
hardware (a naming/PATH collision investigation, resolved separately by
renaming this project's binary to `aivyx-coder`), the conversation moved
to "what's the highest-impact next chapter for the end user." Two
candidates were weighed: multi-repo/workspace awareness (a convenience
feature for a narrow workflow) versus hardening one of the
`README.md`/`CLAUDE.md`-documented "deliberately-undefended
limitations" — indirect prompt injection, where file/command content
re-enters context untagged.

Investigation before design work narrowed the actual gap. Reading
`ConfirmationGate::check`'s autonomous-mode branch in full
(`crates/aivyx-sandbox/src/confirmation.rs:234-311`) found autonomous
mode (`--auto`) already:

- denies every `ActionKind::McpTool` and `ActionKind::Memory` call
  unconditionally (lines 244-261),
- confines `Write`/`Delete` on a `PermissionTarget::Path` to paths
  under `cwd` (`is_outside_autonomous_worktree`, lines 149-157,
  consulted at line 262),
- requires a `PermissionTarget::Command` to hit an exact
  pre-approved `(program, args)` Always-Allow cache entry — a
  different argv is a cache miss, denied unconditionally, since
  autonomous mode never prompts (lines 286-309).

So raw command injection is already fairly well contained: an injected
instruction can't make the model run an arbitrary new command in
autonomous mode, only a command byte-for-byte identical to one the user
already pre-approved. The gap is narrower and specifically in the
`Write`/`Delete`-on-`Path` branch: `is_outside_autonomous_worktree` only
inspects the *target path*, never the *content* being written. Every
file under `cwd` is unconditionally writable once past that one check —
meaning an injected instruction surfaced through a file read, command
output, `web_fetch`/`web_search` result, or MCP tool result could get
autonomous mode to silently write a backdoor into any legitimate
in-project file, entirely unattended, with nothing today to catch it.

Three design questions were resolved with the user via one-at-a-time
questions before this doc was written:

1. **Which content counts as untrusted for v1**: not just tool outputs
   (`read_file`, `grep`/`glob`, `run_command`/`run_shell` output,
   `web_fetch`, `web_search`, MCP results) but also the repo map,
   `AGENTS.md`, and the editor-context descriptor — everything that
   injects external, potentially attacker-influenced text into the
   system prompt each turn, not just tool-call results.
2. **Response when flagged content is detected**: pause the autonomous
   run and surface a warning, mirroring the existing goal-achieved /
   budget-exhausted pause mechanism, rather than a hard deny with no
   pause (would dead-end on any heuristic false positive) or a
   log-only approach (relies on a human checking after the fact, weak
   in practice).
3. **Detection method**: a static, deterministic heuristic scan (cheap,
   fast, no extra model call, fully unit-testable) rather than a
   secondary LLM classification call (extra inference round-trip on
   hardware this project already treats context budget as precious
   for, and itself promptable/foolable by the same injection).

**Explicitly out of scope**: interactive mode's confirmation modal is
unchanged. A human already sees the raw diff/command there before
approving, so provenance-tagging that surface is a smaller, separate
follow-on, not required here.

**Known, accepted limitation**: this is a tripwire, not a classifier —
it will false-positive. This project's own future docs/tests about this
very feature (including this spec, once the agent reads it) are a
near-certain example, since they necessarily contain the trigger
phrases as examples. This is an acceptable cost specifically because
the response is "pause for a human to glance at an obviously-benign
excerpt and resume," not a silent failure or a corrupted write.

## Decisions

1. **New shared type `InjectionTaint`**, in `aivyx-sandbox` alongside
   `PlanMode`/`AutonomousMode` (`crates/aivyx-sandbox/src/lib.rs:119,
   153`) — same `Arc`-shared-flag shape and rationale (cheap clone,
   consumed identically by `ConfirmationGate` and the TUI's autonomous
   loop), but backed by `Arc<Mutex<Option<InjectionFinding>>>` instead
   of `AtomicBool`, since a consumer needs to know *what* tripped it,
   not just *that* something did.

   ```rust
   pub struct InjectionFinding {
       pub source: String,          // e.g. "read_file: src/foo.rs", "web_fetch: <url>", "repo map"
       pub matched_pattern: String,
       pub excerpt: String,         // bounded length, centered on the match
   }

   pub struct InjectionTaint(Arc<Mutex<Option<InjectionFinding>>>);
   impl InjectionTaint {
       pub fn flag(&self, finding: InjectionFinding); // sets only if empty — first finding wins
       pub fn current(&self) -> Option<InjectionFinding>; // read-only peek
       pub fn take(&self) -> Option<InjectionFinding>;    // consume-and-clear
   }
   ```

   First-finding-wins (not last, not accumulated) so the recorded
   excerpt is the likely root cause, not a later echo of the same
   content re-surfacing through a different tool.

2. **New pure function `scan_for_injection_markers(text: &str) ->
   Option<InjectionFinding>`**, co-located in `aivyx-sandbox` with the
   type above (keeps the security-boundary crate the single owner of
   both). Case-insensitive substring/pattern match against a static
   phrase list (e.g. "ignore previous instructions", "disregard your
   instructions", "new system prompt", "you are now", imperative
   directives addressed to an AI/assistant embedded in ingested
   content). Runs unconditionally regardless of mode — cheap, and
   keeps `ConfirmationGate` the single mode-aware decision point rather
   than scattering `AutonomousMode::active()` checks into scan call
   sites. Scan is capped to the first ~64KB of any given text, bounding
   both cost and excerpt size for pathologically large tool output
   (ahead of the separate, existing context-compaction elision that
   happens later at budget time).

3. **Four scan call sites, all in `crates/aivyx-core/src/agent/mod.rs`**:

   - **Tool outputs** — `run_auto_verification` (its `Role::Tool` push
     at line ~701) and the main per-turn tool-dispatch loop (its
     `Role::Tool` push at line ~1473-1478) currently duplicate the same
     three-line pattern (`self.emit(AgentEvent::ToolResult(...))` +
     `self.history.push(Message { role: Role::Tool, ... })`). This spec
     extracts both into one new helper, `fn record_tool_result(&mut
     self, result: ToolResult)`, which is where the scan is added once
     — a real DRY cleanup that also happens to be the single
     centralized interception point for every tool's `ToolOutput::Ok`
     content (`read_file`, `grep`/`glob`, `run_command`/`run_shell`,
     `web_fetch`, `web_search`, MCP results, `git_read`, all of it, for
     free). `record_skipped_tool_result` (line 622) is unaffected — a
     skipped/denied call has no real ingested content to scan.
   - `refresh_repo_map` (line 407) — scans `repo_map_text` once built.
   - `refresh_agents_files` (line 427) — scans each of the global and
     project `AGENTS.md` contents before they join `sections` (the two
     `if` blocks at lines 439-448 and 451-458).
   - `refresh_editor_context` (line 358) — scans the editor-context
     descriptor text. Small surface by design: that path is already
     metadata-only (file path + cursor position, never raw file
     content), per its own module doc.

   Each match calls `self.injection_taint.flag(finding)`. `Agent` gains
   a new `injection_taint: InjectionTaint` field, set via a new
   `Agent::set_injection_taint(...)` constructor/setter, mirroring
   `set_repo_map`/`set_editor_context`'s existing pattern.

4. **`ConfirmationGate` gains an 8th constructor parameter**,
   `injection_taint: InjectionTaint`
   (`crates/aivyx-sandbox/src/confirmation.rs`). In the autonomous-mode
   branch, a new check sits after the existing `McpTool`/`Memory`
   denials (lines 244-261) and before `is_outside_autonomous_worktree`
   (line 262) — a blanket gate over every mutating action type in one
   place, rather than duplicated once for the `Path` arm and once for
   the `Command` arm of the match below it:

   ```rust
   if matches!(request.action, ActionKind::Write | ActionKind::Delete)
       || matches!(request.target, PermissionTarget::Command { .. })
   {
       if let Some(finding) = self.injection_taint.current() {
           tracing::warn!(
               tool = %request.tool_name,
               action = ?request.action,
               target = ?request.target,
               source = %finding.source,
               "permission denied: injection-flagged content ingested this session"
           );
           return PermissionDecision::Deny(Some(format!(
               "permission denied: flagged content was ingested this session \
                (possible prompt injection from {}) — autonomous mode is \
                pausing for human review",
               finding.source
           )));
       }
   }
   ```

   This only blocks *future* mutating calls after the flag is set —
   anything that already landed earlier in the same turn, before the
   scan ran, isn't retroactively undone. That's an accepted property,
   not a gap: the existing checkpoint mechanism
   (`refs/aivyx/checkpoints/<ts>`) is already the recovery path for
   anything that lands: this feature's job is to stop further damage
   and alert a human, not rewrite history.

5. **The autonomous loop stops itself**, not just the one denied call.
   `AutonomousRun` (`crates/aivyx-tui/src/app.rs:110-115`) gains a new
   field `injection_taint: InjectionTaint` — the same `Arc` clone
   `ConfirmationGate` holds. Immediately after `agent.run_turn(...)`
   returns and the existing cancellation check (line ~153-161), before
   computing `next_message` (line 163), a new check mirrors the
   existing `budget_exhausted`/`goal_achieved` pattern exactly:

   ```rust
   if let Some(finding) = autonomous.injection_taint.take() {
       agent.notify(injection_detected_notice(iterations_used, &finding));
       break;
   }
   ```

   `injection_detected_notice(iterations_used: u32, finding:
   &InjectionFinding) -> String` is a new pure function alongside
   `budget_exhausted_notice`/`goal_achieved_notice`
   (`crates/aivyx-tui/src/app.rs:54-70`), same unit-test style, reporting
   the source and a bounded excerpt so the human reviewing the pause
   knows what to go check.

6. **Wiring**: `crates/aivyx/src/agent_builder.rs` constructs one
   `InjectionTaint::new()` (alongside the existing `PlanMode::new()`/
   `AutonomousMode::new()` at lines 130/149) and clones it three ways —
   into `ConfirmationGate::new(...)` (line 161), onto the constructed
   `Agent` via the new setter, and out on the builder's return struct
   (mirroring `built.plan_mode`, consumed in `main.rs` at line 165) for
   `crates/aivyx/src/main.rs` to attach to the `AutonomousRun` literal
   it builds at line 173. `aivyx-acp` doesn't need the autonomous-loop
   half — `--acp --auto` is already rejected at startup (per
   `README.md`'s Editor integration section) — but the scan-and-flag
   half runs identically regardless of frontend, since nothing consults
   `injection_taint` outside the autonomous-mode gate branch and the
   TUI's autonomous loop; it's a harmless no-op read-side everywhere
   else.

## Out of scope for this spec

- Interactive mode's confirmation modal — unchanged, no
  provenance-tagging added there. A human already reviews the raw
  diff/command before approving.
- A secondary LLM-based classifier for detection — static heuristics
  only, per Decision (3) in Context.
- Any change to how `run_command`/`run_shell`'s existing exact-argv
  Always-Allow matching works — that mechanism already contains
  command-injection risk adequately (see Context); this spec targets
  the `Write`/`Delete`-on-`Path` gap specifically, though the
  `ConfirmationGate` check in Decision 4 also covers `Command` targets
  for completeness/defense-in-depth, not because a new gap was found
  there.
- Retroactively undoing a mutating call that already succeeded before
  the flag was set in the same turn — the existing checkpoint mechanism
  is the recovery path, not this feature.
- Tuning/expanding the phrase list beyond an initial reasonable set —
  the list is expected to evolve based on what real live-E2E runs and
  future incidents surface, not to be exhaustive at ship time.

## Testing / verification

- Unit tests for `scan_for_injection_markers`: table-driven — known
  trigger phrases match, ordinary benign text doesn't, case
  variations match, near-miss substrings don't trivially false-positive
  (e.g. the word "instructions" alone shouldn't match).
- Unit tests for `InjectionTaint`: first `flag()` call sets it, a
  second `flag()` call while already set doesn't overwrite the first
  finding, `current()` doesn't clear, `take()` does.
- `ConfirmationGate` unit tests, alongside the existing autonomous-mode
  tests in `crates/aivyx-sandbox/src/confirmation.rs`: a `Write`/
  `Delete`/`Command` request under autonomous mode with the taint
  flagged is denied with the expected reason regardless of whether it
  would otherwise have passed the worktree-boundary/pre-approval
  checks; the same request with no taint flagged is unaffected
  (regression coverage for the file's current passing tests).
- `aivyx-tui` unit tests: `injection_detected_notice` formatting,
  alongside the existing `budget_exhausted_notice_*`/
  `goal_achieved_notice_*` tests (`crates/aivyx-tui/src/app.rs:1088-1113`).
- `aivyx-core` integration test: a fake tool returning
  `ToolOutput::Ok("...ignore previous instructions...")` dispatched
  through a real `Agent::run_turn` results in the shared
  `InjectionTaint` handle being flagged end to end — proving the new
  `record_tool_result` helper actually wires the scan in, not just that
  the scan function itself works in isolation.
- Live E2E, matching this project's stated bar of proving every
  security-critical behavior against real serving: seed a project file
  containing a trigger phrase, run `--auto` with a goal that would
  naturally read that file, and confirm the run pauses with the
  injection-detected notice instead of writing anything further —
  using the existing pyte-based PTY-driving grading harness.

## Sequencing

Single implementation unit — small enough not to need decomposition:
one new shared type + scan function (`aivyx-sandbox`), one new `Agent`
helper/field + four call sites (`aivyx-core`), one new
`ConfirmationGate` branch (`aivyx-sandbox`), one new `AutonomousRun`
field + loop check + notice function (`aivyx-tui`), wired through the
one shared `agent_builder.rs` construction path both frontends already
use.
