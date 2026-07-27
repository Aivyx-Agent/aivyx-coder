# Move/Rename Tool — Design

**Status:** Approved by user 2026-07-27.

## Context

The last item in the second capability audit's backlog (2026-07-22, tracked
in `ROADMAP.md`): the tool set has `read_file`/`write_file`/`edit_file`/
`delete_file` but no atomic move/rename primitive. The model currently has
to synthesize a rename via read + write + delete — three separate
permission prompts and checkpoints for one logical operation, with no
atomicity guarantee if the write succeeds but the delete is denied (or vice
versa, leaving both an old and a new copy on disk). Logged rather than built
ad hoc, since it needs a real design pass: a new `ActionKind`, a new
`PermissionTarget` shape (the existing one has no way to carry two paths),
and updates at every exhaustive match over both enums.

Three design questions were resolved with the user via one-at-a-time
questions before this doc was written:

1. **Scope**: files *and* directories, not file-only. (`delete_file`'s
   file-only precedent was considered and explicitly not followed here —
   directory rename is a common enough "vibe coding" operation, e.g.
   reorganizing a module, that it's worth the extra deny_paths complexity
   documented below.)
2. **Destination-exists behavior**: refuse outright if the destination
   already exists (file or directory). No overwrite mode — predictable,
   matches this project's conservative-default posture, and doesn't
   destroy something the user can't see reflected in a diff. A model that
   really wants to replace an existing path can `delete_file` it first,
   which goes through its own explicit approval.
3. **Cross-filesystem rename (`EXDEV`)**: fails with a clear error rather
   than falling back to a recursive copy+delete. Keeps the implementation
   simple and keeps the tool's atomicity guarantee honest — if it succeeds,
   it was a single atomic `rename()`, not a multi-step operation that could
   be interrupted partway through.

## Decisions

### A new `ActionKind::Move`

Follows the precedent `ActionKind::Delete` already set (see its doc
comment: "every other tool in this codebase declares
`Read`/`Write`/`Execute`/`Internal`/`McpTool`") — a move is neither a pure
write nor a pure delete, and folding it into either would let the
confirmation modal, audit log, and autonomous-mode gating logic describe it
dishonestly. `mutates_outside_session()` stays at the trait's fail-closed
default (`true`), so `move_file` is hidden from Plan mode and checkpointed
before running, with no override needed.

The ACP integration already has a `ToolKind::Move` variant
(`agent-client-protocol-schema` 1.4.0+, "Moving or renaming files") sitting
unused today — this is a direct, non-improvised fit, unlike `ToolKind::Other`
which every other non-Write/Delete/Execute `ActionKind` currently falls
back to.

### A new `PermissionTarget::Move { from: PathBuf, to: PathBuf }`

The existing `PermissionTarget` (`Path(PathBuf)` / `Command{program, args}`
/ `Other(String)`) has no shape that carries two paths without losing
structure. `Other(String)` was considered and rejected: deny_paths' hard
block and the autonomous-mode worktree-boundary check both need to inspect
real `PathBuf`s, not parse them back out of a formatted string.

Every exhaustive match over `PermissionTarget` must gain a `Move` arm (5
sites, confirmed by inventory before this doc was written — all fail to
compile until updated, so none can be silently missed):

- `PermissionKey::from_request` (`aivyx-sandbox/src/confirmation.rs`) — new
  `PermissionKey::Move { action, from, to }` cache key. Approving `move
  a.rs b.rs` must not bless `move c.rs d.rs`, the same exact-target
  discipline `Command`'s cache key already documents.
- The autonomous-mode target dispatch (`ConfirmationGate::check`,
  `confirmation.rs`) — see below.
- `target_lines` (`aivyx-tui/src/app.rs`) — confirmation-modal rendering:
  a `"Move:"` label with `"{from} → {to}"` body.
- `target_string` (`aivyx-acp/src/prompter.rs`) — ACP tool-call title,
  same `"{from} → {to}"` format.
- The target-string builder in `editor_approval.rs` — same format again,
  for the editor-approval JSON payload.

Two further exhaustive matches over `ActionKind` itself also gain a `Move`
arm: the ACP `ToolKind` mapping (→ `ToolKind::Move`, per above) and
`editor_approval::build_pending_request`'s `ApprovalContent` mapping (new
`ApprovalContent::Move { from: String, to: String, preview: Option<String>
}` variant, mirroring the existing `Write`/`Delete` variants' shape).

### deny_paths: two checks at two layers

A single top-level check is not enough once directories are in scope, so
this is the part of the design that needed the most care:

1. **Gate-level, top-level** (`ConfirmationGate::is_denied`,
   `confirmation.rs`): extended to check *both* `from` and `to` against
   `deny_paths` — mirrors `is_outside_autonomous_worktree`'s existing
   pattern of checking a `Path` target's containment. Catches the direct
   cases: moving a denied path itself, or moving something on top of one.
2. **Tool-level, recursive** (`MoveFileTool` itself): the top-level check
   alone would miss a deny_paths entry *nested inside* a directory being
   moved — e.g. `deny_paths` contains `secrets/.env`, and the model moves
   `secrets/` (not `.env` itself) to `archive/secrets/`. After the move,
   `.env` now lives at a path `deny_paths` doesn't literally match,
   silently escaping protection for every future call. `MoveFileTool`
   therefore takes its own `deny_paths: Vec<PathBuf>` constructor
   parameter — the same reasoning `GrepTool`/`GlobTool` already document
   for needing `deny_paths` beyond what `permission_request`'s single-target
   check covers — and walks the source tree (via the `ignore` crate,
   already a dependency, respecting the same `.gitignore`/no-symlink-follow
   behavior `glob`/`grep` use) before returning a `PermissionRequest`,
   refusing the whole move if any descendant matches. This check runs at
   `permission_request` time (a read-only filesystem walk, consistent with
   `resolve()`'s own doc comment: reads are fine there, mutations aren't),
   so a denied nested path is caught and reported *before* the user is
   ever prompted, not after a partial move.

### The tool (`move_file`)

- Args: `from: String`, `to: String`, both resolved via the existing
  `path_resolve::resolve` (symlink-safe, `~`-expanding, consistent with
  every other file tool).
- `permission_request`: resolves both paths, confirms `from` exists
  (clear `ExecutionFailed` error if not, matching `delete_file`'s existing
  nonexistent-path check), confirms `to` does *not* exist, runs the
  recursive deny_paths scan for a directory `from`, and builds the preview
  (below).
- `execute`: a single `tokio::fs::rename(from, to)`. On `ErrorKind` mapping
  to `EXDEV`, returns a clear error naming both paths and stating the
  operation requires a common filesystem — no fallback attempted.
- No new config surface — no timeouts or tunables apply to a single atomic
  syscall, unlike `repl`'s process-lifecycle knobs.

### Preview content

- **File source**: reuses `delete_file`'s content/binary-warning preview
  shape (readable text shown in full, or a `WARNING: ... could not be read
  as text (binary file?)` fallback) headed by `"Move {from} to {to}"`.
  `diff: None` — content isn't changing, so `DiffContent`'s old/new-content
  shape doesn't apply to a relocation.
- **Directory source**: a capped listing of what will move, using the same
  `MAX_PATHS`-style truncation `glob.rs` already applies (first N entries
  from the `ignore::WalkBuilder` walk, plus a "and N more" note if
  truncated), headed the same way.

### Autonomous mode

`Move` is added alongside `Write`/`Delete` in two existing checks in
`ConfirmationGate::check`:

- `is_outside_autonomous_worktree`'s `matches!(action, Write | Delete)`
  guard — extended to include `Move`, and the target-extraction inside it
  extended to check *both* `from` and `to` resolve under the autonomous
  worktree's `cwd` (a move that reaches outside the worktree on either end
  is denied, same reasoning as `Write`/`Delete`).
- The injection-taint gate's `matches!(action, Write | Delete) ||
  matches!(target, Command{..})` guard — extended to include `Move`, so a
  tainted session can't relocate files any more than it can write or
  delete them.

The autonomous-mode target dispatch's `PermissionTarget::Path(_) |
PermissionTarget::Other(_)` catch-all arm does *not* silently absorb
`Move` — it gets its own explicit arm (after the worktree-boundary check
above already ran) that allows, consistent with how `Path`/`Other` behave
once past that check.

## Out of scope for this spec

- Overwrite mode (deferred — refuse-if-exists only, per the resolved
  question above).
- Cross-filesystem fallback via copy+delete (deferred — fails with a clear
  error instead).
- Bulk/glob-pattern moves (e.g. renaming many files by pattern in one
  call) — one `from`/`to` pair per call, matching every other file tool's
  single-target shape.
- Any special handling for moving a path that's an active REPL session's
  cwd or similar cross-tool interaction — not a real scenario since
  `repl_start` doesn't pin any filesystem path.

## Testing / verification

Tool-level (`crates/aivyx-tools/src/tools/move_file.rs`):

- File rename happy path (content and permissions preserved, source gone,
  destination present).
- Directory rename happy path (whole tree relocated atomically).
- Destination-already-exists refusal (file case and directory case).
- Nonexistent-source refusal.
- Directory move refused when a `deny_paths` entry is nested inside the
  source tree, with a clear error naming the nested path — and confirms
  nothing was moved (the walk happens before any mutation).
- `permission_request()` reports `ActionKind::Move` and
  `PermissionTarget::Move { from, to }` with both paths resolved.
- Preview: text-file source shows content headed by the `{from} → {to}`
  line; binary source shows the warning; directory source shows a capped
  listing with a truncation note when over the cap.
- `EXDEV` is surfaced as a clear, distinct error (simulate via a mocked/
  injected `io::Error` rather than requiring two real filesystems in CI).

Gate-level (`crates/aivyx-sandbox/src/confirmation.rs`):

- `deny_paths` blocks a `Move` whose `from` matches, whose `to` matches,
  and — as a defense-in-depth belt-and-braces check even though the
  tool-level scan above is the primary guard — confirms the top-level
  check alone still catches the direct (non-nested) cases if the tool-level
  scan were ever bypassed.
- Always-Allow cache key scoping: approving one `(from, to)` pair does not
  satisfy a different pair, even with the same `from` or the same `to`.
- Autonomous mode: `Move` denied when either endpoint resolves outside the
  worktree `cwd`; `Move` denied when the session is injection-tainted;
  `Move` allowed (within worktree, untainted) via the explicit dispatch
  arm.
- Plan mode: `Move` denied (via the existing `mutates_outside_session()`-
  driven Plan-mode filtering — no `Move`-specific gate logic needed here,
  same as `Write`/`Delete` today).

ACP/editor-approval mapping tests: `Move` produces the expected
`ToolKind::Move` and title string in `aivyx-acp`; `Move` produces the
expected `ApprovalContent::Move` shape in `editor_approval.rs`.

**Live E2E verification** (manual follow-up after implementation, matching
how every other security-relevant tool in this project has been verified —
see `docs/HISTORY.md`): drive a real file rename and a real directory
rename through the actual release binary on the bare-metal rig, confirming
the permission preview renders correctly in the TUI for both cases, and
that a directory move containing a `deny_paths`-listed file is refused
live, not just in the unit test.

## Documentation

`README.md` gets a new row in the Tools table (`move_file`) and a new
bullet in "Known limitations": cross-filesystem moves are refused rather
than transparently falling back to copy+delete.
