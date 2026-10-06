# A working session

## The screen

- **The transcript** — your messages, the model's replies, and every tool
  call it makes, as they happen. A reasoning model's thinking shows as a
  dimmed *thinking:* line; it's display-only and never saved.
- **The task panel** — the model's own plan for multi-step work, kept up to
  date as it goes.
- **The status line** — what's happening (`ready`, `streaming…`), a
  **PLAN** badge in plan mode, the context budget (`ctx 6.1k/8.2k (74%)`,
  turning amber then red as it fills), and — with routing on — the model
  this conversation is using.
- **The input box** — type and press Enter. While you type a `/` command,
  matching commands are suggested.

Before your first message it shows a few example requests and the test
command it found for this project.

## Keys

| Key | What it does |
|---|---|
| Enter | Send |
| Ctrl+C | Stop the reply in progress; when idle, quit |
| Ctrl+P | Plan mode on or off ([Plan mode](04-plan-mode.md)) |
| `y` / `a` / `n` | In an approval prompt: allow once / always allow / deny |

The transcript itself doesn't scroll by key; `/diff` opens a scrollable
view. `/help` lists every command.

## Approving what it does

Reading and searching happen without asking. Anything that changes
something — writing, editing, moving or deleting a file, running a
command, committing — stops with an approval prompt showing exactly what
will happen: a diff for a file change, the full command for a command.

- **`y`** allows it once.
- **`a`** allows it and remembers that exact target for the rest of the
  session — that file, or that exact command with those arguments. It never
  blesses a different file or different arguments.
- **`n`** refuses; the model is told and can try another way.

A few things never get **always allow**: edits to files that run code later
(your shell start-up files, autostart entries, systemd user units) show an
extra warning and must be approved each time, as must changes to your
personal `AGENTS.md`.

Some things are refused outright, with no prompt: your secrets (SSH keys,
cloud credentials, `.env` and key files — the full list is in the
[security model](../reference/05-security-model.md)), and anything inside a
`.git` folder or your global git config.

### Commands you trust

The model can run two kinds of command. `run_shell` runs anything and always
asks. `run_command` runs only commands you've named in your config — and
those run without asking:

```toml
[[permissions.allowed_commands]]
name = "test"
program = "cargo"
args = ["test"]
```

The model asks for `test` by name; it can't change the program or add
arguments.

## What the model knows about your project

- **A repository map.** Each turn it gets a compact map of the project's
  most important files and symbols (Rust, Python, JavaScript and
  TypeScript), so it knows where to look. `[repo_map]` sets its size.
- **`AGENTS.md`.** Instructions in `AGENTS.md` at the project root, and in
  your personal one, are included in every turn — see
  [Memory and learning](07-memory-and-learning.md).
- **Your editor**, if you connect one — see
  [Editor integration](12-editor-integration.md).

It reads files itself when it needs them.

## Long conversations

When the conversation nears the model's context window
(`[backend] context_tokens`), aivyx-coder makes room: long tool results are
trimmed to their start and end first, then the oldest turns are dropped —
always with a notice. Set `context_tokens` to the window your server
actually serves; see [Local model servers](09-local-model-servers.md).

## Edit formats

How the model writes edits can matter on small models:

- **`native`** (default) — edits arrive as tool calls.
- **`prompted`** — the model writes SEARCH/REPLACE blocks as plain text,
  which some small models get right more often than JSON-escaped code.

Both go through the same approval, diff, deny list and checkpoints. Set
`[backend] edit_format`, or try one for a session with
`aivyx-coder --edit-format prompted`.

## When a turn goes wrong

If one reply makes several changes and a later one fails, the earlier
changes from that same reply are rolled back automatically, and the model
is told. To take back a whole turn yourself, use `/undo` — see
[Undo, diff, commit and test](05-undo-diff-commit-test.md).
