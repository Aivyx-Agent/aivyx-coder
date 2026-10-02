# /undo, /redo and /checkpoints — Design

**Date:** 2026-10-02
**Status:** Approved in brainstorming; awaiting implementation plan
**Repo:** aivyx-coder (`crates/aivyx-core`, `crates/aivyx-tools`, `crates/aivyx-tui`; uses `aivyx-checkpoint`)
**Part 1 of 4** of the aivyx-coder feature set chosen 2026-10-02. The other parts, each with its own spec:
change summary + `/diff` + `/commit`; tests found at setup + `/test`; several sessions per project.

## Why

Before every approved mutating tool call, aivyx-coder already snapshots the worktree to
`refs/aivyx/checkpoints/<ts>`. It keeps the newest 50, and the agent's own verification loop already rewinds
through `GitCheckpointer::restore_to`. A *user* who wants to take back what the assistant just did is sent to
the README, which tells them to run raw git commands with long ref names. This design adds in-app commands for
that.

## What the user gets

### /undo — take back the whole last turn

1. **Find the turn.** Pick the most recent turn that changed something. A turn is the assistant's work in
   answer to one user message, including all its edits and commands.
2. **Snapshot now.** Snapshot the current state; this becomes the `/redo` point.
3. **Compute the preview** by diffing the current state against the turn's "before" point:
   - each path with its status: modified `~`, will be removed `−`, will come back `+`;
   - "⚠ changed after the turn" on any file whose content differs from what the turn left;
   - "(git-ignored files are not touched)" when any ignored path was involved.
4. **Ask y/n** in the same modal the permission prompts use.
5. **On yes:**
   - restore the "before" point with `restore_to`;
   - add a note to the conversation the model sees: "The user undid your changes from the last turn:
     stats.py, notes.md.";
   - show "Undone: …" in the transcript.
6. **Repeat to go further back.** Running `/undo` again goes back one more turn.

### /redo — put back what the last /undo removed

- It shows the same preview and asks the same y/n question.
- The conversation note reads "The user restored your changes: …".
- Any new turn that changes files clears the redo stack.

### /checkpoints — list recent undoable turns

Read-only, newest first, one turn per line: local time, the first 60 characters of the user's message, and
the files changed (for example "+1 file, ~2 files"). Turns whose checkpoint was pruned show "too old to undo".

### Limits, stated plainly

- **Not a git repository:** "No checkpoints here — this folder isn't a git repository."
- **Nothing to undo:** "Nothing to undo — no changes made in this session." Likewise for `/redo`.
- **Turn too old:** only the newest 50 checkpoints are kept. Store the commit id as well as the ref name. If
  `git cat-file -e <oid>` fails, say "That turn is too old to undo (only the newest 50 checkpoints are kept)."
- **Ignored files:** git-ignored files are never captured or restored. The preview says so.
- **Busy agent:** while a turn is running, `/undo` and `/redo` are refused with "Wait for the reply to finish
  (or press Ctrl+C) first."
- **Other frontends:** TUI only. ACP editors have their own undo; MCP-server sessions are isolated and short.

## Design

### Turn markers (in the agent)

Add an `UndoLedger` to `crates/aivyx-core` (new module `undo.rs`):

```text
TurnMark { turn_index, user_text_preview, before_ref, before_oid, created_unix }
UndoLedger { marks: Vec<TurnMark> (newest last, capped at 50), redo: Vec<RedoMark> }
RedoMark { undone_mark: TurnMark, redo_ref, redo_oid }
```

Recording a mark:

- When the first mutating tool call of a turn completes, record a `TurnMark`.
- Use the checkpoint ref that `ToolExecutor::latest_checkpoint_ref` reports right after that call. This is the
  same anchor the existing `batch_start_ref` logic uses: dispatch checkpoints *before* running the tool, so it
  is "the state just before the turn's first change".
- Resolve the oid with `git rev-parse <ref>^{commit}`.

What clears the redo stack:

- Any turn that records a mark.
- `/clear` resets the whole ledger, matching how `/clear` already resets tasks and missions.

### Persistence

Add `undo: UndoLedger` to `SessionState`, marked `#[serde(default)]` so old session files still load. A
`--resume`d session can therefore still undo its earlier turns. Persist the ledger exactly when the session
persists today; see audit-2 fix B1 for when that happens.

### Commands

`/undo`, `/redo` and `/checkpoints` are intercepted in `Agent::run_turn`, alongside `/council` and the
routing commands. They never enter the model's history, apart from the conversation note on success.

Steps for `/undo` and `/redo`:

1. Take a checkpoint of the current state through the executor's checkpointer. This is the redo point. Label
   it `undo` so it is distinguishable in `git for-each-ref`.
2. Build the preview from `git diff --name-status <before_oid> <redo_oid>`, run through
   `aivyx_checkpoint::run_git`, which is unconfined and has fsmonitor off.
3. Flag "changed after the turn": a path counts if its content in the current state differs from its content
   at the *end* of that turn. The end of a turn is the before-point of the next mark, or the current
   checkpoint when it's the last turn.
4. Ask through the agent's `PermissionPrompter`. Use a synthetic `PermissionRequest` with
   `tool_name: "undo"`, `ActionKind::Write`, `target: Other("undo last turn")` and the preview as its
   `preview`, so the TUI's existing modal shows it.
   - It must never be cached: the request carries no cacheable target, and `UserResponse::AllowAlways` is
     treated as Allow.
   - A Deny leaves everything untouched: "Undo cancelled."
5. Restore with `restore_to(before_oid)`. Then add the note to history as a user-role system note, using the
   same mechanism other notices use (check `history` notice conventions). Pop the mark, push a `RedoMark`
   (for `/undo`) or the reverse (for `/redo`), and persist.

### Errors

Any git failure becomes "Couldn't undo: <git's message>". Nothing is left half-done: the redo checkpoint is
taken first, and `read-tree --reset -u` is atomic for the index. A failed restore leaves the redo point in the
ledger so `/redo` can recover.

## Testing

- **`UndoLedger` unit tests:** recording on first mutation only, the redo stack cleared by a new mark, the cap
  at 50, and serde round trip plus default-on-missing.
- **Integration tests with a temp git repo and a fake backend:**
  - a turn edits A and creates B; `/undo` restores A and removes B; `/redo` brings both back;
  - a hand edit to A after the turn is flagged in the preview;
  - Deny leaves the tree untouched;
  - a pruned or garbage oid gives "too old";
  - outside a git repo, the message is clear;
  - after `--resume` (restore from `SessionState`), `/undo` still works.
- **Preview text builder unit tests:** status symbols, the ⚠ flag, the ignored-files note.
- **TUI:** none beyond the existing modal tests; the modal is reused.

## Out of scope

- Undo in ACP and MCP-server modes.
- Choosing a turn from the list (`/undo N`).
- Undoing changes made outside the agent.
- Undo when git is unavailable.
