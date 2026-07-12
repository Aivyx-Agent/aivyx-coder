# Phase 11c: Autonomous Coding Loop — Design

**Status:** Approved, not yet implemented.
**Date:** 2026-07-12

## Context

Phase 11c is the last of the three candidate directions scoped in ROADMAP.md's Phase 11 (2026-07-11): an unattended `aivyx --auto "<goal>"` mode inspired by karpathy/autoresearch — plan a small change, apply it, run a configured verification command, keep or discard the result, repeat within a budget. ROADMAP flagged it as needing "a Phase-5-grade security design pass of its own" before any code is written, since removing the human from `ConfirmationGate`'s interactive prompt is the single most security-sensitive change this project could make.

Phase 12 (shipped 2026-07-12, commit `4d8838a`) built the two loop-mechanics prerequisites this phase needs:
- **Part A**: `AgentEvent::TurnPaused` — the per-round-trip iteration cap pauses a turn instead of failing it, so a turn can be resumed by sending another message.
- **Part B**: enforced verification — a configured `[verification] command` is auto-run via `run_command` after file edits, before a turn can end, with fix-and-retry on failure.

This design assumes both are in place and reuses them rather than re-deriving equivalent mechanics.

The capability audit that produced Phase 12 also concluded the existing security foundation (`ConfirmationGate` → checkpoints → Landlock/seccomp) "doesn't need a redesign for autonomy, only extension." This design holds to that: no existing tier's behavior changes for interactive use; autonomous mode is strictly additive.

## Goals

- Let the agent work toward a stated goal across multiple turns with no human clicking through permission modals.
- Never weaken any existing interactive-mode security property.
- Reuse Phase 7's checkpoint machinery as the keep/discard primitive, per the original ROADMAP sketch.
- Reuse Phase 12's verification loop as the keep/discard *signal*, rather than inventing a new one.
- Fail closed: anything not explicitly covered by the trust profile below is denied, not prompted (there's no one to prompt).

## Non-goals (explicitly out of scope for this version)

- **Headless/no-TUI operation.** The TUI stays up and renders normally; a human can watch and cancel (Ctrl+C) but is never required to approve anything. A fully detached/daemonized mode is a possible future iteration, not this one.
- **Numeric metric comparison.** Keep/discard is the same binary pass/fail signal Phase 12B already built (the configured command's exit code). A user wanting metric-driven iteration (e.g. "only keep if a benchmark improved") expresses that inside their own verification command/script, which exits 0 only when their own threshold is met.
- **A separate structured experiment log file.** Checkpoint refs (`git log refs/aivyx/checkpoints/...`) plus the existing session transcript are the record of what happened. No new log format.
- **Autonomous session resume** (`--auto` combined with `--resume`). New per-session state this phase introduces (`unverified_edits`, `verify_retries`, `pre_experiment_ref`) is not added to `SessionState` persistence in this version, so an interrupted autonomous run cannot be resumed — the user reviews checkpoint state manually and starts a fresh session. Revisit if this proves painful in practice.
- Any change to `run_command`'s fixed-menu design, `deny_paths`, or the Landlock/seccomp scope itself.

## Architecture: `AutonomousMode` + one new `ConfirmationGate` tier

A new `AutonomousMode(Arc<AtomicBool>)` in `aivyx-sandbox`, structurally identical to `PlanMode` (same `Relaxed`-ordering, no-data-published rationale — a toggle racing one in-flight check is acceptable, same as `PlanMode`'s own doc comment already argues). Set once at startup from the `--auto` CLI flag; unlike `PlanMode` it is not expected to toggle mid-session (no keybinding), but using the same shared-flag shape keeps `ConfirmationGate`'s consumption pattern uniform.

Two consumers, mirroring exactly how `PlanMode` already has multiple: `ConfirmationGate` holds a clone for enforcement (below), and `Agent` holds its own clone — the same wiring pattern already used for `PlanMode` (`Agent::new` takes it as a constructor argument) — for the three `self.autonomous_mode.active()` uses described later (system-prompt note, tool-list filtering, and gating the discard/rewind behavior).

`ConfirmationGate::check`'s tier order becomes:

```
1. deny_paths (hard block) — unchanged
2. Read/Internal auto-allow — unchanged
3. plan-mode deny — unchanged (still wins if both flags were ever
   simultaneously active; defense in depth, not an expected state)
4. autonomous-mode branch — NEW, detailed below
5. Always-Allow cache — unchanged
6. interactive prompt — unchanged, structurally unreachable when
   autonomous mode is active (see below)
```

**Why this approach and not the two alternatives considered:**
- *A wholly separate `AutonomousGate` reimplementing `PermissionGate` from scratch* was rejected — it would duplicate `deny_paths` checking, path resolution, and the pre-approved-commands mechanic. This project has already been bitten once by exactly this class of duplication bug (audit finding #3: the Command Always-Allow cache only hashing `program`, not `args`, because a duplicate check existed independently of the "real" one).
- *A decorator wrapping `ConfirmationGate`* was rejected because `ConfirmationGate::check` is one monolithic method with no separately-callable sub-checks — a wrapper can't safely delegate "check deny_paths, but never reach the prompt fallback" without refactoring `ConfirmationGate`'s internals anyway, at which point it isn't actually simpler than extending it directly.

### The autonomous-mode branch, in detail

Entered only when `autonomous_mode.active()` (checked once plan-mode-deny has already been ruled out):

- **`Write` or `Delete` action on a `PermissionTarget::Path`**: deny unless the resolved path (already symlink-resolved by `path_resolve::resolve`, same as every other check) is at-or-under the process's `cwd`. This is a **new check**, not currently enforced anywhere — see "Gap found during design" below for why it's necessary. `deny_paths` is still checked first (tier 1), unchanged, so this is *additional* scoping on top of it, not a replacement.
- **`Execute` action on a `PermissionTarget::Command`**: resolve directly against the Always-Allow cache (the same cache `allowed_commands` pre-seeds at `ConfirmationGate::new` construction). Found → allow. Not found → **deny, with no prompt fallback**. This is the key structural change from interactive mode: step 6 (the interactive prompt) is never reached from inside this branch.
- Everything else (`Read`/`Internal`) never reaches this branch at all — already resolved at tier 2.

**Why `git_commit` needs no special-casing.** Its permission target is `PermissionTarget::Command { program: "git", args: ["commit", "-m", <message>, ...] }` — and per Phase 7's own design, that target is deliberately never cacheable ("every distinct commit message is therefore a distinct cache key, so Always-Allow can never blanket-approve future commits"). It can therefore never be present in the Always-Allow cache, so the "not found in cache → deny" rule above denies it automatically, with zero code specific to `git_commit`.

### Gap found during design: `write_file`/`edit_file` never go through `ExecutionConfiner`

`ExecutionConfiner`/Landlock only wraps the child processes spawned by `run_command`/`run_shell`/`git_read`/`git_commit`. File edits are direct `tokio::fs` calls with no kernel-level scoping at all. In interactive mode, the *only* things stopping a model from writing to an arbitrary absolute path (e.g. `~/.bashrc`) are `deny_paths` (narrow default: `~/.ssh`, `~/.aws`) and a human seeing the target path in the confirmation modal and declining if it looks wrong. Autonomous mode removes that human glance entirely — so the new cwd-boundary check above is required, not optional, for auto-approving edits to be safe. This gap exists in interactive mode too (a human could still rubber-stamp a bad path), but it's informally mitigated there in a way that has no equivalent once nothing is watching.

## Trust profile summary

| Action | Autonomous mode |
|---|---|
| `read_file` / `grep` / `glob` / `git_read` | Auto-allowed (unchanged from today) |
| `set_tasks` | Auto-allowed (unchanged, `ActionKind::Internal`) |
| `write_file` / `edit_file` | Auto-allowed **iff** resolved target is inside `cwd` (new) and not under `deny_paths` (unchanged) |
| `run_command` | Auto-allowed **iff** the named command is a pre-seeded `allowed_commands` entry (same set already used for today's pre-approval tier) |
| `run_shell` | Not offered to the model (§ Tool availability); anything reaching the gate regardless is denied (not pre-cacheable by construction unless it happens to exactly match an `allowed_commands` entry's `sh -c` form, which is already pre-seeded today too) |
| `git_commit` | Not offered to the model (§ Tool availability); always denied at the gate as a backstop (see above) |

## Tool availability

Reuses the exact pattern `EditFormat::Prompted` already established in `Agent::run_turn_inner` (`tools.retain(|d| !PROMPTED_EDIT_HIDDEN_TOOLS.contains(&d.name.as_str()))`), not a new `ToolRegistry` method. A new constant:

```rust
const AUTONOMOUS_HIDDEN_TOOLS: &[&str] = &["run_shell", "git_commit"];
```

filtered out of the offered tool list when `self.autonomous_mode.active()`, applied to the full (non-plan-filtered) definitions list — autonomous and plan mode are mutually exclusive so this branch and the plan-mode branch never both apply. Belt-and-braces, matching plan mode exactly: hidden from the model *and* independently denied at the gate, so a hallucinated call still fails safely.

## Loop driver & experiment lifecycle

New orchestration code in the `aivyx` binary crate (not a new capability inside `Agent` beyond §"Discard on exhausted verification" below) — a thin driver that:

1. Sends the goal as the first turn's user message.
2. On `AgentEvent::TurnPaused` (Phase 12A): sends a synthesized "continue" message, budget permitting.
3. On `AgentEvent::TurnComplete`: inspects the current task list.
   - Non-empty and every task `status == Done` → **stop, goal achieved.**
   - Otherwise, budget permitting: send another "continue working toward the goal" prompt.
4. Budget exhausted at any point (§ Stopping conditions) → stop, print a summary (iterations used, final task list state, most recent checkpoint ref).
5. Ctrl+C cancels the in-flight action *and* stops the driver loop entirely — it does not send a further "continue" after a cancellation. This matches the plausible user intent of "I hit Ctrl+C because I want this to stop," not "skip to the next experiment."

If the model never calls `set_tasks` at all, the goal-achieved heuristic never fires and the loop runs until budget exhaustion — a known, acceptable degradation (documented in the system prompt note, § below) rather than a blocking requirement to invent a second signal.

### Discard on exhausted verification (new `Agent` behavior, autonomous-only)

Today (Phase 12B, unchanged for interactive use): exhausted verification retries emit a loud `AgentEvent::Error` notice and let the turn end, leaving the failed edits in place — deliberately, so a human can inspect and decide.

In autonomous mode there is no human to decide, so `Agent` additionally performs a **rewind**:

- The moment `self.unverified_edits` transitions `false → true` (the first edit of a new batch), `Agent` records the *current* checkpoint ref as `self.pre_experiment_ref`. This is safe to capture at that exact moment because `ToolExecutor::dispatch` already checkpoints synchronously *before* the mutating call executes — so "the newest checkpoint ref right now" is exactly "the state immediately before this edit."
- On exhaustion, if autonomous mode is active: restore the worktree to `pre_experiment_ref` (full restore — see risk note below), then continue the loop with remaining budget rather than stopping.
- On a *passing* verification (either mode): `pre_experiment_ref` is cleared, ready to be re-captured on the next batch's first edit.

This needs two small additions to `GitCheckpointer` (`aivyx-tools`), keeping the mechanism encapsulated where checkpointing already lives rather than duplicating git plumbing in `aivyx-core`:
- A way to query the current newest checkpoint ref.
- A way to restore the worktree to a given ref.

**Implementation risk, flagged rather than pre-decided:** a *complete* worktree restore must also remove files created during the discarded experiment, not just update tracked paths (`git checkout <ref> -- .` alone does not delete additions). The exact git plumbing for this needs verification against real git behavior before coding — the same discipline Phase 5 applied to the `landlock`/`seccompiler` crates and Phase 7 applied to the checkpoint mechanism itself, before either was implemented. Do not treat any specific command sequence in this document as decided; the implementation plan must verify and record the actual approach. Also verify: `GitCheckpointer`'s `last_tree` dedup-tracking field likely needs updating after a rewind, so the next checkpoint doesn't incorrectly compare against a tree that no longer reflects the (restored) worktree.

## Stopping conditions & config

New `[autonomous]` config section:

```toml
[autonomous]
max_iterations = 20      # total "continue" round-trips for the whole run
max_duration_secs = 3600 # wall-clock ceiling for the whole run
```

Both are hard stops, independent of and in addition to the existing per-turn `max_tool_iterations` cap (Phase 12A's territory — unchanged). Conservative defaults so a bare `--auto` without further tuning cannot run indefinitely.

**Hard startup requirement**: `--auto` refuses to start (clear error, no partial/degraded run) if `[verification] command` is not configured. Auto-approving edits is only defensible because deterministic verification is the safety net (Phase 12B, reused here as the keep/discard signal); without it, "autonomous" would mean "unchecked," which this design does not support.

## CLI

- `aivyx --auto "<goal>"`.
- Mutually exclusive with `--plan` (contradictory purposes: one is enforced read-only, the other is unattended action-taking) and, for this version, with `--resume` (see Non-goals).

## System prompt

A new `AUTONOMOUS_PROMPT` constant, appended the same way `PLAN_MODE_PROMPT`/`EDIT_FORMAT_PROMPT`/`VERIFICATION_PROMPT` already are. Must convey, at minimum:
- The agent is running unattended; edits inside the project directory are automatically approved (no need to ask or wait).
- Verification runs automatically after edits; failures are fed back for the model to fix — it does not need to (and cannot) invoke verification itself.
- Marking every task `done` via `set_tasks` is how the model signals the goal is achieved and the loop should stop; leaving tasks incomplete means the loop will prompt it to continue.

## Testing strategy

Mirrors the plan-mode and Phase 12 precedent directly:

- **Gate ordering-lock-in tests**, analogous to `plan_mode_denies_mutations_even_when_cached_or_pre_approved` and `deny_paths_wins_over_read_auto_allow`: autonomous-mode denies must win/lose in the documented tier order relative to `deny_paths` and plan mode specifically.
- **cwd-boundary check**: inside-cwd allowed, outside-cwd (including a symlink pointing outside, matching the existing `symlink_escaping_cwd_does_not_resolve_to_a_path_under_cwd` test's rigor) denied.
- **`git_commit` auto-denies with no special-casing** — a regression test proving this falls out of the existing cache-miss behavior rather than silently depending on code that could later be "simplified" away.
- **Tool-list filtering**: `run_shell`/`git_commit` absent from autonomous-mode definitions, present otherwise.
- **Discard/rewind path**: needs a real-git test fixture in the style of the existing `checkpoint.rs` tests (`init_repo`/`test_support::git`) — write, fail verification repeatedly, confirm the worktree is fully restored including deletion of newly-added files.
- **Driver-level tests**: goal-achieved-via-task-completion stop, budget-exhaustion stop, cancellation stop (no further "continue" sent).
- **One live E2E through the real binary** before calling this phase done, matching every other phase's verification bar in ROADMAP.md.

## Decision log

| Fork | Decision |
|---|---|
| Human attendance | TUI stays up; human can watch/cancel but never approves |
| `git_commit` in autonomous mode | Always denied; checkpoints are the record, human reviews and commits afterward |
| `run_shell` availability | Hidden from the model entirely, not just denied at the gate |
| Edit boundary | New gate check: writes/edits must resolve inside `cwd` |
| Keep/discard signal | Reuse Phase 12B's binary verification pass/fail exactly; no numeric metric comparison |
| Exhausted-verification behavior | Auto-rewind to the pre-experiment checkpoint (true discard), not just a notice |
| Trust-profile mechanism | Extend `ConfirmationGate` with a new tier (Approach 1), not a separate gate or a decorator |
