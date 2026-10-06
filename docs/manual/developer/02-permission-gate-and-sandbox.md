# Permission gate and sandbox

The user-facing account is the [security model](../reference/05-security-model.md).
This page is what you need to know before changing the code behind it.

## The gate (`aivyx-sandbox`, `ConfirmationGate::check`)

Tiers, in order — the first that decides, wins:

1. **Deny list** — `deny_paths` (merged with the built-in defaults, never
   replaced) blocks any path-target at or under an entry; separator-free
   entries are basename globs.
2. **Git metadata** — a `Write`/`Delete`/`Move` touching a `.git` path
   component or the global git config is refused (`touches_git_metadata`,
   `touches_global_git_config`).
3. **Auto-allow** — `Read` and `Internal` (session-only state).
4. **Plan mode** — refuses anything else. It sits *before* the cache so an
   approval granted earlier can't leak into plan mode.
5. **Autonomous mode** — its own fixed trust profile (edits inside the
   working directory, pre-approved commands; MCP, memory and preferences
   always refused).
6. **`Network` and `Interact`** — auto-allowed after the mode checks above.
7. **Always-Allow cache** — keyed on the *exact* target: a full path, or a
   full `(program, args)`. Seeded at start-up from `allowed_commands`.
   `Memory` (`remember_preference`) never uses the cache;
   targets that run code later (`runs_code_later`) never offer it.
8. **Ask** — the front end's prompter. It must fail closed: a prompter that
   can't get an answer returns Deny.

`ActionKind`s: `Read`, `Write`, `Execute`, `Delete`, `Move`, `Internal`,
`Network`, `Interact`, `McpTool`, `Memory`, `PersistentMemory`. Pick the
honest one for a new tool — `Internal` is only for state that never leaves
the session. Denials carry a reason the model can read.

### Defaults that fail closed

- `Tool::mutates_outside_session()` defaults to `true`: a new tool is hidden
  in plan mode and checkpointed unless it opts out.
- `needs_checkpoint()` defaults to `mutates_outside_session()`; only the
  network tools differ (they're not session-local, but can't change the
  worktree).
- `git_commit`'s target is the full git command, so each distinct message
  is a distinct cache key — Always Allow can't bless future commits.

## Checkpoints

On Allow, any tool with `needs_checkpoint()` snapshots the worktree to
`refs/aivyx/checkpoints/<ts>` first (`aivyx-checkpoint`, plumbing only —
HEAD, index and files untouched; newest 50 kept). A failed call later in
the same model response rolls back the earlier successful ones from that
response. `/undo` brackets each user message with two snapshots.

## The confiner (`aivyx-confine`)

Linux-only, behind the default-on `sandbox-backend` feature;
`--no-default-features` builds (the macOS release) use `NoopConfiner`.

- **Landlock** (ABI 7): write — the working directory and a private
  `TMPDIR`; read — the working directory, system and toolchain paths,
  `$HOME`'s `.cargo`, `.rustup`, git config, and `extra_read_paths`.
  Denied paths inside a granted root are carved out by granting around
  them (Landlock has no deny rule).
- **seccomp-bpf** denylist (`EPERM`): `ptrace`, `process_vm_*`, `io_uring_*`,
  `mount`, `bpf`, `perf_event_open`, keyring calls, `unshare`/`setns`,
  `clone(CLONE_NEW*)`, module loading and more; `socket(AF_UNIX)` unless
  `allow_unix_sockets` (and the session IPC variables are scrubbed);
  `setsid`/`setpgid` unless `allow_leaving_process_group`.
- **Process groups** — every confined spawn leads its own group, owned by an
  `aivyx_tools::ProcessGroup` that kills it on finish, timeout, cancel or
  drop. Use `aivyx_tools::run` / `output_in_group`, never a bare
  `.output()` on a confined command.
- **`require_enforcement`** is checked twice, fail-closed when true: the
  parent failing to build the ruleset, and the child seeing
  `RulesetStatus::NotEnforced`. `PartiallyEnforced` (an older kernel ABI) is
  accepted. The `pre_exec` closure is allocation-free — it runs between
  fork and exec.

`agent_builder::confine_options` maps `[sandbox]` onto
`aivyx_sandbox::build_confiner`, the one constructor. The confiner's own
source and detailed documentation live in the `aivyx-confine` repository.

## Prompt-injection scan

Tool results are scanned (`scan_for_injection_markers`, from
`aivyx-injection-guard`) as they re-enter context. Interactive front ends
show a notice; autonomous and MCP-server sessions record an
`InjectionTaint` that pauses the run and makes the gate refuse further
mutating, network and command calls. It's a phrase list, not a structural
defence.
