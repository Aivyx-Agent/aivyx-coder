# ACP Editor Integration (Zed + VS Code) — Design

**Status:** Approved by user 2026-07-20. Follow-on to
`docs/superpowers/specs/2026-07-18-editor-context-integration-design.md`
("context in") and
`docs/superpowers/specs/2026-07-19-editor-approval-integration-design.md`
("context out"), both of which explicitly scoped out "any specific
editor's plugin/extension code" as a separate, later project. This is
that project.

## Context

The user asked to scope a VS Code extension and a "Zen" (confirmed: Zed)
editor extension so an end user can plug aivyx-coder into their editor.
Research done before this design was written changed the shape of the
ask:

- **Zed's WASM extension API cannot build a custom agent UI.** Extensions
  compiled to `wasm32-wasip2` have no way to add panels or otherwise touch
  the GPUI app context, and Zed's own docs confirm "extension-provided
  agents are deprecated" in favor of the **Agent Client Protocol (ACP)** —
  a JSON-RPC-2.0-over-stdio standard Zed authored and open-sourced
  (Apache-licensed, `agentclientprotocol.com`). The editor spawns the
  agent as a subprocess and exchanges session lifecycle, prompt turns,
  tool calls, diffs, permission requests, and plan/todo updates over
  stdin/stdout. This is the *only* integration point for Zed — a
  traditional "Zed extension" is not viable for this use case.
- **VS Code already has a mature, open-source ACP client**
  (`formulahendry.acp-client` on the Marketplace, MIT-licensed) that
  connects to any ACP-compatible agent. If aivyx-coder speaks ACP, it
  works inside that extension with zero VS Code-specific code from this
  project.
- JetBrains is adopting ACP across its IDE suite, so the same work
  plausibly covers a third editor family, though this is not separately
  verified by this spec.
- An official Rust crate, `agent-client-protocol` (maintained by the ACP
  org / used by Zed itself), implements both sides of the protocol —
  this project depends on it rather than hand-rolling JSON-RPC framing.

Given this, the scope is one ACP server mode inside aivyx-coder, not two
bespoke editor extensions. This was confirmed with the user
("ACP-first") before design work continued.

Facts confirmed against the current codebase before this design was
written:

- `AgentEvent` (`crates/aivyx-core/src/agent/types.rs:10-58`) has twelve
  variants: `TextDelta`, `ReasoningDelta`, `ToolCallDetected`,
  `ToolResult`, `TurnComplete`, `Error`, `TurnPaused`, `ContextUsage`,
  `TasksUpdated`, `CouncilNote`, `ArchitectNote`, `SubAgentActivity`.
  These map cleanly onto ACP session-update notifications — see the
  Protocol Mapping table below.
- `PermissionPrompter` (`crates/aivyx-sandbox/src/lib.rs:175-177`) is a
  one-method trait: `async fn prompt(&self, request: &PermissionRequest)
  -> UserResponse`. `TuiPrompter`
  (`crates/aivyx-tui/src/permission.rs:24-45`) is the only existing
  implementation — it bridges the background agent task to the render
  loop via an `mpsc` channel of `ModalRequest { request, reply_tx:
  oneshot::Sender<UserResponse> }`. A new `AcpPrompter` is a direct
  structural parallel: instead of a channel to a render loop, it sends
  `session/request_permission` over the ACP connection and awaits the
  client's response.
- `Agent::run_turn(&mut self, user_input: String, cwd: &Path,
  cancellation: CancellationToken) -> Result<(), AgentError>`
  (`crates/aivyx-core/src/agent/mod.rs:711`) is the single entry point
  the TUI already calls for every user message. Slash-command
  interception (`/council`, and by the same pattern `/architect`,
  `/wiki`) happens *inside* `run_turn`, before the input reaches LLM
  history (`mod.rs:721-730`, confirmed via
  `crate::council::parse_command`) — so an ACP prompter needs no special
  handling for these; forwarding the raw ACP `session/prompt` text into
  `run_turn` verbatim reproduces every terminal-typed slash command,
  including `/council` and `/architect` output (rendered via
  `CouncilNote`/`ArchitectNote`, confirmed handled identically —
  `crates/aivyx-tui/src/app.rs:403-409` renders both as plain transcript
  text) and `/wiki` (dispatched via `run_wiki_turn`/
  `run_wiki_turn_for_pages`, `mod.rs:868,921`, confirmed by the existing
  test `run_turn_dispatches_wiki_commands_before_normal_turn_processing`,
  `agent/tests.rs:2584` — it emits ordinary `TextDelta`/`ToolCallDetected`/
  `ToolResult` events, no dedicated `AgentEvent` variant, so it needs no
  new mapping either).
- `AgentConfig` (`crates/aivyx-core/src/agent/types.rs:88-95`):
  `max_tool_iterations`, `context_tokens`, `edit_format` — unrelated to
  frontend choice, reused as-is.
- The `aivyx` binary's CLI (`crates/aivyx/src/main.rs:83-118`) is a flat
  `clap::Parser` struct (`base_url`, `model`, `resume`, `plan`, `auto`,
  `edit_format`) — **no subcommands exist today.** ACP mode is added as a
  new boolean flag, `--acp`, consistent with this existing convention,
  not a subcommand — correcting the informal "`aivyx acp` subcommand"
  phrasing used earlier in conversation.
- `editor_context.rs` (`crates/aivyx-core/src/editor_context.rs`) and
  `editor_approval.rs` (`crates/aivyx-sandbox/src/editor_approval.rs`)
  are both polling-file channels keyed by a canonicalized-`cwd` FNV-1a
  hash under `~/.local/state/aivyx-coder/`. Both stay exactly as they are
  for terminal-TUI users pairing aivyx-coder with an editor that doesn't
  speak ACP; neither is touched by this spec. In `--acp` mode they are
  simply not wired up — ACP supplies equivalent (richer) context/approval
  natively over the protocol connection itself.
- Verification (`[verification] command`), worktree checkpoints, and
  multi-file-batch rollback are all implemented as automatic behavior
  around tool dispatch / `run_command`, surfacing only as ordinary tool
  results or error text appended to a failing call — confirmed to need no
  ACP-specific handling.

## Decisions (confirmed with user via one-at-a-time questions before this doc was written)

1. **Target editor clarified**: "ZEN Editor" meant Zed (`zed.dev`), not a
   browser or another tool.
2. **ACP-first scoping**: build one ACP server mode in aivyx-coder rather
   than bespoke VS Code + Zed extensions. Zed integration and VS Code
   integration both ride on top of this single piece of work.
3. **v1 feature scope**: core turn loop (chat, tool calls, diffs,
   permissions, task list/plan, plan mode) **plus** `/council` and
   `/architect` output — both included because, per the facts above, they
   cost nothing extra: forwarding raw prompt text through the existing
   `run_turn` entry point reproduces them automatically, and their output
   already renders as plain text with no dedicated protocol mapping
   needed. `/wiki` behaves identically and is included for the same
   reason. The wiki *tool/feature* itself (agent-maintained doc
   generation) is unaffected either way; only its own note-shaped browser
   UI (there is none — it already renders as plain events) was ever in
   question.
4. **Session model**: one OS process per ACP session (no in-process
   multi-tenancy). If an editor wants parallel agent threads, it spawns
   multiple `aivyx --acp` processes, each scoped to its own `cwd` —
   matching aivyx-coder's existing single-conversation-per-directory
   model and avoiding new concurrency machinery in `aivyx-core`.
5. **Editor registration**: Zed via a `settings.json` `agent_servers`
   entry pointing at the `aivyx --acp` binary (a one-click "Agent Server
   Extension" manifest is a possible later follow-up, not part of this
   spec). VS Code via documentation for pointing the existing
   `formulahendry.acp-client` Marketplace extension at the same binary —
   no first-party VS Code extension is built by this spec.

## Architecture

### New crate: `crates/aivyx-acp`

A sibling to `crates/aivyx-tui`, filling the same role: a frontend over
`Agent`, nothing more. Depends on:

- `aivyx-core` (`Agent`, `AgentConfig`, `AgentEvent`)
- `aivyx-sandbox` (`ConfirmationGate`, `PermissionPrompter`,
  `PermissionRequest`, `UserResponse`)
- `aivyx-tools`, `aivyx-config` (unchanged, same construction as the TUI
  path)
- `agent-client-protocol` (external crate) for the JSON-RPC/stdio
  transport and the `Agent`-trait-shaped server harness it provides on
  the Rust side

No changes to `aivyx-core`'s turn loop. Two new pieces:

- **`AcpPrompter`**: implements `PermissionPrompter` by sending
  `session/request_permission` over the ACP connection and awaiting the
  client's response, translating `UserResponse::{Allow, AllowAlways,
  Deny}` to/from whatever the `agent-client-protocol` crate's own
  response type is. Structurally identical in shape to `TuiPrompter`,
  swapping the `oneshot` render-loop bridge for a protocol round-trip.
- **Event translator**: a function/task that drains the same
  `mpsc::UnboundedReceiver<AgentEvent>` the TUI already consumes and
  emits ACP `session/update` notifications per the mapping table below,
  instead of mutating ratatui widget state.

The `aivyx` binary crate (`crates/aivyx/src/main.rs`) gains a `--acp`
boolean flag on the existing `Cli` struct. When set, `main` constructs
`Agent`/`AgentConfig` exactly as it does today, but hands control to
`aivyx_acp::run(...)` (stdin/stdout server loop) instead of
`aivyx_tui::app::run(...)`. `--acp` is mutually exclusive with `--plan`
(ACP's own session-mode mechanism supersedes the CLI flag) and with
`--auto` (autonomous mode's unattended-approval model needs its own
design pass against ACP's permission flow before combining the two —
deferred, see Out of Scope).

### Protocol mapping

| aivyx-coder concept | ACP concept |
|---|---|
| `AgentEvent::TextDelta` | `agent_message_chunk` session update |
| `AgentEvent::ReasoningDelta` | `agent_thought_chunk` session update |
| `AgentEvent::ToolCallDetected` / `ToolResult` + `DiffContent` | `tool_call` / `tool_call_update`, with existing old/new content as the ACP diff payload |
| `ConfirmationGate` interactive tier | `session/request_permission` via `AcpPrompter` (replaces `TuiPrompter` for this frontend); Allow/Deny/Always-Allow map directly onto ACP's permission options |
| `AgentEvent::TasksUpdated` (`set_tasks`) | ACP plan/todo entries |
| Plan mode (`Ctrl+P` in the TUI) | an ACP session mode toggle, backed by the same `Arc<AtomicBool>` `PlanMode` the gate already reads — `aivyx-acp` becomes a second writer alongside the TUI's Ctrl+P handler |
| `AgentEvent::CouncilNote` / `ArchitectNote` / `SubAgentActivity` | plain `agent_message_chunk` text — no new ACP semantics invented, matching how the TUI already renders these as transcript lines |
| `editor_context.rs` polling | not used in `--acp` mode; ACP clients supply current-file/selection context natively |
| `editor_approval.rs` polling | superseded by `session/request_permission` in `--acp` mode |
| Verification / checkpoints / multi-file rollback | unchanged — already surface as ordinary tool-call events or error text |
| `AgentEvent::TurnComplete` | the `stopReason: "end_turn"` on the `session/prompt` response itself, not a session-update notification |
| `AgentEvent::TurnPaused` | `stopReason: "max_turn_requests"` (or the closest matching reason the `agent-client-protocol` crate defines) on the same response, distinct from `end_turn` |
| `AgentEvent::Error` | the JSON-RPC error response for the in-flight `session/prompt` call |
| `AgentEvent::ContextUsage` | no ACP-standard equivalent; not surfaced in v1 (dropped, not queued or logged specially) |

### Session lifecycle & config

One process per ACP session; `initialize` → `session/new` (scoped to the
process's `cwd`) → repeated `session/prompt` turns, each forwarded
verbatim into `Agent::run_turn`. Config loading (`config.toml`, backend
selection), the Landlock/seccomp sandbox, and session persistence
(`~/.local/state/aivyx-coder/sessions/`, keyed by canonicalized `cwd`)
are all reused unchanged — an ACP-launched process and a terminal
`--resume` of the same directory share history, since both go through
the same `Agent` construction and the same session file.

### Editor registration

- **Zed**: a `settings.json` `agent_servers` entry naming the `aivyx
  --acp` binary and its `cwd`. No protocol code here — discovery/config
  only. A packaged Agent Server Extension for one-click install from
  Zed's registry is a possible later follow-up, out of scope here.
- **VS Code**: documentation only — point `formulahendry.acp-client` at
  the `aivyx --acp` binary. No extension of our own.

## Out of scope for this spec

- Multi-session-per-process (parallelism is achieved by spawning multiple
  processes instead).
- A first-party VS Code extension (branding/marketplace presence,
  deferred, not ruled out).
- A packaged Zed Agent Server Extension for one-click install (manual
  `settings.json` registration ships first).
- Separate JetBrains verification (expected to work via ACP compliance,
  not independently tested by this spec).
- Combining `--acp` with `--auto` (autonomous mode) — needs its own
  design pass reconciling unattended-approval semantics with ACP's
  permission-request flow.
- Marketplace publishing/branding of any kind.

## Testing / verification

The project's existing Live E2E harness drives the TUI over a PTY via
`pyte` — inapplicable here, since ACP mode has no terminal to drive.
Plan: a new, narrower E2E layer using the `agent-client-protocol` crate's
*client* side to drive `aivyx --acp` over stdio the way Zed would (send
`session/prompt`, assert on the update stream and permission-request
shape), plus at least one manual smoke test against real Zed before
calling this done — protocol-shape bugs (schema mismatches, wire framing)
are exactly what an automated-only test using the same crate on both
sides would risk missing.

## Sequencing

This spec covers the ACP core adapter (`aivyx-acp` crate + `--acp` flag +
protocol mapping) as a single implementation unit — small enough not to
need further decomposition. Zed registration and VS Code documentation
are lightweight follow-on steps once the adapter itself passes its E2E
harness and a manual Zed smoke test, not separate specs.
