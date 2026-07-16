# `delete_file` Tool — Design

**Status:** Approved by user 2026-07-17.

## Problem

`ActionKind::Delete` has been defined in the permission-tier enum since the
project's original security model was designed, but no tool has ever
constructed one — file deletion today is only reachable through
`run_shell`'s arbitrary command execution, gated by that tool's own
generic confirm tier rather than a dedicated, purpose-built one. This is
the last remaining item from the original tool/capability audit, framed
there as low-medium priority cleanup rather than a missing capability
(`run_shell` already covers the case), so this design stays deliberately
narrow.

## Scope

One new tool, `delete_file(path: String)`, covering single-file deletion
only. Directory/recursive deletion is an explicit non-goal (see below) —
`run_shell` remains the path for that, exactly as it is today.

## Architecture

`crates/aivyx-tools/src/tools/delete_file.rs`, following the exact
established `Tool` trait pattern every file tool already uses
(`read_file`/`write_file`/`edit_file`). No new crate, no new dependency.

Deletion itself is `tokio::fs::remove_file` — pure Rust, no subprocess
spawned at all. This isn't just simpler than shelling out to `rm`; it has
zero argv-injection surface by construction, directly sidestepping the
exact vulnerability class the `git_branch`/`git_push` phase just found and
fixed (bare positional argv values getting misparsed as CLI flags) — there
is no argv to construct in the first place.

## Permission tier

`ActionKind::Delete` + `PermissionTarget::Path(resolved)` — this tool is
`ActionKind::Delete`'s first real constructor, closing the audit gap
directly. Always confirm-gated, matching `write_file`/`edit_file`'s
existing tier (no auto-allow case for any file mutation in this project).

No `deny_paths` field on the tool itself, mirroring `write_file`/
`edit_file` (neither takes one either): `ConfirmationGate::is_denied`
already checks any `PermissionTarget::Path` against configured
`deny_paths` centrally, before any tool-specific logic runs, so a
dedicated per-tool check would be redundant. (This differs from
`git_read`/`git_commit`'s own `deny_paths` field, which those tools use to
*exclude* denied paths from broader status/diff/commit-all pathspecs — a
different problem `delete_file`, which only ever targets one explicit
path, doesn't have.)

## Behavior

- **Preview**: shows the file's current content (mirroring `write_file`'s
  existing preview pattern) so the user sees exactly what's about to be
  destroyed before approving — or, for a file that can't be read as text,
  the same "binary file" warning `write_file` already shows for an
  about-to-be-overwritten binary file, reusing that exact wording/pattern
  rather than inventing new copy.
- **Directories and nonexistent paths**: both checked in
  `permission_request` (via `std::fs::metadata` on the resolved path,
  synchronous and bounded — the same kind of preflight read
  `write_file`'s own preview-building already does), *before* any
  confirmation modal is shown — there's nothing useful to confirm for a
  call that's already known to fail. Both return
  `Err(ToolError::ExecutionFailed(...))`, not `InvalidArguments`: the
  `path` argument itself is well-formed in both cases (it's a legitimate
  string), the problem is a runtime precondition about what's actually on
  disk — matching how `git_push`'s own execute()-time
  `current_branch()` failure already uses `ExecutionFailed` for the same
  kind of "argument shape was fine, reality doesn't support it" case.
  A directory gets: "`<path>` is a directory — `delete_file` only removes
  single files; use `run_shell` for directory removal." A nonexistent path
  gets: "`<path>` does not exist."
- **`mutates_outside_session()`**: left at the trait default (`true`, not
  overridden) — hidden in Plan Mode, matching `write_file`/`edit_file`.

## Checkpoint safety net

No new plumbing needed here at all. `ToolExecutor::dispatch_inner` already
checkpoints the whole worktree before any call where
`tool.mutates_outside_session()` is true, keyed off that one trait method,
not off `ActionKind` or a hardcoded tool-name list. `delete_file` gets this
automatically for free, the same way `write_file`/`edit_file`/`run_command`/
`run_shell`/`git_branch`/`git_push`/`git_pr` all already do — a deleted
file is one `git checkout <checkpoint-ref> -- <path>` away from being
restored, which is this project's established answer to "does an
irreversible-feeling action need bespoke confirmation richness or
recovery machinery" (enforced verification's own retry-exhaustion path
already relies on the same checkpoint-rewind mechanism rather than
anything bespoke).

## Configuration

None. `delete_file` is registered unconditionally in `main.rs`, right
after `write_file`/`edit_file` — no config flag, matching how those two
tools are also always present.

## Testing strategy

- Unit tests for: successful deletion of an existing file (confirmed gone
  afterward); a clear error when the target is a directory; a clear error
  when the target doesn't exist; the preview shows file content for a
  text file; the preview shows the binary-file warning for a non-UTF8
  file; the permission request's `ActionKind`/`PermissionTarget` shape.
- No live E2E network/subprocess concerns here (pure `tokio::fs`, no
  external process) — this phase's live E2E through the real binary
  simply confirms a real `delete_file` call shows a confirmation modal,
  succeeds on approval, and that the deleted file is genuinely gone from
  disk afterward — plus (since this is the direct payoff of the checkpoint
  design) confirming the automatic pre-delete checkpoint actually lets the
  file be restored via `git checkout <ref> -- <path>`.

## Non-Goals

- Directory or recursive deletion — `run_shell` remains the path for this,
  unchanged from today.
- A `deny_paths` field on the tool itself (superseded by the existing
  central `ConfirmationGate` check, per the Permission tier section).
- Any new confirmation-richness or bespoke recovery mechanism beyond the
  existing preview + automatic checkpoint — consistent with this
  project's established pattern of deferring to checkpoints rather than
  building bespoke undo machinery per feature.
- Shelling out to `rm` or any other external command for the deletion
  itself.
