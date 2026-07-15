# MCP Client Support — Design

**Status:** Approved by user 2026-07-16.

## Problem

aivyx-coder's tool set today is entirely hand-written: every tool is code
this project wrote and can honestly self-describe (its `ActionKind`, its
schema, its behavior). The Model Context Protocol (MCP) lets a user plug in
*arbitrary* third-party servers — filesystem variants, databases, issue
trackers, custom internal tools — without this project having to hand-build
an integration for each one. This is the highest-leverage capability gap
identified after the `web_fetch`/`web_search` and `AGENTS.md` phases: instead
of one bespoke tool per integration, MCP support multiplies what the agent
can reach by however many MCP servers the end user chooses to configure.

This is architecturally a new kind of tool for this codebase: one whose
name, description, schema, and *behavior* are all supplied at runtime by
code aivyx-coder didn't write and can't inspect. Every existing permission
and trust decision in this project assumed the opposite.

## Scope

v1 covers the full three-primitive MCP client surface — **tools**,
**resources**, and **prompts** — over **stdio transport only**. Remote
(HTTP/SSE) transport is an explicit non-goal for this phase (see Non-Goals).

## Architecture

A new module, `crates/aivyx-tools/src/mcp/` (mirroring the existing
`lsp/mod.rs` / `transport.rs` / `protocol.rs` split), implements an MCP
client speaking JSON-RPC 2.0 over each configured server's stdin/stdout.

Each configured server gets its own spawned child process and its own
`McpConnection`, architecturally a sibling of the LSP client's `Connection`:
the same request/response correlation via a `PendingMap` of `oneshot`
channels and a background read-loop task, the same `is_dead()` liveness
check. The stdio *framing* differs — MCP uses newline-delimited JSON, not
LSP's `Content-Length: N\r\n\r\n` header block — so `lsp/transport.rs` isn't
reused verbatim, only its proven shape (background reader task, pending-map
correlation, liveness check, `Drop`-based task cleanup).

At startup, before `ToolRegistry` is constructed, `main.rs` connects to
every configured server **concurrently**: spawns the child process (through
the same `ExecutionConfiner` every other spawned process in this codebase
goes through — no special-casing for MCP), performs the MCP `initialize`
handshake, then calls `tools/list`, `resources/list`, and `prompts/list`.
Each server's full discovery sequence is bounded by that server's own
`timeout_secs`. A server that fails to spawn, fails the handshake, or times
out is skipped with a one-time warning — exactly like a missing
`rust-analyzer` binary is reported today — and the rest of the session
proceeds with every other server's tools/resources/prompts still available.

This eager-at-startup design is forced by `ToolRegistry` being a fixed list
built once, synchronously, before the TUI event loop starts: a tool's
name and schema must be known before the model can ever be offered it, so
discovery has to complete before registration, not lazily on first call
(unlike the LSP client, which lazily spawns `rust-analyzer` on first actual
use — MCP's server *set* is user-configured and arbitrary, not a single
well-known always-present tool like Rust's LSP server).

A server whose connection dies mid-session (crash, pipe closed) is
respawned on its next tool call — mirroring the LSP client's
`ensure_started`/`is_dead()` respawn pattern exactly, including its
discovery step being re-run against the freshly spawned process.

## Tools → `Tool` trait mapping

Each discovered MCP tool becomes one `Arc<dyn Tool>`, registered in
`ToolRegistry` under the name:

```
mcp__<server_name>__<tool_name>
```

The double-underscore separator avoids collisions when a server name or
tool name itself contains a single underscore. This isn't an existing
aivyx-coder convention — it's borrowed from how other MCP clients (Claude
Code among them) already prefix discovered MCP tool names, so it should
look familiar to anyone who's used MCP through a different client.

Its `ToolDefinition` is built directly from the server's advertised
`name`/`description`/`inputSchema` — MCP's `inputSchema` is JSON Schema, the
exact shape `ToolDefinition.parameters_schema` (`serde_json::Value`)
already expects, so no schema-translation layer is needed.

`execute()` calls `tools/call` on that server's `McpConnection` and converts
the result's content blocks into a `ToolOutput::Ok(String)`:
- Text content blocks are concatenated directly.
- Non-text content blocks (images, audio, embedded resources) render as a
  placeholder note (e.g. `[non-text content of type "image" omitted]`)
  rather than being decoded/forwarded — full multimodal MCP content is a
  non-goal for v1 (see Non-Goals).
- An MCP-level tool error (the `isError: true` result shape) maps to
  `ToolOutput::Error`, not `Ok`, so the model can see the call failed.

**Permission tier:** every MCP tool call declares a new `ActionKind::McpTool`
— not reusing `Write` — so it is **always confirm-gated**, uniformly,
regardless of anything the server itself claims about its own behavior
(including MCP's optional, advisory `readOnlyHint`/`destructiveHint`
annotations, which this design does not trust). This mirrors the existing
precedent for why `ActionKind::Internal` is kept distinct from `Read`: a
capability that touches the outside world in a way this project didn't
write and can't verify must not be able to honestly describe itself as
safe. `ConfirmationGate`'s existing Always-Allow cache still applies per
exact `(tool, arguments)` key, same as any other confirm-gated tool — the
user can approve once per session and not be re-prompted for the identical
call.

Like every other `Tool`, an MCP tool's `mutates_outside_session()` is left
at the trait default (`true`, fail-closed) rather than overridden — so MCP
tools are also not offered to the model at all while Plan Mode is active,
the same treatment `Write`/`Execute`/`Delete` tools already get, since a
capability this project can't verify is at least as deserving of that
restriction as its own hand-written mutating tools.

## Resources & prompts

Unlike arbitrary tools, MCP's resources and prompts primitives are
protocol-guaranteed read-only — a server cannot define a resource or prompt
that has a mutating side effect through those primitives. Rather than
registering one tool per discovered resource/prompt (which could be
unbounded and churns every time a server's resource list changes), these
become four fixed meta-tools, registered once at least one server is
connected (not per-server, not per-item):

- **`list_mcp_resources(server?: String)`** — lists resource
  `uri`/`name`/`description`/`mimeType` across all connected servers, or one
  server if `server` is given.
- **`read_mcp_resource(uri: String)`** — fetches one resource's content
  (text resources return their text directly; binary resources render a
  placeholder note, same non-text-content treatment as tool results).
- **`list_mcp_prompts(server?: String)`** — lists available prompt
  templates and their declared arguments.
- **`get_mcp_prompt(server: String, name: String, arguments?: object)`** —
  fetches the expanded prompt (its resolved messages, concatenated to
  text) and returns it as a string for the model to read and use directly
  in its own reasoning. This does **not** inject new conversation turns or
  wire into a slash-command mechanism — that would require new `aivyx-tui`
  UI plumbing orthogonal to the existing `Tool` trait model, and is out of
  scope for v1 (see Non-Goals).

All four meta-tools use `ActionKind::Read` (auto-allowed, no confirmation
modal) — the read-only guarantee here comes from the MCP protocol's own
definition of what a resource/prompt *is*, not from trusting any individual
server's self-description, so this does not contradict the tools decision
above. Each also overrides `mutates_outside_session()` to `false`, matching
every other auto-allow `Read` tool (`read_file`/`grep`/etc.) — so, unlike
MCP tools, all four remain available and offered to the model while Plan
Mode is active.

## Configuration

A new `[[mcp.servers]]` array in `config.toml`, one entry per server:

```toml
[[mcp.servers]]
name = "filesystem"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/home/user/project"]
env = { API_KEY = "..." }
timeout_secs = 30
```

- `name` (`String`, required) — used verbatim in the `mcp__<name>__<tool>`
  prefix and in resource/prompt server-filter arguments.
- `command` (`String`, required) — the executable to spawn.
- `args` (`Vec<String>`, default empty).
- `env` (`HashMap<String, String>`, default empty) — additional environment
  variables for the spawned process (e.g. API keys the server itself
  needs), merged over the agent's own environment.
- `timeout_secs` (`u64`, default `30`) — bounds this server's spawn +
  handshake + discovery sequence, mirroring `[lsp] timeout_secs`'s role.

There is no top-level `[mcp] enabled` flag: an empty `mcp.servers` list is
already a complete no-op (no servers to connect to, no tools/resources/
prompts to register), so a separate toggle would just be a second way to
say the same thing. This differs from `[web] enabled`, which had to gate
two tools that are otherwise always statically present regardless of
configuration — there's no equivalent "always present" MCP tool to gate.

## Non-Goals (v1)

- **aivyx-coder exposing itself as an MCP server.** This design is
  client-only — consuming other servers' capabilities, not exposing
  aivyx-coder's own tools over MCP to other clients. A separate concern if
  ever pursued.
- **HTTP/SSE (remote) transport.** stdio-spawned local servers only.
  Revisit if real demand emerges for connecting to an already-running
  remote MCP server, per this project's established "ship the narrow
  version first" pattern.
- **MCP `sampling` capability** (a server asking the client to run an LLM
  completion on its behalf) and **`roots` capability** (the client
  advertising which directories are in scope to servers). Neither is
  required for a useful v1 tools/resources/prompts client.
- **Non-text content blocks** (images, audio, embedded binary resources) —
  rendered as a placeholder note, not decoded or forwarded to the model.
- **Per-server trust configuration / auto-allow for any MCP tool.**
  Superseded by the uniform "every MCP tool is always confirm-gated"
  decision above — there is no config knob to weaken it in v1.
- **Prompt-to-slash-command wiring**, or any other new `aivyx-tui` UI
  surface for prompts — `get_mcp_prompt` returns text for the model to use
  in its own reasoning, nothing more.

## Testing strategy

Following this project's established "test doubles over real network/
process calls" convention (the LSP client's fake JSON-RPC server, web
tools' hand-rolled HTTP mock server):

- Unit tests for the MCP stdio transport (newline-delimited JSON framing,
  request/response correlation, notification handling) against an
  in-process `tokio::io::duplex()` pair, mirroring `lsp/transport.rs`'s own
  test shape exactly.
- Unit tests for the `initialize` handshake, `tools/list`/`resources/list`/
  `prompts/list` discovery, and `tools/call`/`resources/read`/
  `prompts/get` against the same duplex-pair test double, scripting a
  minimal fake MCP server's responses.
- Unit tests for the `Tool` trait adapter: schema passthrough, text vs.
  non-text content-block handling, `isError` mapping to
  `ToolOutput::Error`.
- Unit tests for the four resources/prompts meta-tools' argument handling
  (server-filter present/absent, multi-server aggregation) against the
  same fake-server test double.
- Unit test confirming a server that fails/times out during startup
  discovery is skipped with a warning, without preventing other configured
  servers' tools from registering.
- Unit test confirming `ActionKind::McpTool` is never auto-allowed by
  `ConfirmationGate` (ends up on the confirm-gated path, not the
  `Read`/`Internal` auto-allow branch) and that the four meta-tools' new
  `ActionKind::Read` usage does auto-allow.
- One live E2E through the real binary against a real, small MCP server
  (a minimal reference server, or a purpose-built throwaway one if no
  suitable lightweight reference server is available in the implementation
  environment) — matching this project's strict live-verification
  precedent for every phase shipped so far: a real tool call showing the
  confirmation modal and succeeding on approval, and a real resource/prompt
  read via the auto-allowed meta-tools, both verified via the persisted
  session JSON.
