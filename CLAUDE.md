# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

`aivyx-coder` (binary name `aivyx-coder`, crate `crates/aivyx`) is a terminal (TUI)
coding agent that talks **only** to local LLMs — Ollama, vLLM, or llama.cpp's
`llama-server`, all via their OpenAI-compatible `/v1/chat/completions`
endpoint. It never calls a cloud API. The model drives tool calls
(read/write/edit files, grep/glob, run commands, git) against the real
filesystem, and every call passes through a layered permission + sandboxing
model built on the premise that the model may be wrong or actively
manipulated. This project is unrelated to `Rust/aivyx` (the other, much
larger Aivyx agent platform) beyond shared naming and authorship — don't
assume shared code or conventions between them.

The security boundary was designed before the tools that need it (see
`docs/HISTORY.md` for phase history) and is the load-bearing property of
this codebase — read the "Security model" section of `README.md` in full
before touching `aivyx-sandbox`, `aivyx-tools`, or the permission-gate
logic in `aivyx-core`.

## Build, run, test, lint

```sh
cargo run -p aivyx                         # run the TUI agent
cargo run -p aivyx -- --acp                # run as an ACP server (Zed/VS Code), stdio

cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets

# Single crate / single test
cargo test -p aivyx-core
cargo test -p aivyx-core some_test_name
```

- The real sandbox (Linux Landlock + seccomp) is on by default. To build
  without it (non-Linux, or a kernel without Landlock), use
  `--no-default-features` on `aivyx-sandbox` — this changes runtime behavior
  (see `sandbox.require_enforcement` below), not just the build.
- Config lives at `~/.config/aivyx-coder/config.toml` (written `0600` on
  first run — it may hold an `api_key`). Sessions persist under
  `~/.local/state/aivyx-coder/sessions/`, one per project directory keyed by
  canonicalized cwd, also `0600`.
- `AIVYX_DEBUG_LOG` (env var) captures raw wire traffic in plaintext,
  append-only, no rotation — treat as sensitive, delete when done debugging.

## Architecture

Workspace crates (`crates/*`), roughly bottom-up:

| Crate | What it owns |
|---|---|
| `aivyx-types` | Shared wire/domain types (`Role`, message shapes) with no logic |
| `aivyx-llm` | `LlmBackend` trait + `OpenAiCompatBackend`, streaming, context-window probing (`probe.rs` — detects Ollama's hidden default window) |
| `aivyx-sandbox` | The security boundary: `PermissionGate`/`ConfirmationGate` (`confirmation.rs` — decision point every tool call passes through before `Tool::execute`); `ExecutionConfiner`/`NoopConfiner`/`LandlockConfiner`/`default_confiner` (Landlock/seccomp process confinement, behind the default-on `sandbox-backend` feature) are re-exported from the `aivyx-confine` crate (a pinned `git` dependency, extracted 2026-08-16 so `aivyx` could share the same primitive — see that repo's own `CLAUDE.md`), not implemented here; the prompt-injection phrase-list tripwire (`InjectionFinding`/`InjectionTaint`/`scan_for_injection_markers`) is similarly re-exported from the `aivyx-injection-guard` crate (extracted 2026-09-05, same rationale), also not implemented here |
| `aivyx-tools` | `Tool` trait + `ToolExecutor::dispatch`, which centralizes the permission check so individual tools can't forget to gate a dangerous action; concrete tools under `tools/*.rs` (`read_file`, `write_file`, `edit_file`, `grep`, `glob`, `run_command`, `run_shell`, `git_read`, `git_commit`, `set_tasks`); the git snapshot mechanism (`GitCheckpointer`) is re-exported from the `aivyx-checkpoint` crate (a pinned `git` dependency, extracted 2026-08-18 so `aivyx` could share the same primitive — see that repo's own `CLAUDE.md`), not implemented here |
| `aivyx-repomap` | Aider-style repo map: tree-sitter symbol extraction + PageRank over the cross-file reference graph, token-budgeted and appended to the system prompt each turn (Rust, Python, JavaScript/JSX, and TypeScript/TSX today; other languages degrade gracefully to no map). Deliberately zero-dependency on any other workspace crate (pure filesystem-in/string-out) |
| `aivyx-config` | `Settings` — XDG config loading, defaults, `0600` writing |
| `aivyx-core` | `Agent`/`AgentConfig`/`AgentEvent`, the turn loop, `EditFormat` (native tool-call JSON vs. prompted SEARCH/REPLACE), `council` (multi-model `/council` mode), `session` (persistence + `--resume`), `edit_blocks` (SEARCH/REPLACE parsing) |
| `aivyx-tui` | The ratatui terminal UI: `app::run` (`app.rs` — spawns the agent as a background task, `tokio::select!`s over input/agent-events/permission-requests), `permission.rs` (`TuiPrompter` bridges `PermissionPrompter::prompt` from the background agent task to the render loop via a `oneshot` reply — fails closed to Deny if the render loop is gone), status line (context-budget indicator, plan-mode badge) |
| `aivyx-acp` | An [Agent Client Protocol](https://agentclientprotocol.com) frontend (JSON-RPC over stdio, via the `agent-client-protocol` crate) — the same `Agent` core embedded directly in an editor (Zed's Agent panel; VS Code via the third-party `formulahendry.acp-client` extension) instead of the terminal. `translate.rs` (pure `AgentEvent` → ACP `SessionUpdate`/`StopReason` mapping, no I/O), `prompter.rs` (`AcpPrompter` — `PermissionPrompter` over `session/request_permission`; `DeferredPrompter`/`PrompterInstaller` bridge the gap between `ConfirmationGate` construction, which happens before any ACP connection exists, and `NewSessionRequest`, which is when a real connection first does), `session.rs` (one session per process; `session/prompt` runs `Agent::run_turn` inside `connection.spawn(...)`, not inline in the request handler — a deadlock fix, since `AcpPrompter`'s own `session/request_permission` round-trip needs the dispatch loop the handler would otherwise be blocking). See `docs/superpowers/specs/2026-07-20-acp-editor-integration-design.md` and README's "Editor integration (ACP)" section. |
| `aivyx-mcp-server` | A third frontend, over stdio via the `rmcp` crate: exposes aivyx-coder as an MCP server (`code`/`code_reply` tools) for delegation from another local MCP client instead of a terminal or editor. `tiers.rs` (`AccessLevel::{Plan,Edit,Execute}`, each additive over the previous, capped by the required `[mcp_server].max_access_level` config — no default, refuses to start unset), `session.rs` (`build_session_agent` builds one fresh, isolated `Agent` per MCP session via `tier_registry`, filtering the shared `mcp_registry` base down to the session's tier; `TieredPrompter` auto-resolves every in-tier call, since an MCP-server session has no human to prompt), `server.rs` (the `rmcp`-facing layer: `code`/`code_reply` tool definitions, an in-memory TTL-evicted session map). See README's "MCP server integration" section. |
| `aivyx` (`crates/aivyx`) | The binary — `agent_builder.rs` builds `Agent` + every collaborator identically regardless of frontend (only the `PermissionPrompter` differs); `main.rs` wires config → backend → agent → either the TUI or, behind `--acp`/`--mcp-server`, `aivyx-acp`/`aivyx-mcp-server` |

### Data flow for one turn

model emits a tool call → `ToolExecutor::dispatch` (`aivyx-tools/src/lib.rs`)
→ `tool.permission_request()` builds a `PermissionRequest` tagged with an
`ActionKind` (`Read | Write | Execute | Delete | Internal` — `Internal`
exists so session-only state like `set_tasks` can auto-allow without
dishonestly claiming to be a `Read`) → `gate.check()` walks a fixed tier
order: deny_paths hard block (path-only) → `Read`/`Internal` auto-allow →
plan-mode deny (checked *before* the cache below, specifically so an
approval granted before entering plan mode can't leak through) →
Always-Allow cache lookup, keyed on the **exact** target — full path or full
`(program, args)`, never the tool or program alone, so approving `write
a.rs` never blesses `write b.rs` — → interactive prompt (cache seeded at
startup from config `allowed_commands`) → on Allow, **any** tool whose
`mutates_outside_session()` is true is checkpointed to
`refs/aivyx/checkpoints/<ts>` first (not just `run_command`/`run_shell` —
`write_file`/`edit_file`/`git_commit` checkpoint too, since the gate check
already ran) → if a command tool, `ExecutionConfiner` applies Landlock
(filesystem scoping) + seccomp (syscall denylist) to the forked child before
`exec` → result returned to the model.

`git_commit`'s permission target is a `Command{"git", [...]}`, not a
`Path` — every distinct commit message is therefore a distinct cache key, so
Always-Allow can never blanket-approve future commits the way it could for a
path-scoped tool.

**"All tool calls go through the gate" is enforced by convention** (the
executor is the only caller of `Tool::execute`), not by the type system —
keep this invariant in mind when adding a new call path to a tool.
`Tool::mutates_outside_session()` defaults to `true` (fail-closed), so a new
tool is hidden from plan mode and checkpointed unless it explicitly opts out.

### Edit formats

`native` (default): edits arrive as `edit_file`/`write_file` tool-call JSON.
`prompted`: the model is taught to emit Aider-style SEARCH/REPLACE text
blocks instead (parsed by `edit_blocks` in `aivyx-core`), which survives
better on small models than JSON string-escaping multiline code. Both are
parsed into the *same* tool calls, so permission modal / diff preview /
plan-mode denial / `deny_paths` / checkpoints behave identically regardless
of format.

### Plan mode

`Ctrl+P` (or `--plan`) makes the agent read-only: `ToolRegistry::plan_definitions()`
filters the tool list sent to the model down to `!mutates_outside_session()`
tools, *and* `ConfirmationGate::check`'s plan-mode branch independently
denies a mutating call if the model invents one anyway — belt-and-braces,
with the type-level omission as the primary UX and the gate as backstop.
`PlanMode` is an `Arc<AtomicBool>` shared between whichever frontend is
running (the TUI, the only writer via Ctrl+P; or `aivyx-acp`, via ACP's
`session/set_mode`) and the gate (a reader) — the model has no path to it
at all, not just no incentive to flip it.

## Sandbox internals (now in the `aivyx-confine` crate)

This confinement logic used to live in `aivyx-sandbox/src/confiner.rs`;
as of 2026-08-16 it's in the standalone `aivyx-confine` repo (a pinned
`git` dependency of `aivyx-sandbox`, re-exported so every call site here
is unchanged — see `aivyx-sandbox/src/lib.rs`'s own doc comment). The
policy summary below is still accurate and worth knowing before touching
`ExecutionConfiner` or its call sites, but the actual source — and its
own more detailed doc comments — now lives in that other repo, not here.

- **Landlock** (ABI V7): write grants are `cwd` + the system temp dir(s);
  read grants are `cwd` + a fixed system/toolchain list (`/usr`, `/lib`,
  `/lib64`, `/bin`, `/sbin`, `/etc`, plus `$HOME`-relative `.cargo`,
  `.rustup`, `.gitconfig`, `.config/git` — git needs a readable global
  config or it dies fatally) + `sandbox.extra_read_paths`. `deny_paths`
  nested inside a granted root are carved out by recursively enumerating
  and excluding that child (Landlock has no native "deny" rule); a few
  `/dev/{null,zero,urandom,random}` device paths are granted read+write
  explicitly — `/dev` itself is deliberately not granted wholesale.
- **seccomp-bpf** denylist (allow-by-default, `EPERM` on match): `ptrace`,
  `process_vm_readv`/`writev`, `io_uring_setup`/`enter`/`register`, `mount`,
  `umount2`, `reboot`, `kexec_load`/`file_load`, `init_module`,
  `finit_module`, `delete_module`, `pivot_root`, `swapon`/`off`, `acct`,
  `bpf`, `perf_event_open`, `keyctl`/`add_key`/`request_key`,
  `userfaultfd`, `unshare`, `setns`, `personality` — deliberate
  defense-in-depth compensating for not using Linux namespaces.
- **`sandbox.require_enforcement`** is checked at *two* points, both
  fail-closed when `true` / fail-open (log + run unconfined) when `false`:
  the parent failing to build the Landlock ruleset at all, and the forked
  child's `pre_exec` observing a `restrict_self()` status that isn't
  `RulesetStatus::FullyEnforced`. The `pre_exec` closure is written
  allocation-free (`ErrorKind`-based `io::Error` only, no
  `.to_string()`/`io::Error::other`) since it runs post-fork/pre-exec under
  async-signal-safety constraints.

## Serving backends

Ollama is the zero-setup default, but has a footgun worth knowing when
debugging truncated/odd responses: it serves its own default context window
(often 4096) unless the Modelfile sets `num_ctx` or `OLLAMA_CONTEXT_LENGTH`
is set — the `/v1` endpoint can't request a larger window per-call.
`llama-server` is recommended for serious use since the window is explicit
on the command line (`-c N`). See `README.md` "Serving" for full setup
(including a CUDA/ccache build gotcha) and the `[backend] context_tokens`
config field.

## Known, deliberately-undefended limitations

Documented in `README.md` "Known limitations" — worth checking before
assuming a gap is a bug: indirect prompt injection (a heuristic
scan-and-pause guard exists in autonomous mode — see `aivyx-sandbox`'s
`InjectionTaint`/`scan_for_injection_markers`, now sourced from the
`aivyx-injection-guard` crate — but it's pattern-based, not structural,
and doesn't run in interactive mode at all), network is unrestricted for
approved commands, env vars are inherited by spawned commands, TOCTOU
windows on path resolution, and `git_commit` (re)stages full paths rather
than partial hunks.

## Where to look next

- `README.md` — the primary reference: full security model, config
  reference, tool table, serving setup. More authoritative and current than
  any summary here.
- `ROADMAP.md` — current status, in brief.
- `docs/HISTORY.md` — full phase-by-phase history, every design decision
  and its evidence, including the project's own audit history.
