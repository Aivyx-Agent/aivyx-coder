# Files and paths

Paths are shown for Linux. On macOS the config directory is
`~/Library/Application Support/aivyx-coder/`, and the state and data
directories are the platform's equivalents.

## Your own files

| What | Where |
|---|---|
| Config | `~/.config/aivyx-coder/config.toml` (mode `0600`; `--setup` keeps the previous one as `config.toml.bak`) |
| Your personal instructions | `~/.config/aivyx-coder/AGENTS.md` — loaded into every project; `remember_preference` proposes edits to it |
| The log, including every permission decision | `~/.config/aivyx-coder/aivyx.log` |

## Per-project state

| What | Where |
|---|---|
| Saved conversations | `~/.local/state/aivyx-coder/sessions/<project-key>/`, one file per conversation, newest 20 kept, mode `0600`. `<project-key>` comes from the project folder's real path. |
| Editor context (from your editor) | `~/.local/state/aivyx-coder/editor-context/` |
| Editor approvals | `~/.local/state/aivyx-coder/editor-approval/` |
| Cross-session memory (`memory_write`) | `~/.local/state/aivyx-coder/memory/` |
| KV-cache slots (`kind = "llama_server"`) | `~/.local/share/aivyx-coder/kvcache/` (`[backend] kvcache_store_path` moves it) |

The whole `~/.config/aivyx-coder` and `~/.local/state/aivyx-coder` trees are
on the built-in deny list: the model can't read or write them — so it can't
plant a memory, edit its own config, or approve its own permission prompt.

## Inside the project

| What | Where |
|---|---|
| Project instructions | `AGENTS.md` at the project root |
| Agent-maintained wiki (`/wiki`) | `docs/wiki/` |
| Generated images | `assets/generated/` |
| Project skills (optional) | the folder `[skills] project_dir` names |
| Worktree checkpoints (what `/undo` uses) | git refs `refs/aivyx/checkpoints/*` — not files in your tree, never on a branch; newest 50 kept |

## Temporary files

Every confined command gets a private temporary directory, exported as
`TMPDIR`, instead of the shared `/tmp`. The main agent's lasts until
aivyx-coder exits; each specialist session or delegation gets its own,
removed when it ends.

## Debug capture

`AIVYX_DEBUG_LOG=<file>` appends the raw traffic to and from the model to
that file, in plain text, with no rotation. Treat it as sensitive and delete
it when you're done. See [Environment variables](07-environment-variables.md).
