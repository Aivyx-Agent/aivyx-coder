# Configuration reference

Every section and key `config.toml` accepts. This page is generated from
the config structs in `crates/aivyx-config` by
`scripts/gen-config-reference.py` — edit the code or the script, not this
page. A commented example is in [`config-example.toml`](config-example.toml);
the **Example** column below comes from it.

## Where the config lives

`~/.config/aivyx-coder/config.toml` on Linux (`$XDG_CONFIG_HOME` is
honoured), `~/Library/Application Support/aivyx-coder/config.toml` on macOS.
`aivyx-coder --setup` writes it, with `0600` permissions since it may hold
an `api_key`; on a first run without it, defaults are written. Every
section is optional. For one session, `--base-url`, `--model` and
`--edit-format` override the file — see the [command line](01-command-line.md).

## Reading the tables

- **Type**: `string`, `integer`, `number`, `bool`, `path`, a list, or the
  allowed words (`` `a` | `b` ``).
- `[[name]]` is an array of tables: repeat the block once per entry.
- `<name>` in a heading is a name you choose.


## The model

### `[backend]`

The local model server and model aivyx-coder talks to. `aivyx-coder --setup` writes this for you. See [Install and first run](../guide/02-install-and-first-run.md) and [Local model servers](../guide/09-local-model-servers.md).

| Key | Type | Example | Meaning |
|---|---|---|---|
| `base_url` | string | `"http://localhost:11434/v1"` | The model server's OpenAI-compatible address, e.g. `http://localhost:11434/v1` (Ollama) or `http://localhost:8080/v1` (llama.cpp). `--base-url` overrides it. |
| `model` | string | `"qwen3.5:9b"` | The model id, as the server names it. `--model` overrides it. |
| `api_key` | string | `"..."` | Key for an auth-protected local server. Usually unset. |
| `tool_calling_mode` | `native` \| `text_fallback` \| `native_with_fallback` |  | Reserved for a text-fallback tool-call parser; not used yet. |
| `context_tokens` | integer | `8192` | The model's context window, in tokens — used to show a live budget indicator and to trigger context compaction before the window overflows. Conservative default; **set this to your model's actual window** (qwen3.5/qwen3.6 support far more than the default). |
| `edit_format` | `native` \| `prompted` |  | How edit content travels: "native" sends edits as edit_file / write_file tool-call arguments; "prompted" teaches the model to write SEARCH/REPLACE blocks as plain text instead (parsed by aivyx and applied through the same permission gate). |
| `kind` | `generic` \| `llama_server` \| `llama_server_broker` \| `mistral_rs` | `"llama_server"` | Which server this is, for server-specific features: `generic` (default), `llama_server` (KV-cache persistence), `llama_server_broker` (GPU sharing through aivyx-broker; needs `broker_base_url`), `mistral_rs` (the embedded engine, if built in). |
| `kvcache_max_bytes` | integer | `10737418240` | Maximum bytes the kvcache store (docs/README's KV-cache persistence section) will hold on disk before evicting the least-recently-used entry. Only meaningful when `kind = "llama_server"`. |
| `kvcache_store_path` | string | `"~/.local/share/shared-kvcache"` | Where saved KV-cache slots go (default under `~/.local/share/aivyx-coder/kvcache`). Point it at the same directory as Aivyx PA's `[kvcache] store_path` when both share one llama.cpp server. |
| `broker_base_url` | string | `"http://127.0.0.1:8899"` | Address of a running `aivyx-broker` instance (e.g. `http://127.0.0.1:8899`). |
| `locality` | `local` \| `cloud` | `"local"` | Whether this backend is on your own network, for model routing only (`[routing] enabled = true`). |
| `mistralrs_model_path` | string |  | With `kind = "mistral_rs"`: a GGUF file, or a directory of them. Required for that kind. |
| `mistralrs_model_file` | string |  | Selects a specific `.gguf` file inside `mistralrs_model_path` when it's a directory containing more than one candidate. |
| `mistralrs_chat_template_path` | string |  | Overrides the chat template mistral.rs would otherwise infer from the model's own metadata. |
| `mistralrs_constrain_tool_calls` | bool |  | Reserved for grammar-constrained tool calls in the embedded engine; not used yet. |

### `[routing]`

Per-call model routing across several local models. Off by default. See [Models and routing](../guide/08-models-and-routing.md).

| Key | Type | Example | Meaning |
|---|---|---|---|
| `enabled` | bool |  | Turn routing on. Off (the default) means every call uses `[backend] model`. |
| `discover` | bool |  | Ask each endpoint what models it serves at startup. Off means use only `[[routing.models]]`. |
| `vram_bytes` | integer |  | Host GPU memory in bytes, for residency scoring when no `aivyx-broker` reports it. Unset: no won't-fit term without a broker. |

#### `[routing.endpoints.<name>]`

| Key | Type | Example | Meaning |
|---|---|---|---|
| `kind` | `ollama` \| `llama_router` \| `openai_compat` \| `lemonade` \| `anthropic` \| `openai` |  | What kind of server this is. |
| `base_url` | string |  | The server's URL. Unset uses the kind's usual local address. |
| `locality` | `local` \| `cloud` |  | Operator override of the locality the address implies: `local` marks a non-cloud kind local even when its host looks public (a LAN box with a public DNS name); `cloud` marks it cloud. Never makes a cloud kind local. |

#### `[[routing.models]]`

| Key | Type | Example | Meaning |
|---|---|---|---|
| `id` | string |  | The model id, as its endpoint names it. |
| `endpoint` | string |  | Which `[routing.endpoints.<name>]` serves it. Unset means `[backend]`. |
| `locality` | `local` \| `cloud` |  | `cloud` marks this model cloud even on a local endpoint. `local` never makes a model on a cloud endpoint local (it is ignored, and `RoutingConfig::validate` reports it). |
| `tier` | `small` \| `medium` \| `large` |  | Its size class, for matching tasks to models. |
| `strengths` | list of `code` \| `reasoning` \| `chat` \| `summarize` |  | What it is good at. |
| `priority` | integer |  | Operator tie-break; higher wins. Unset = 0 (or an earlier entry's). |
| `capabilities` | list of `completion` \| `tools` \| `vision` \| `thinking` \| `audio` \| `embedding` |  | Added to what discovery found. |
| `capabilities_deny` | list of `completion` \| `tools` \| `vision` \| `thinking` \| `audio` \| `embedding` |  | Removed from what discovery found (e.g. unreliable tool calling). |
| `context_window` | integer |  | The context window you actually serve it with, in tokens. |

#### `[routing.tasks.<name>]`

| Key | Type | Example | Meaning |
|---|---|---|---|
| `tier` | `small` \| `medium` \| `large` |  | The tier this task prefers. Keyed by task name (`chat`, `code_edit`, `plan`, `judge`, `summarize`, `compact`, `classify`, `embed`). |
| `strengths` | list of `code` \| `reasoning` \| `chat` \| `summarize` |  | Strengths this task prefers in a model. |

## Safety

### `[permissions]`

What the model may touch and run: the deny list, pre-approved commands, and per-turn limits. See the [security model](05-security-model.md).

| Key | Type | Example | Meaning |
|---|---|---|---|
| `mode` | `confirm` |  | How permission is decided. `confirm` is the only mode: anything that isn't read-only asks you. |
| `deny_paths` | list of string | `["~/.ssh", "~/.aws", ".env", "*.pem"]` | Hard-blocked regardless of prompt/allow-list state, enforced by `ConfirmationGate` before any prompt or Always-Allow cache lookup. Entries may use a leading `~` for the home directory — see `resolved_deny_paths`. |
| `max_tool_iterations_per_turn` | integer | `25` | Most tool-call round trips in one turn before it stops. |

#### `[[permissions.allowed_commands]]`

Fixed set of commands the `run_command` tool may execute — the model selects one by `name`, it never supplies a program or arbitrary args. Empty by default: nothing is runnable until a project explicitly opts in.

| Key | Type | Example | Meaning |
|---|---|---|---|
| `name` | string | `"test"` | The name the model uses to ask for this command. |
| `program` | string | `"cargo"` | The program to run. |
| `args` | list of string | `["test"]` | Its fixed arguments. The model can't add to them. |
| `timeout_secs` | integer | `600` | Overrides the tool's default timeout for this command when set. |

### `[sandbox]`

Kernel confinement (Landlock + seccomp) for the commands the model runs, and its opt-outs. See the [security model](05-security-model.md).

| Key | Type | Example | Meaning |
|---|---|---|---|
| `extra_read_paths` | list of string | `[]` | Additional filesystem paths process-executing tools may read from, beyond the built-in default (the working directory plus common system/toolchain paths). |
| `require_enforcement` | bool | `true` | Whether process-execution tools must refuse to run at all if real Landlock confinement can't actually be established (kernel support missing/disabled, or the kernel only partially enforces the requested ruleset) — fail closed rather than silently running unconfined. |
| `allow_unix_sockets` | bool | `false` | Let confined commands (`run_command`, `run_shell`, `/test`, git, REPLs, MCP and LSP servers) create `AF_UNIX` sockets, and pass them the session IPC variables (`SSH_AUTH_SOCK`, `DBUS_SESSION_BUS_ADDRESS`, `XDG_RUNTIME_DIR`,...) that are otherwise removed from their environment. |
| `allow_leaving_process_group` | bool | `false` | Let confined commands call `setsid`/`setpgid`. Off by default: every confined command leads its own process group and everything in it is killed when the tool call ends, which only holds if nothing can leave the group. |
| `share_system_tmp` | bool | `false` | Give confined commands read+write on the shared system temp dir (`/tmp`, and `$TMPDIR` if set) instead of a private per-session temp dir exported as `TMPDIR`. Off by default, so a confined command can't read or tamper with other programs' files in `/tmp`. |

### `[git]`

Worktree checkpoints (what `/undo` uses) and the generated-file ignore list. See [Undo, diff, commit and test](../guide/05-undo-diff-commit-test.md).

| Key | Type | Example | Meaning |
|---|---|---|---|
| `checkpoints` | bool | `true` | Snapshot the worktree to `refs/aivyx/checkpoints/*` before every mutating tool call, so any agent change (including arbitrary `run_shell` effects) can be rewound with plain git commands. Never touches HEAD, the index, or the worktree; silently disabled when the working directory isn't a git repository. |
| `ignore` | list of string |  | Generated files, in `.gitignore` syntax: hidden from aivyx-coder's own change views (the per-turn change summary, `/diff`, the `/undo`/`/redo` previews) and never auto-staged by `/commit`, but only while untracked — exactly like `.gitignore`, a file git already tracks is always shown. |

## Context the model gets

### `[repo_map]`

The token-budgeted map of the repository's symbols added to every turn.

| Key | Type | Example | Meaning |
|---|---|---|---|
| `enabled` | bool | `true` | Append a token-budgeted map of the repository's top-ranked files and symbols to the system prompt each turn (Rust, Python, JavaScript/JSX, and TypeScript/TSX today; other languages degrade gracefully to no map). Costs prompt tokens every request but gives the model repository orientation it won't ask for on its own. |
| `budget_tokens` | integer | `1024` | Rough token budget the rendered map may consume. Counted against `backend.context_tokens` by the compaction estimator. |

### `[agents_file]`

Project and personal instructions (`AGENTS.md`) loaded into every turn. See [Memory and learning](../guide/07-memory-and-learning.md).

| Key | Type | Example | Meaning |
|---|---|---|---|
| `enabled` | bool |  | Turn this on or off. |
| `budget_tokens` | integer |  | Applied per file independently, not as a combined pool. A file over budget is still included in full — this is hand-written prose with no natural truncation point, unlike the repo map — but triggers a notice so the user knows to trim it or raise this value. |

### `[editor_context]`

Your editor's open file, cursor and selection, read from a small JSON file. See [Editor integration](../guide/12-editor-integration.md).

| Key | Type | Example | Meaning |
|---|---|---|---|
| `enabled` | bool | `true` | Turn this on or off. |

### `[skills]`

The shared library of `SKILL.md` capability packages the model can load.

| Key | Type | Example | Meaning |
|---|---|---|---|
| `enabled` | bool | `true` | Turn this on or off. |
| `project_dir` | string | `".aivyx/skills"` | An extra skills directory for this project, holding one `<skill-name>/SKILL.md` per skill. |
| `user_dir` | string | `"~/.config/aivyx-coder/skills"` | An extra personal skills directory, same layout. |

## Ways of working

### `[verification]`

Check the work after edits by running a test command before a turn may end. See [Undo, diff, commit and test](../guide/05-undo-diff-commit-test.md).

| Key | Type | Example | Meaning |
|---|---|---|---|
| `command` | string | `"test"` | Name of a `[[permissions.allowed_commands]]` entry to run after edits, before a turn may end. Setting it turns verification on. |
| `max_auto_verify_retries` | integer | `3` | How many (edit, re-verify) cycles may fail before the agent gives up on this round of edits and lets the turn end anyway, with a loud notice rather than silence. Clamped to a minimum of 1 by the agent. |
| `scoped_command` | string | `"test_scoped"` | Another `[[permissions.allowed_commands]]` entry name, whose `args` may contain the literal token `"{touched_paths}"` — substituted at runtime with the files touched since edits became unverified, one argv entry per path. |

### `[autonomous]`

Limits for `--auto` runs. See [Autonomous and advanced modes](../guide/10-autonomous-and-advanced-modes.md).

| Key | Type | Example | Meaning |
|---|---|---|---|
| `max_iterations` | integer |  | Total "continue" round-trips for the whole autonomous run. |
| `max_duration_secs` | integer |  | Wall-clock ceiling, in seconds, for the whole autonomous run. |

### `[architect]`

The model that plans for `/architect <task>`.

| Key | Type | Example | Meaning |
|---|---|---|---|
| `base_url` | string |  | The server's OpenAI-compatible address, e.g. `http://localhost:11434/v1`. |
| `model` | string |  | The model id, as the server names it. |
| `api_key` | string |  | Key for an auth-protected local server. Usually unset. |
| `tail_budget_tokens` | integer |  | Token budget for the conversation-tail digest the architect sees alongside the task — separate from `council.tail_budget_tokens` since the two features are configured and toggled independently. |

### `[council]`

The models `/council` consults, and the chairman that sums up.

| Key | Type | Example | Meaning |
|---|---|---|---|
| `tail_budget_tokens` | integer | `3072` | Token budget for the conversation-tail digest each member sees alongside the question. |

#### `[[council.members]]`

| Key | Type | Example | Meaning |
|---|---|---|---|
| `base_url` | string |  | The server's OpenAI-compatible address, e.g. `http://localhost:11434/v1`. |
| `model` | string |  | The model id, as the server names it. |
| `api_key` | string |  | Key for an auth-protected local server. Usually unset. |

#### `[council.chairman]`

Deliberately no "first member chairs" fallback — which model gets the last word should always be an explicit choice.

| Key | Type | Example | Meaning |
|---|---|---|---|
| `base_url` | string |  | The server's OpenAI-compatible address, e.g. `http://localhost:11434/v1`. |
| `model` | string |  | The model id, as the server names it. |
| `api_key` | string |  | Key for an auth-protected local server. Usually unset. |

### `[sub_agent]`

Limits for `delegate_task`, which hands a sub-task to a fresh agent.

| Key | Type | Example | Meaning |
|---|---|---|---|
| `max_iterations` | integer |  | A delegated task's own tool-call budget — deliberately separate from, and smaller than, `[permissions] max_tool_iterations_per_turn`: a delegated task is meant to be a scoped, bounded piece of work, not a full session. |

### `[team]`

Specialist teams (`delegate_to_specialist`). Off by default. See [Specialist teams](../guide/11-specialist-teams.md).

| Key | Type | Example | Meaning |
|---|---|---|---|
| `enabled` | bool | `false` | Turn this on or off. |
| `max_concurrent_specialist_sessions` | integer | `3` | Hard cap on specialist sessions (`spawn_specialist`) that may be open at once. A `spawn_specialist` call beyond this cap errors clearly rather than evicting an existing session -- see `docs/superpowers/specs/2026-09-20-nonagon-team-specialist-sessions-design.md`. |
| `specialist_session_idle_timeout_secs` | integer | `600` | How long, in seconds, a specialist session may sit with no `query_specialist` activity before it's considered stale -- a safety net against a model that spawns sessions and forgets to close them, matching `ReplSettings.idle_timeout_secs`'s own precedent and default. |
| `roster_path` | string | `"~/my-team.toml"` | A team-config TOML to use instead of the built-in coding team. |

## Tools

### `[web]`

The `web_fetch` and `web_search` tools. Inference stays local; these reach the network.

| Key | Type | Example | Meaning |
|---|---|---|---|
| `enabled` | bool |  | Turn this on or off. |
| `search_base_url` | string |  | Your SearXNG instance. Without it, `web_search` explains that it isn't set up. |
| `max_search_results` | integer |  | Applied by `web_search`; results beyond this are dropped, not an error. |
| `fetch_timeout_secs` | integer |  | Applied to both tools' HTTP client. |
| `allow_private_targets` | bool |  | Let `web_fetch` reach loopback, private and link-local addresses. Off by default. |

### `[lsp]`

Language-server tools (`go_to_definition`, `find_references`).

| Key | Type | Example | Meaning |
|---|---|---|---|
| `timeout_secs` | integer |  | Longest wait for one language-server request, in seconds. Default 60 — the first call in a big project may index for a while. |

### `[repl]`

Interactive processes the model can drive (`repl_start`, `repl_send`, `repl_stop`).

| Key | Type | Example | Meaning |
|---|---|---|---|
| `quiet_window_ms` | integer | `300` | How long output must be silent (no new bytes) before `repl_send` returns, in milliseconds. |
| `max_wait_secs` | integer | `10` | Hard per-call backstop, in seconds, in case output never goes quiet (e.g. a build tool spewing output continuously). |
| `idle_timeout_secs` | integer | `600` | Auto-kill a session with no `repl_send` activity for this long, in seconds — a safety net against a forgotten session lingering indefinitely. |

### `[vision]`

Image generation tools, which need an image backend.

| Key | Type | Example | Meaning |
|---|---|---|---|
| `enabled` | bool |  | Turn this on or off. |
| `broker_url` | string |  | The aivyx-broker address. Default `http://127.0.0.1:8899`. |
| `mold_url` | string |  | The `mold serve` image server address. Default `http://127.0.0.1:7680`. |
| `api_key` | string |  | Key for the image backend, if it needs one. |

### `[mcp]`

MCP servers whose tools the model can use, one `[[mcp.servers]]` block each. See [MCP](../guide/13-mcp.md).


#### `[[mcp.servers]]`

| Key | Type | Example | Meaning |
|---|---|---|---|
| `name` | string |  | Used verbatim in the registered `mcp__<name>__<tool>` tool name and in the resources/prompts meta-tools' `server` filter argument. |
| `command` | string |  | The executable to spawn. |
| `args` | list of string |  | Arguments for the server's command. |
| `env` | table of string |  | Additional environment variables for the spawned process (e.g. API keys the server itself needs), merged over the agent's own environment. |
| `timeout_secs` | integer |  | Bounds this server's spawn + `initialize` handshake + discovery (`tools/list`/`resources/list`/`prompts/list`) sequence at startup. A server that doesn't finish within this budget is skipped for the session with a warning, rather than blocking startup indefinitely. |

### `[persona]`

Whether the model may propose additions to your personal `AGENTS.md`. See [Memory and learning](../guide/07-memory-and-learning.md).

| Key | Type | Example | Meaning |
|---|---|---|---|
| `enabled` | bool |  | Turn this on or off. |

## Running inside other tools

### `[mcp_server]`

Running aivyx-coder as an MCP server (`--mcp-server`). Refuses to start until `max_access_level` is set. See [MCP](../guide/13-mcp.md).

| Key | Type | Example | Meaning |
|---|---|---|---|
| `max_access_level` | string |  | "plan" \| "edit" \| "execute" — the ceiling. A `code` call requesting a level above this is rejected, never silently downgraded. |
| `session_ttl_secs` | integer |  | An idle MCP session (no `code_reply` call) is evicted after this long. |
| `max_concurrent_sessions` | integer |  | Bounded in-memory session map; the oldest idle session is evicted if a new session would exceed this. |
| `max_iterations` | integer |  | A single `code` or `code_reply` call's own round-trip budget — mirrors `[sub_agent].max_iterations`'s shape exactly (an outer "continue" loop, not `AgentConfig.max_tool_iterations`). |

### `[editor_approval]`

Answering approval prompts from your editor instead of the terminal. See [Editor integration](../guide/12-editor-integration.md).

| Key | Type | Example | Meaning |
|---|---|---|---|
| `enabled` | bool | `true` | Turn this on or off. |
