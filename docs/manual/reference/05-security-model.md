# Security model

aivyx-coder assumes the model may be wrong — or deliberately manipulated by
something it read. Protection is layered, and no single layer is the whole
story:

1. **`deny_paths`** — files and folders that are never accessible.
2. **The confirmation gate** — every tool call is checked, and anything
   with an effect asks you.
3. **Landlock + seccomp** — on Linux, every program the model runs is
   confined by the kernel.

Plus checkpoints, so a turn can be taken back (`/undo`), and a
prompt-injection scan on everything that re-enters the model's context. What
this does **not** protect against is in
[Troubleshooting → Known limitations](../guide/15-troubleshooting.md#known-limitations).

## The default deny list

Built in, and always merged with any `deny_paths` you add:

`~/.ssh`, `~/.aws`, `~/.config/aivyx-coder`, `~/.local/state/aivyx-coder`,
`~/.gnupg`, `~/.netrc`, `~/.docker/config.json`, `~/.kube/config`,
`~/.npmrc`, `~/.pypirc`, `~/.config/gcloud`, `~/.azure`,
`~/.cargo/credentials.toml`, `~/.config/gh`, and anywhere in a project:
`.env`, `.env.*`, `id_rsa`, `id_ed25519`, `*.pem`, `*.key`.

## 1. `deny_paths` — a hard block

`permissions.deny_paths` (default `~/.ssh`, `~/.aws`, plus several more —
see the [configuration reference](03-configuration.md)) are paths that are never accessible,
checked before any prompt or cache. An entry containing a `/` (or a bare
`~`) is `~`-expanded and **symlink-canonicalized**, and the check matches
any path *at or under* that denied entry — unchanged from before. An
entry with **no path separator** (e.g. `.env`, `*.pem`) is instead a
**basename-glob pattern**: it matches any file with that name anywhere,
not just one fixed absolute location — useful for a project-local secret
file that recurs across every project directory the agent might be
pointed at, which a fixed absolute path can't express.

A `deny_paths` list in `config.toml` is **merged** with the built-in
defaults, never a replacement for them: the deserializer unions your
entries with the current build's defaults (de-duplicated by exact string
match) before anything else sees the list. This is deliberate — silently
losing a security-critical default (e.g. `~/.local/state/aivyx-coder`,
which stops the model from planting a fake memory-topic file or
self-approving its own pending permission gate) just because you set your
own `deny_paths` for a project-local secret would be a much worse failure
mode than "you can't remove a default via config." There is currently no
way to *remove* a built-in default through config. This covers:

- **File tools** (`read_file`/`write_file`/`edit_file`): the resolved target
  is checked directly.
- **Search tools** (`grep`/`glob`): every walked entry is checked, so a search
  rooted *above* a denied directory still can't descend into it.
- **Command tools** (`run_command`/`run_shell`): for a path-separator entry,
  `deny_paths` is enforced at the **kernel** level on the Linux binary — the
  Landlock sandbox (below) simply never grants access to a denied path, so a
  shell command physically cannot read or write it regardless of how the
  command is phrased (redirection, env vars, etc.). On the macOS binary there
  is no Landlock, so this reduces to the same gate-only enforcement as reads
  below — see [Install and first run](../guide/02-install-and-first-run.md#platform-support). Basename-glob entries (no separator, e.g.
  `.env`, `*.pem`) are enforced too: `LandlockConfiner` resolves them to
  concrete file paths on **every** spawn by scanning the working directory
  and any configured `extra_read_paths` (the roots a project's own secrets
  could plausibly live under), then excludes those resolved paths — and any
  hard-link alias of them under those roots — from *every* grant, including
  the fixed system paths (`/usr`, `/lib`, etc.), in case the working
  directory happens to be nested inside one of them. A denied file created
  after startup is therefore denied to the next command too. What's *not*
  scanned is the system paths' own contents for unrelated matches
  (recursively walking `/usr` for a project-local pattern would be
  substantial, pointless work) — only files nested under the working
  directory or `extra_read_paths` are ever discovered.
  `~/.cargo/credentials(.toml)`, `~/.config/git/credentials` and
  `~/.git-credentials` are always
  unreadable to commands, even with no `deny_paths` configured.

## 2. `ConfirmationGate` — human-in-the-loop, tiered trust

Every tool call passes through the gate before it runs:

1. **Denied** (`deny_paths`) → blocked, no prompt.
2. **`.git` write block**: any `Write`/`Delete`/`Move` whose target has a
   `.git` path component (a repo's own metadata directory, at any depth —
   `touches_git_metadata`), or targets the global `~/.gitconfig`/
   `~/.config/git` (`touches_global_git_config`), is hard-denied here too —
   same tier as `deny_paths`, before any prompt, in every mode. A file
   written there (a hooks script, `core.fsmonitor`/`filter.<x>.clean` in
   `.git/config`, or a tracked `.gitattributes` line) makes a *later*,
   unconfined git invocation — the pre-write checkpoint's `git add -A`,
   `git_read`, `git_commit` — run an attacker-chosen program, turning "may
   edit files" into "may run any program." Deliberately **not** folded into
   `deny_paths` itself for the global-config case: that list is also
   consumed by the Landlock read-grant builder below, which grants exactly
   `~/.gitconfig`/`~/.config/git` so a *confined* `git` child process can
   still read the user's own identity (an existing-but-unreadable git
   config is fatal to git) — a `deny_paths` entry there would strip that
   grant instead of just blocking the model's own write. Read actions are
   unaffected (`read_file .git/HEAD` stays allowed). `git_read`'s own
   invocations additionally prefix every argv with `-c core.fsmonitor=false`
   (plus `--no-ext-diff --no-textconv` for `diff`, `log.showSignature=false`
   and `--no-show-signature` for `log`), and `git_commit`'s `add`/`commit`
   calls get the same `-c core.fsmonitor=false` prefix, as defense in depth
   against a config entry that predates this block or was written by the
   user themselves — the user's own hooks still run intentionally either way.
   The user-typed `/diff` and `/commit` commands also run some git
   unconfined (reading diffs; staging and unstaging), as do the pre-write
   checkpoint, `/undo`'s restore, and the `git_commit`/`git_push`/
   `git_branch` approval previews. The gate's block stops *tools* writing
   `.git`, but a confined, approved command may still write anything under
   the project — `.git/config`, `.git/hooks` and `.gitattributes`
   included. So every unconfined git call switches off fsmonitor, hooks
   (`core.hooksPath=/dev/null` — plumbing like `update-ref` would otherwise
   run `reference-transaction`) and commit signing, and turns off every
   filter driver defined in the repository's *own* config (filters from
   your global config, such as git-lfs, still apply); every unconfined diff
   adds `--no-ext-diff --no-textconv`. The commit itself runs confined, like
   `git_commit`, so your own hooks still run there.
3. **Reads** (`read_file`/`grep`/`glob`) → auto-allowed, no prompt. A read has
   no side effect on its own, so prompting on every read would make the tool
   unusable. This auto-allow is **not** scoped to the project's working
   directory — any path the model asks to read is served unless it falls
   under a `deny_paths` entry, since there is no Landlock-style kernel
   enforcement on in-process file reads (Landlock only confines spawned
   child processes, see "Landlock + seccomp" below). `deny_paths` is
   therefore the *only* boundary on what the model can read; keep it
   current for anything sensitive outside the default list. (But note: read
   output re-enters the model's context — see [Known limitations](../guide/15-troubleshooting.md#known-limitations).)
   `Internal` actions (`set_tasks`, which mutates only
   the agent's own session state, and `load_skill`, which only reads from the
   skill library and returns its content) are auto-allowed on the same basis,
   but are a distinct action kind so audit logs never record a state change as
   a "read" — and so a tool that touches the outside world can't honestly
   describe itself as internal.
4. **Plan mode** (when active) → every remaining action is denied outright,
   with a reason the model can read. This check deliberately sits *before*
   the Always-Allow cache and the pre-approved command tier below: an
   approval you granted before entering plan mode cannot execute during it.
   Only the user can toggle the mode (Ctrl+P / `--plan`) — the model has no
   way to exit it. Belt-and-braces: in plan mode the mutating tools aren't
   even offered to the model in the request, so this gate tier is the
   backstop for a hallucinated call, not the primary UX.
5. **Everything else** → an interactive confirmation modal, showing the exact
   target/command and (for edits) a diff. Choosing **Always Allow** caches that
   *exact* target `(program, args)` or path for the rest of the session —
   except for a target flagged by `runs_code_later` (below), where Always
   Allow isn't offered at all.
6. **Pre-approved commands**: entries in `permissions.allowed_commands` are
   seeded into the Always-Allow cache at startup, so a command you already
   trusted by writing it into config runs without a prompt. This is the
   command-level allowlist tier. Under `--auto` with no `[verification]
   command` configured, a detected test command is seeded the same way, as
   a synthetic `detected-tests` entry (see [Autonomous and advanced modes](../guide/10-autonomous-and-advanced-modes.md)) — this
   lets the model run that one command via `run_command` too, but grants no
   new capability, since auto-verify already runs it unconditionally after
   every batch of edits.

**Files that run code later** (`runs_code_later`, audit finding M1,
2026-10-02): a shell startup file (`~/.bashrc`, `~/.zshrc`, `~/.profile`,
`~/.zshenv`, `~/.bash_profile`/`.bash_login`/`.zprofile`/`.zlogin`,
`~/.config/fish/config.fish` or anything under `~/.config/fish/conf.d/`),
an XDG autostart entry (`~/.config/autostart/`), or a systemd user unit
(`~/.config/systemd/user/`) runs outside Landlock's confinement scope
every time it's triggered — a new shell, a login, a service start — not
just within this session, and Landlock only ever scopes *this* process's
spawned children. These aren't blocked (editing your own dotfiles is a
legitimate, common request), but tier 5's confirmation modal shows an
extra warning line ("⚠ This file runs every time you open a shell —
approving lets it run code outside the sandbox.") and doesn't offer
**Always Allow** at all for such a target, in both the TUI and ACP's
`session/request_permission` — one approval must not silently bless every
future edit to a file with this kind of reach.

Denials carry their reason through to the model (`plan mode is active…`,
`target is under a configured deny_paths entry…`, `the user denied this
action`), so it can adapt instead of blindly retrying.

All gate decisions (allow / deny / always-allow) are logged to `aivyx.log` in
the config directory (`~/.config/aivyx-coder/aivyx.log` on Linux), so a session's permission history is auditable after the
fact.

## 3. Landlock + seccomp — kernel-enforced process confinement

This tier is Linux-only — see [Install and first run](../guide/02-install-and-first-run.md#platform-support). On the macOS
binary, process execution stops after tier 2 (`ConfirmationGate`); no
OS-level confinement applies.

Confinement isn't limited to `run_command`/`run_shell` — it applies to every
tool that spawns a child process: `git_commit`/`git_push`/`git_branch`/
`git_pr`/`git_read`'s `git` invocations (and `gh`), `/test`, `/commit`,
`repl_start`, `find_references`/`go_to_definition`'s `rust-analyzer` spawn,
and MCP servers at startup. Each such child process is confined via Linux
**Landlock** (filesystem scoping) and a **seccomp-bpf** syscall denylist,
applied in the forked child before `exec`:

- **Write** access: the working directory + a **private temp directory**
  (exported to commands as `TMPDIR`, one per confiner: the lead agent's
  lives until aivyx-coder exits normally, each specialist session and each
  `delegate_to_specialist` call gets its own, deleted when that session or
  delegation ends — so a specialist's commands see a different `$TMPDIR` than yours), minus any `deny_paths` nested inside them (carved out
  precisely, since Landlock has no "deny" rule — the working directory is
  granted child-by-child around a denied subpath rather than wholesale). The
  shared `/tmp` is **not** writable, so a command can't read or tamper with
  other programs' temp files; tools that honour `TMPDIR` (`mktemp`, rustc,
  Python's `tempfile`, ...) just work. `[sandbox] share_system_tmp = true`
  restores the old shared-`/tmp` grant for tools that hard-code `/tmp`.
- **Read** access: the working directory + a bounded list of common
  system/toolchain paths (`/usr`, `/lib`, `/bin`, `/etc`, `~/.cargo`,
  `~/.rustup`) + any `sandbox.extra_read_paths` you configure — again minus
  `deny_paths`. Deliberately *not* "read everything," so credentials outside
  the granted set are unreadable by a shell command even after you approve it.
- **Blocked syscalls**: `ptrace`, `process_vm_readv/writev`, `io_uring_*`,
  `mount`/`umount2`, `reboot`, `kexec_*`, module loading, `pivot_root`,
  `swapon/off`, `bpf`, `perf_event_open`, the keyring calls (`keyctl`/`add_key`/
  `request_key`), `userfaultfd`, `unshare`/`setns`, `clone` with any
  `CLONE_NEW*` flag (so no rootless containers, `bwrap` or Chromium's
  sandbox), `personality`, and a few more — confinement-escape and
  privilege-escalation primitives a coding agent's commands never
  legitimately need.
- **No local daemons**: `socket(AF_UNIX)` fails with `EPERM`, and
  `SSH_AUTH_SOCK`, `GPG_AGENT_INFO`, `DBUS_SESSION_BUS_ADDRESS`,
  `XDG_RUNTIME_DIR`, `WAYLAND_DISPLAY` and `DISPLAY` are removed from the
  command's environment. Landlock doesn't gate `connect()` to an existing
  Unix socket, so without this a command could ask the D-Bus session bus
  (`systemd-run --user ...`) to run anything **outside** the sandbox. The
  cost: ssh-agent (`git push` over ssh with an agent), gpg/ssh commit
  signing, `docker`, `psql`/`mysql` over their default sockets, git
  credential-cache/libsecret helpers and `systemctl --user` don't work in
  confined commands. MCP servers are confined the same way: their stdio
  transport is a pair of pipes and needs no Unix socket, so a stdio MCP
  server works unchanged, but one that itself talks to a local daemon
  (docker, a database socket) needs the opt-out. `[sandbox]
  allow_unix_sockets = true` lifts the block for every confined command
  and hands those variables back — at the price of the sandbox no longer
  containing code execution.
- **Process groups**: every confined command leads its own process group,
  `setsid`/`setpgid` fail with `EPERM` (unless `[sandbox]
  allow_leaving_process_group = true`), and the whole group is killed with
  `SIGKILL` when the call **finishes**, times out or is cancelled — so a
  `cargo run &` or a daemonising grandchild never outlives the tool call,
  and a background job holding the output pipe doesn't stall it either.
  Commands also can't signal processes outside their own sandbox (Landlock
  ABI 6+), including aivyx-coder and servers an earlier command started.

`sandbox.require_enforcement` (default **true** on Linux, **false** on every
other platform — non-Linux builds have no Landlock/seccomp backend compiled
in to enforce with in the first place): if real Landlock confinement
can't actually be established at all — the kernel lacks Landlock or it's
disabled — command tools **refuse to run** rather than silently executing
unconfined. A kernel that only *partially* enforces the requested ruleset
(Landlock's own designed graceful degradation when it doesn't support every
requested restriction at the running `LANDLOCK_ABI` level) is not treated as
a failure here — it still gets real, meaningful restriction, just not
literally every requested one, so the command runs confined rather than being
refused. Set `require_enforcement` to `false` to allow unconfined execution
when even partial enforcement isn't available (you'll get a warning logged
either way).

Process execution also: races each command against a timeout (default 300s)
and Ctrl+C cancellation, killing the whole **process group** on either and
when the command exits (so backgrounded grandchildren don't survive); bounds
captured output to the last 50 KiB per
stream during collection (so a runaway `yes`-style command can't exhaust
memory); and reports a non-zero exit as normal output, not a tool failure.

