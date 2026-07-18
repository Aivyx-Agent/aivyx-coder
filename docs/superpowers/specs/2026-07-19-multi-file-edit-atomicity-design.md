# Multi-File Edit Atomicity — Design

**Status:** Approved by user 2026-07-19. First of 4 sub-projects closing the
capability gaps identified in a fresh audit of aivyx-coder's actual
code-writing ability (the other three — reasoning visibility, structured
verification memory, repo-map multi-language support — are separate, later
specs).

## Context

**Problem this spec solves:** a cross-file change (e.g. rename a function
and update its callers across 5 files) today happens as N independent
`edit_file`/`write_file` calls with no transactional guarantee. If call 3 of
5 fails, calls 1-2 have already landed on disk with no automatic way back —
the codebase is left in a partial, inconsistent state that is often *worse*
than either not having started or having fully finished, and this risk
compounds with N (each call's own reliability ceiling multiplies rather than
averaging out). This is a real, previously-identified gap (`docs/HISTORY.md`'s
capability audit), not a hypothetical.

Facts confirmed against the current codebase before this design was written:

- `ToolExecutor::dispatch_inner` (`crates/aivyx-tools/src/lib.rs:188-245`)
  processes exactly one `ToolCall` per invocation: `tool.permission_request`
  (213) → `gate.check` (216-226, a Deny returns early, no checkpoint, no
  execute) → checkpoint, only if `tool.mutates_outside_session()` (228-236)
  → `tool.execute` (244).
- Checkpointing (`crates/aivyx-tools/src/checkpoint.rs`) is strictly
  per-call: a private git index (`.git/aivyx/index`) does `add -A` (minus
  `deny_paths`), `write-tree`, `commit-tree`, then `update-ref` to a
  **brand-new ref per call**, `refs/aivyx/checkpoints/{millis:013}-{seq:04}`
  (line 148) — one ref per mutating tool call, not per turn. A tree-oid dedup
  (lines 34-37, 109-115) skips a redundant ref only when the tree is
  byte-identical to the last one (incidental, not intentional batching).
  Checkpointing is best-effort and never fails the caller (lines 74-80).
  `restore_to(ref_name, ...)` (lines 204-243) does a full private-index
  `read-tree --reset -u`, which **deletes files created since the
  checkpoint**, not just reverts modified ones — real HEAD and anything
  outside the worktree are untouched.
- `run_turn_inner` (`crates/aivyx-core/src/agent/mod.rs`) accumulates every
  tool call the model emits in **one response** into a single `Vec<ToolCall>`
  during streaming (lines 1047-1099), records all of them as one assistant
  `ContentBlock::ToolCall` message in history (1185-1201) — i.e. the model
  has already fully committed to this set of actions — and only then
  dispatches them **one at a time, sequentially, awaited in order** (the
  `for (index, call) in tool_calls.into_iter().enumerate()` loop, dispatch at
  1336-1339), pushing each result into history immediately (1353-1358).
  Confirmed: within one response, the model gets no chance to revise a later
  call based on an earlier call's outcome — call 3 was already decided
  before call 1 even executed. This is the natural, already-existing
  transaction boundary this spec adopts: **"all mutating calls in one model
  response" is the atomic unit**, requiring no new signaling from the model.
- `edit_file`'s `permission_request` (`edit_file.rs:122-158`) already does a
  full non-mutating dry run — reads the file and calls the exact same pure
  `apply_edit` (32-73) that `execute` calls again at execute time (172),
  surfacing every failure mode (empty/no-op `old_string`, zero matches with a
  nearest-miss hint, ambiguous multiple matches) before any permission is
  even granted. `write_file`'s `permission_request` (40-81) only builds a
  diff/preview and does not validate that the write will succeed — the only
  realistic `execute`-time failure there is a genuine I/O error.
- `ToolOutput` (`crates/aivyx-types/src/lib.rs:86-90`) has exactly three
  variants: `Ok(String) | Error(String) | Denied(String)` — a clean,
  pre-existing distinction between "ran and failed" (`Error`) and "the user
  said no" (`Denied`), which this spec's Decision 3 (Deny doesn't trigger
  rollback) can key off directly with no new type needed.
- `GitCheckpointer::restore_to` (`checkpoint.rs:204`) returns
  `Result<(), String>` — unlike `checkpoint()`, it is **not** documented or
  implemented as best-effort/infallible; a caller must handle its `Err`
  case explicitly rather than assume rollback always succeeds.
- No rollback is exposed to the model as a tool. `ToolExecutor::
  latest_checkpoint_ref`/`restore_to_checkpoint` (`lib.rs:159-177`) are thin
  Rust-only wrappers, used exactly once today: `Agent`'s autonomous-mode
  verification-failure path captures `pre_experiment_ref` right after the
  first unverified edit checkpoints (`agent/mod.rs:1342-1351`) and calls
  `restore_to_checkpoint` only when `autonomous_mode.active()` and
  auto-verification keeps failing past its retry cap (1252-1284).
  Interactive mode today leaves the worktree as-is on verification failure
  and tells the user to inspect/rewind manually via
  `git log refs/aivyx/checkpoints/` (1286-1289) — no in-product rollback
  exists for a human either, only the raw git ref namespace.

## Decisions (confirmed with user via one-at-a-time questions before this doc was written)

1. **Batch unit**: "all mutating tool calls dispatched from one model
   response" — the boundary the turn loop already enforces structurally, not
   a new concept the model has to signal.
2. **Trigger**: a **genuine tool execution failure** later in the same batch
   (e.g. `edit_file`'s `old_string` no longer matches, an I/O error) rolls
   back every file-touching call that succeeded earlier in the *same*
   batch, back to the checkpoint taken before the batch's first mutating
   call.
3. **Deny is not a trigger**: if the user explicitly denies one call
   partway through a batch, that is a deliberate choice ("do these two, not
   that one"), not a plan going wrong — it must NOT roll back the calls the
   user already approved earlier in the same batch.
4. **This is a new, separate mechanism, not a generalization of the existing
   one** — and it fires in interactive mode from day one, with **no new
   confirmation prompt**. The existing autonomous-only rollback
   (`pre_experiment_ref`, triggered by repeated *verification* failure
   across however many edits happened since the last successful check) is
   untouched by this spec; this spec adds a second, parallel trigger (a
   *mid-batch tool execution failure*, scoped to just the current
   response's batch) that runs in both interactive and autonomous mode.
   Rollback happens automatically, backed by a loud, un-missable notice
   (matching this project's existing "never silent" pattern for
   truncation/denial/plan-mode notices), not a blocking Allow/Deny gate on
   the rollback itself.
5. **History must stay accurate for the model's next attempt**: inject one
   clear synthetic notice, immediately after the failing call's own error
   result, explicitly listing every file/path that was just rolled back and
   stating the codebase is back to its pre-batch state — rather than
   leaving the earlier calls' now-stale "success" results unexplained in
   history and hoping the model infers the rollback from context.
6. **Scope is honest about what "rollback" can and can't undo**: the
   mechanism only reverts filesystem state (exactly what a checkpoint
   captures). A `run_command`/`run_shell` call's own side effects (output
   already shown to the user, network calls, spawned processes) are not and
   cannot be undone. If such a call fails mid-batch, any file edits that
   preceded it in the same batch still get rolled back — the failure
   trigger isn't limited to `edit_file`/`write_file` — but the notice must
   not claim the command's own effects were reverted, only the files.

## Changes

### 1. Track the batch's starting checkpoint ref

In the per-response dispatch loop in `run_turn_inner`
(`crates/aivyx-core/src/agent/mod.rs`, the `for (index, call) in
tool_calls.into_iter().enumerate()` loop), add a local variable (scoped to
this one response's batch, reset to `None` each time a new response's
`tool_calls` begins processing — **not** a struct field, since it must never
leak across turns or responses):

```rust
let mut batch_start_ref: Option<String> = None;
let mut batch_touched_paths: Vec<String> = Vec::new();
```

Immediately after a call dispatches with a genuine success (not `Denied`,
not skipped/cancelled) **and the tool is mutating**
(`tool.mutates_outside_session()`), if `batch_start_ref` is still `None`,
set it via `self.executor.latest_checkpoint_ref()` (querying the ref
*after* this call's own checkpoint-then-execute has already run inside
`dispatch_inner` — this returns exactly the ref taken immediately before
this first mutating call, i.e. the correct pre-batch state to roll back to)
and record this call's target path/description in `batch_touched_paths`.
For every subsequent successful mutating call in the same batch (while
`batch_start_ref` is already `Some`), append its target to
`batch_touched_paths` too — these are the calls a later failure would need
to unwind.

### 2. Detect a genuine tool-execution failure and roll back

When a call's dispatch result is `ToolOutput::Error(_)` (a genuine execution
failure — not `Denied`, and not a call skipped due to cancellation or the
`MAX_TOOL_CALLS_PER_RESPONSE` cap) **and `batch_start_ref` is `Some`**:

1. Call `self.executor.restore_to_checkpoint(&batch_start_ref, &cancellation)`,
   which returns `Result<(), String>` (`checkpoint.rs:204`) — not
   infallible, unlike `checkpoint()` itself.
2. **If the restore itself returns `Err`**, surface a distinct, loud error to
   the user/model: the codebase may now be in an inconsistent state that
   neither matches "all edits applied" nor "cleanly rolled back," and this
   must be reported honestly, not silently treated as a normal rollback.
3. On successful restore, push a synthetic notice into history (after the
   failing call's own error result) listing every path in
   `batch_touched_paths`, stating plainly that those edits have been undone
   and the codebase is back to its state before this batch started.
4. **Do not dispatch the remaining calls in this batch.** Their arguments
   were planned against the pre-rollback (and pre-failure) state, so
   continuing would very likely operate on assumptions that no longer hold.
   Mark them with an explicit "skipped — batch rolled back" result (matching
   this codebase's existing pattern for other skipped-result cases), so the
   call/result balance invariant in history still holds and the model can
   see clearly that they never ran.
5. Reset `batch_start_ref`/`batch_touched_paths` — the current batch is
   over; nothing further in this response's tool-call list executes.

The next model turn (bounded by the existing `max_tool_iterations` — no new
retry cap is introduced) sees the failure, the rollback notice, and the
skipped-call markers, and can retry the whole intended change fresh against
the known-good, rolled-back state.

### 3. No new config surface

This is a pure reliability improvement with no behavioral downside for
anyone who'd want the old "leave partial edits in place" behavior — no new
`[section]` flag is introduced. Always on, for both interactive and
autonomous mode.

## Out of scope for this spec

- Any new tool exposing rollback to the model directly (e.g. a
  model-invokable "undo my last batch") — this spec's rollback is
  agent-loop-internal, triggered automatically on a detected failure, not
  something the model calls itself.
- Reverting anything a `run_command`/`run_shell` call itself did beyond the
  filesystem (network calls, spawned processes, already-shown output) —
  Decision 6.
- A confirmation prompt before rollback — Decision 4.
- Any change to `deny_paths`, the permission-gate tier order, or the
  Always-Allow cache — this spec only changes what happens *after* a call
  has already been individually approved and then later fails.
- Any change to the *existing* autonomous-mode verification-failure
  rollback (`pre_experiment_ref`, `agent/mod.rs:1252-1284`) — that
  mechanism is untouched; this spec adds a new, separate, parallel trigger
  (Decision 4) rather than modifying or merging with it.
- Dedicated rename/move-with-import-fixup primitives (the audit's other
  suggested angle on this same gap) — a narrower, LSP-assisted mechanism
  that would help the common "rename" case specifically; a separate,
  future spec if ever pursued, not a substitute for general batch rollback.
- Cross-*turn* atomicity (a change spanning multiple separate model
  responses, each individually completing before the next begins) — out of
  scope; the batch unit is strictly "one response," per Decision 1.

## Testing / verification

- Unit tests for the batch-tracking logic in `run_turn_inner`: a batch of 2+
  successful mutating calls followed by a failing one triggers exactly one
  `restore_to_checkpoint` call, targeting the ref captured before the
  batch's first mutating call (not any later one).
- Unit test confirming a `Denied` call partway through a batch does NOT
  trigger rollback of the earlier successful calls in the same batch
  (Decision 3).
- Unit test confirming the rollback notice lists every touched path from
  the batch, and that remaining not-yet-dispatched calls in the same batch
  are marked as skipped rather than executed.
- Unit test confirming a single, solo mutating call that fails (no earlier
  successful call in the same batch, `batch_start_ref` is `None`) behaves
  exactly as it does today — no rollback attempted, since there is nothing
  to roll back.
- Unit test confirming this fires identically in both interactive and
  autonomous mode (Decision 4) — no mode-gating left over from the
  autonomous-only code path this spec extends.
- Live E2E (through the real binary, PTY harness, per this project's
  established method): prompt the model to make a genuinely cross-file
  change where one edit is engineered to fail (e.g. a stale `old_string`
  that won't match after an earlier renaming edit in the same response),
  and confirm via the persisted session JSON both that the earlier edit's
  file is back to its original content on disk and that history contains
  the rollback notice.

## Sequencing

Written now, at the user's request, as the first of four gap-closing
sub-projects (this one, then reasoning visibility, structured verification
memory, and repo-map multi-language support, in whatever order the user
picks after this one ships) — not the whole set combined into one spec. The
eventual bare-metal test-rig trial is the motivating context, not something
this spec itself designs.
