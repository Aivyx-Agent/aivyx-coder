# Real PTY for `repl_start`/`repl_send` — Design

**Status:** Approved by user 2026-07-31.

## Context

`README.md`'s "Known limitations" documents this gap directly: `repl_start`/
`repl_send` (`crates/aivyx-tools/src/tools/repl.rs`) currently spawn the
target process with three plain OS pipes (`Stdio::piped()` for stdin,
stdout, stderr). A program that checks `isatty()` sees `false` and may
behave very differently than it would at a real terminal — disabled
line-editing/readline history, no color, or in the worst case an outright
refusal to run non-interactively at all.

This feature replaces the three pipes with a real pseudo-terminal (pty),
so an interactive program run via `repl_start` behaves as if a human were
sitting at a terminal: `isatty()` is true, readline/color work normally,
and the process can be resized live as `aivyx-coder`'s own terminal is
resized.

## Decisions

### PTY allocation: raw `libc` calls, opened in the parent, no new dependency

Consistent with this project's existing precedent (`process.rs`'s
`kill_process_group` already uses raw `libc::kill` rather than a wrapper
crate), pty allocation uses `libc::posix_openpt`, `libc::grantpt`,
`libc::unlockpt`, and `libc::ptsname_r` directly. No new crate dependency.

Both the **master** and **slave** file descriptors are opened in the
**parent** process (`aivyx-coder` itself), before `fork`:

1. `posix_openpt(O_RDWR | O_NOCTTY)` → master fd.
2. `grantpt(master)`, `unlockpt(master)`.
3. `ptsname_r(master)` → resolve the slave's path (e.g. `/dev/pts/7`).
4. Open the slave path directly (`libc::open`, `O_RDWR | O_NOCTTY`) — in
   the parent, which is unconfined, so this is a completely ordinary open.

The slave fd becomes the child's stdin, stdout, *and* stderr (a real
terminal is one merged stream — no separate stdout/stderr distinction).
The master fd stays in `aivyx-coder`, read continuously by one background
task (replacing today's two `spawn_output_reader` tasks, one per pipe).

**Why this needs zero Landlock changes**: because the slave fd is already
open before `fork`, the child never calls `open()` on any `/dev/pts/*`
path itself — it only inherits already-open file descriptors across
`exec`. Landlock governs new path-open syscalls; it has no opinion on
fds a process already holds. `ExecutionConfiner`'s Landlock ruleset (in
`aivyx-sandbox/src/confiner.rs`) needs no new grant for this feature at
all. Confirmed against the seccomp denylist too: `setsid` and `ioctl` are
not on it (`ptrace`, `process_vm_readv`/`writev`, `io_uring_*`, `mount`,
`umount2`, `reboot`, `kexec_*`, `*_module`, `pivot_root`, `swapon`/`off`,
`acct`, `bpf`, `perf_event_open`, `keyctl`/`add_key`/`request_key`,
`userfaultfd`, `unshare`, `setns`, `personality` — none of these are
touched by pty setup).

### Session-leader setup: a new `pre_exec` closure, composed with the confiner's existing one

The child must become its own session leader and adopt the slave as its
controlling terminal, via a second `pre_exec` closure calling
`libc::setsid()` then `ioctl(child_stdin_fd, TIOCSCTTY, 0)`.

**This is the one real implementation risk in this feature, not assumed
away**: `ExecutionConfiner::confine` (`aivyx-sandbox/src/confiner.rs`)
already calls `command.pre_exec(...)` once, for Landlock/seccomp setup.
Whether Rust's `std::os::unix::process::CommandExt::pre_exec` chains
multiple registered closures (each runs, in order) or the second call
silently replaces the first is **not assumed from memory** — it is
verified empirically as the implementation plan's first step (Task 1,
Step 1 below), before any other code depends on the answer.

- **If it chains**: `repl_start` calls `.pre_exec()` a second time,
  directly, after `ctx.confiner.confine(command)` returns.
- **If it does not chain**: `ExecutionConfiner` (the trait in
  `aivyx-sandbox`, both its `LandlockConfiner` and test-only
  `NoopConfiner` implementations) gains a new method,
  `confine_with_pty_setup(&self, command: Command, controlling_fd: RawFd)
  -> Command`, which folds the session-leader `pre_exec` work into the
  *same* closure as the Landlock/seccomp setup, run after it (ordering
  doesn't matter functionally — `TIOCSCTTY` isn't Landlock-governed —
  but running Landlock/seccomp restriction first keeps the "most
  restrictive state as early as possible" property the rest of this
  codebase follows). `repl_start` calls this new method instead of
  `confine` whenever pty mode is active.

The plan decides which of these two shapes to build based on the Task 1
verification result — both are specified here so the plan doesn't stall
on an unresolved question.

### Window size: initial size from `aivyx-coder`'s own terminal, live resize forwarding from the TUI

At `repl_start` time, the pty's window size is set via
`ioctl(master_fd, TIOCSWINSZ, ...)`:
- **TUI frontend**: sized to `crossterm::terminal::size()` at that
  moment.
- **ACP frontend** (no real terminal — an editor spawns `aivyx-coder`
  over stdio): falls back to a fixed 80×24 default.

**Live resize**: `crossterm::event::EventStream` (already polled inside
`aivyx-tui/src/app.rs`'s main `tokio::select!` loop via
`crossterm_events`) already emits `CtEvent::Resize(cols, rows)` on every
terminal resize — today this event type falls through unmatched (only
`CtEvent::Key` is matched; ratatui just redraws on any event, resized or
not). This feature adds a new match arm: on `CtEvent::Resize`, if a REPL
session is currently running, forward the new size to its pty master via
another `ioctl(TIOCSWINSZ)` call.

To reach the session from `app.rs`, `SharedReplSession` must be exposed
outward. Today, `crates/aivyx/src/agent_builder.rs:233` constructs it
locally (`new_shared_repl_session()`) and moves the only handle straight
into `ReplStopTool::new(repl_session)` at tool-registration time — no
surviving reference escapes the function. This feature adds a
`repl_session: SharedReplSession` field to `BuiltAgent`
(`agent_builder.rs:43-51`), populated from a clone taken *before* that
final move, and threads it through to `app::run` in
`aivyx-tui/src/app.rs`. The ACP frontend's session setup ignores this
field entirely — there's no live terminal there to resize from, so an
ACP-hosted REPL session just keeps its fixed initial size for its
lifetime.

### Echo behavior: natural pty defaults, not suppressed

A real pty's line discipline defaults to cooked mode with local echo:
bytes written to the master (via `repl_send`'s input) are echoed back
through the master before the child program's own output appears. This
is **left as the natural default, not suppressed** — matching a human
typing at a real terminal is the entire point of this feature, and many
interactive programs (readline-based REPLs, `vim`, etc.) additionally
set raw mode themselves once they start, at which point the kernel-level
echo stops applying and the *program's own* echo (if any) takes over.
`repl_send`'s tool description and `README.md`'s known-limitations entry
are both updated to say plainly that echoed input may appear in output,
so this isn't mistaken for a bug by whoever reads either.

### Output plumbing: one merged reader replaces two

`ReplSession`'s `output: Arc<std::sync::Mutex<VecDeque<u8>>>` buffer,
`spawn_output_reader`, `wait_for_quiet`, and `drain_output`
(`crates/aivyx-tools/src/tools/repl.rs:37-140`) are unchanged in
signature and behavior — only the *source* changes, from two
`spawn_output_reader` calls (one on the stdout pipe, one on stderr) down
to one, reading the pty master fd. Everything downstream (idle-timeout
detection, exit-status reporting, the quiet-window polling loop) is
untouched: it already operates purely on the shared buffer, agnostic to
where bytes came from.

## Out of scope for this spec

- Any change to `repl_start`'s existing single-session-at-a-time limit.
- Windows or non-Linux support — this project is Linux-only already
  (Landlock is Linux-specific).
- ACP-side live resize — there is no real terminal on that frontend to
  resize *from*; an ACP-hosted session keeps its fixed initial size.
- Any change to `ExecutionConfiner`'s Landlock ruleset construction
  itself (`build_ruleset`) — this feature needs no new filesystem grant,
  as established above.

## Testing / verification

All new tests follow this file's existing style: real spawned child
processes via the test-only `NoopConfiner`, not mocks.

1. **The `pre_exec`-chaining question, first, before anything else
   depends on the answer**: spawn a trivial child with two `.pre_exec()`
   closures registered on the same `Command`, each writing a distinct
   byte to a shared pipe or file, and confirm whether both writes land.
   This single test result decides which of the two shapes in "Decisions"
   above the rest of the plan builds.
2. A test program that hard-refuses to run without a tty (a small
   Python one-liner checking `os.isatty(0)`) now runs and reports
   `True`, where it previously would have reported `False` under the
   old piped-stdio design.
3. `TIOCSWINSZ`: after `repl_start`, an explicit resize call sets a new
   size, and a query (`stty size`, or reading columns/lines via ioctl)
   reflects it.
4. Echo: `repl_send`'s returned output contains the echoed input line
   ahead of the program's real response — asserted as expected shape,
   not filtered out.
5. The full existing `repl_start`/`repl_send`/`repl_stop` test suite
   (idle timeout, exit-status reporting on natural exit, broken-pipe/
   already-exited handling) is adapted from two-pipe to one-pty
   plumbing; expected *outcomes* are unchanged, only how the test sets
   up the child changes.

**Live E2E follow-up** (this project's standing practice for every
shipped phase): drive `aivyx-coder`'s real TUI binary through the
existing pyte-based PTY harness (see
`feedback_live_e2e_grading` project memory for the established pitfalls
to avoid), issue a `repl_start` for `python3 -i`, confirm the interpreter
banner and `>>>` prompt render correctly (readline now active), and
confirm a terminal resize is visibly reflected in the child's own
`stty size` output queried via `repl_send`.

## Documentation

`README.md`'s "Known limitations" bullet ("`repl_start`/`repl_send` use
plain pipes, not a real pseudo-terminal (PTY)...") is rewritten to
describe the new pty behavior, including the echo caveat. `ROADMAP.md`
gets a "shipped" entry in Current status. `docs/HISTORY.md` gets a
narrative chapter, matching this session's established depth and style
for a feature of this size (comparable to the Landlock + `aivyx-repomap`
basename-glob enforcement chapter, the largest so far).
