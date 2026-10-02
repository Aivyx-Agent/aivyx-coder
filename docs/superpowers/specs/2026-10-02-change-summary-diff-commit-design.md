# Change summary, /diff and /commit — Design

**Date:** 2026-10-02
**Status:** Approved in brainstorming; awaiting implementation plan
**Repo:** aivyx-coder (`crates/aivyx-core`, `crates/aivyx-tools`, `crates/aivyx-tui`)
**Part 2 of 4.** Depends on part 1 (`/undo`): the turn's undo point is the baseline for the change summary and
for `/diff turn`.

## Why

After a turn the user can't see at a glance what the assistant touched. Reviewing changes means leaving the
app to run `git diff`, and committing means writing the message by hand or asking the model in a normal turn.

## What the user gets

### Change summary after every turn that changed files

One line under the reply:

```text
Changed: stats.py (+3 −1) · test_stats.py (+12, new)
```

- It comes from `git diff --numstat` between the turn's undo point and the state at the end of the turn.
- New files are marked `new`, removed files `removed`, and binary files show `(binary)` instead of counts.
- At most 5 files are listed, then "and N more".
- No line appears for a turn that changed nothing, or outside a git repository.

### /diff — review uncommitted changes

- **Default:** everything uncommitted, i.e. `git diff HEAD` plus untracked, non-ignored files shown as
  additions.
- **`/diff turn`:** only the last turn's changes, from its undo point to now.
- **Display:** a scrollable full-screen panel coloured like the approval modal's diff. Use PgUp/PgDn/arrows to
  move and Esc to close.
- **Repo with no commits yet:** diff against the empty tree (`4b825dc642cb6eb9a060e54bf8d69288fbee4904`).
- **Large diffs:** the panel shows at most 5,000 lines, then "… diff truncated (N more lines) — use git diff
  for the rest".
- **Nothing to show:** "No uncommitted changes."

### /commit — commit with a drafted message

1. **Pick what to commit.** If anything is staged, commit exactly the staged changes. Otherwise stage all
   uncommitted changes (`git add -A`, which respects `.gitignore`).
2. **Draft the message.** Send one model request, outside the conversation history, containing the staged
   diff trimmed to about a third of the context budget, with file names always listed in full. The
   instruction: "Write a git commit message for this diff: a subject line of at most 72 characters in the
   imperative mood, then optionally a blank line and a short body. Reply with the message only."
3. **Show the modal** with the message and the file list:
   - [y] commit;
   - [e] edit: puts `/commit -m "<draft>"` in the message box, and Enter commits with exactly that text;
   - [n] cancel.
4. **Commit** with the user's git identity. The user's own hooks run, through the same confined git
   invocation `git_commit` uses (`-c core.fsmonitor=false`, under the `ExecutionConfiner`).
5. **Restore staging on cancel or failure.** If `/commit` did the staging, it unstages exactly what it
   staged, so a hand-staged set is never disturbed.
6. **Report:** `Committed abc1234: <subject>`.

`/commit -m "message"` skips the model and the modal and commits immediately. The user typed the message, so
that is the approval.

**Errors, all plain:**

- "Nothing to commit."
- "The commit hook rejected it:" followed by the hook's output, with staging restored.
- "Not a git repository."
- If the model is unreachable: "Couldn't draft a message (<reason>) — commit with /commit -m \"…\"."

## Design

- **New module `crates/aivyx-core/src/changes.rs`**, pure apart from one git-runner function:
  - `parse_numstat(&str) -> Vec<FileChange>` and `parse_name_status`;
  - `summary_line(&[FileChange]) -> Option<String>`;
  - `trim_diff_for_prompt(diff, budget_chars) -> String`.
- **New module `crates/aivyx-core/src/commit.rs`:**
  - `plan_commit(cwd) -> CommitPlan { staged_by_us: Vec<PathBuf>, files, diff }`;
  - `draft_message(llm, diff, model) -> Result<String>`;
  - `do_commit(cwd, message, confiner)`;
  - `restore_staging(plan)`.

  All git calls go through `aivyx_checkpoint::run_git` (unconfined, fsmonitor off) for reads and staging. The
  final `git commit` goes through the confined path the `git_commit` tool uses. Factor that into a shared
  helper rather than duplicating it.
- **Commands:** `/diff`, `/commit` and `/commit -m` are intercepted in `Agent::run_turn` like `/undo`.
  - `/diff` emits a new `AgentEvent::ShowDiff { title, text }`, rendered by the TUI as the panel.
  - The `/commit` modal reuses the permission modal through a synthetic `PermissionRequest`
    (`tool_name: "commit"`). The TUI maps the extra `e` key to an "edit" response; add
    `UserResponse::Edit` if needed, or a commit-specific prompt seam. The plan picks the smaller change.
- **Change summary:** after a turn whose `UndoLedger` gained a mark, compute and emit
  `AgentEvent::Notice(summary_line)`. Run it before `TurnComplete`.

## Testing

- **Pure tests:** numstat and name-status parsing (renames, binary `-\t-`, new and removed), summary line
  formatting with the 5-file cap, and diff trimming.
- **Temp-repo integration tests:**
  - staged-only versus everything;
  - cancel restores staging exactly, including a hand-staged file staying staged;
  - `-m` commits without a model call (the fake backend panics if called);
  - a pre-commit hook that exits 1 gives a clear error and restored staging;
  - a repo with no commits;
  - `/diff` with an untracked file.
- **Draft prompt:** the fake backend receives a trimmed diff and the instruction; a multi-line reply is passed
  through as is.

## Out of scope

- Pushing, branches and pull requests (separate tools exist for the model).
- Partial staging (hunks) from the UI.
- `/diff` in ACP or MCP modes.
