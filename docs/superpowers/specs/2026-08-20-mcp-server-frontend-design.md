# MCP-server frontend — design

_2026-08-20._ Piece A of the "bring the ecosystem together" project (see
`aivyx-ecosystem/docs/research/2026-08-20-ecosystem-cohesion-research.md`
and the follow-up investigation into `aivyx-team`'s specialist pattern).
`aivyx-coder` gains a third frontend — alongside the TUI and `--acp` — that
exposes itself as an MCP server, so any local MCP client (starting with,
but not limited to, `aivyx`) can delegate a bounded coding task to it.
Piece B (wiring `aivyx-team` to consume this as a Nonagon specialist) is
explicitly out of scope here and gets its own brainstorm once this
interface is real.

## Why this doesn't need new security design

`aivyx-coder`'s security boundary — Landlock/seccomp confinement, the
permission gate, the git-checkpoint safety net — is the load-bearing
property of this codebase and is explicitly not being redesigned here.
Two facts make an MCP-server frontend an *addition* to the existing
architecture rather than a new one:

- **The frontend/core split already exists.** The same `Agent` runs
  under the TUI and under `--acp` — only the `PermissionPrompter`
  differs per frontend (`aivyx-cli`'s `agent_builder.rs` builds every
  other collaborator identically). A third frontend needs its own
  `PermissionPrompter` impl and its own entry point; nothing about
  `Agent`, `ToolExecutor`, or `ExecutionConfiner` changes.
- **`delegate_task` (`aivyx-core/src/delegate.rs`, Phase 9) already has
  almost exactly the shape a delegated MCP call needs**: spin up a
  fresh, isolated `Agent` — full tool access (within whatever it's
  granted), a completely fresh conversation history, a bounded
  iteration budget, one final text answer back (not the raw tool
  trail) — with a best-effort partial result and the existing
  `CUTOFF_NOTICE` framing if the budget runs out before a natural
  finish. This design reuses that shape for the MCP tool call itself,
  even though (per an explicit decision below) it does **not** reuse
  `delegate_task`'s trust model (which inherits the parent session's
  own gate/mode) or `AutonomousMode` (which is a different, existing,
  already-tested mechanism this project deliberately keeps separate,
  rather than risk entangling `--auto`'s semantics with a new remote-
  callable surface's).

## Architecture

A new frontend crate, `aivyx-mcp-server`, mirroring `aivyx-acp`'s
existing shape: one entry point, its own `PermissionPrompter`
implementation, no new dependency on `aivyx-core` beyond what `aivyx-acp`
already has. A new CLI flag, `aivyx --mcp-server`, stdio JSON-RPC (same
transport shape `--acp` already uses), mutually exclusive with
`--acp`/`--auto`/`--plan`/`--resume` — the same mutual-exclusion pattern
`--acp` already enforces in `main.rs`, extended with this new flag.

One server process runs against one project directory (its own `cwd`),
exactly like the TUI and `--acp` today — the existing Landlock/seccomp
confinement scope for that process is unchanged and applies to every
session the server ever runs. No per-call working-directory switching:
letting a remote caller pick arbitrary directories on the host is a much
bigger, separately-dangerous feature this design does not attempt.

## Access levels: a tool allowlist, not an `ActionKind` rule

Three tiers — `plan`, `edit`, `execute` — each defined as an explicit
set of tool names exposed to the model, the same "least privilege via
explicit allowlist" pattern `aivyx-team`'s own `filter_tools` uses
independently on the other side of this ecosystem (worth naming: the two
codebases converged on the same shape without sharing code, which is a
reasonable sign it's the right shape here too).

- **`plan`** reuses `ToolRegistry::plan_definitions()` verbatim — the
  exact same filtering `--plan` mode already applies. No new logic for
  this tier at all: read/search/task-list only, zero mutation possible.
- **`edit`** = `plan`'s set + `write_file` + `edit_file`. File mutation
  within the confined worktree, no shell, no git.
- **`execute`** = `edit`'s set + `run_command` + `run_shell` +
  `git_commit` + `repl_start`. Full tool access, still Landlock/seccomp-
  confined to the worktree exactly as every other frontend's shell/exec
  tools already are — this tier never turns confinement off, it only
  adds which tool *categories* the model may reach for.

A new `PermissionPrompter` implementation for this frontend (this
project's equivalent of `TuiPrompter`/`AcpPrompter`) auto-resolves any
request for a tool in the active session's tier set — no modal, no
human. This is belt-and-braces with the tool-list filtering above,
mirroring `--plan` mode's own existing dual-enforcement shape (CLAUDE.md:
"the type-level omission as the primary UX and the gate as backstop"):
the tool list is the primary defense (the model literally cannot select
an excluded tool), the prompter's auto-resolution is what happens if it
invents a tool call anyway.

No fourth "no sandbox" tier. `execute` is the highest level this design
offers, and it is still fully confined — a caller cannot ask this server
to run anything unconfined.

## The startup ceiling

A new `[mcp_server]` config section:

```toml
[mcp_server]
max_access_level = "edit"     # "plan" | "edit" | "execute" — the ceiling
session_ttl_secs = 1800       # idle sessions are evicted after this long
max_concurrent_sessions = 8   # bounded in-memory session map
max_iterations = 10           # per-session budget, mirrors [sub_agent]'s existing default
```

Every `code` call names the access level it wants. If that level is
above `max_access_level`, the call is **rejected outright** — an error
naming the configured ceiling — never silently downgraded. A caller
that gets a confusing tool-execution failure because its request was
quietly capped to a lower tier than it asked for is a worse experience
than an explicit, actionable rejection at the point of request.

`max_access_level` has no default value baked into this design's own
code — the operator must set it explicitly in `config.toml` before
`--mcp-server` will start at all. (Rationale: every other frontend this
codebase ships is either interactive, human-supervised (TUI, `--acp`) or
requires an explicit `--auto <goal>` invocation with its own required
`[verification].command` gate. An MCP server is the first frontend a
remote *process*, not a human, drives — it should not have a working
default that lets it run before an operator has made a deliberate
choice about how much access to grant.)

## Tool surface

Two MCP tools:

- **`code(task: string, access_level: "plan" | "edit" | "execute") →
  { session_id: string, result: string }`** — starts a fresh, isolated
  `Agent` (the `delegate_task` shape: full access within the tier, fresh
  conversation, the configured iteration budget, best-effort partial
  result with the existing `CUTOFF_NOTICE` framing on budget exhaustion).
  `access_level` above the configured ceiling is rejected before any
  agent is constructed.
- **`code_reply(session_id: string, message: string) → { result:
  string }`** — continues that session's conversation. The access level
  is fixed at session creation and is not renegotiable via `code_reply`
  — a tier is chosen once, when the session starts.

No third `close_session` tool for v1. TTL eviction (`session_ttl_secs`)
is the only cleanup path — a caller doesn't need to remember to release
a session, and the surface stays minimal (YAGNI: add an explicit close
later only if idle sessions genuinely need to be reclaimed faster than
the TTL allows in practice).

## Sessions

Held **in-memory only**, in a bounded map inside the running server
process (`max_concurrent_sessions`; the oldest idle session is evicted
first if a new `code` call would exceed the bound, same as the TTL
eviction path). This is deliberately **separate from the existing
on-disk `--resume` session store** — that store serves a different
purpose (a human's own long-running project conversation, keyed by
canonicalized cwd) and conflating the two would blur what each is for.
An MCP session does not survive a server restart; this matches Codex's
own actual `mcp-server` behavior and `delegate_task`'s own ephemeral
shape, and is a smaller, easier-to-reason-about mechanism than adding a
new on-disk key scheme.

## Explicit non-goals

- **Not reusing `AutonomousMode`.** A deliberate decision, not an
  oversight: `AutonomousMode` already has real, tested, `--auto`-specific
  semantics (including a hard-coded denial of MCP tool *calls* the agent
  itself makes while autonomous — a different concept from this project,
  which makes `aivyx-coder` reachable *as* an MCP server, but close
  enough in naming to risk real confusion if the two mechanisms were
  merged). Keeping them separate means tuning one can never silently
  change the other's behavior.
- **No per-call working-directory switching.**
- **No fourth "unconfined" access tier.**
- **No session persistence across a server restart.**
- **No mid-session access-level renegotiation.**
- **No `close_session` tool** (TTL eviction only, for v1).
- **Piece B (the `aivyx-team` side)** — wiring this server into the
  Nonagon as a remote specialist, and the capability-attenuation-across-
  a-process-boundary problem that requires — is a separate, later
  brainstorm, once this interface is real and stable to design against.

## Testing

- Unit: the tier→tool-allowlist mapping (`plan`/`edit`/`execute` produce
  exactly the expected tool sets); the ceiling-rejection path (a `code`
  call above `max_access_level` is rejected before agent construction,
  for all three ceiling settings); TTL eviction (an idle session past
  `session_ttl_secs` is gone on the next access attempt); the
  `max_concurrent_sessions` bound (a new session past the limit evicts
  the oldest idle one).
- Integration: a real `code` call against a tiny fixture repo at each
  access level, asserting the tier's tool boundary holds (e.g. an
  `edit`-level task that tries to run a shell command gets no such tool
  to call, and — if the model invents one anyway via a raw tool-call
  JSON injection test — the new `PermissionPrompter` denies it); a
  `code` → `code_reply` round trip proving the second call sees the
  first's conversation history; a budget-exhaustion case proving the
  `CUTOFF_NOTICE` partial-result behavior carries over unchanged from
  `delegate_task`.
