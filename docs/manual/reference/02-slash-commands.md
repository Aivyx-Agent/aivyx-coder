# Slash commands

Type a command at the start of a message. While you type a `/` word the TUI
shows matching commands; keep typing and press Enter. `/help` lists them
in the app.

**Where they work.** Every command works in the terminal UI. In an editor
over ACP, all appear in the editor's command picker except `/resume` and
`/quit`. Commands that need someone to confirm (`/undo`, `/redo`,
`/commit`) aren't available when aivyx-coder runs as an MCP server.

| Command | Needs the model? |
|---|---|
| `/council`, `/wiki`, `/architect` | yes — they run a turn |
| everything else | no — they act on the session directly |

## Conversations

### `/clear`

Start a new conversation: clears the history and the task list, keeps plan
mode as it is. The old conversation stays in `/sessions`. In an editor it
also empties the Plan panel.

### `/sessions`

List this project's saved conversations, newest first, numbered for
`/resume`. The newest 20 are kept.

### `/resume`

`/resume N` switches to conversation N from `/sessions`. Terminal UI only.
(To start aivyx-coder in a saved conversation, use `--resume`.)

## Reviewing and undoing changes

### `/undo`

Take back the assistant's last turn — every edit and command effect,
including a delegated sub-agent's. It shows which files will change
(flagging any you edited after that turn) and asks first. Run it again to
go back another turn. Git-ignored files are never touched; a commit made
with `/commit` stays (only your files rewind, and the preview says so).
Needs a git repository with checkpoints on (`[git] checkpoints`, the
default).

### `/redo`

Put back what the last `/undo` removed.

### `/checkpoints`

List the turns `/undo` can take back (the newest 50 snapshots are kept).

### `/diff`

Show everything uncommitted in a scrollable view (↑ ↓, PgUp/PgDn,
Home/End; Esc closes). `/diff turn` shows just the last turn's changes.
Files on your deny list are never shown.

### `/commit`

Commit what you've staged — or, if nothing is staged, every uncommitted
change (respecting `.gitignore` and `[git] ignore`) — with a message the
model drafts from the diff. Approve it, press **e** to edit it, or cancel;
your staging is left as it was. `/commit -m "message"` commits with your own
message straight away. Ctrl+C cancels before it lands. It uses your git
identity and runs your hooks, inside the sandbox. Files on your deny list
are never staged by it, and their contents are never sent to the model.

### `/test`

Run the project's tests: `[verification] command` if set, otherwise a test
command detected from the project (`cargo test`, `go test ./...`,
`npm`/`pnpm`/`yarn test`, `pytest`, `unittest`, `make test`). No approval
prompt — you typed it — and it runs confined like any command, even in plan
mode. Ctrl+C cancels; it stops after 10 minutes. The result and the last 80
lines of output are added to your next message, clearly marked as program
output.

## Models

### `/models`

List the models routing can choose from and what's loaded.
`/models refresh` looks for models on your servers again; `/models why`
explains the last choice. See [Models and routing](../guide/08-models-and-routing.md).

### `/model`

`/model <id>` pins this conversation to one model; `/model auto` lets
routing choose again.

## Bigger moves

### `/architect`

`/architect <task>`: the configured architect model writes a plan for the
task, then your main model carries it out. Needs `[architect]` in the
config. See [Autonomous and advanced modes](../guide/10-autonomous-and-advanced-modes.md).

### `/council`

`/council <question>` puts a question to several local models, has them
rank each other's anonymised answers, and a chairman model writes one
recommendation. Bare `/council` reviews the last assistant message. Members
get no tools and don't see your files. Needs `[council]` in the config.

### `/wiki`

Regenerate the project wiki pages that are out of date, or `/wiki <page>`
to regenerate one. See [Memory and learning](../guide/07-memory-and-learning.md).

## The app

### `/help`

List the commands, the key bindings, and the test command in use.

### `/quit`

Exit aivyx-coder (same as Ctrl+C when nothing is running). Terminal UI
only.
