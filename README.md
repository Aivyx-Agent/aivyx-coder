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

**Edit format** (`[backend] edit_format`, or `--edit-format` per session):
`native` sends edits as `edit_file`/`write_file` tool-call JSON; `prompted`
teaches the model to write Aider-style SEARCH/REPLACE blocks as plain text
instead — multiline code survives plain text better than JSON string
escaping on small models. Parsed blocks are applied through the *same*
tools, so the permission modal, diff preview, plan-mode denial, deny_paths,
and checkpoints all behave identically in both formats; malformed blocks
get corrective feedback the model can retry from. See ROADMAP.md Phase 2
for the A/B measurements behind the default.

**Repository map**: on each turn a token-budgeted map of the repo's
top-ranked files and symbol signatures (tree-sitter extraction, PageRank
over the internal reference graph — Rust files only for now) is appended to
the system prompt, giving the model orientation it wouldn't ask for on its
own. Gitignore-aware, `deny_paths` excluded, cached per file so only edits
re-parse. Its token weight is counted by the compaction estimator. Configure
or disable under `[repo_map]`; non-Rust projects simply get no map and pay
no cost.

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

**Enforced verification** (`[verification] command`, off by default): once
configured, a turn that made file edits can't end silently unverified — the
agent auto-runs the named `allowed_commands` entry via `run_command` before
letting the model finish, feeding a failure back so the model can fix and
retry (up to `max_auto_verify_retries`, default 3) rather than relying on
the model choosing to verify its own work. A passing run costs no extra
model round-trip; retries exhausted still ends the turn (never blocks
completion) but with a loud, un-missable notice pointing at the worktree
checkpoints already taken before each edit. The auto-triggered call is
labeled `auto-verify:` in the transcript so it's never mistaken for
something the model asked for itself.

**Autonomous mode** (`aivyx --auto "<goal>"`): runs unattended — the TUI
stays up so you can watch (and Ctrl+C at any point), but nothing waits on a
permission modal. `ConfirmationGate` gains a dedicated autonomous-mode tier
that trades the interactive prompt for a narrower, unconditional trust
profile: `write_file`/`edit_file` are auto-allowed only when the resolved
target is inside the process's `cwd` (a new boundary check — file edits
have no Landlock scoping the way spawned commands do) and outside
`deny_paths`; `run_command` is auto-allowed only for a pre-seeded
`allowed_commands` entry; `run_shell` and `git_commit` are hidden from the
model entirely and denied at the gate as a backstop if invoked anyway
(`git_commit`'s target is never cacheable, so it falls out of the same
cache-miss-denies rule with no special-casing). A driver loop sends the
goal, continues on `AgentEvent::TurnPaused`, and stops once every task in
the task list is `Done`, the iteration/wall-clock budget from `[autonomous]`
is exhausted, or you cancel. If enforced verification (above) exhausts its
retries, autonomous mode goes one step further than the interactive loud
notice: it rewinds the worktree to the checkpoint taken before that batch
of edits, discarding the failed experiment, and continues with remaining
budget. Because auto-approving edits is only safe with a deterministic
keep/discard signal, `--auto` refuses to start unless `[verification]
command` is configured. Config:

```toml
[autonomous]
max_iterations = 20      # total "continue" round-trips for the whole run
max_duration_secs = 3600 # wall-clock ceiling for the whole run
```

Mutually exclusive with `--plan` and `--resume`. See ROADMAP.md's Phase 11c
entry for the full trust-profile rationale and design forks.

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

## Serving

aivyx speaks the OpenAI-compatible `/v1` API, so any local server works.
Two supported setups:

**Ollama (quick start).** Zero-setup, and the shipped default. One trap to
know: Ollama serves its *own* default context window (typically 4096)
unless the model sets `num_ctx` or the service sets
`OLLAMA_CONTEXT_LENGTH` — `/v1` cannot request more, and a too-small
window truncates responses mid-thought (reasoning models burn it
invisibly). aivyx probes for this at startup and warns in the transcript;
the fix is a derived model (see the config reference below).

**llama-server (recommended for serious use).** Same GGUF models and
kernels as Ollama, everything explicit — the hidden-window trap can't
exist when `-c` is on the command line.

*Installing it*: use your distro's packaged build where one exists (Arch:
AUR `llama.cpp-cuda`, which installs `/usr/bin/llama-server`), or the
prebuilt binaries from ggml-org's releases, or a source build. One
CUDA-build gotcha that has bitten twice here: ggml's cmake auto-adopts any
`ccache`/`sccache` it finds on PATH (`GGML_CCACHE` defaults ON), and
sccache-wrapped nvcc corrupts parallel builds with
`fatbinary: Could not open input file '*.cubin'` errors. Disable it
explicitly — for the AUR package:

```
LLAMA_BUILD_EXTRA_ARGS="-DGGML_CCACHE=OFF -DCMAKE_CUDA_ARCHITECTURES=89" makepkg -si
```

(arch `89` = RTX 40-series; use your GPU's compute capability. AUR helpers
don't always pass environment through to `build()` — if the build fails
with the cubin error, check `GGML_CCACHE` in the build dir's
`CMakeCache.txt` and prefer running `makepkg` directly.)

*Running it*:

```
llama-server -hf unsloth/Qwen3.5-9B-GGUF:Q4_K_M \
  -c 16384 -ngl 99 --jinja --cache-reuse 256 \
  --chat-template-kwargs '{"enable_thinking": false}' \
  --temp 0.7 --top-k 20 --top-p 0.8 --presence-penalty 1.5 \
  --host 127.0.0.1 --port 8080
```

`--jinja` is load-bearing (native tool-call templating); `--cache-reuse`
keeps agent-loop prompts warm across tool round-trips. Point aivyx at it
with `base_url = "http://127.0.0.1:8080/v1"`. The startup probe reads
llama-server's `/props` and confirms the served window.

**Sampling does not migrate from Ollama.** Ollama applies the Modelfile's
sampling parameters server-side; llama-server uses its own generic
defaults instead — and reasoning models are sensitive to this. Always
pass the model family's recommended sampling flags (the values above are
Qwen's non-thinking set; `ollama show <model>` lists what Ollama used).

**Disable thinking for agent work.** Measured here: qwen3.5 on
continuation turns (after a tool result) emits its next tool call *inside
an unclosed think block*; llama-server's reasoning parser then classifies
the entire action as `reasoning_content` and the turn ends having done
nothing — and the same happens to prompted SEARCH/REPLACE blocks, so no
edit format escapes it. `--reasoning-budget 0` is inert on templates
without `enable_thinking` support; the switch that works is
`--chat-template-kwargs '{"enable_thinking": false}'`. With thinking
disabled, the aivyx edit benchmark went from 0/3 on affected tasks to
9/9 overall at ~2s per edit — thinking buys nothing for tool-driving on
this model class and costs both latency and, on llama-server, silent
no-op turns.

Prefer GGUFs from HuggingFace (`-hf repo:QUANT` downloads and caches
them). Reusing Ollama's blob files directly (`ollama show --modelfile`
reveals the path) sometimes works but is **not reliable**: Ollama's fork
writes metadata upstream llama.cpp may reject, and Ollama stores chat
templates outside the GGUF — a missing template silently breaks native
tool-calling (calls stream through as plain text).

Example systemd user unit (`~/.config/systemd/user/llama-server.service`):

```ini
[Unit]
Description=llama-server for aivyx
[Service]
ExecStart=/usr/bin/llama-server \
  -hf unsloth/Qwen3.5-9B-GGUF:Q4_K_M -c 16384 -ngl 99 \
  --jinja --cache-reuse 256 \
  --chat-template-kwargs '{"enable_thinking": false}' \
  --temp 0.7 --top-k 20 --top-p 0.8 --presence-penalty 1.5 \
  --host 127.0.0.1 --port 8080
Restart=on-failure
[Install]
WantedBy=default.target
```

Not every Qwen3 release has a thinking toggle at all: the `-Instruct-2507`
line (e.g. `Qwen3-4B-Instruct-2507`) ships non-thinking only — no
`enable_thinking` template variable, no `<think>` tags regardless of
flags — so `--chat-template-kwargs` is simply inert there, not something
to debug if it appears to do nothing.

**Lemonade (distro-packaged alternative to a manual llama-server
install).** [lemonade-sdk/lemonade](https://github.com/lemonade-sdk/lemonade)
wraps llama.cpp (plus other backends) with model pull/load management and
a distro package (CachyOS/Arch: `lemonade-server`, binary `lemonade` +
`lemond` service) — its CUDA backend ships prebuilt binaries per compute
capability, sidestepping the `GGML_CCACHE` build gotcha above entirely.
Two things verified live (ROADMAP.md Phase 10) before pointing aivyx at
it:

- **Target the underlying llama-server port, not Lemonade's gateway
  port.** Lemonade spawns a real `llama-server` process per loaded model
  (find its port with `ss -tlnp | grep llama-server` or `ps aux | grep
  llama-server`) alongside its own stable gateway (`lemonade config`'s
  `port`, default 13305). The gateway's `/props` returns Lemonade's web-UI
  HTML, not JSON, and its Ollama-compatible `/api/show` always reports
  `"parameters": "num_ctx -1"` regardless of the model's actual loaded
  context — aivyx's startup probe parses that as "Ollama's hidden
  default" and emits a **false** truncation warning even when the model
  is correctly configured. Pointing `base_url` at the real llama-server
  port instead gets an accurate `/props` and a correct probe, identical
  to a native llama-server install — the tradeoff is that this port isn't
  a documented, stable Lemonade interface and may shift across restarts
  or model reloads.
- **`--llamacpp-args` needs single-quote-wrapping for flags carrying
  embedded JSON.** Lemonade's own argument splitter strips bare double
  quotes before they reach llama-server, so
  `--llamacpp-args "--chat-template-kwargs {\"enable_thinking\":false}"`
  silently breaks the JSON. Wrap the JSON in single quotes instead:
  ```
  lemonade load <model> --ctx-size 16384 \
    --llamacpp-args "--chat-template-kwargs '{\"enable_thinking\":false}' \
    --temp 0.7 --top-k 20 --top-p 0.8 --presence-penalty 1.5"
  ```

Once pointed correctly, everything else behaves exactly like a native
llama-server install: `ctx_size`/`--ctx-size` is an explicit first-class
control (no Ollama-style hidden default), and the Phase 10 acceptance
benchmark reproduced the native 9/9 / prompted 6/9 result exactly against
a Lemonade-managed `qwen3.5:9b`.

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

## Council mode

For hard design decisions, `/council <question>` puts the question to several
local models at once (inspired by karpathy/llm-council, reinterpreted for
local-only serving). Each configured member answers independently, the
answers are anonymized and cross-ranked by the members themselves, and a
chairman model synthesizes one recommendation. Bare `/council` convenes the
council on the last assistant message — "review what you just proposed".

- **Read-only by construction.** Members never receive tools, so a council
  adds no permission surface and works identically in plan mode.
- **Sequential on purpose.** One GPU: members run one at a time, and a
  server like Ollama swaps models per request. A council trades minutes of
  latency for a second (and third, and fourth) opinion — use it where that
  trade is worth it.
- **Only the synthesis persists.** The full deliberation renders in the
  transcript; just the chairman's recommendation (with the identity reveal)
  enters the conversation the model sees, so a council doesn't eat the
  context window.
- **Fails closed.** Members that error or answer emptily are skipped; fewer
  than two usable answers aborts; if the chairman fails, nothing at all is
  added to the conversation.

Members see your question plus a token-budgeted digest of the recent
conversation (`tail_budget_tokens`) — not your files, and not the repo map.
Configure it with `[council]` (see below); unconfigured, `/council` just
explains what it needs.

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
#
# IMPORTANT: the server must actually SERVE this window. Ollama defaults to
# 4096 unless the model's Modelfile sets num_ctx or the service sets
# OLLAMA_CONTEXT_LENGTH — the /v1 endpoint cannot request it per-call. A
# too-small server window shows up as responses truncating "before they
# finished", especially on reasoning models whose thinking phase invisibly
# consumes the remainder. Cheap fix without touching the service:
#   printf 'FROM qwen3.5:9b\nPARAMETER num_ctx 8192\n' | ollama create qwen35-8k -f -
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

[repo_map]
enabled = true       # append a ranked symbol map to the system prompt
budget_tokens = 1024 # rough token budget the map may consume per request

# Enforced verification (ROADMAP.md Phase 12 Part B): after file edits,
# before a turn is allowed to end, auto-run this named allowed_commands
# entry via run_command and let the model react to the result — fix and
# retry on failure, or actually end the turn on success. Off by default;
# setting `command` alone is the opt-in, no separate enable flag. `command`
# must match a `[[permissions.allowed_commands]]` name, or a startup
# warning fires and verification stays disabled.
# [verification]
# command = "test"            # references [[permissions.allowed_commands]]
# max_auto_verify_retries = 3 # (edit, re-verify) cycles before giving up
#                              # on this round of edits — never silently:
#                              # the turn still ends, with a loud notice.

# /council — several local models answer, cross-rank, and a chairman
# synthesizes. Off until at least two members AND a chairman are set. Any
# OpenAI-compatible endpoint works per seat; with one GPU, pointing members
# at Ollama (which swaps models per request) alongside a resident
# llama-server daily driver is the intended shape.
[council]
tail_budget_tokens = 3072  # recent-conversation digest members see
# members = [
#   { base_url = "http://localhost:11434/v1", model = "qwen3.5:9b" },
#   { base_url = "http://localhost:11434/v1", model = "ornith:9b" },
# ]
# chairman = { base_url = "http://localhost:11434/v1", model = "qwen3.6:27b" }
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
