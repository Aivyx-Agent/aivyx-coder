# Several sessions per project — Design

**Date:** 2026-10-02
**Status:** Approved in brainstorming; awaiting implementation plan
**Repo:** aivyx-coder (`crates/aivyx-core` session + agent, `crates/aivyx-tui`, `crates/aivyx` CLI)
**Part 4 of 4.** It builds on audit-2 fix B1 (a failed first turn never persists) and stores part 1's undo
ledger per session.

## Why

Each project has exactly one session slot (`~/.local/state/aivyx-coder/sessions/<project-key>.json`). Starting
aivyx-coder without `--resume` begins a fresh conversation, and its first completed turn silently replaces the
saved one. `/clear` wipes the conversation too. There is no way back to yesterday's conversation.

## What the user gets

- **Several conversations per project.** The newest 20 are kept, pruned by last-updated time.
- **`/sessions`** lists them, newest first:
  `1  today 14:02 · 6 turns · "the tests in test_stats.py fail…"  (current)`.
  Times are local: "today HH:MM", "yesterday HH:MM", otherwise "Mon 29 Sep HH:MM".
- **`/resume N`** switches to conversation N without restarting:
  - While a reply is running it refuses with "Wait for the reply to finish (or press Ctrl+C) first."
  - Otherwise it saves the current conversation and loads N.
  - It redraws the transcript from N's history and shows "Resumed conversation N (T turns)".
- **`--resume`** opens the latest, as today.
- **`--resume=N`** opens a specific conversation. If N is out of range it fails with "There are only K saved
  conversations for this project (see /sessions)."
- **`/clear`** starts a new conversation and keeps the previous one in `/sessions`. Its `/help` line becomes
  "Start a new conversation (the old one stays in /sessions)".
- **Migration.** The existing single-slot file is moved into the new layout as the first entry the first time
  the new version runs. Nothing is lost.

## Storage

- **Layout:**
  - one directory per project: `~/.local/state/aivyx-coder/sessions/<project-key>/`, with the same key as
    today (canonical cwd plus hash), created 0700;
  - one file per conversation: `<created_unix_ms>-<8 hex>.json`, mode 0600, written atomically (temp file and
    rename) as today.
- **File contents:** today's `SessionState` (history, tasks, plan mode, specialist sessions, and the
  `UndoLedger` from part 1), plus a header:
  `SessionMeta { id, created_unix, updated_unix, first_user_text (60 chars), turns }`.
  All new fields are `#[serde(default)]`.
- **When a conversation file is created:** only when its first turn completes successfully (audit-2 B1). After
  that, every turn persists to the same file, and a new conversation never overwrites another.
- **Pruning:** after each persist, keep the newest 20 by `updated_unix` and delete the rest.
- **Migration:** if `<project-key>.json` exists and the directory doesn't, create the directory and move the
  file in under a new id. Take created and updated times from the file's mtime, `first_user_text` from its
  first user message, and `turns` from its history.

## Design

- **Store.** In `crates/aivyx-core/src/session.rs`, a `SessionStore { dir }` with `list() -> Vec<SessionMeta>`
  (newest first), `load(id)`, `save(meta, state)`, `prune(keep)` and `migrate_legacy(old_path)`. Keep the
  existing functions as thin wrappers where call sites need them.
- **Agent.** It holds `session_id: Option<String>`, which is `None` until the first successful persist. It
  gains `start_new_conversation()` (used by `/clear`) and `switch_to(id)`, which reuses the restore path that
  `--resume` uses (`Agent::restore`, the specialist rehydration, plan mode and the undo ledger).
- **Commands.** `/sessions` and `/resume N` are intercepted in `Agent::run_turn` like the other commands.
  Switching emits `AgentEvent::SessionSwitched { history }`, and the TUI rebuilds its transcript from it using
  the same rendering `--resume` uses at startup.
- **CLI.** `--resume` becomes `--resume[=N]`: a clap `Option<Option<usize>>`, or `num_args = 0..=1` with
  `default_missing_value`. Conflicts with `--acp`, `--mcp-server` and `--auto` are unchanged.
- **Other frontends.** ACP and MCP behave as today: no persistence changes, with the new store not used.

## Testing

- **Store:** list order, prune to 20, atomic write mode 0600, directory 0700, legacy migration (preserves
  history; first text and turns derived), and new fields defaulting on old files.
- **Agent:**
  - a fresh agent whose first turn fails creates no file;
  - two separate fresh sessions produce two files;
  - `/clear` keeps the previous file and the next turn creates a new one;
  - `/resume N` restores history, tasks and the undo ledger, and refuses while busy;
  - `/sessions` formatting with a fixed clock.
- **CLI parsing:** `--resume`, `--resume=2`, `--resume=0` and an out-of-range value.

## Out of scope

- Deleting or renaming conversations from the UI.
- Searching across conversations.
- Sharing sessions between projects.
- ACP and MCP session lists.
