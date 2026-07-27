# Patch-Apply Tool — Design

**Status:** Approved by user 2026-07-27.

## Context

The second-to-last item in the second capability audit's backlog
(2026-07-22, tracked in `ROADMAP.md` until the move/rename tool closed the
first item earlier the same day): edits go through `edit_file` (single
exact-substring search/replace) or a full `write_file` rewrite; there's no
tool that takes ready-made unified-diff/patch text and applies it
directly. Relevant when a model (or the user) already has a well-formed
patch rather than needing to re-derive it as a search/replace pair —
e.g. a patch pasted from elsewhere, or a model that reasons more reliably
in diff form for a multi-hunk change to one file.

Three design questions were resolved with the user via one-at-a-time
questions before this doc was written:

1. **Scope**: one file per call, matching every existing file tool's
   shape (`write_file`/`edit_file`/`delete_file`/`move_file` all take a
   single target). A model with a multi-file patch splits it into
   multiple calls, the same way it already has to for separate
   `edit_file` calls today.
2. **Preview basis**: recompute the diff from real before/after content
   (apply first, in a dry run, then render with the same `unified_diff`
   helper `write_file`/`edit_file` already use), not an echo of the
   model's raw supplied patch text. This guarantees the human always sees
   the true result — including wherever fuzzy hunk-matching (below)
   actually placed a change — rather than the model's possibly-stale
   assumption about where its patch would land.
3. **Create/delete scope**: existing files only, mirroring `edit_file`'s
   own precondition. A patch that creates a new file (diff against
   `/dev/null`) or deletes one entirely is out of scope — `write_file`/
   `delete_file` already own those operations, and supporting them here
   would mean reasoning about `ActionKind::Write` vs. `Delete` depending
   on patch *content*, which this project's tools otherwise never do
   (the action kind is always a static property of the tool, not derived
   from what a specific call's payload happens to contain).

## Decisions

### No new `ActionKind` or `PermissionTarget` — reuses `Write` + `Path`

Unlike the move/rename tool (which needed a new `ActionKind::Move` and a
two-path `PermissionTarget::Move` because it genuinely didn't fit the
existing shapes), applying a patch to an existing file is exactly the
same kind of operation `edit_file` already performs: content mutation on
one existing path. `ActionKind::Write` + `PermissionTarget::Path` cover
it precisely and honestly — there is no case here for a new gate tier the
way `Delete`/`Move` each needed one. Consequently this feature touches
**zero** lines in `aivyx-sandbox`, `aivyx-tui`, or `aivyx-acp` — the gate,
the confirmation modal, the ACP protocol mapping, and the editor-approval
channel all already handle this shape correctly for every existing
`Write`-tier tool, with full existing test coverage. The entire new
surface is one tool file in `aivyx-tools` plus its wiring.

### The library: `diffy`

New dependency for `aivyx-tools` (not previously used anywhere in this
workspace — `similar`, already a dependency, computes/renders diffs but
does not parse-and-apply external unified-diff text, which is a genuinely
different capability). `diffy::Patch::from_str` parses raw unified-diff
text; `diffy::apply(base_image: &str, patch: &Patch<'_, str>) -> Result<String, ApplyError>`
applies it to in-memory text and returns the patched string — both pure,
no filesystem I/O of diffy's own, matching this project's existing
pattern of doing the actual `tokio::fs` read/write itself around a pure
transform function (`edit_file`'s `apply_edit` is the direct precedent).

Chosen specifically for its **fuzzy hunk-position matching**: per its own
documentation, it "can detect when line numbers specified in the patch
are incorrect and will attempt to find the correct place to apply each
hunk by iterating forward and backward from the given position until all
context lines from a hunk match the base image." This is exactly the
failure mode a model-generated patch is prone to — the hunk's actual
`-`/`+`/context lines are correct, but the `@@ -X,Y +A,B @@` header's line
numbers have drifted (from the model working off slightly-stale file
content, or from an earlier hunk in the same patch already having shifted
line numbers the model didn't recompute). Without this, a hand-rolled
exact-position patch applier would reject a large fraction of otherwise-
perfectly-good model-generated patches for a cosmetic reason. This
project does not hand-roll the fuzzy-matching logic itself — it is
exactly the kind of well-trodden, easy-to-get-subtly-wrong logic (patch
utilities have had decades of edge-case hardening) this project's
existing `similar`/`ignore`/`globset` dependency choices already model:
reach for a maintained library over reimplementing something this fiddly.

### The tool (`patch_file`)

- Args: `path: String` (target, resolved via the existing
  `path_resolve::resolve` like every other file tool), `patch: String`
  (raw unified-diff text).
- `permission_request`: resolves `path`, reads its current content
  (`ExecutionFailed` if it doesn't exist, matching `edit_file`'s "cannot
  edit {path}: {err}" wording), parses `patch` via `diffy::Patch::from_str`
  (`InvalidArguments` with the parse error's message on failure — a
  malformed patch is a usage error for the model to fix, not something to
  prompt a human about), applies it via `diffy::apply` (`InvalidArguments`
  with the `ApplyError`'s own message on failure — mirrors `edit_file`'s
  "old_string not found" being `InvalidArguments`, not
  `ExecutionFailed`, since this is the model's patch failing to match the
  file, not an environment problem). If the resulting content is
  identical to the original, rejected as a no-op with the same wording
  `edit_file`'s own no-op guard uses — same reasoning: prevents a
  confused model from looping on "successful" patches that change
  nothing (`edit_file`'s doc comment cites this as an observed live
  failure mode, not a hypothetical).
- The target file is **never** derived from the patch's own `---`/`+++`
  header paths (which are frequently synthetic, e.g. `a/file.rs`/
  `b/file.rs`, and untrustworthy for anything security-relevant) — only
  the explicit `path` argument, resolved and deny_paths-checked exactly
  like every other file tool's target. Same principle `move_file`'s
  design already established: a tool's real target for permission
  purposes is always an explicit, resolved argument, never something
  parsed out of file/patch *content*.
- `execute`: re-reads the file, re-parses and re-applies the patch (same
  pure functions `permission_request` used for its dry run — no shared
  mutable state between the two calls, matching `edit_file`'s existing
  two-pass shape exactly), writes the result via `tokio::fs::write`.
- No new config surface — nothing here is tunable.

### Preview / diff content

Identical shape to `edit_file`: `preview` is `unified_diff(path, old_content, new_content)`
(the existing renderer, reused verbatim), `diff` is
`Some(DiffContent { old_content, new_content })` built from the real
before/after strings the dry-run apply produced — never a rendering of
the model's raw input patch text. This also means the editor-approval
channel's existing `ActionKind::Write` handling picks this up for free
with zero changes there, exactly as intended by reusing the existing
action kind.

## Out of scope for this spec

- Multi-file patches in one call (deferred — one `path`/`patch` pair per
  call, per the resolved scope question above).
- New-file creation or whole-file deletion via a patch (deferred — use
  `write_file`/`delete_file`).
- Any patch format other than unified diff (e.g. context diffs, `git
  diff`'s extended headers like rename/mode-change lines) — `diffy`
  parses standard unified-diff hunks; anything `Patch::from_str` can't
  parse is surfaced as the same `InvalidArguments` malformed-patch error,
  not specially detected/messaged.
- Binary file patches — `edit_file`/`write_file` already only handle text
  content in this codebase; `patch_file` inherits the same assumption
  with no special binary-file warning path (unlike `delete_file`'s/
  `move_file`'s binary-file preview fallback, since a unified diff is
  inherently a text format and a binary target simply fails to read as
  UTF-8, an `ExecutionFailed` like any other text-read failure in this
  codebase).

## Testing / verification

Tool-level (`crates/aivyx-tools/src/tools/patch_file.rs`):

- Happy path: a valid single-hunk unified diff applies cleanly, producing
  the expected new content.
- Fuzzy matching actually engages: a patch whose hunk header line numbers
  are deliberately wrong (but whose context/`-`/`+` lines still match the
  file's real content at a different location) still applies correctly —
  this is the regression test proving `diffy`'s fuzz tolerance is real,
  not just trivially exercised by exact-position patches.
- Malformed patch text (unparseable as a unified diff) is rejected as
  `InvalidArguments`.
- A patch whose context doesn't match the file's actual content anywhere
  (not just at the stated position) is rejected as `InvalidArguments`
  with `diffy`'s own error message surfaced.
- A patch targeting a nonexistent file is rejected as `ExecutionFailed`.
- A patch that reproduces the file's existing content exactly (a
  no-op) is rejected, matching `edit_file`'s no-op wording.
- `permission_request()` reports `ActionKind::Write` and
  `PermissionTarget::Path` (not a new variant — this is itself a
  meaningful assertion, proving the "no new gate primitive" design
  decision above actually holds in the shipped code).
- Preview/diff reflect the real applied content: assert `request.diff`'s
  `old_content`/`new_content` match the actual file transform, and
  `request.preview` contains the `unified_diff`-rendered text (not the
  raw input `patch` string echoed back) — this is the test that would
  catch a regression back toward the rejected "echo the supplied patch"
  alternative.

No gate-level, TUI, ACP, or editor-approval tests are needed — this
feature adds no new `ActionKind`/`PermissionTarget` variant, so every
existing test at those layers already covers this tool's behavior by
covering `ActionKind::Write`/`PermissionTarget::Path` generically.

**Live E2E verification** (manual follow-up after implementation, matching
how every other tool in this project has been verified — see
`docs/HISTORY.md`): have a real model generate a genuine unified diff for
a real multi-hunk change and apply it through the actual release binary,
confirming the permission preview renders correctly and the resulting
file content is correct.

## Documentation

`README.md` gets a new row in the Tools table (`patch_file`) and a short
prose paragraph matching `edit_file`'s/`delete_file`'s own style,
including the "existing files only, use write_file/delete_file for
create/delete" scope note and a one-line mention of the fuzzy-matching
behavior (since it's a real, user-visible property of this tool a human
approving a patch should understand: the change might land at a slightly
different line than the raw patch text implies, if the file drifted).
