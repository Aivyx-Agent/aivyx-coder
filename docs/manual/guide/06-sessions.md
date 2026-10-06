# Sessions

Every conversation is saved automatically after each completed turn —
transcript, task list and plan-mode state — one file per conversation,
per project. Each project keeps its newest 20.

## Come back to one

- **`aivyx-coder --resume`** — start in this project's most recent
  conversation.
- **`aivyx-coder --resume=N`** — start in conversation N from the list.
- **`/sessions`** — list this project's conversations, newest first:
  number, when it was last used, how many turns, and the start of its
  first message.
- **`/resume N`** — switch to conversation N without restarting (terminal
  UI only).

## Start fresh

**`/clear`** starts a new conversation: it clears the history and task
list and keeps plan mode as it is. The old conversation stays in
`/sessions`, so you can go back to it. With routing on, `/clear` also lets
routing pick a model afresh, but keeps a `/model` pin.

## Where they're kept

`~/.local/state/aivyx-coder/sessions/<project-key>/`, readable only by you
(they contain file contents and command output from the session). The
project key comes from the project folder's real path, so the same folder
always finds its conversations. The model can't read or change these files.

Sessions saved by older versions, one file per project, are moved into the
new layout automatically; a file that can't be read is kept, not deleted.

## Limits

- `--resume` can't be combined with `--auto`, `--acp` or `--mcp-server`.
- In an editor over ACP, the editor keeps its own conversation history, so
  `/resume` isn't offered there.
