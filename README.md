# aivyx-coder

A terminal (TUI) coding agent for **local** LLMs only — Ollama, vLLM, or
llama.cpp over their OpenAI-compatible `/chat/completions` endpoints. It never
talks to a cloud API. An LLM drives tool calls (read/write/edit files, search,
run commands) against your real filesystem, gated by a layered permission and
sandboxing model designed so that a mistaken — or actively manipulated — model
cannot quietly do damage.

This is a from-scratch Rust project built reliability-first: the security
boundary was designed before the tools that need it, and hardened through
repeated full-codebase audits (see `ROADMAP.md` for the phase history).

## Building and running

```
cargo run -p aivyx
```

Requires a local inference server. On first run a config file is written to
your XDG config directory (`~/.config/aivyx-coder/config.toml`) with defaults
pointing at Ollama (`http://localhost:11434/v1`); edit it to taste.

Sessions persist automatically: after every completed turn the conversation
history and task list are saved (one session per project directory, keyed by
the canonicalized cwd, under `~/.local/state/aivyx-coder/sessions/`, written
`0600` since they embed file contents and command output read during the
session). `aivyx --resume` restores the previous session for the current
directory — transcript, task list, and all; without the flag a fresh session
starts and its first completed turn replaces the stored one.

The status line shows a live context-budget indicator (`ctx 6.1k/8.2k (74%)`,
colored green/amber/red) once the backend reports token usage. When the
conversation approaches the configured window (`backend.context_tokens`,
below), the agent compacts: oversized tool results are elided to head+tail
excerpts first, then the oldest turns are dropped — with a visible notice,
never silently.

**Worktree checkpoints**: when the working directory is a git repository,
the agent snapshots the entire worktree to `refs/aivyx/checkpoints/<ts>`
*before every mutating tool call* (file writes, edits, and any
`run_command`/`run_shell` execution) — via plumbing that never touches your
HEAD, index, or worktree, respecting `.gitignore`, deduplicating identical
states, and keeping the newest 50. To inspect or rewind:

```
git for-each-ref refs/aivyx/checkpoints/          # list checkpoints
git log --oneline <ref>                           # see one in context
git diff <ref>                                    # what changed since it
git checkout <ref> -- <path>                      # restore one file
git checkout <ref> -- .                           # restore everything
```

Disable with `[git] checkpoints = false`. Checkpoints use a synthetic
`aivyx` author identity and never appear in your branch history — deleting
a ref is enough to let its objects age out via normal `git gc`.

**Plan mode** (`Ctrl+P` in the TUI, or start with `aivyx --plan`) makes the
agent read-only while you scope out work: it can read, search, and build a
task list (the task panel becomes the reviewable plan), but tools that touch
files or run commands are withheld from the model entirely — and the
permission gate independently denies them even if the model invents a call.
Press `Ctrl+P` again to approve the plan and switch back to Act mode; a
magenta `PLAN` badge in the status line shows the current stance. See
"Security model" for why this is an enforced boundary, not a suggestion.

Build/test the workspace:

```
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets
```

The real OS-level sandbox (Linux Landlock + seccomp) is on by default. To
build without it (non-Linux, or a kernel without Landlock), use
`--no-default-features` on `aivyx-sandbox` — the agent then falls back to no
process confinement (see "require_enforcement" below for how that interacts
with running commands).

## Tools

| Tool | Action | Confirmation |
|---|---|---|
| `read_file` | read a file | none (auto-allowed) |
| `grep` | content search under a directory | none (auto-allowed) |
| `glob` | path search under a directory | none (auto-allowed) |
| `write_file` | create/overwrite a file | prompt (then cacheable) |
| `edit_file` | exact-substring replace in a file | prompt (then cacheable) |
| `run_command` | run one of a fixed, configured allowlist by name | prompt / pre-approved |
| `run_shell` | run an arbitrary `sh -c` command | prompt / pre-approved |
| `set_tasks` | replace the agent's own task list (shown in the TUI) | none (internal state only) |
| `git_read` | git status / diff / log (read-only, fixed argv shapes) | none (auto-allowed) |
| `git_commit` | stage + commit, with your identity/hooks/config | prompt, with a change-summary preview |

`run_command` and `run_shell` are only useful once you configure them (see
`allowed_commands` below); `run_shell` is always registered but every command
still needs approval unless it exactly matches a pre-approved entry.

## Security model

The agent assumes the LLM may be wrong or manipulated. Protection is layered —
no single layer is the whole story.

### 1. `deny_paths` — a hard block

`permissions.deny_paths` (default `~/.ssh`, `~/.aws`) are paths that are never
accessible, checked before any prompt or cache. Entries are `~`-expanded and
**symlink-canonicalized**, and the check matches any path *at or under* a
denied entry. This covers:

- **File tools** (`read_file`/`write_file`/`edit_file`): the resolved target
  is checked directly.
- **Search tools** (`grep`/`glob`): every walked entry is checked, so a search
  rooted *above* a denied directory still can't descend into it.
- **Command tools** (`run_command`/`run_shell`): the shell can reference any
  path, so `deny_paths` here is enforced at the **kernel** level — the Landlock
  sandbox (below) simply never grants access to a denied path, so a shell
  command physically cannot read or write it regardless of how the command is
  phrased (redirection, env vars, etc.).

### 2. `ConfirmationGate` — human-in-the-loop, tiered trust

Every tool call passes through the gate before it runs:

1. **Denied** (`deny_paths`) → blocked, no prompt.
2. **Reads** (`read_file`/`grep`/`glob`) → auto-allowed, no prompt. A read has
   no side effect on its own, so prompting on every read would make the tool
   unusable. (But note: read output re-enters the model's context — see
   "Known limitations".) `Internal` actions (`set_tasks`, which mutates only
   the agent's own session state) are auto-allowed on the same basis, but are
   a distinct action kind so audit logs never record a state change as a
   "read" — and so a tool that touches the outside world can't honestly
   describe itself as internal.
3. **Plan mode** (when active) → every remaining action is denied outright,
   with a reason the model can read. This check deliberately sits *before*
   the Always-Allow cache and the pre-approved command tier below: an
   approval you granted before entering plan mode cannot execute during it.
   Only the user can toggle the mode (Ctrl+P / `--plan`) — the model has no
   way to exit it. Belt-and-braces: in plan mode the mutating tools aren't
   even offered to the model in the request, so this gate tier is the
   backstop for a hallucinated call, not the primary UX.
4. **Everything else** → an interactive confirmation modal, showing the exact
   target/command and (for edits) a diff. Choosing **Always Allow** caches that
   *exact* target `(program, args)` or path for the rest of the session.
5. **Pre-approved commands**: entries in `permissions.allowed_commands` are
   seeded into the Always-Allow cache at startup, so a command you already
   trusted by writing it into config runs without a prompt. This is the
   command-level allowlist tier.

Denials carry their reason through to the model (`plan mode is active…`,
`target is under a configured deny_paths entry…`, `the user denied this
action`), so it can adapt instead of blindly retrying.

All gate decisions (allow / deny / always-allow) are logged to `aivyx.log` in
the config directory, so a session's permission history is auditable after the
fact.

### 3. Landlock + seccomp — kernel-enforced process confinement

When a command runs (`run_command`/`run_shell`), the child process is confined
via Linux **Landlock** (filesystem scoping) and a **seccomp-bpf** syscall
denylist, applied in the forked child before `exec`:

- **Write** access: the working directory + the system temp dir(s), minus any
  `deny_paths` nested inside them (carved out precisely, since Landlock has no
  "deny" rule — the working directory is granted child-by-child around a denied
  subpath rather than wholesale).
- **Read** access: the working directory + a bounded list of common
  system/toolchain paths (`/usr`, `/lib`, `/bin`, `/etc`, `~/.cargo`,
  `~/.rustup`) + any `sandbox.extra_read_paths` you configure — again minus
  `deny_paths`. Deliberately *not* "read everything," so credentials outside
  the granted set are unreadable by a shell command even after you approve it.
- **Blocked syscalls**: `ptrace`, `process_vm_readv/writev`, `io_uring_*`,
  `mount`/`umount2`, `reboot`, `kexec_*`, module loading, `pivot_root`,
  `swapon/off`, `bpf`, `perf_event_open`, the keyring calls (`keyctl`/`add_key`/
  `request_key`), `userfaultfd`, `unshare`/`setns`, `personality`, and a few
  more — confinement-escape and privilege-escalation primitives a coding
  agent's commands never legitimately need.

`sandbox.require_enforcement` (default **true**): if real Landlock confinement
can't actually be established — the kernel lacks Landlock, it's disabled, or it
only partially enforces the ruleset — command tools **refuse to run** rather
than silently executing unconfined. Set it to `false` to allow unconfined
execution on such systems (you'll get a warning logged either way).

Process execution also: races each command against a timeout (default 300s)
and Ctrl+C cancellation, killing the whole **process group** (so backgrounded
grandchildren don't survive); bounds captured output to the last 50 KiB per
stream during collection (so a runaway `yes`-style command can't exhaust
memory); and reports a non-zero exit as normal output, not a tool failure.

## Configuration reference

`~/.config/aivyx-coder/config.toml` (written with `0600` permissions, since it
may hold an `api_key`):

```toml
[backend]
base_url = "http://localhost:11434/v1"
model = "qwen3.5:9b"
# api_key = "..."   # only for auth-protected local endpoints
# Your model's context window, in tokens. Drives the status-line budget
# indicator and history compaction. Conservative default (8192) — set it to
# what your model actually supports; it is not auto-detected because the
# OpenAI-compatible /v1 surface doesn't expose it reliably.
context_tokens = 8192

[permissions]
deny_paths = ["~/.ssh", "~/.aws"]
max_tool_iterations_per_turn = 25

# Commands the model may run by name (run_command), or that skip the
# confirmation prompt (run_shell). Empty by default — nothing runnable until
# you opt in.
# [[permissions.allowed_commands]]
# name = "test"
# program = "cargo"
# args = ["test"]
# timeout_secs = 600   # optional; defaults to 300

[sandbox]
require_enforcement = true
extra_read_paths = []   # extra paths shell commands may read, e.g. a venv

[git]
checkpoints = true   # snapshot the worktree before every mutating tool call
```

## Known limitations

Deliberately not (yet) addressed — documented rather than hidden:

- **Indirect prompt injection**: content the agent reads (files, command
  output, search results) re-enters the model's context with no
  trust/provenance tag — the model cannot structurally distinguish "the user
  said X" from "a file said X." The system prompt instructs the model to treat
  such content as data, not instructions, but this is a mitigation, not a
  guarantee. Be especially careful pointing the agent at untrusted repositories
  while `allowed_commands` is configured, since a pre-approved command runs
  without a per-invocation prompt.
- **Network is not restricted** by the sandbox. A command you approve can make
  network connections (needed for `cargo build`, `npm install`, `git clone`,
  etc.). Combined with the read scope, an approved command could in principle
  exfiltrate anything in that scope.
- **Environment variables are inherited** by spawned commands (needed by real
  toolchains). A command like `env` will see whatever secrets are in your
  shell environment. This is standard for shell tools but worth knowing.
- **TOCTOU windows**: `write_file`/`edit_file`/`read_file` resolve their path
  once for the confirmation preview and again at execution; a symlink swapped
  in between could redirect the operation. `deny_paths` and the Landlock scope
  still bound the blast radius.
- **`AIVYX_DEBUG_LOG`**: if you set this env var to capture raw wire traffic
  for debugging, it logs the full conversation (including file/command content)
  in plaintext, append-only, forever. The file is `0600` but has no rotation or
  expiry — treat it as sensitive and delete it when done.
- **`Tool::execute` bypass**: the "all tool calls go through the permission
  gate" property is enforced by convention (the executor is the only caller),
  not by the type system.
- **Git specifics**: `git_commit` (re)stages the paths it commits, so a
  carefully staged partial hunk within those paths is staged in full (the
  modal preview shows the full scope first). Commit signing (GPG/SSH) and
  hooks that read paths outside the sandbox's grants will fail under
  confinement. `git_read`'s status/diff exclude `deny_paths` via pathspecs,
  but `log` shows committed history as-is — anything already committed is
  considered yours to see.

See `ROADMAP.md` for what's planned next (repo map, git integration, richer
agentic UX) and the project's own audit history.
