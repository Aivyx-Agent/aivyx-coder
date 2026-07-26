# REPL / Interactive-Process Support — Design

**Status:** Approved by user 2026-07-26.

## Context

Item #12 in this project's second capability audit (2026-07-22): `run_command`/
`run_shell` are strictly one-shot — each call spawns a process, drains it to
completion (or times out/is cancelled), and reaps it. There's no way to hold a
process open across multiple tool calls, so the model can't interactively
iterate against a language REPL, a database/debugger CLI, or a long-running
dev server it wants to keep alive and poll. Logged to `ROADMAP.md`'s backlog
rather than built ad hoc during that audit, since it's a new capability
surface (new tool, new `ActionKind`, new gate tier) that deserves its own
design pass.

The user's stated use cases span three shapes that all need the same
underlying primitive: quick language-REPL snippet testing (`python3 -i`,
`node`), multi-step interactive CLIs with their own command language (`psql`,
`gdb`, `lldb`), and long-running dev servers kept alive across turns and
polled rather than conversed with turn-by-turn (`npm run dev`).

Three design questions were resolved with the user via one-at-a-time
questions before this doc was written:

1. **Concurrency**: one process at a time, not multiple named/concurrent
   sessions. Simpler tool interface (no session-ID threading), covers every
   stated use case, extensible later if it turns out to matter.
2. **Send-gating** (the security-critical question): starting a process goes
   through the normal `ConfirmationGate` `Execute` flow, same as
   `run_shell` today. Once running, sending it input does **not** re-prompt
   per call — prompting on every REPL line would be as unusable as
   prompting on every `read_file` call, and the real security boundary is
   what the confined process can reach (Landlock/seccomp), not per-input
   review, the same way approving `psql` once doesn't get a human re-asked
   per SQL statement they type.
3. **Pipes vs. PTY**: plain pipes (`Stdio::piped()`, the same mechanism
   `run_command`/`run_shell` already use), not a real pseudo-terminal. Every
   program in scope supports piped stdin/stdout for its core protocol, and a
   PTY would import exactly the ANSI-escape/`\r`-vs-`\n` complexity this
   project's own live-E2E test scripts just hit and had to work around
   (`docs/HISTORY.md`'s "Codebase audit + rig deployment" chapter) — into
   the product itself, not a throwaway script. Risk accepted: a program that
   hard-refuses to run non-interactively (checks `isatty()` and exits) won't
   work; revisit if that comes up in practice.

**Explicitly out of scope**: multiple concurrent sessions, PTY allocation,
prompt-pattern detection (`>>> `-style per-program prompt matching) for
deciding when output is "done," and any REPL-specific special-casing (no
hardcoded knowledge of Python/Node/psql/gdb — the tool is a generic
persistent-process primitive, "REPL" is just the use case that motivated it).

## Decisions

### Tool shape: three tools sharing one piece of state

`repl_start`, `repl_send`, `repl_stop` — following this project's existing
one-tool-per-verb convention (`read_file`/`write_file`/`edit_file`/
`delete_file` are separate tools rather than one unified file-op tool), not
a single tool with an `action` discriminator. All three share one
`Arc<Mutex<Option<ReplSession>>>`, the same sharing pattern already used for
`Agent`'s task list (`Arc<Mutex<Vec<Task>>>`) and `InjectionTaint` — a
struct field on `Agent`/`agent_builder.rs`'s wiring, constructed once and
handed to all three tool instances at registration time, exactly like
`set_tasks`'s shared task list today.

- **`repl_start(program: String, args: Vec<String>)`** — spawns
  `program` with `args` directly (no shell parsing — matches `git_commit`/
  `git_push`'s `PermissionTarget::Command{program, args}` shape, not
  `run_shell`'s `sh -c "..."` string, since launching an interactive
  program doesn't need shell features like pipes/redirects and avoiding
  shell parsing keeps this simpler and avoids an extra injection-adjacent
  surface). `ActionKind::Execute`, `PermissionTarget::Command{program,
  args}` — goes through the normal gate (interactive prompt / Always-Allow
  cache / pre-approved `allowed_commands`), gets checkpointed before
  running (`mutates_outside_session()` stays at the trait default `true`),
  and is hidden from the model entirely in Plan mode (same treatment as
  every other mutating tool) and in Autonomous mode (same treatment as
  `run_shell`/`git_commit` — an unattended session has no one to review an
  interactive process's arbitrary back-and-forth). Errors immediately if a
  session is already running ("a REPL session is already running (`{program}
  {args}`) — call repl_stop first"), rather than silently killing the old
  one.
- **`repl_send(input: Option<String>)`** — if `input` is present and
  non-empty, writes it plus a trailing newline to the child's stdin; if
  absent/empty, writes nothing (pure poll — reads back whatever
  accumulated since the last read without sending anything new, which is
  what makes checking on a dev server's output work). Errors clearly if no
  session is running ("no REPL session is running — call repl_start
  first").
- **`repl_stop()`** — kills the process group, returns any final buffered
  output plus a confirmation, clears the shared state. No-op-with-error if
  nothing is running (same "no REPL session is running" message).

### A new `ActionKind::Interact` — auto-allows in Act mode only, checked after plan-mode/autonomous-mode denial

`repl_send`/`repl_stop` both report `ActionKind::Interact` — auto-allowed
(no interactive prompt, no Always-Allow cache lookup) once past the
plan-mode/autonomous-mode checks, distinct from `Internal` (which is
documented as "no filesystem, process, or network effect" — dishonest for a
tool that writes to a live process's stdin) and distinct from `Execute`
(which would mean re-prompting every send, the exact outcome the
send-gating decision above rejected).

The auto-allow check for `Interact` is placed in `ConfirmationGate::check`
**after** the plan-mode and autonomous-mode branches, not alongside the
existing `Read | Internal` auto-allow (which sits *before* plan-mode,
deliberately, per that check's own comment: "an approval granted before
plan mode was entered must not leak through it"). If `Interact` auto-allowed
in that same early tier, a session started in Act mode that's still running
when the user presses Ctrl+P would keep silently accepting input during
Plan mode — exactly the leak-through failure mode that comment already
guards against for the Always-Allow cache, just via a live process instead
of a cached decision. With `Interact` checked after plan-mode, a session
already running when Plan mode is entered stops accepting input
immediately (denied, with a clear reason), and can resume once Plan mode is
exited.

In Autonomous mode, `Interact` is **not** special-cased to auto-allow — it
falls through to the existing generic "not pre-approved, autonomous mode
never prompts" deny. `repl_start` is already unreachable there (hidden from
the model, matching `run_shell`), so this is defense in depth rather than a
load-bearing check, the same reasoning already applied to `git_commit`'s
target being permanently non-cacheable as a backstop even though it's also
hidden.

`repl_send`/`repl_stop` override `mutates_outside_session()` to `false` —
they don't introduce a new mutation risk beyond what `repl_start` already
had checkpointed and gate-approved (mirrors `git_read` overriding to
`false` despite touching the outside world in a read-only way). This also
means no new checkpoint fires per send — a checkpoint per REPL line in a
fast back-and-forth loop would flood `refs/aivyx/checkpoints/` for no
practical rollback benefit, since a live interactive session is one
continuous unattended interaction from the user's point of view, not a
sequence of independently-reviewable actions. Practical effect: `repl_send`/
`repl_stop` are technically *offered* to the model in Plan mode (since
`plan_definitions()` filters on `mutates_outside_session()`), but since
`repl_start` can never have run in Plan mode, they'll always find no
running session and return the "no REPL session is running" error — offered
but functionally inert, same shape as `git_read` staying available while
`git_commit` doesn't.

### Process lifecycle

Spawned via `tokio::process::Command` with `Stdio::piped()` for stdin/
stdout/stderr, `.process_group(0)` (group-kill support, matching
`process.rs`'s existing `kill_process_group` — reused, not reimplemented),
and confined through `ctx.confiner.confine(command)` before `.spawn()` —
identical Landlock+seccomp treatment to `run_command`/`run_shell`.

Two background `tokio` tasks are spawned alongside the child (one per
stdout/stderr) that continuously drain into one shared, bounded, tail-capped
buffer — reusing `process.rs`'s existing ring-buffer eviction logic
(`drain_capped_tail`'s approach: keep the *last* N bytes, evicting oldest
first) rather than a new implementation. This is why polling works for the
dev-server use case: output keeps accumulating in the background even
between `repl_send` calls, not just during them.

`repl_send`'s read side: after optionally writing input, wait using a
quiet-window strategy — reset a "time since last new byte" timer every time
the buffer grows, return once that timer exceeds `quiet_window_ms` (default
300ms) with nothing new, or once `max_wait_secs` (default 10s) elapses
regardless, whichever comes first. Then drain-and-consume (not
peek-and-leave) whatever's accumulated since the last read, so output is
never reported twice across calls.

**Spontaneous exit**: if the child exits on its own (crash, explicit
`exit()`, an externally-killed dev server) between or during calls, the
background reader tasks observe EOF and/or `child.wait()` resolving. The
next `repl_send`/`repl_stop` call reports `process exited with status {N}`
(plus any final buffered output) and clears the shared running-session
state, so a subsequent `repl_start` works without requiring an explicit
`repl_stop` first.

**Idle timeout**: `idle_timeout_secs` (default 600 / 10 minutes) since the
last `repl_send` call — a safety net against a forgotten session lingering
indefinitely, not a limit on how long a legitimate dev-server session can
stay useful (as long as the model polls at least once per window, it never
fires).

**Shutdown**: any still-live REPL child is killed (via the same
process-group kill used for idle-timeout/explicit-stop) when the shared
session state is dropped — which happens naturally when `Agent`/`Tool`
instances are dropped at process exit, since the state lives behind the
same `Arc` ownership chain as the rest of the agent's shared state.
Implementation must confirm the TUI's shutdown path (Ctrl+C, normal quit)
actually drops these structures rather than calling `std::process::exit`
directly, which would skip destructors — flagged as a verification item
for the implementation plan, not resolved here.

**Not persisted across `--resume`**: `SessionState` doesn't attempt to
serialize a live process — a REPL session is process-lifetime-scoped, not
conversation-lifetime-scoped. After `--resume`, `repl_start` behaves as if
nothing had run before; no special-casing needed since the shared session
state is constructed fresh every process run regardless of whether history
was restored.

### Config surface

New optional `[repl]` config section, all fields defaulted so zero-config
works out of the box:

```toml
[repl]
quiet_window_ms = 300     # how long output must be silent before repl_send returns
max_wait_secs = 10        # hard per-call backstop, in case output never goes quiet
idle_timeout_secs = 600   # auto-kill a session with no repl_send activity for this long
```

### Tool descriptions / schema

- `repl_start(program: String, args: Vec<String>)` → returns whatever output
  appears during the initial quiet-window right after spawn (e.g. Python's
  startup banner), so the model sees the initial state without needing a
  separate empty `repl_send` just to observe it.
- `repl_send(input: Option<String>)` → returns accumulated output since the
  last read.
- `repl_stop()` → returns final buffered output plus a confirmation that
  the process was stopped.

## Out of scope for this spec

- Multiple concurrent/named sessions.
- Real PTY allocation (isatty()-sensitive programs may behave slightly
  differently than in a real terminal — documented as a Known Limitation,
  not solved here).
- Per-program prompt-pattern detection.
- Any REPL-specific behavior (this is a generic persistent-process
  primitive; nothing here knows about Python, Node, psql, or gdb
  specifically).

## Testing / verification

Unit tests (`crates/aivyx-tools/src/tools/repl.rs`) use a plain `sh`
one-liner as a deterministic fake REPL (e.g. a small `sh -c` read-loop that
echoes each line back with a fixed prefix) rather than relying on Python/
Node being installed in the test/CI environment:

- `repl_start` + `repl_send` with input returns the expected echoed output.
- `repl_send` with no input polls without writing to stdin (no new output
  when nothing is pending).
- Starting a second session while one is already running errors clearly and
  does not kill the running one.
- `repl_stop` kills the process; a subsequent `repl_send` reports no
  session running.
- A child that exits on its own is detected on the next call, reported with
  its exit status, and clears state so `repl_start` works again without an
  explicit `repl_stop`.
- Idle-timeout auto-kill (using a short configured `idle_timeout_secs` to
  keep the test fast).
- `permission_request()`: `repl_start` reports `ActionKind::Execute` with a
  `PermissionTarget::Command`; `repl_send`/`repl_stop` report
  `ActionKind::Interact`.
- `mutates_outside_session()`: `true` for `repl_start` (trait default,
  unchanged), `false` for `repl_send`/`repl_stop`.

Gate-level tests (`crates/aivyx-sandbox/src/confirmation.rs`):

- `Interact` auto-allows outside Plan mode and Autonomous mode.
- `Interact` is denied when Plan mode is active, even for a request that
  would otherwise auto-allow (the leak-through regression this design
  specifically guards against).
- `Interact` is denied in Autonomous mode (falls through to the existing
  generic "not pre-approved" denial).

Tool-list filtering tests (`crates/aivyx-tools/src/lib.rs`, extending the
existing `plan_definitions_offer_only_session_safe_tools` pattern):
`repl_start` excluded from Plan-mode tool definitions; `repl_send`/
`repl_stop` included. A parallel check for whatever mechanism hides
`run_shell`/`git_commit` from Autonomous mode's tool list today, extended
to also hide `repl_start`.

**Live E2E verification** (manual follow-up after implementation, matching
how every other security-relevant tool in this project has been verified —
see `docs/HISTORY.md`): drive a real `python3 -i` (or similar) on the
bare-metal rig through the actual release binary, confirming a real
back-and-forth exchange works, the process is properly cleaned up on
`repl_stop`, and — since this is the newest and most novel trust-tier
addition — that the Plan-mode leak-through guard actually holds live (start
a session, enter Plan mode mid-conversation, confirm `repl_send` is denied).

## Documentation

`README.md` gets a new row in the Tools table (`repl_start`/`repl_send`/
`repl_stop`) and a new bullet in "Known limitations": no PTY is allocated,
so a program that checks `isatty()` may behave differently than it would
in a real terminal (disabled line-editing, no color, or in the worst case
refusing to run non-interactively at all).
