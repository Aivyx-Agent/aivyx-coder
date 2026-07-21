# aivyx-coder

A terminal (TUI) coding agent for **local** LLMs only — Ollama, vLLM, or
llama.cpp over their OpenAI-compatible `/chat/completions` endpoints. It never
talks to a cloud API. An LLM drives tool calls (read/write/edit files, search,
run commands) against your real filesystem, gated by a layered permission and
sandboxing model designed so that a mistaken — or actively manipulated — model
cannot quietly do damage.

This is a from-scratch Rust project built reliability-first: the security
boundary was designed before the tools that need it, and hardened through
repeated full-codebase audits (see `docs/HISTORY.md` for the phase history).

## Building and running

```
cargo run -p aivyx
```

For a release build (Linux x86_64 only, static musl binary, matching
where the real Landlock+seccomp sandbox actually works):

```
scripts/build-release.sh
```

Produces `dist/aivyx-coder-v<version>-x86_64-linux-musl.tar.gz` plus a
`.sha256` checksum alongside it. Tagged releases (`vX.Y.Z`) are also
built and published automatically via GitHub Actions once this
repository is pushed to GitHub — check the repository's Releases page
for pre-built downloads at that point.

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
get corrective feedback the model can retry from. See `docs/HISTORY.md`'s
Phase 2 for the A/B measurements behind the default.

**Reasoning visibility**: a reasoning-capable model's chain-of-thought
renders live as a dimmed, italicized "thinking:" line in the terminal
transcript, distinct from its final answer. Display-only — reasoning
content never enters the agent's own history or the session JSON, and is
invisible to prompted-edit-mode's SEARCH/REPLACE parser.

**Repository map**: on each turn a token-budgeted map of the repo's
top-ranked files and symbol signatures (tree-sitter extraction, PageRank
over the internal reference graph — Rust, Python, JavaScript/JSX, and
TypeScript/TSX today; other languages degrade gracefully to no map) is
appended to the system prompt, giving the model orientation it wouldn't ask
for on its own. Gitignore-aware, `deny_paths` excluded, cached per file so only edits
re-parse. Its token weight is counted by the compaction estimator. Configure
or disable under `[repo_map]`; a project in an unsupported language simply
gets no map and pays no cost.

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
a ref is enough to let its objects age out via normal `git gc`. When a
single model response contains multiple mutating tool calls and a later
one fails, every earlier successful call in that same response is
automatically rolled back to the checkpoint from before the batch started
— the model doesn't have to notice and manually undo a partial multi-file
change itself. The rollback notice is folded directly into the failing
call's own error text, so the model sees exactly what happened and what
was undone in the same turn.

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
something the model asked for itself. A failing verification result also
gets a short note appended listing which lines are new since the
immediately preceding verification attempt
— whatever that prior attempt's own outcome was — so the model can tell a
newly-introduced regression apart from an already-known failure without
re-deriving that context from raw output each time. This is a coarse,
framework-agnostic line-set comparison, not real test-parsing, and says so
explicitly in the note itself.

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

Mutually exclusive with `--plan` and `--resume`. See `docs/HISTORY.md`'s
Phase 11 section (the "11c" subsection) for the full trust-profile
rationale and design forks.

**Agent-maintained wiki** (`/wiki`, `/wiki <page>`): generates and keeps
`docs/wiki/*.md` up to date — one page per workspace crate plus
`architecture-overview.md` — using the agent's normal gated tools, no new
trust tier. Bare `/wiki` regenerates only pages whose covered source files
changed since they were last generated (tracked per-page, in each page's
frontmatter, against the commit it was generated at — not against
uncommitted changes); `/wiki <page>` forces one page regardless of
staleness. Every page write still goes through the standard confirmation
modal. The repo map lists existing pages (path + one-line summary) as
pointers so the model can `read_file` the relevant one on demand, at no new
token-budget cost. Each page is its own turn, so if `[verification]
command` is configured it auto-runs once per regenerated page (not once per
`/wiki` invocation) — expect a slow verification command to dominate a
first-run, full-skeleton regeneration. See `docs/HISTORY.md`'s Phase 11
section (the "11b" subsection) for the full design rationale.

**Sub-agent delegation** (`delegate_task`): a tool the model can call
mid-turn to hand a bounded task to a fresh, isolated agent — full tool
access, the same `ConfirmationGate`/checkpoint/plan-mode boundary as the
main session, but a completely separate conversation history, so
exploring or working on something unfamiliar doesn't clutter the main
session's own context window. Only the sub-agent's final text answer
enters the main session's history; its own tool calls/results/reasoning
render live in the transcript (prefixed `sub-agent>`, visually distinct)
but never join history directly. Bounded by `[sub_agent] max_iterations`
(default 10); a sub-agent that runs out of budget still returns its
best-effort partial result rather than failing outright. Delegation is
capped at one level — a sub-agent's own tool list never includes
`delegate_task`.

**Architect/editor model-pairing** (`/architect <task>`): a separately
configured, typically stronger model produces a prose implementation plan
for the stated task, which is then handed directly to the primary
("editor") model's own turn loop — one continuous action, no re-prompt
needed. Unlike `/council`, there's exactly one architect seat and no
deliberation/ranking; unlike `delegate_task`, the architect never calls
tools itself, and its output feeds the *same* session's next turn rather
than spawning an isolated agent. Off until both `base_url` and `model` are
set under `[architect]` in `config.toml`; an unconfigured or bare
`/architect` explains itself instead of running. The plan streams live
(prefixed `architect>`, cyan, visually distinct from `council>` and
`sub-agent>`), and any planning failure (backend error, an empty response)
ends the turn without ever invoking the editor — a failed plan never
silently falls back to running the raw task unplanned.

**LSP integration** (`go_to_definition`, `find_references`): two read-only
tools giving exact symbol resolution the repo map's static, ranked symbol
list can't provide — which exact definition a call site resolves to when
several candidates share a name, and every reference site across the whole
workspace. Backed by a `rust-analyzer` subprocess, spawned lazily on first
use and reused for the rest of the session, confined by the same
Landlock/seccomp `ExecutionConfiner` every other process-executing tool
already gets. `path`/`line`/`column` arguments are 1-indexed, matching
`grep`'s existing `path:line_number:text` convention; output mirrors it too
(`path:line:text`, one line per result). Both tools are always registered —
a missing `rust-analyzer` on `PATH` surfaces as a clear error on first call
rather than a silent startup probe. Bounded by `[lsp] timeout_secs`
(default 60s — cold `rust-analyzer` indexing on a larger workspace can be
slow). Rust-only for now; no hover, workspace symbol search, or rename.

**`AGENTS.md` project instructions**: an optional `<cwd>/AGENTS.md`
(project-level) and/or `<config_dir>/AGENTS.md` (user-global, sibling to
`config.toml`) — conventions, architecture notes, "don't touch X," build/test
commands, style preferences, stated once instead of re-derived every
session. Both are refreshed every turn, not read once at startup, so an
edit mid-session applies on the very next turn with no restart needed. When
both are present, user-global content renders first, then a one-line note,
then project content — project instructions take precedence over user
preferences if they conflict. Governed by `[agents_file]` (`enabled`,
default `true`; `budget_tokens`, default `1024`, applied per file
independently). A file over its budget is still included in full — never
truncated, since hand-written prose has no safe cut point — but triggers a
one-time notice naming which file and how to fix it.

`AGENTS.md` content is injected directly into the system prompt — the
trusted position, unlike tool results (which the system prompt explicitly
tells the model to treat as untrusted data). This is deliberate: it's the
whole point of the feature, and it's standard behavior for every comparable
tool. It means a project's `AGENTS.md` is followed from the very first turn,
on by default, before the user has necessarily reviewed it — e.g. right
after cloning an unfamiliar repo. The actual security boundary stays the
sandbox and `ConfirmationGate`, not prompt trust: no `AGENTS.md` content can
skip a permission tier or bypass the approval gate on a mutating action, so
review the file the same way you'd review any other project instructions
before trusting them, not as a sandboxed-away concern.

**Learning over time**: the agent can propose updates to your *global*
`AGENTS.md` itself, via a dedicated `remember_preference` tool — either
because you asked it to remember something, or because it noticed a
clear, repeated pattern. Every proposed change goes through the exact
same review as any other file write: you see the diff, you approve or
deny it. Disable with `[persona] enabled = false`. Unlike every other
mutating tool, this one never uses the Always-Allow cache — you always
see every change to this file, individually, even if you've approved a
previous one. Not available in `--auto` (autonomous) mode: there's no
human to review the change.

**Editor context**: an optional per-project JSON file
(`~/.local/state/aivyx-coder/editor-context/<hash>.json`, keyed by the same
canonicalized-`cwd` hash as session files) that any editor integration can
write to, reporting the currently open file, cursor position, and
selection. Re-read every turn and surfaced as a one-line addition to the
system prompt ("Currently open in editor: src/foo.rs, cursor at line
42.") — metadata only, never file content; the model calls `read_file`
itself for actual code, exactly as it already does everywhere else. A
file that's missing, malformed, reports an unrecognized `schema_version`,
is more than 5 minutes stale, whose `workspace_root` doesn't match this
session's own directory, or whose reported path falls under a configured
`deny_paths` entry is silently ignored — none of these are user-facing
errors. No editor plugin ships with aivyx-coder; this is the file-format
contract such a plugin (for any editor) would write to. Governed by
`[editor_context]`: `enabled` (default `true` — a no-op until some
integration actually writes the file).

The JSON schema (`schema_version: 1`):

```json
{
  "schema_version": 1,
  "workspace_root": "/abs/path/to/project",
  "file": "src/foo.rs",
  "cursor": { "line": 42, "column": 8 },
  "selection": { "start_line": 40, "end_line": 45 },
  "updated_at": "2026-07-18T12:00:00Z"
}
```

`workspace_root` is absolute and must canonicalize to aivyx-coder's own
`cwd`. `file` is relative to `workspace_root`. `cursor` is required,
1-indexed. `selection` is optional — omit the `selection` field (or set it
to `null`) when there's no active selection; 1-indexed, inclusive line
range, no column granularity in this version. `updated_at` is an RFC 3339
timestamp.

**Editor approval**: a follow-on to editor context — lets the user's
editor answer a pending permission decision (a file write/edit/delete, a
shell command, or an MCP tool call) instead of requiring a terminal
keypress. When enabled and a pending decision is raised, aivyx-coder
writes a request file describing it (the real before/after file content
for a write/edit/delete, or the command text for a shell/MCP action) to
`~/.local/state/aivyx-coder/editor-approval/<hash>-request.json`, then
waits on either the terminal's own Allow/Deny/Always-Allow prompt or a
matching `~/.local/state/aivyx-coder/editor-approval/<hash>-response.json`
file — whichever answers first wins; the other is dropped. The request file
is created 0600; the response file's permissions are determined by the editor
plugin. Both files are deleted the instant the decision resolves, whichever
surface answered. No editor integration ships in this repo for any
specific editor — this is a schema contract (see below) an editor plugin
implements against, exactly like editor context. Governed by
`[editor_approval]`: `enabled` (default `true` — inert without an
external process actually writing a response file).

Request file schema:

```json
{
  "schema_version": 1,
  "request_id": "6a6e...-uuid",
  "target": "/home/user/project/src/foo.rs",
  "action_kind": "write",
  "old_content": "fn foo() {}\n",
  "new_content": "fn foo() -> i32 { 42 }\n"
}
```

`target` is an absolute, resolved path for `write`/`delete` (contrast this
with editor context's `file` field, which is relative to `workspace_root`);
for `execute` it's the command string (e.g. `"cargo test"`), and for
`mcp_tool` a description string (e.g. `"search (server: filesystem)"`) —
neither of those is a path. `action_kind`
is one of `write`, `delete`, `execute`, `mcp_tool`, each with different content
fields: `write` carries `old_content`/`new_content` (old empty for a brand-new
file); `delete` carries `old_content` plus `will_delete: true` (no
`new_content` key at all); `execute` carries `command`/`args`; `mcp_tool`
carries a `description` string. A `write` or `delete` request whose underlying
file can't be read as text (a binary file) never generates a request file at
all — the terminal remains the sole surface for that one decision, same as when
no editor integration is running.

Response file schema (written by the editor integration):

```json
{
  "schema_version": 1,
  "request_id": "6a6e...-uuid",
  "decision": "allow"
}
```

`decision` is one of `allow`, `deny`, `always_allow` — `always_allow`
feeds the exact same Always-Allow cache a terminal Always-Allow does,
keyed on the same exact target. A response whose `request_id` doesn't
match the currently pending request is ignored.

**`web_fetch`/`web_search`**: aivyx-coder is local-only in where LLM
inference happens (Ollama/vLLM/llama.cpp), not in network isolation — the
agent has full network access. `web_fetch(url)` fetches a URL and converts
its HTML to readable text via `html2text`, head-truncated at 50KB.
`web_search(query)` queries a configured SearXNG instance and returns
ranked `title | url | content` results, one per line. Both use the same
`ActionKind::Read` auto-allow tier as `read_file`/`grep` — no confirmation
modal, a deliberate choice over the more conservative confirm-by-default
alternative, on the reasoning that fetched/searched content is already
covered by the standing untrusted-tool-output convention. `web_fetch`
carries its own pre-flight SSRF check: before connecting, it resolves the
target host and refuses loopback/private/link-local addresses (override
with `[web] allow_private_targets = true`) — a best-effort mitigation with
a known DNS-resolution TOCTOU gap, not the project's primary security
boundary (that remains the sandbox/`ConfirmationGate`, same distinction as
`AGENTS.md` above). `web_search` has no such check; it only ever talks to
the one explicitly-configured, admin-trusted `search_base_url`. Governed by
`[web]`: `enabled` (default `true`, gates registration of both tools),
`search_base_url` (default unset — `web_search` self-explains how to
configure it when called unconfigured, rather than being silently absent),
`max_search_results` (default `10`), `fetch_timeout_secs` (default `30`),
`allow_private_targets` (default `false`).

**MCP (Model Context Protocol) client support**: aivyx-coder can connect to
arbitrary user-configured MCP servers over stdio, covering all three MCP
primitives — tools, resources, and prompts. Each configured
`[[mcp.servers]]` entry is spawned and discovered concurrently at startup
(bounded by that server's own `timeout_secs`); a server that fails or times
out is skipped with a warning rather than blocking the rest of startup or
any other server. Every discovered tool is registered under
`mcp__<server>__<tool>` and always requires confirmation via a dedicated
`ActionKind::McpTool` — unlike `web_fetch`/`web_search`'s auto-allow, an MCP
tool's actual behavior is arbitrary third-party code this project can't
verify, so it's never auto-allowed regardless of anything the server itself
claims about being read-only. Resources and prompts, by contrast, are
protocol-guaranteed read-only, so they surface through four fixed,
auto-allowed (`ActionKind::Read`) meta-tools instead of one tool per
discovered item: `list_mcp_resources`/`read_mcp_resource` and
`list_mcp_prompts`/`get_mcp_prompt`, each aggregating across every connected
server or filterable to one by name. A connection that dies mid-session is
respawned on its next use, re-running only the `initialize` handshake (not
full rediscovery, which only ever runs once at startup). Configured via
`[[mcp.servers]]`: `name`, `command`, `args` (default empty), `env` (default
empty), `timeout_secs` (default `30`) — no separate `[mcp] enabled` flag,
since an empty server list is already a complete no-op. `npx`-based servers
may need cache-directory read access added to `[sandbox] extra_read_paths`
(or an absolute path to an already-installed server binary used instead) to
avoid startup timeouts under the sandbox, since its default-deny policy has
no read access to `npx`'s cache directory by default.

**Branch/PR tooling**: `git_branch`, `git_push`, and `git_pr` give the model
purpose-built branch/push/PR-creation tools instead of leaving them to
`run_shell` alone. All three share `git_commit`'s exact permission tier —
`ActionKind::Execute` with a `PermissionTarget::Command` target, confirm-gated
— rather than introducing a new tier: these are structured invocations of
the same trusted `git`/`gh` CLIs `git_commit` already shells out to, not
arbitrary or unverifiable code. `git_branch(mode, name, base?)` creates (and
switches to) a new branch or switches to an existing one; `name` (when
switching) and `base` (when creating, if given) are rejected if they start
with `-` (a real, live-git-reproduced vulnerability found during review: a
dash-prefixed value in these two bare-positional argv slots gets parsed by
git as a flag rather than a value — e.g. a branch name of `-f` when
switching would silently force-discard uncommitted changes instead of
erroring "branch not found." Note `create`'s own new-branch `name` — the
value right after `-b` — is a different, genuinely safe position: git
unconditionally consumes it positionally and rejects a dash-prefixed
branch name outright, so it isn't guarded). `git_push(remote?)` always
pushes with `-u` (a no-op once
upstream tracking exists) and carries the same leading-dash rejection on
`remote` for the same reason; it has **no `--force`/`--force-with-lease`
support at all**, not even as an internal, unexposed flag. `git_pr(title,
body?, base?, draft?)` opens a pull request via the `gh` CLI, gated behind
two deterministic preflight checks (never by parsing either tool's stderr
text): the current branch must already have an upstream (checked via `git
rev-parse`, directing the model to `git_push` first if missing) and `gh`
must be installed and authenticated (checked via `gh auth status`,
distinguishing "not installed" from "not authenticated" with different
fixes). `git_pr` is always registered, no config flag — a missing or
unauthenticated `gh` is an environmental accident, the same reasoning
`go_to_definition`/`find_references` already apply to a missing
`rust-analyzer`. Branch *listing* (read-only) is a fourth mode on the
existing `git_read` tool instead of a new tool, staying auto-allowed.

**`delete_file(path)`**: `ActionKind::Delete`'s first real constructor —
every prior tool declared `Read`/`Write`/`Execute`/`Internal`/`McpTool`,
this closes out the original tool/capability audit's last remaining item.
Confirm-gated, same tier as `write_file`/`edit_file`; the user sees the
file's content in the preview (or a binary-file warning, reusing
`write_file`'s exact wording) before approving. Single-file only — a
directory target is refused with a clear error, `run_shell` remains the
path for directory removal. Deletion itself is plain `tokio::fs::remove_file`,
no subprocess spawned at all — a deliberate contrast with the branch/PR
tooling phase's own argv-injection lesson above: there's no argv here to
misparse in the first place. No bespoke recovery mechanism either — a
deleted file is one `git checkout <checkpoint-ref> -- <path>` away from
being restored via the existing automatic pre-mutation checkpoint every
mutating tool already gets, live-verified end to end (delete, then
restore) as part of this tool's own testing. That safety net inherits the
checkpointer's own existing limits, not new ones this tool introduces: a
gitignored file (staged via `git add -A`, which never picks up ignored
paths) has no checkpoint to restore from, and a non-git working directory
has no checkpointer active at all.

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
Two things verified live (`docs/HISTORY.md` Phase 10) before pointing aivyx
at it:

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

## Editor integration (ACP)

`aivyx --acp` runs as an [Agent Client Protocol](https://agentclientprotocol.com)
server over stdin/stdout, for embedding aivyx-coder directly in an
editor's own UI instead of the terminal. Same security model as the TUI
— every tool call still passes through `ConfirmationGate`, now surfaced
as the editor's own permission UI instead of a modal.

**Zed**: add to your `settings.json`:

```json
{
  "agent_servers": {
    "aivyx": {
      "command": "/path/to/aivyx",
      "args": ["--acp"]
    }
  }
}
```

**VS Code**: install the [ACP Client](https://marketplace.visualstudio.com/items?itemName=formulahendry.acp-client)
extension, then point it at the same `aivyx --acp` command — no
aivyx-specific VS Code extension exists or is needed.

**Not yet supported over ACP**: `--auto` (autonomous mode), `--resume`
(TUI-only — the editor manages its own conversation view, so resumed
history would be invisible to it; `--acp --resume` is rejected at
startup), mid-turn cancellation (`session/cancel`), and non-text prompt
content (images, embedded resources) — see `docs/superpowers/specs/
2026-07-20-acp-editor-integration-design.md` for the full scope.

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
| `delete_file` | delete a file | prompt (then cacheable) |
| `git_branch` | create or switch git branches | prompt (then cacheable) |
| `git_push` | push the current branch to a remote | prompt (then cacheable) |
| `git_pr` | open a pull request via `gh` | prompt (then cacheable) |
| `web_fetch` | fetch a URL and convert to readable text | none (auto-allowed) |
| `web_search` | query a configured SearXNG instance | none (auto-allowed) |
| `go_to_definition` | resolve a symbol to its definition (via `rust-analyzer`) | none (auto-allowed) |
| `find_references` | find every reference to a symbol across the workspace | none (auto-allowed) |
| `delegate_task` | hand a bounded task to a fresh sub-agent | none (internal state only) |
| `list_mcp_resources` / `read_mcp_resource` | list/read resources from connected MCP servers | none (auto-allowed) |
| `list_mcp_prompts` / `get_mcp_prompt` | list/get prompts from connected MCP servers | none (auto-allowed) |
| `mcp__<server>__<tool>` | dynamically discovered tool from a connected MCP server | prompt (then cacheable) |

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

[editor_context]
enabled = true  # a no-op until some editor integration writes the context file

[editor_approval]
enabled = true  # a no-op until an external editor plugin writes a response file

# Enforced verification (docs/HISTORY.md Phase 12 Part B): after file edits,
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

See `ROADMAP.md` for current status and `docs/HISTORY.md` for the full
phase-by-phase history and audit trail.
