# Command-line reference

```
aivyx-coder [FLAGS]
```

Run it from the project you want to work on: the current directory is the
project. With no flags it opens the terminal UI. `aivyx-coder --help` prints
this list; `aivyx-coder --version` the version.

## Modes at a glance

| You want | Run |
|---|---|
| The terminal UI | `aivyx-coder` |
| To read and plan before any change | `aivyx-coder --plan` |
| To pick up where you left off | `aivyx-coder --resume` |
| To work unattended toward a goal | `aivyx-coder --auto "<goal>"` |
| To use it inside your editor | `aivyx-coder --acp` (your editor launches this) |
| To let another tool delegate coding to it | `aivyx-coder --mcp-server` |
| To set up, or change, the model | `aivyx-coder --setup` |

Combinations that aren't allowed are refused with a message: `--acp` and
`--mcp-server` each work alone (no `--plan`, `--auto` or `--resume`, and not
with each other), and `--auto` can't be combined with `--plan` or
`--resume`.

## Flags

### `--setup`

Run the setup wizard instead of the agent: it finds the local model server
that's running, lists its models, checks the one you pick really answers,
and writes `~/.config/aivyx-coder/config.toml`. If a config already exists
it asks before replacing it, keeping the old one as `config.toml.bak`.
Editors that use aivyx-coder over ACP also launch this for their
"terminal" sign-in step. See
[Install and first run](../guide/02-install-and-first-run.md).

### `--base-url`

Use this server address for this session instead of `[backend] base_url`,
e.g. `--base-url http://localhost:11434/v1`.

### `--model`

Use this model for this session instead of `[backend] model`, e.g.
`--model qwen3-coder:30b`.

### `--edit-format`

`native` or `prompted` — how the model writes file edits, for this session
only (overrides `[backend] edit_format`). `native` uses tool calls;
`prompted` teaches SEARCH/REPLACE text blocks, which small models often
manage better. Mainly for comparing the two on a given model. See
[A working session](../guide/03-a-working-session.md).

### `--resume`

Reopen a saved conversation for this project: the latest with `--resume`,
or number N from `/sessions` with `--resume=N` (note the `=`). See
[Sessions](../guide/06-sessions.md).

### `--plan`

Start in plan mode: the model can only read, search and build a task list
until you approve the plan with **Ctrl+P**. See [Plan mode](../guide/04-plan-mode.md).

### `--auto`

```
aivyx-coder --auto "make the parser handle trailing commas"
```

Work unattended toward the goal: no approval prompts, file edits and
pre-approved commands go ahead on their own, and the loop keeps going until
every task is done or the `[autonomous]` budget runs out. It needs a way to
check its work — a `[verification] command`, or a test command it can
detect in the project — and refuses to start without one. Not with `--plan`
or `--resume`. See [Autonomous and advanced modes](../guide/10-autonomous-and-advanced-modes.md).

### `--acp`

Run as an Agent Client Protocol server over standard input and output, so
an editor (Zed, or VS Code with an ACP client extension) can embed
aivyx-coder in its agent panel. You don't normally run this yourself — the
editor does. See [Editor integration](../guide/12-editor-integration.md).

### `--mcp-server`

Run as an MCP server over standard input and output, so another local MCP
client (such as Aivyx PA) can hand it coding tasks through its `code` and
`code_reply` tools. Refuses to start until `[mcp_server] max_access_level`
is set in the config. See [MCP](../guide/13-mcp.md).

### `--help`

Print the flags and exit.

### `--version`

Print the version and exit.
