# Architecture

aivyx-coder is a Cargo workspace. The binary is `aivyx-coder`, built from
the crate (package) `aivyx` in `crates/aivyx` — renamed so it can't collide
with Aivyx PA's own binary. The two projects share some building blocks
(`aivyx-confine`, `aivyx-checkpoint`, `aivyx-route`, `aivyx-skills`,
`aivyx-injection-guard`, `aivyx-vision`) as pinned git dependencies, but
nothing else.

The project's load-bearing property is its security boundary: the model
may be wrong or manipulated, and nothing it emits gets to act without
passing the gate. Read [Permission gate and sandbox](02-permission-gate-and-sandbox.md)
before touching `aivyx-sandbox`, `aivyx-tools` or the gate logic.

## Crates, bottom up

| Crate | Owns |
|---|---|
| `aivyx-types` | Shared message and role types, no logic |
| `aivyx-llm` | The `LlmBackend` trait, the OpenAI-compatible backend with streaming, context-window probing (`probe.rs`, which spots Ollama's hidden default window), and `RoutedBackend` (per-call model choice via `aivyx-route`) |
| `aivyx-sandbox` | The security boundary: `ConfirmationGate` (every tool call's decision point), and re-exports of the confiner (`aivyx-confine`) and the injection scanner (`aivyx-injection-guard`) |
| `aivyx-tools` | The `Tool` trait and `ToolExecutor::dispatch` — the single place permission is checked — plus every concrete tool under `tools/`, and the git checkpointer (`aivyx-checkpoint`) |
| `aivyx-repomap` | The repository map: tree-sitter symbols, PageRank over cross-file references, token-budgeted. Depends on no other workspace crate. |
| `aivyx-team` | Team schema, validation and attenuation (`effective_deny_paths`, `effective_tool_allowlist`). Depends on no other workspace crate. |
| `aivyx-config` | `Settings`: loading, defaults, `0600` writing |
| `aivyx-core` | `Agent` and the turn loop, edit formats and SEARCH/REPLACE parsing, sessions, `/council`, routing commands, undo, change summaries and `/commit`, test detection and `/test`, the slash-command table (`commands.rs`) |
| `aivyx-tui` | The ratatui front end; `TuiPrompter` bridges permission prompts from the agent task to the screen, failing closed if the screen is gone |
| `aivyx-acp` | The editor front end over ACP (JSON-RPC on stdio) |
| `aivyx-mcp-server` | The MCP-server front end: `code`/`code_reply`, access tiers, one isolated agent per session |
| `aivyx` | The binary: `agent_builder.rs` builds the agent identically for every front end (only the prompter differs); `main.rs` wires config → backend → agent → front end; `routing.rs` wraps the backend when routing is on |

## One turn

1. The model emits a tool call.
2. `ToolExecutor::dispatch` asks the tool for a `PermissionRequest`: an
   `ActionKind` (what sort of action) and a target (the exact path, or the
   exact program and arguments).
3. `ConfirmationGate::check` decides: block, auto-allow, refuse in plan or
   autonomous mode, use a cached approval, or ask the user.
4. If allowed and the tool can change the worktree, a checkpoint is taken.
5. If the tool spawns a process, the confiner applies Landlock and seccomp
   to the child before `exec`.
6. The result goes back to the model — scanned for prompt-injection phrasing
   on the way in.

"Every tool call goes through the gate" holds because the executor is the
only caller of `Tool::execute` — a convention, not a type-system guarantee.
Keep it when adding a call path.

## Edit formats

`native` edits arrive as `edit_file`/`write_file` tool calls. `prompted`
edits arrive as SEARCH/REPLACE text blocks, parsed by `edit_blocks` into
the **same** tool calls — so approval, diff preview, plan mode, the deny
list and checkpoints behave identically either way.

## Plan mode

Plan mode is an `Arc<AtomicBool>` written only by the front end (Ctrl+P in
the TUI, `session/set_mode` over ACP) and read by the gate. In plan mode
`ToolRegistry::plan_definitions()` withholds every tool whose
`mutates_outside_session()` is true, and the gate independently refuses
such a call if the model invents one. The model has no path to the flag.

## Front ends

The TUI, ACP and MCP-server front ends all run the same `Agent`, built the
same way; they differ only in the `PermissionPrompter` (a TUI modal, ACP's
`session/request_permission`, or the MCP tier filter) and in how events are
rendered.

## Further reading

- [`docs/HISTORY.md`](../../HISTORY.md) — every phase, decision and audit.
- `docs/superpowers/specs/` — the design documents behind individual
  features.
