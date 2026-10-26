# Command-line reference

```
aivyx-coder [FLAGS]
aivyx-coder pack <install|use|off|list|remove|check|inspect> …
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

### `--report`

```
aivyx-coder --auto "build the app in BRIEF.md" --report run.json
```

With `--auto` only. When the run stops, for any reason, write what
aivyx-coder checked itself to this file as JSON:
- the outcome and the stop notice;
- iterations and duration;
- the last verification result;
- the `[verification] final_checks` results with their warning counts;
- every file the run changed, marking any in `read_only_paths`, and
  `read_only_touched`, the read-only files that changed anyway;
- the `[backend]` model, address (any `user:password@` part removed) and `reasoning_effort`;
- `model_requests`, `prompt_tokens`, `completion_tokens` and
  `peak_context_tokens` over the main loop's model requests (as the server
  reported them; sub-agents aren't counted);
- `checkpoints` and `checkpoint_failures` during the run;
- `report_version` (1) and `coder_version`. The version goes up only when a
  field changes meaning or is removed, not when one is added.

The file is private to you (mode 600).

### `--headless`

```
aivyx-coder --auto "build the app in BRIEF.md" --headless --report run.json
```

With `--auto` only. Run without the terminal UI: for scripts, CI and
benchmarks. It's the default when stdout isn't a terminal, so piping or
redirecting the output is enough.

- **Progress** goes to stderr, one line per step, with the time since the
  run started: `[0:04:12] tool run_command test`, then
  `[0:04:31]   exit status: 101 (failed)`. Warnings and notices are printed
  in full; the model's own text only with `--verbose`.
- **The stop notice** and its facts go to stdout when the run ends.
- **The exit code** says how it ended:

  | Code | Meaning |
  |---|---|
  | 0 | goal achieved, the last verification passed (or never ran), and every final check passed |
  | 1 | goal achieved, but the last verification or a final check failed |
  | 2 | the `[autonomous]` budget ran out |
  | 3 | turns kept failing (for example, the model server is down), or an error stopped the run from starting (for example, a config error) |
  | 4 | possible prompt injection detected |
  | 130 | stopped by `SIGINT` or `SIGTERM`, including during the final checks |

- **Stopping it:** `SIGINT` or `SIGTERM` ends the run the way Ctrl+C does in
  the TUI: running commands are killed and `--report` is still written. A
  signal during the final checks counts as cancelled (130). A second signal
  exits at once with 128 plus the signal (130 for `SIGINT`, 143 for
  `SIGTERM`) and writes no report. `SIGHUP` exits at once with 129 and
  writes no report.
- **Permissions:** nothing can be asked without a terminal, so anything that
  would need a person is refused. `--auto`'s gate already works this way.

### `--verbose`

```
aivyx-coder --auto "fix the failing test" --headless --verbose
```

With `--auto` in headless mode: also print the model's text to stderr as it
streams. `-v` for short.

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

## Subcommands

`aivyx-coder pack …` manages config packs — see [Packs](../guide/16-packs.md).
These work on files only: no model, no session, and they never create
`config.toml`.

### `pack install`

`aivyx-coder pack install <file>` — verifies the pack's signature against
`[pack] trusted_publishers` and the built-in keys, checks its aivyx-coder
part, and keeps it under `~/.config/aivyx-coder/packs/<name>/<version>/`,
replacing an older version.

### `pack use`

`aivyx-coder pack use <name> [--global]` — use an installed pack in this
project, or with `--global` in every project. Asks before allowing each of
the pack's MCP servers (no terminal: none allowed).

### `pack off`

`aivyx-coder pack off [--global]` — stop using the pack in this project, or
the one used everywhere.

### `pack list`

`aivyx-coder pack list` — installed packs, their versions and where each is
in use.

### `pack remove`

`aivyx-coder pack remove <name>` — delete an installed pack and switch it
off everywhere.

### `pack check`

`aivyx-coder pack check <folder>` — for pack authors: checks a pack
folder's aivyx-coder part (its `AGENTS.md`, skills, roster and MCP servers)
and lists every problem.

### `pack inspect`

`aivyx-coder pack inspect <file> [--allow-untrusted]` — verifies a pack and
shows what it contains, including the exact commands its MCP servers would
run, and whether it passes its checks. `--allow-untrusted` shows a pack
signed by a key you haven't trusted yet.
