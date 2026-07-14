# LSP Integration — Design

**Status:** Approved by user 2026-07-14. Phase 9 stretch goal, next item after
architect/editor model-pairing and sub-agent delegation.

## Problem

`aivyx-repomap` already gives the model a tree-sitter-based, ranked, static
symbol list (names, signatures) injected into the system prompt every turn —
but it can't answer two questions a small local model genuinely needs: which
exact definition a call site resolves to (when several candidates share a
name, e.g. same-named methods across different `impl` blocks), and every
call site of a symbol across the whole workspace. `grep` finds textual
matches, not semantic ones. LSP (specifically `rust-analyzer`) is the
standard source of this exact symbol resolution.

This is a structurally new capability shape for the project: every existing
process-executing tool (`run_shell`, `run_command`) is one-shot
(spawn → drain → exit); an LSP server is a long-lived, stateful subprocess
speaking JSON-RPC over stdio, indexing a workspace incrementally. This design
resolves how that shape fits the project's existing tool/permission/sandbox
architecture without introducing a background-daemon posture the project has
deliberately avoided elsewhere.

## Scope (explicit forks resolved with the user)

- **Capabilities**: `go_to_definition` and `find_references` only — the two
  read-only queries a ranked static symbol list can't answer. No hover,
  no workspace symbol search, no rename. Zero mutation surface; no new
  permission tier.
- **Language**: Rust only, via `rust-analyzer` — matches `aivyx-repomap`'s
  current Rust-only scope exactly. Multi-language support stays the
  separately-tracked "multi-language repo map support" roadmap item; not
  front-run here.
- **Server lifecycle**: lazy — `rust-analyzer` spawns on the first
  `go_to_definition`/`find_references` call in a session, then stays alive
  and is reused for the rest of the session. Sessions that never call these
  tools pay zero startup cost.
- **Sandboxing**: the `rust-analyzer` subprocess is spawned through the
  existing `ExecutionConfiner` — same Landlock/seccomp confinement every
  other process-executing tool already gets, no special-casing. It only
  needs read access to the workspace and toolchain paths the confiner
  already grants by default.
- **Code placement**: a new module inside `aivyx-tools` (`aivyx-tools/src/lsp/`),
  not a new crate — mirrors `GitCheckpointer`'s precedent of a substantial
  (~650-line-class) stateful component living directly in `aivyx-tools`
  rather than its own crate, since nothing outside `aivyx-tools` needs it.
- **Missing binary**: `go_to_definition`/`find_references` are always
  registered and offered to the model. If `rust-analyzer` isn't on `PATH`,
  the first call's spawn attempt fails and returns a clear tool-result
  error explaining how to install it — no startup probe, no conditional
  registration.

## Wiring: `LspClient` is constructor-baked, not `ToolExecutor`-shared

`GitCheckpointer` lives as a field on `ToolExecutor` because
`ToolExecutor::dispatch` calls into it around *every* mutating tool call — a
genuinely cross-cutting concern spanning the whole tool-dispatch path.
`LspClient` isn't cross-cutting: only `GoToDefinitionTool` and
`FindReferencesTool` ever touch it. It therefore follows the simpler,
already-established `RunCommandTool` pattern instead — tool-specific
dependencies baked into the tool's own constructor, not threaded through
`ToolExecutionContext`/`ToolExecutor`:

- `LspClient` is constructed once in `aivyx/src/main.rs`, wrapped in a single
  `Arc<LspClient>`.
- That one `Arc` is passed into both tools' constructors —
  `GoToDefinitionTool::new(lsp: Arc<LspClient>)`,
  `FindReferencesTool::new(lsp: Arc<LspClient>)` — so both share the one
  running `rust-analyzer` process without any new field on
  `ToolExecutionContext` or `ToolExecutor`.
- The confiner each tool needs is already available per-call via the
  existing `ctx.confiner: Arc<dyn ExecutionConfiner>` field (the same one
  `run_shell`/`run_command` already use) — passed into
  `LspClient::ensure_started(cwd, confiner)` at call time, no new plumbing.

## `LspClient`: process management and JSON-RPC

- **Lazy spawn**: `async fn ensure_started(&self, cwd: &Path, confiner: &Arc<dyn ExecutionConfiner>) -> Result<(), LspError>`,
  internally guarded (e.g. `tokio::sync::OnceCell` or an internal
  `Mutex<Option<ChildState>>`) so concurrent calls don't double-spawn.
  Builds a `tokio::process::Command` for `rust-analyzer` with `cwd` as the
  working directory, passes it through `confiner.confine(command)` exactly
  as `run_shell`/`run_command` do, then spawns it and performs the LSP
  `initialize`/`initialized` handshake (`rootUri`/`workspaceFolders` set to
  `cwd`).
- **Framing**: LSP's `Content-Length: N\r\n\r\n<json>` message framing,
  parsed from the child's stdout by a background task spawned once at
  startup.
- **Request/response correlation**: a monotonically increasing request-id
  counter plus a `Mutex<HashMap<i64, oneshot::Sender<serde_json::Value>>>` —
  the background reader task dispatches each incoming response to its
  waiting `oneshot` sender; a request-sending call writes to the child's
  stdin (serialized by its own lock) and awaits its own `oneshot::Receiver`.
- **Respawn on crash**: `ensure_started` detects a dead child (e.g. a failed
  write, or the reader task observing EOF) and respawns once, transparently
  — the caller never sees a "the server died" error for a routine crash
  recovery.

## Keeping `rust-analyzer`'s view in sync with disk

aivyx-coder has no persistent "open editor buffer" concept — every edit
lands on disk via `write_file`/`edit_file` directly, with nothing analogous
to an editor's live buffer state. Left unaddressed, this creates a real
staleness risk: a file opened once in `rust-analyzer` and never re-synced
would be queried against stale in-memory content after a later edit.

Resolution: immediately before every `go_to_definition`/`find_references`
call, `LspClient` reads the target file's current on-disk content and sends
`textDocument/didOpen` (first time seeing that URI) or a full-document
`textDocument/didChange` (every subsequent time) with that fresh content —
forcing `rust-analyzer`'s view of the file current right before the query,
regardless of what edits happened since the file was last touched. No
filesystem watcher, no hook into `write_file`/`edit_file`, no `didClose`
(sessions are short-lived; workspace file counts stay small enough that
leaking open documents for a session's duration is fine).

## Tool shapes

Both tools take `path: String, line: u32, column: u32` — **1-indexed**,
matching `grep`'s existing `path:line_number:text` convention the model
already sees in this project (`grep-searcher`'s line numbers are 1-indexed).
Converted to LSP's 0-indexed `Position` at the boundary, inside `LspClient`
— nothing 0-indexed ever reaches the model in either direction.

Output mirrors `grep`'s format: one `path:line:text` line per result (the
resolved definition's location for `go_to_definition`, one line per
reference site for `find_references`) — `text` is that line's current
source, read fresh from disk at format time, so results read consistently
with what `grep` output already looks like.

A new `[lsp] timeout_secs` config field (`u64`, generous default — cold
`rust-analyzer` indexing on first call can take tens of seconds on a larger
workspace) bounds each JSON-RPC request, matching the project's existing
pattern of per-feature configurable timeouts (`CommandSpec.timeout_secs`,
council's `COUNCIL_IDLE_TIMEOUT`).

## Error handling

- `rust-analyzer` not on `PATH` → spawn fails on first call → clear
  `ToolOutput` error: `"rust-analyzer not found on PATH — install it to use
  go_to_definition/find_references"`.
- Request exceeds `[lsp] timeout_secs` (cold indexing, or a hung server) →
  clear timeout error, same shape as `run_command`'s existing timeout
  handling.
- No definition/no references found → **not an error** — an `Ok` result
  stating nothing was found, exactly like a zero-match `grep`.
- A crashed `rust-analyzer` process → the next call's `ensure_started`
  detects the dead process and respawns once, transparently (see above).

## Testing strategy

- Unit tests for JSON-RPC framing (`Content-Length` parse/write) and
  request/response correlation against an in-process mock reader/writer —
  no real `rust-analyzer` needed.
- Unit tests for 1-indexed↔0-indexed position conversion in both
  directions, and for `path:line:text` output formatting.
- An integration test spawning the real `rust-analyzer` against a small
  fixture crate (skipped/ignored when `rust-analyzer` isn't present on the
  dev/CI machine, matching this project's existing treatment of
  environment-dependent tests), asserting a real go-to-definition and
  find-references round trip.
- Confiner-reuse test verified the same way `run_shell`/`run_command`'s
  confinement is already tested — asserting `confine()` is actually invoked
  on the command built for `rust-analyzer`.
- One live E2E through the real binary, same PTY-harness pattern used by
  every prior phase.

## Non-goals

- No hover/type-info, no workspace symbol search, no rename — all
  explicitly deferred; rename in particular needs its own
  permission-gate/confirmation/checkpoint story since it's a real mutation,
  not a read-only query.
- No multi-language support — Rust/`rust-analyzer` only; a pluggable
  per-language server config table is deferred to the "multi-language repo
  map support" roadmap item, not front-run here.
- No startup probe for `rust-analyzer`'s presence — the tools are always
  registered; absence is surfaced as a clear first-call error instead.
- No filesystem watcher or `write_file`/`edit_file` hook — staleness is
  handled entirely by the fresh-sync-before-every-query strategy above.
