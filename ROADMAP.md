# aivyx-coder Roadmap

_Last updated: 2026-09-16 (the header date and test count had drifted
again — this file's own git history shows real content edits through
2026-09-07, with 6 more days/~15 commits of shipped work never reflected
here at all; see the new "CI hardening" and "Wick & Compass visual
surface" entries below and in `docs/HISTORY.md`)_

A terminal (TUI) coding agent for local LLMs only (Ollama, vLLM, or
llama.cpp) — see `README.md` for what it does and how to run it. This
file is the current-status summary; the full phase-by-phase history,
every design decision, and the evidence behind it lives in
`docs/HISTORY.md`. Every phase's original design spec and implementation
plan is tracked in `docs/superpowers/specs/` and `docs/superpowers/plans/`,
for the curious.

## Current status

**Shipped and live-verified** (Phases 1–8, 10 Parts A & B, 11a/11b/11c, 12,
and the full Phase 9 stretch-goal list): the full agent loop — streaming
chat, native + prompted SEARCH/REPLACE edit formats (A/B-measured, native
default), grep/glob search, `run_command`/`run_shell` behind real
Landlock+seccomp confinement, git tools (including `git_branch`/
`git_push`/`git_pr`) + automatic worktree checkpoint refs, `delete_file`
(`ActionKind::Delete`'s first constructor), tree-sitter repo map
injection, an agent-maintained wiki (`/wiki`), session persistence/resume,
context budget + compaction, plan mode (gate-enforced read-only),
autonomous mode (`--auto`) with a heuristic prompt-injection scan/taint/pause
guard, `/council` multi-model deliberation, AGENTS.md
project instructions, `web_fetch`/`web_search`, full MCP client support, a
startup probe of the *served* context window, goal-bounded turn pausing
instead of a hard iteration-cap failure, enforced post-edit verification
with automatic fix-and-retry, and an ACP editor-integration frontend
(see below). 695 workspace tests (freshly re-run 2026-09-16; up from
473 as of this doc's original content); every security-critical
behavior also proven by live E2E against real serving.

**Embedded Rust-native inference (2026-09-07)**: `aivyx-coder` can now
run a local GGUF model **inside its own process** via the `mistralrs`
crate, opt-in via new `provider-mistral-rs`/`-cuda`/`-metal`/
`-accelerate` Cargo features — the same capability the sibling `aivyx`
product shipped in its own Phase 134, ported here with real token
streaming from day one (an mpsc-forwarding shim around mistral.rs's
lifetime-bound stream) rather than the non-streaming compromise
`aivyx`'s own version shipped and never followed up on. See
`docs/HISTORY.md`'s own entry for the full account, including two
Critical final-review findings (streamed tool calls were initially
never read at all, and a bad model path could have silently triggered
an outbound Hugging Face download) both fixed and independently
re-verified before merge.

**Multi-process GPU-slot coordination via `aivyx-broker` (2026-09-07)**:
`aivyx-coder` can now share a single `llama-server`'s finite KV-cache
slots safely with other local processes (e.g. `aivyx`'s own daemon
pointed at the same server) via a new `backend.kind =
"llama_server_broker"` mode, routing requests through `aivyx-broker` — a
new standalone repo/daemon built as a companion to this work — instead
of talking to `llama-server` directly. Confirmed via direct source
grounding (this project's own `KvSlotPool`-equivalent tracker, and
llama.cpp's own `server-context.cpp`) that two independent local
processes really do race for the same physical slot with zero
coordination today, though llama-server's own server-side handling
already prevents actual data corruption — the real cost is silent
head-of-line blocking and KV-cache-locality thrash, not corrupted
output. `aivyx-broker` closes that gap: cache-locality-aware slot
admission plus the full restore/warm/save lifecycle now live in the
broker itself rather than duplicated per-client, and this repo's own
local slot-picking/kvcache logic is skipped entirely on this path — an
additive `aivyx_slot_hint` field is all that's sent. Broker mode is
threaded through every place this process can construct an `Agent`
against a shared broker (the top-level agent, delegated sub-agents, and
MCP-server sessions), not just the main one — a real gap caught and
fixed during review, since a hint-less request from any of those other
paths would otherwise have silently wiped the broker's own
cache-locality bookkeeping. See `docs/HISTORY.md`'s own entry, and
`aivyx-broker`'s own repo (`docs/superpowers/specs/2026-09-07-aivyx-broker-design.md`)
for the full cross-repo design and review history — the broker daemon
itself went through three whole-branch review rounds before merge,
the most of any single piece of work this project has produced.

**CI hardening + LSP handshake hang, root-caused (2026-09-11)**: this
repo's first push/PR-triggered CI (previously only version tags
triggered a workflow) hung for 50+ minutes on its very first real run;
root-caused to an unbounded LSP `initialize()` call the real-rust-analyzer
integration test exercises, which GitHub Actions' runner (unlike a local
machine lacking `rust-analyzer` on `PATH`) actually reaches. Fixed the
real gap (`initialize()` now shares the same timeout every other LSP
request already had) and re-ignored the integration test with a
verified, evidence-based reason (`rust-analyzer` itself never answers
the handshake on this runner) rather than the earlier speculative one.
Same day: a root `LICENSE` pointer file, CI/license README badges, and
issue templates. See `docs/HISTORY.md`'s own entry.

**Wick & Compass visual surface (2026-09-12/13)**: this project's slice
of the ecosystem-wide rebrand — a terminal startup banner (real
brass/rust/slate colors, printed before the TUI takes over the screen)
and a theme-adaptive README logo (a `<picture>`/`prefers-color-scheme`
element fixing a real bug where the old dark-only lockup was nearly
invisible on GitHub's light theme). A final review caught the release
tarball omitting the new logo files the README now references — fixed
and confirmed by simulating the real packaging commands. Also added
`CONTRIBUTING.md`. See `docs/HISTORY.md`'s own entry.

**Serving verdict (Phase 10 Part A)**: the serving configuration — not the
model, not the edit format — was the dominant reliability variable.
Correctly-configured llama-server (explicit 16k window, thinking
disabled) took the same qwen3.5:9b from Ollama's best 7/9 to 9/9 at ~10×
the speed on the edit benchmark. The daily driver runs llama-server;
Ollama stays as the zero-setup default and serves the council's
swap-per-request members.

**Constrained-decoding verdict (Phase 10 Part B)**: SGLang's
xgrammar-forced tool calls matched llama-server's already-clean 9/9,
0-malformed-call baseline exactly — no material win, so no config surface
was added. Radix caching was confirmed to automatically reuse the entire
growing conversation prefix on every turn after the first, a genuine (if
not decision-gating) positive finding.

**vLLM compat pass**: the last README-claimed provider
(Ollama/vLLM/llama.cpp) is live-verified. vLLM served the same
Qwen3.5-9B-AWQ weights cleanly on the first attempt — no compat bugs,
unlike SGLang's dtype crash — with correct native AWQ quantization and
GDN/Mamba dtype auto-detection, and a working
`--default-chat-template-kwargs` launch flag SGLang lacked. No new config
surface needed — `base_url` already generically targets any
OpenAI-compatible endpoint.

**Capability audit — fully closed.** A broader audit against the
project's actual end-goal — a high-end vibe-coding agent with a path to
full autonomy — found the security/checkpoint foundation doesn't need a
redesign for autonomy, only extension, but surfaced two structural gaps
in the agent loop itself (closed via Phase 12) plus five smaller gaps
(closed via Phase 9: AGENTS.md, `web_fetch`/`web_search`, MCP client
support, branch/PR tooling, `delete_file`). No tracked items remain from
this audit.

**Editor/IDE context integration — shipped.** The agent now polls a
local, editor-agnostic JSON descriptor file for the user's currently
open file, cursor position, and selection, and injects a one-line
metadata note into the system prompt every turn (e.g. "Currently open
in editor: {file}, cursor at line {line}."). Deliberately metadata-only
— the model never receives raw file/selection content this way, only a
path and line/column numbers; it still has to call `read_file` itself
for actual code, so the project's existing invariant (file content only
enters the conversation via an explicit, visible tool call) holds. No
editor-specific plugin code ships here — just the schema contract and
the agent-side read/inject path, gated by a new `[editor_context]`
config flag (default on). Live-E2E verified through the real release
binary. The project's own security review of this phase caught and
fixed a real gap before merge: the injected file-path string wasn't
sanitized before being interpolated into the trusted system prompt,
which could have let a crafted descriptor forge fake instructions via
embedded control characters — fixed by sanitizing the displayed copy
while leaving the real path untouched for the security-relevant
`deny_paths` check.

**Editor approval integration — shipped.** The "context out" follow-on:
the user's editor can now answer a pending permission decision
(write/edit/delete/execute/MCP-tool) as a fully equal-trust second surface
alongside the terminal's own Allow/Deny/Always-Allow prompt — first
decision wins. Transport is two polled JSON files under
`~/.local/state/aivyx-coder/editor-approval/`, the same keying scheme as
`editor_context`/sessions. `[editor_approval] enabled` defaults on — a
deliberate exception to this project's usual conservative-default posture,
since the feature is inert without an active external process writing a
response file. Live-E2E verified; the pre-existing security tier order in
`ConfirmationGate::check` was independently re-verified twice to be
completely untouched by the new race logic.

**Capability-gap-closing chapter — shipped (all 4 sub-projects).**
Following a direct audit of whether aivyx-coder can actually write real
code/scripts/small applications, 4 concrete gaps were found and closed in
sequence: (1) multi-file edit atomicity — a batch of mutating tool calls
in one response now rolls back entirely if a later call in it fails; (2)
reasoning visibility — a reasoning-capable model's chain-of-thought now
renders live in the TUI, display-only, never persisted; (3) structured
verification memory — a failing verification result now gets a short
"what's new since the last attempt" note, comparing against the
immediately-preceding run regardless of its own pass/fail outcome; (4)
repo-map multi-language support — the repo map now covers Python,
JavaScript/JSX, and TypeScript/TSX, not just Rust, via a new
per-language `LanguageConfig` dispatch table. Several real bugs were
found and fixed along the way, independently verified rather than
trusted from any single report — see `docs/HISTORY.md`'s
"Capability-gap-closing chapter" section for the full account. `main` was
pushed to GitHub immediately after this chapter closed.

**ACP editor integration — shipped, one verification step still open.**
`aivyx --acp` is a new frontend — a new `aivyx-acp` crate speaking the
[Agent Client Protocol](https://agentclientprotocol.com) (JSON-RPC over
stdio) — letting Zed's Agent panel, and VS Code via the existing
third-party `formulahendry.acp-client` extension, drive the exact same
`Agent` core the TUI does, with zero editor-specific plugin code of this
project's own. What started as a request to scope two bespoke editor
extensions turned out, on investigation, to be one protocol adapter
instead: Zed's own extension API can no longer build custom agent UI at
all (extension-provided agents are deprecated in favor of ACP), and VS
Code already has a mature community ACP client — so a single `--acp`
mode covers both. `crates/aivyx/src/main.rs`'s large TUI-agnostic
construction sequence was first extracted into a shared
`agent_builder.rs` so both frontends build `Agent` identically. Two real
bugs were caught during review, not just at plan-writing time: (1) a
genuine deadlock — running a turn inline inside the ACP `PromptRequest`
handler would hang the first time a permission decision was needed,
since that round-trip needs the same dispatch loop the handler would be
blocking; fixed by offloading the turn to a spawned task, independently
re-verified against the actual crate internals rather than trusted from
the fix's own claim; (2) `AgentEvent::Error` was silently dropped
instead of reaching the editor — a bug in the original design's own
protocol mapping, not just the implementation, caught only by the final
whole-branch review.

**ACP manual Zed smoke test — done, and it found a real bug.** Run live
against a real Zed session on the bare-metal test rig (see below): every
edit was denied no matter what the user clicked. Root-caused to a bug in
`agent-client-protocol` 1.2.0 itself — the crate's response-dispatch
ordering wasn't actually enforced despite being documented as if it
were, so `AcpPrompter::prompt`'s `block_task()` could receive a spurious
`-32601 Method not found` instead of a client's genuine "Allow" —
confirmed with a from-scratch, in-process reproduction using only the
crate's own public API, independent of any of this project's code.
Fixed by upgrading to `agent-client-protocol` 2.0.0 (the maintainers'
own migration guide describes fixing exactly this class of bug), which
turned out to need almost no source changes on our side — the affected
usage surface was small enough that only one now-redundant
`on_receive_dispatch` catch-all needed removing (2.0's built-in default
already does the same thing, correctly). A new regression test
(`prompter.rs`'s `prompt_resolves_to_allow_over_a_real_connection`)
exercises the exact mechanism that broke — an agent-initiated request
sent from a spawned task, resolved over a real in-process connection, no
LLM required — so this class of bug can't silently regress again. The
deadlock fix itself is now also live-verified, not just statically
analyzed: the same session exercised real gated tool calls end to end.
See `docs/HISTORY.md` for the full account.

**Bare-metal test-rig trial — done.** The `aivyx-coder` binary was
renamed to avoid a `PATH` collision with the sibling Aivyx Personal
Assistant (both previously built a binary literally named `aivyx`),
deployed and live-tested on real hardware against a real local backend
(`llama-server` + `qwen3.5:9b`, matching this project's own documented
best-serving-config verdict): a graduated series of coding tasks (a
script, a multi-file task with tests, a small Flask application) all
completed correctly end to end, and the ACP/Zed integration above was
exercised as part of the same trial. Two real bugs found and fixed
along the way: the TUI permission modal's Allow/Deny legend going
invisible for long diffs, and the `agent-client-protocol` 1.2.0
response-routing bug covered above.

The sibling Aivyx Personal Assistant was then built, deployed, and run
on the same rig alongside `aivyx-coder` — the original motivating
question behind the rename. Confirmed genuinely disjoint: `~/.aivyx/` +
`~/.local/share/aivyx/` for the assistant vs. `~/.config/aivyx-coder/`
+ `~/.local/state/aivyx-coder/` for the coder, no `PATH` collision, and
both ran concurrently against the *same* shared `llama-server` instance
(via the assistant's `llamacpp` provider) with no interference —
verified by re-running the ACP permission round-trip live while the
assistant's daemon was active.

A final verification pass re-confirmed both fixes live in the actual
release binary (not just the automated regression tests) and closed the
one remaining deferred item from the autonomous-mode injection guard's
own plan: a live `--auto` run seeded with a real prompt-injection
payload correctly detected it, refused to act on it (both the model
itself and the guard independently), and stopped the run with the
designed notice — `config.toml` and the project's git state both
provably untouched afterward.

**REPL / interactive-process support — shipped.** `run_command`/`run_shell`
were strictly one-shot — spawn, run to completion, reap — with no way to
hold a process open across multiple tool calls. Three new tools
(`repl_start`/`repl_send`/`repl_stop`) share one process slot: starting
goes through the normal `Execute`-tier gate and checkpoint like any other
command, but a new `ActionKind::Interact` lets subsequent sends/stops
auto-allow in Act mode (re-prompting on every REPL line would be as
unusable as re-prompting on every `read_file` call) while still being
correctly denied the instant Plan mode is entered mid-session — a
deliberate leak-through guard, checked *after* the plan-mode tier rather
than alongside the ordinary Read/Internal auto-allow. Plain pipes, not a
real PTY (documented in Known limitations below). Live-E2E verified: a
real model chaining `repl_start`/`repl_send`/`repl_stop` against a real
`python3` process, the Plan-mode leak-through guard holding under a live
Ctrl+P mid-session, and Drop-based shutdown confirmed not to orphan a
never-stopped process. See `docs/HISTORY.md` for the full account.

**Move/rename tool — shipped.** The tool set had `read_file`/`write_file`/
`edit_file`/`delete_file` but no atomic move/rename primitive — the model
had to synthesize a rename via read+write+delete, three separate prompts
with no atomicity guarantee. `move_file` closes this with a single atomic
`tokio::fs::rename`, covering both files and directories, via a new
`ActionKind::Move`/`PermissionTarget::Move{from,to}` threaded through
every gate tier (deny_paths on both endpoints, autonomous-mode worktree
boundary on both endpoints, exact-pair Always-Allow caching) and every
protocol surface (TUI modal, ACP, editor-approval). Refuses outright on an
existing destination (no overwrite mode) and on a cross-filesystem move
(no copy+delete fallback) — both deliberate choices favoring atomicity
and predictability over flexibility. A directory move runs an extra,
security-critical recursive scan (deliberately *not* gitignore-aware,
unlike this project's `grep`/`glob` walks) so a `deny_paths` entry nested
inside the moved tree — a gitignored `.env`, say — can't be silently
relocated out of protection. See `docs/HISTORY.md` for the full account,
including two real bugs review caught: a TOCTOU window on the
destination-exists check (narrowed via a re-check immediately before the
rename, a deliberate tradeoff over a full `renameat2(RENAME_NOREPLACE)`
fix to avoid musl-cross-compile complexity), and a second ACP tool-kind
mapping site the original plan's inventory missed entirely.

**Patch-apply tool — shipped.** The tool set had `edit_file` (exact-substring
search/replace) and `write_file` (full rewrite) but no way to apply
ready-made unified-diff/patch text directly — relevant when a model (or
the user) already has a well-formed patch rather than needing to re-derive
it as a search/replace pair. `patch_file` closes this via the `diffy`
crate, chosen specifically for its fuzzy hunk-position matching (tolerates
a model-generated patch's drifted line numbers by searching nearby for
matching context). Unlike `move_file`, this needed **zero** new gate
primitive — applying a patch to an existing file is architecturally
identical to `edit_file` (content mutation on one existing path), so it
reuses `ActionKind::Write`/`PermissionTarget::Path` verbatim, touching zero
lines in `aivyx-sandbox`, `aivyx-tui`, or `aivyx-acp`. Scoped to existing
files only (no patch-driven create/delete — `write_file`/`delete_file`
already own those) and one file per call, matching every other file tool's
shape. See `docs/HISTORY.md` for the full account, including a real
`diffy` behavior verified empirically before it was written into the
plan: parsing non-diff text does not error, it silently succeeds as a
zero-hunk patch.

**Verification test-selection — shipped.** The last item in the
2026-07-22 backlog, closing it out entirely. Enforced verification's
fix-and-retry loop always re-ran the *entire* configured `[verification]
command` on every retry, even for a one-line edit in a large test suite.
A new optional `[verification] scoped_command` config field lets interim
retries run a fast, user-configured command scoped to just the files
touched by that batch of edits, falling back to the full command only once
scoping passes (or isn't configured) — one full, unscoped run is still
mandatory before a batch is ever declared verified, preserving today's
completeness guarantee. The scoped run deliberately bypasses
`ConfirmationGate` for this one internal call (its arguments legitimately
vary every retry — different touched files — which the gate's exact-match
Always-Allow cache can't accommodate without either re-prompting every
retry or weakening that cache's invariant for every other tool). Also
fixed a real pre-existing bug found during this feature's own design: three
mutating file tools added in earlier phases (`patch_file`, `delete_file`,
`move_file`) never triggered enforced verification retries at all, only
`edit_file`/`write_file` did. See `docs/HISTORY.md` for the full account.

**`deny_paths` basename-glob matching — shipped.** The first item in the
2026-07-28 capability audit's backlog: `deny_paths` matching was
`starts_with` on fixed, absolute/home-relative paths only, so a
project-local secret file like `.env` — which recurs across arbitrary
project directories the agent might be pointed at — had no default
protection; a user would have had to hand-add every project's own
absolute path one at a time. A `deny_paths` entry with no path separator
(e.g. `.env`, `*.pem`) is now a basename-glob pattern instead, matching
any file with that name anywhere via the `globset` crate. No new config
schema — the existing `deny_paths: Vec<String>` list accepts both shapes,
distinguished by whether the entry contains a `/`. The default list gained
six new basename-glob entries (`.env`, `.env.*`, `id_rsa`, `id_ed25519`,
`*.pem`, `*.key`) alongside the existing absolute-path defaults, closing
the exact gap the audit found out of the box. A pre-existing duplicate of
the matching logic in `aivyx-tools` (used by `grep`/`glob`/`move_file`/
`git_commit`'s own directory-walk checks, and flagged by the same audit as
a drift risk) was consolidated onto the one canonical
`aivyx_sandbox::path_is_denied` function as part of this work.

**`delegate_task` REPL isolation — shipped.** The last item in the
2026-07-28 capability audit's backlog, closing it out entirely.
`delegate_task` sub-agents previously shared the parent's single global
REPL session (`ToolRegistry::clone()` clones `Arc` pointers, not
underlying state, so a sub-agent's registry held the exact same
`ReplStartTool`/`ReplSendTool`/`ReplStopTool` instances, bound to the
same session, as the parent) — breaking the "fresh, isolated agent,
completely separate conversation history" invariant `delegate_task` is
documented to provide. Fixed with a new `ToolRegistry::exclude` method,
called once in `agent_builder.rs` right after the parent registry is
cloned for sub-agent use: REPL tools are simply never offered to a
sub-agent, the same "just never in the list" outcome `delegate_task`
already achieves for its own one-level recursion cap. No new
`ActionKind`, `PermissionTarget`, or gate logic — pure tool-list
composition. Sub-agents keep `run_command`/`run_shell` for one-shot
needs; only interactive multi-turn REPL sessions are unavailable to them.

**Landlock + `aivyx-repomap` basename-glob enforcement — shipped.** The
last open item from the `deny_paths` basename-glob feature's own final
review, closing the entire 2026-07-28 audit lineage's backlog: Landlock's
command-tool grants (`grant_paths_excluding` in
`crates/aivyx-sandbox/src/confiner.rs`) and `aivyx-repomap`'s own
duplicate `is_denied` didn't understand basename-glob `deny_paths`
entries (`.env`, `*.pem`, etc.), so a confined `run_shell`/`run_command`
child could still read a matching file, and a repo-map-parsed source file
matching a user's own bare pattern could still have its symbols reach the
system prompt. Fixed for Landlock by resolving bare patterns into
concrete file paths, once at session startup, by scanning the working
directory and any configured `extra_read_paths` — then applying the
resolved matches uniformly to *every* grant, including the fixed system
paths and the OS temp directory, so a working directory nested inside one
of them (an `/etc/nixos`-style system-config-as-project-repo, or a test's
scratch directory under the system temp dir) is still fully protected.
The existing, well-tested carve-out algorithm (`grant_paths_excluding`)
needed zero changes, receiving the resolved concrete paths exactly like
any other denial. Fixed for `aivyx-repomap` by adding `globset` as a new
dependency and mirroring the canonical matcher's logic locally — a
deliberate, justified duplicate this time, since the crate's real
architectural boundary (zero dependency on *other workspace crates*, not
zero external dependencies at all) stays intact.

**Docker Model Runner serving support — partially live-verified,
container connectivity also shipped.** Base URL, model-naming
convention, and basic chat completions confirmed live 2026-09-21 against
a real DMR instance (both from a native `aivyx-coder` and, new this
entry, from inside `aivyx-coder`'s own Docker container via
`host.docker.internal:12434/engines/v1` — including a real host-firewall
hairpin-NAT fix required to make that path work at all). Still open: the
context-window runtime default (a real pulled model's GGUF metadata
shows 8192, a different fact from the actual serving-time default, which
remains unconfirmed) and whether tool-calling works end-to-end through
aivyx's native edit format. `probe.rs`'s DMR extension is now confirmed
blocked, not just deferred — no diagnostic endpoint exists at either
shape checked. See `docs/HISTORY.md` for the full account.

**`wiki_pointer_lines` `deny_paths` enforcement — shipped.** Found at the
Landlock + `aivyx-repomap` basename-glob enforcement feature's own final
review: `aivyx-repomap`'s `wiki_pointer_lines` read `docs/wiki/*.md`
files and injected each page's path and summary into the system prompt
every turn with no `deny_paths` check at all — unlike `collect_tags`,
which that same feature had just given basename-glob-aware matching to.
Fixed with a one-line addition to `wiki_pointer_lines`'s existing filter
chain, reusing the same `is_denied` function verbatim. No new logic, no
new dependency.

**`Agent::refresh_agents_files` `deny_paths` enforcement — shipped.**
Found at the `wiki_pointer_lines` fix's own final review: both the
global and project `AGENTS.md` files were spliced into the system
prompt every turn with no `deny_paths` check at all — unlike the
sibling `refresh_editor_context`, which already checked. Fixed by
adding a `deny_paths` field to `AgentsFileConfig` and checking both
paths (via the same global `deny_paths` list every other tool already
reuses) before either file is read; a denied file is silently skipped,
matching `refresh_editor_context`'s own behavior. No new config
surface, no new dependency.

**Real PTY for `repl_start`/`repl_send` — shipped.** Replaced the three
plain OS pipes with a genuine pseudo-terminal, allocated via raw `libc`
`posix_openpt`/`grantpt`/`unlockpt`/`ptsname_r` calls (consistent with
this project's existing raw-syscall precedent, no new dependency) — a
program run via `repl_start` now sees `isatty()` as true, gets working
readline/color, and can be resized live as `aivyx-coder`'s own terminal
resizes (a new `aivyx_sandbox::ResizeTarget` trait lets the TUI forward
`crossterm`'s resize events down to the active session without a new
inter-crate dependency between `aivyx-tui` and `aivyx-tools`). Needed no
Landlock ruleset change at all: both pty fds are opened in the parent
before `fork`, so the child only ever inherits already-open descriptors
rather than calling `open()` on any `/dev/pts/*` path itself.

**Slash command framework — shipped.** `/council`/`/wiki`/`/architect`
existed as three separately-implemented ad hoc commands with no shared
metadata and no TUI-side awareness that `/`-prefixed input was special.
Added a shared `aivyx_core::commands` table (name, description, and
which of three dispatch tiers each belongs to) that both a new `/help`
listing and a new TUI autocomplete hint read from, plus two new built-in
commands: `/clear` (starts a fresh conversation, via a new
`Agent::clear_conversation` that never calls the model) and `/quit`. The
three existing commands' triplicated word-boundary parsing logic was
de-duplicated onto one shared helper as a side effect. TUI-only —
`/council`/`/wiki`/`/architect` still work identically under the ACP
frontend (unchanged, they already flowed through `Agent::run_turn`
unconditionally), but `/help`/`/clear`/`/quit` and the autocomplete hint
are not wired into ACP.

**Cross-session memory (`aivyx-recall`) — shipped.** `memory_write`/
`memory_read`/`memory_forget` give the agent topic-scoped facts that
persist across sessions — global (`global:`) or project-scoped
(`project:`, keyed by the same cwd hash session persistence already
uses) — recalled only on an explicit `memory_read` call, never injected
ambiently. Backed by a new standalone crate/repo, `aivyx-recall`
(`Aivyx-Agent/aivyx-recall`), deliberately factored out so the sibling
Aivyx Personal Assistant can eventually adopt the same substrate instead
of reimplementing an equivalent one — see `docs/superpowers/specs/
2026-08-09-aivyx-recall-design.md`. A new `ActionKind::PersistentMemory`
backs the write/forget tools' gating: cacheable per exact topic in
interactive mode (unlike the existing, differently-shaped
`ActionKind::Memory` behind `remember_preference`), but unconditionally
denied under `--auto` for the same reason `remember_preference` already
is — both persist state outside the project working tree with no
checkpoint/rollback safety net.

**MCP-server frontend — shipped.** `aivyx --mcp-server` is a fourth
frontend — a new `aivyx-mcp-server` crate exposing `aivyx-coder` as an
MCP (Model Context Protocol) server over stdio (via the official `rmcp`
SDK), so another local MCP client can delegate a bounded coding task to
it, without needing shared code or conventions between the two products.
Each session runs at one of three access tiers (`plan`/`edit`/`execute`),
each an explicit tool-name allowlist filtered via `ToolRegistry::exclude`
from a delegate-shaped base registry (mirrors `agent_builder.rs`'s
existing `sub_agent_registry` precedent, plus two MCP-specific
exclusions: dynamically-bridged third-party MCP tools, and the
resource/prompt "meta" tools that read from those same servers — both
excluded for the same confused-deputy reason). A new `PermissionPrompter`
(`TieredPrompter`) auto-resolves any call the tier's own registry
already permits — belt-and-braces, since an excluded tool was never
registered to begin with. `[mcp_server].max_access_level` has no working
default; the server refuses to start unconfigured, mirroring `--auto`'s
existing posture for its own required `[verification].command`. Sessions
are in-memory only, TTL-evicted, deliberately separate from the on-disk
`--resume` store. `AutonomousMode` is never reused for this frontend — a
deliberate architectural choice to keep the two unattended-execution
mechanisms independently tunable.

The final whole-branch review caught two real gaps neither task-scoped
review could see: `--mcp-server --acp` silently started an ACP server
instead of erroring, because `--acp`'s own unconditional early return in
`main.rs` made a later `--mcp-server`-side mutual-exclusion check
unreachable dead code (fixed by hoisting the check above both branches);
and the single line that actually enforces the `edit`/`execute` tier
boundary (`registry.exclude(...)`) had zero real test coverage — deleting
it left all 63 then-existing tests passing. Fixed by extracting a
`tier_registry` helper and a test that filters a realistic registry
across all three tiers, verified to genuinely discriminate by deliberately
breaking the exclusion and watching it fail before restoring the fix. The
review also found a real architecture question, resolved by explicit
decision rather than silently either way: `code_reply` originally held
the session-map lock for an entire agentic turn, serializing every
session's turns system-wide despite `max_concurrent_sessions` implying
real concurrency — fixed with a check-out (`take`)/run-lock-free/check-in
(`put_back`) pattern, independently re-reviewed via hand-traced control
flow.

**KV-cache persistence (`aivyx-kvcache`) — shipped.** For a `llama-server`
backend (`[backend] kind = "llama_server"`), `aivyx-coder` now persists
each session's stable-prefix KV-cache (system prompt + tool defs + repo
map) to disk via the standalone `aivyx-kvcache` crate/repo, so a fresh
process's first turn on a previously-seen repo can restore rather than
re-prefill that prefix — a new `KvSlotPool` (`aivyx-llm`) checks out a
numeric llama-server slot per session, `Agent::ensure_kv_slot_checked_out`
warms/restores/saves it against a `LlamaServerSlotStore`, gated end-to-end
on a best-effort `/props` probe so every failure mode is fail-open (never
blocks startup, never surfaces as an error). New `kvcache_max_bytes`
config knob (default 10 GiB) and a `deny_paths` default protecting the
store directory. See `README.md`'s "KV-cache persistence" section for
setup, including the one load-bearing requirement (`llama-server`'s own
`--slot-save-path` must match exactly). The final whole-branch review
caught and fixed three real issues before merge: an unbounded `/props`
probe client that could hang startup indefinitely against a slow-loading
model, a failed `restore_into_slot` (`Ok(false)`, not `Err`) being
silently discarded instead of falling through to re-warm (leaving a key
stuck cold forever), and a slot-pool leak if the warm-up future was
dropped mid-flight before the checked-out id was recorded.

**Specialist deny-paths attenuation — shipped (2026-09-21).** Previously,
a delegated "specialist" sub-agent (`delegate_to_specialist`/
`spawn_specialist`) ran under the exact same, unattenuated
`ConfirmationGate`/`ExecutionConfiner` instances as the lead — only which
*tools* were visible to it was scoped, not which paths those tools could
touch. Fixed with a new `crates/aivyx-core/src/specialist_enforcement.rs`
(`scoped_gate_and_confiner`, `SpecialistEnforcementIngredients`), which
builds each specialist its own gate/confiner from the lead's
`deny_paths` unioned with that member's own `TeamMember.extra_deny_paths`
(`aivyx_team::effective_deny_paths`); wired into both delegation paths
(`delegate_to_specialist.rs`, `specialist_sessions.rs`) and constructed
in `crates/aivyx/src/agent_builder.rs`. A no-op for the shipped default
roster today — no member in `aivyx_team::default_coding_roster()` sets
`extra_deny_paths` yet — but the enforcement path is real and proven: a
specialist's own `extra_deny_paths` genuinely blocks a `write_file` the
lead itself could do, confirmed by both a real test (and its positive
control, proving the same test fails for the right reason when
`extra_deny_paths` is absent) and by hand-tracing confiner propagation
and `MoveFileTool`'s both-endpoint check. See
`docs/superpowers/specs/2026-09-21-specialist-deny-paths-attenuation-design.md`.
Deferred follow-ups from the final review are tracked below.

**Custom roster loading — shipped (2026-09-22).** `[team] roster_path`
lets a user point at a TOML file (`{lead, members: [{name, role, persona,
tool_allowlist, extra_deny_paths}]}`, `aivyx_team::TeamConfig`'s existing
schema) instead of always using the fixed `default_coding_roster()` — the
piece that makes the just-shipped `extra_deny_paths` attenuation above
actually reachable/configurable. Loaded and validated once, in a new,
directly-testable `resolve_team_config` (`crates/aivyx/src/agent_builder.rs`,
mirroring `build_llm_backend`'s own extraction precedent in that file),
against the real registered tool-name snapshot — the same one
`team_parent_registry` is itself cloned from, so a custom roster's
`tool_allowlist` still can't name any of the six mission/specialist-session
tools, unchanged from before this feature existed. Any failure (missing
file, unparseable TOML, or a real `TeamConfigError` — unknown lead,
duplicate member, unknown tool) refuses to start rather than silently
falling back to the default roster or running with a broken one.
`TeamConfig::validate` is now also called for the *default* roster,
closing a real, previously-latent gap where nothing in production code
ever validated it. See
`docs/superpowers/specs/2026-09-22-custom-roster-loading-design.md`.

**`/clear` resets real mission/specialist-session state — shipped
(2026-09-22).** Previously, `Agent::clear_conversation()` (the real
implementation behind the TUI's `/clear`) cleared `history` and the task
list but had zero knowledge of `MissionPlan`/`SpecialistSessionPool` — the
TUI's own displayed panels reset correctly, but the *real* shared
`Arc<Mutex<MissionPlan>>` and `SpecialistSessionPool` a later
`decompose_task`/`verify_output`/`synthesize_results`/`spawn_specialist`
call actually reads/writes were untouched, so those calls could
"resurrect" the panel with stale pre-clear content. Fixed: `Agent` gains
two new optional fields (`mission_plan`, `specialist_session_pool`) with
setters mirroring the existing `set_repo_map`/`set_injection_taint`
convention, wired in `agent_builder.rs` from the same handles already
threaded to `BuiltAgent`; `SpecialistSessionPool` gained a new synchronous
`close_all()` (verified correct without awaiting `forward_task` — dropping
a parked session's `Agent` closes the event channel either its own
`sub_tx` or `barrier_tx` end reads from, so the forwarding task ends on
its own either way); `clear_conversation()` now resets the `MissionPlan`
to its pristine startup default and calls `close_all()`, both only when
`[team] enabled = true` (both fields stay `None`, a genuine no-op,
otherwise). Whole-branch review traced the handle-sharing end-to-end
(`agent_builder.rs`'s `mission_plan`/`specialist_session_pool` locals are
the *same* `Arc`/pool clones handed to `MissionToolsConfig`/
`SpecialistSessionsConfig`, confirmed via `SpecialistSessionPool`'s
`Clone`-over-`Arc<Mutex<_>>` derive) and caught a real, since-fixed issue:
this branch's own bug description had been living as a doc comment on
`AgentEvent::MissionsUpdated` (`agent/types.rs`), explicitly telling future
maintainers the resurrection was a permanent, accepted limitation — now
corrected to describe the actual, fixed behavior. See
`docs/superpowers/specs/2026-09-22-clear-resets-real-mission-state-design.md`.
Deferred follow-ups from the final review, none blocking: DRY the
pristine-`MissionPlan` literal (now duplicated three ways —
`agent_builder.rs`, `agent/mod.rs`, `mission_tools.rs` — behind
`MissionPlan::default()`, once `MissionPlan` derives `Default`);
`close_all()` drops parked sessions while holding the pool mutex, unlike
`close_specialist`'s drop-outside-the-lock convention — harmless today
since `Agent::drop` only releases a kv-cache slot, but worth aligning if a
future `Drop` impl ever touches the pool; the `agent_builder.rs` wiring
itself (the two `if let` setter calls) has no dedicated regression test —
deleting them would silently no-op the whole fix while every
`clear_conversation`-level test still passes, since those tests set the
handles by hand; and a `/clear`-vs-in-flight-`query_specialist` race is
structurally possible (`pool.take()` removes a session for a turn,
`close_all()` runs, `put_back()` would resurrect it) but not reachable
today since the TUI handles `/clear` and turns sequentially on one input
loop — would need to cooperate with `take`/`put_back` if `/clear` is ever
made concurrent with a running turn (e.g. a future cancel-and-clear
affordance, or ACP wiring).

**Parked specialist-session persistence across restart — shipped
(2026-09-22).** A parked specialist session (opened via `spawn_specialist`)
previously lived only in-memory in `SpecialistSessionPool` — a process
restart silently lost it, and `query_specialist`/`close_specialist` against
its old `session_id` failed (already a clean, actionable error, not a
crash). Fixed with lazy, on-demand rehydration: `SessionState` (the lead's
own existing `--resume` persistence format) gained a `specialist_sessions:
Vec<PersistedSpecialistSession>` field (`{session_id, member, history}`,
`#[serde(default)]`), populated by `Agent::persist()` from the lead's
`specialist_session_pool` handle. `SpecialistSessionPool` gained a second
internal map, `dehydrated`, seeded from disk at `--resume` time. On a live
lookup miss, `query_specialist` now also checks `dehydrated`; if found, it
rebuilds a real `Agent` via the same `build_specialist_agent` helper
`spawn_specialist` already uses (so a specialist rehydrates under its
*current* roster attenuation/persona, not a stale snapshot), restores the
persisted history onto it, and re-parks it as a normal live session —
transparent to the model. `close_specialist` on a dehydrated-only id just
discards the record, no `Agent` ever built. Dehydrated sessions count
against `max_concurrent_specialist_sessions` (bounding the persisted
file's growth across many restart-without-closing cycles) but don't expire
from inactivity, unlike live sessions. See
`docs/superpowers/specs/2026-09-22-specialist-session-persistence-design.md`.

The final whole-branch review caught a real, only-visible-at-whole-branch-
scope regression: `close_all()` (`/clear`'s own specialist-session reset,
shipped in the previous phase) only cleared the LIVE map, not the new
`dehydrated` one — so an explicit `/clear` didn't actually discard
persisted specialist sessions, and the very next `persist()` call
immediately wrote them right back to disk, contradicting `close_all`'s own
doc comment. Fixed (`close_all` now clears both maps) with a test proving
non-vacuity via the union-aware `snapshot_for_persistence()` accessor
rather than the live-only `open_sessions()`, which would have passed
trivially either way. A second finding — no test proved `Agent::persist()`
itself actually wired the pool's snapshot into the saved file, as opposed
to the pool's own snapshot logic and the tool-level rehydration logic
being separately correct — was closed with a dedicated round-trip test.
Both findings were independently verified via control experiments
(reverting each fix and confirming its new test fails).

Deferred follow-ups from the final review, none blocking: a rehydrated
session doesn't emit `AgentEvent::SpecialistSessionsUpdated`, so the
TUI/ACP panel under-reports the live set until the next `spawn_specialist`/
`close_specialist` refreshes it (self-healing, cosmetic); `persist()` now
deep-clones up to `max_concurrent` specialist histories on top of the
lead's own on every turn (bounded, acceptable, worth knowing before the
cap is ever raised); rehydrating a session makes a previously-immortal
(no-expiry) dehydrated session subject to the live idle-timeout again,
including eventual removal from disk if left untouched afterward — correct
per the design, but a sharp edge not written down anywhere a future
maintainer would find it without reading the source.

See `docs/HISTORY.md` for the full phase-by-phase narrative behind
every item above.

## Backlog — capability opportunities, not yet scheduled

A follow-up audit (2026-07-22) covering security posture, tool coverage,
test quality, and documentation closed 10 findings directly (argument-blind
MCP Always-Allow cache, sub-agent injection-taint isolation, thin
`deny_paths` defaults, five documentation-accuracy fixes, an untested
injection-scan tie-break rule, unlabeled web_fetch/web_search injection
sources, and Plan mode not surviving `--resume`). The remaining finding,
verification test-selection, needed a real design pass of its own (tool-trait
shape, permission/`ActionKind` wiring, config surface) rather than a
same-session patch; it shipped as `[verification] scoped_command` (see
"Verification test-selection" above). With that done, this audit's
backlog is now fully resolved.

A fresh audit (2026-07-28) covering the same four dimensions against the
current codebase found two documentation-accuracy gaps, fixed directly
(the autonomous-mode paragraph was missing `repl_start`/MCP-tool/
`remember_preference` denials added since it was written; the TOCTOU
known-limitation bullet was missing `patch_file`, which has the identical
resolve-twice exposure), and two genuine new-capability gaps, both since
shipped: `deny_paths` basename-glob matching and `delegate_task` REPL
isolation (see "Current status" above for both). With both done, this
audit's backlog is now fully resolved.

No new capability opportunities were found in test quality or the
security/gate-tier-order/Landlock dimension this pass — see
`docs/HISTORY.md`'s "2026-07-28 capability audit" chapter for the full
account of what was checked.

A follow-up audit (2026-07-30) covering the same four dimensions against
everything shipped since 2026-07-28 (`deny_paths` basename-glob matching,
`delegate_task` REPL isolation, Landlock + `aivyx-repomap` basename-glob
enforcement, Docker Model Runner docs, and the `wiki_pointer_lines`/
`Agent::refresh_agents_files` `deny_paths` fixes) found nothing new in
security posture, tool coverage, or test quality. One documentation-only
gap: this entry itself was missing — `docs/HISTORY.md`'s Docker Model
Runner chapter states that whether Docker's default seccomp profile
blocks the Landlock syscalls needed for `aivyx-coder`'s own sandbox is
"logged as a separate, real, future design question," but no
corresponding backlog entry existed here. ~~It's a real open question,
not yet scheduled: **does containerizing `aivyx-coder` itself defeat its
own Landlock enforcement?** Docker's default seccomp profile likely
blocks `landlock_create_ruleset`/`landlock_add_rule`/
`landlock_restrict_self` (reasonably corroborated via research, not
empirically confirmed — no Docker daemon was accessible to test
directly).~~ **Empirically resolved 2026-09-21 — the answer is no, it
does not.** A real Docker daemon (v29.7.2) was accessible this session.
Built a minimal static (musl) C probe performing the full real Landlock
restriction cycle — `landlock_create_ruleset` (a genuine ruleset, not
just an ABI-version query), `landlock_add_rule` (a real path-beneath
rule granting read access to `/`), `prctl(PR_SET_NO_NEW_PRIVS)`, then
`landlock_restrict_self` — and ran it via a completely default
`docker run` (no `--privileged`, no custom `--security-opt`, no
`--cap-add`) against a stock `alpine:latest` image. All three syscalls
succeeded end to end, identical to the bare-host baseline (ABI version
9 both times). Confirmed this wasn't accidentally privileged/unconfined
three independent ways: `cat /proc/self/status` inside the container
showed `Seccomp: 2` (`SECCOMP_MODE_FILTER`, a real BPF filter active —
not `0`/disabled) and `Seccomp_filters: 1` (exactly one filter, Docker's
own default, not zero for unconfined or several for a custom stack);
`docker info --format '{{.SecurityOptions}}'` reported
`profile=builtin` (Docker's real, current built-in default profile, not
`unconfined`); and the container's effective capability set matched
Docker's normal non-privileged default, not a privileged/full set.
**Conclusion**: Landlock's three syscalls aren't on Docker's default
seccomp profile's denylist (that profile blocks a fixed list of ~44
specifically dangerous syscalls — `ptrace`, `mount`, `reboot`, and
similar — rather than allowlisting only pre-approved ones, so a newer
syscall like Landlock's, added in kernel 5.13, passes through
unaffected unless Docker's maintainers specifically added it to the
denylist, which they haven't). A containerized `aivyx-coder` build does
**not** need `require_enforcement: false` and does **not** silently run
unconfined — Landlock enforcement works identically inside a
completely default container as on the bare host. This **unblocks**
the original, larger "package aivyx-coder as a container for end users"
idea this question was gating — see `docs/HISTORY.md`'s "Docker Model
Runner serving support" chapter for the prior descoping story, now
superseded by this finding. (Caveat, worth remembering: this result is
specific to the kernel/Docker version tested — v29.7.2, kernel Landlock
ABI 9 — and to `alpine:latest`'s own base image; a different
kernel/Docker/container-runtime combination, or a host running Docker
inside another layer of containerization — e.g. Docker-in-Docker, a
managed CI runner, gVisor/Kata — was not tested and could plausibly
differ, since some of those add their own additional seccomp/namespace
restrictions on top of Docker's own default.)

A follow-up audit (2026-08-01) covering the same four dimensions against
everything shipped since 2026-07-30 (the real PTY for `repl_start`/
`repl_send`, and the slash command framework) found nothing new in
security posture, tool coverage, test quality, or documentation. Two
risks specific to those features' own history were checked with extra
scrutiny and confirmed still closed: the `O_CLOEXEC` fix (no raw-fd-open
site anywhere in the workspace reintroduced a similar leak) and the
exhaustive `AgentEvent` match count (all three sites — `aivyx-tui`'s
`handle_agent_event` and `sub_agent_event_text`, and `aivyx-acp`'s
`translate_event` — still explicitly handle `ConversationCleared`, no
fourth site and no silent wildcard arm anywhere). See `docs/HISTORY.md`'s
"2026-08-01 capability audit" chapter for the full account of what was
checked.

The final whole-branch review of cross-session memory (`aivyx-recall`,
2026-08-09 — see `docs/HISTORY.md`'s own entry for that chapter) found
and closed a real bug before merge (`memory_write`/`memory_forget`
sharing one Always-Allow cache key) plus three gaps (a cross-repo
dependency that would have broken on the next release tag, a missing
`deny_paths` default, a missing `docs/HISTORY.md` entry). Three smaller
findings from that same review are logged here rather than fixed ad hoc:

- ~~`repl_send`/`repl_stop` share the identical `PermissionKey`
  collision class Fix 1 (above) just closed for `memory_write`/
  `memory_forget`.~~ **Fixed 2026-08-10.** Confirmed by reading
  `confirmation.rs` directly that this one was never actually
  exploitable — `ActionKind::Interact` returns `Allow` unconditionally
  before the function ever reaches the Always-Allow cache, so it never
  touched `self.always_allow` in either direction. Both tools'
  `PermissionTarget::Other` strings are now tool-qualified
  (`"repl_send session"`/`"repl_stop session"`) as defense-in-depth
  against a future refactor that starts caching `Interact`, backed by a
  new gate-level test proving the prompter is never even called for an
  Interact request, plus a tool-level test proving the two targets now
  differ. The broader "audit every `PermissionTarget::Other` producer in
  the workspace" sweep this bullet also called for was not done —
  `repl_send`/`repl_stop` were the only other pair found, not a
  full audit.
- ~~`aivyx-recall` (the new sibling repo) has no `CLAUDE.md`, isn't
  listed in the root `~/Projects/Rust/CLAUDE.md` workspace table~~
  **Resolved.** `aivyx-recall` has its own `CLAUDE.md`; root `CLAUDE.md`
  now points at the new `aivyx-ecosystem` repo (`README.md`/`ROADMAP.md`/
  `GLOSSARY.md`), which lists it and keeps the table current going
  forward instead of duplicating it here.
- ~~The new `write_approval_does_not_satisfy_a_forget_on_the_same_topic`
  gate test lives in `aivyx-sandbox`, which has no dependency on
  `aivyx-tools`~~ — it proves the cache mechanism directly with
  hand-built `PermissionRequest`s, but can't itself detect a regression
  in `memory_write.rs`/`memory_forget.rs`'s actual `permission_request`
  output (that's covered by each tool's own updated unit test instead,
  as two decoupled halves rather than one end-to-end proof). ~~An
  `aivyx-tools`-side test driving both tools' real `permission_request()`
  through a real `ConfirmationGate` would close the loop properly.~~
  **Fixed 2026-08-26.** Added
  `write_approval_does_not_satisfy_a_forget_via_real_tool_output` in
  `aivyx-tools/src/lib.rs`, driving both tools' real `permission_request()`
  output through a real `ConfirmationGate::check()`. Mutation-proofed: made
  `memory_forget.rs`'s target collide with `memory_write.rs`'s, confirmed
  the test failed (1 prompt instead of 2), restored.

**New backlog, from the MCP-server frontend's own final review** (see
"MCP-server frontend — shipped" above for the two Important findings
that WERE fixed before merge; these Minor ones were deliberately
deferred, not overlooked):
- ~~`code`/`code_reply` construct a fresh `CancellationToken::new()` per
  call that nothing ever cancels — an MCP `notifications/cancelled` or a
  client disconnect currently cannot stop a runaway turn early;
  `max_iterations` is the only real bound.~~ — **Fixed 2026-09-21.** Both
  tools now accept a `CancellationToken` parameter, auto-injected by
  `rmcp`'s own `#[tool]` macro system per-request (via its
  `FromContextPart` extractor — no manual `request_id` tracking needed),
  threaded straight into `run_bounded_turn`. Confirmed the request-handling
  task itself isn't forcibly dropped on cancellation — only the token
  flips — so the earlier "`code_reply` future dropped mid-turn, `put_back`
  never runs" worry doesn't materialize for a graceful MCP-level cancel;
  it remains a real risk only for an actual raw connection/process death,
  out of reach of any code-level fix here.
- ~~`run_bounded_turn`'s `cap_hit` (`session.rs`) is `result.is_ok() &&
  agent.last_turn_paused()`, which is also true if a turn stopped via
  cancellation rather than genuinely exhausting `max_iterations` — today
  unreachable (see above), but would mislabel a cancelled turn as
  "reached its iteration budget" the moment real cancellation is wired
  in. Distinguish via `iterations_used >= max_iterations` instead.~~ —
  **Fixed 2026-09-21**, alongside the item above (the two were always
  coupled: this one only became a reachable bug once real cancellation
  existed). The stop-reason check is now a 3-way, explicitly-ordered
  chain: cancellation is checked first and unconditionally (found mid-fix
  to need this — gating it on `agent.last_turn_paused()`, as this entry
  originally proposed, turned out to make that branch structurally
  unreachable, since `run_turn_inner`'s own cancellation checkpoints
  always fire before the code that could set that flag), then
  injection-taint, then genuine budget exhaustion via
  `iterations_used >= max_iterations` as originally proposed.
- ~~`[mcp_server].session_ttl_secs`/`max_concurrent_sessions` accept `0`
  with no startup validation~~ — **Fixed 2026-08-26.**
  `validate_mcp_server_session_limits` in `crates/aivyx/src/main.rs` now
  rejects both at startup (matching the existing `max_access_level`
  rejection), called right after the `--mcp-server` flag's config is
  parsed. Mutation-proofed: temporarily short-circuited the `ttl_secs ==
  0` check, confirmed the rejection test failed, restored.
- ~~The three tier tool-name lists in `tiers.rs` are string denylists
  against `ToolRegistry::exclude`, which is a documented no-op for an
  unregistered name~~ — correct today (every name independently verified
  against real registered tools during the final review), but a future
  tool rename in `aivyx-tools` would silently widen a tier with no test
  failure. ~~A test asserting every listed name is actually present in a
  full registry would make a rename loud instead of silent.~~ **Fixed
  2026-08-26.** Added `every_excluded_tool_name_matches_a_real_registered_tool`
  in `tiers.rs`, constructing all 14 real tools and asserting each's
  `Tool::name()` matches. Mutation-proofed: corrupted one `EDIT_ONLY`
  entry, confirmed the test failed, restored.
- ~~`code`'s turn-error path discards both the `Agent` and any
  accumulated partial text (the session never reaches `insert`), while
  `code_reply`'s turn-error path keeps the session alive via `put_back`
  regardless of outcome~~ — a transient backend failure on a session's
  first turn loses everything; the identical failure on its second
  doesn't. Defensible (a `code` failure never had a session identity to
  preserve in the first place), ~~but undocumented as an intentional
  asymmetry.~~ **Fixed 2026-08-26.** Documented at both call sites in
  `server.rs`, cross-referencing each other — no behavior change, this
  was always the intended shape.

**New backlog, from KV-cache persistence's (`aivyx-kvcache`) task and
final reviews** (see "KV-cache persistence — shipped" above for the three
Important findings that WERE fixed before merge; these were deliberately
deferred, not overlooked):
- `KvSlotPool` has no concurrent-checkout stress test — a nice-to-have
  regression guard for its `Mutex<HashSet<u32>>` checkout logic, not a
  known bug.
- The warm-up request sends the *unfiltered* tool list, while real turns
  filter it by plan-mode/prompted-edit/autonomous-mode — this narrows
  cache reuse in those modes (a plan-mode turn's real tool list won't
  match the warmed prefix hash), but is not a correctness risk.
- ~~No `info`-level observability for kvcache hit/miss/save~~ — only `warn`
  on failure previously, so an operator couldn't easily tell from normal
  logs whether the feature was silently never engaging. **Fixed
  2026-08-26.** `ensure_kv_slot_checked_out` now logs `info` on hit, miss,
  and successful save, each carrying `prefix_hash`/`slot_id`.
- ~~Multi-process contention: `KvSlotPool`'s deterministic lowest-first
  checkout means two concurrent `aivyx-coder` processes against one
  shared `llama-server` both check out slot 0 first, pinning both
  sessions to the same physical slot and mutually invalidating each
  other's cache. A lock file or a per-process pool-index offset would
  fix this.~~ — **Fixed 2026-09-21.** `SlotPoolLock`
  (`crates/aivyx-llm/src/slot_pool_lock.rs`) claims a distinct starting
  offset per process via a real OS-level advisory file lock, scoped by a
  stable hash of the `llama-server` origin, held for the process's
  lifetime (auto-released on exit or crash). Falls back to offset 0 on
  any lock-acquisition failure — confirmed at final review to be a
  strict improvement over the old behavior even in that fallback case,
  never a regression (the overflow subset beyond real slot count still
  contends, which is unavoidable once genuinely oversubscribed).
  **Known real gap, not fixable in this repo**: this only coordinates
  `aivyx-coder` processes against each other — `aivyx-pa`, which
  `README.md`'s "KV-cache persistence" section explicitly documents as
  able to share the same store and `llama-server`, has no equivalent
  locking and will still default to slot 0, so the exact collision this
  fix targets can still occur in that specific cross-product scenario.
  The natural fix is lifting `SlotPoolLock` into the shared
  `aivyx-kvcache` crate both products already depend on — cross-repo
  follow-up, not scoped here.
- Cache-key volatility: any repo-map/tool-set/system-prompt change mints
  a new, potentially hundreds-of-MB cache entry, so real-world hit rates
  depend heavily on how stable a given project's own context is — a
  known, plan-mandated trade-off (see README's "KV-cache persistence"
  section), not a bug.
- ~~`LlamaServerSlotStore`'s own HTTP client (`aivyx-kvcache`, not this
  repo) has no per-request timeout on `restore_into_slot`/`save_from_slot`~~
  — the same unbounded-hang class the `/props` probe fix closed here, but
  on the *per-turn* path (`ensure_kv_slot_checked_out` runs at the top of
  every `run_turn`, before the cancellation check). A hung llama-server on
  a slot restore/save stalled the turn indefinitely and un-cancellably.
  **Fixed 2026-08-26** upstream in `aivyx-kvcache` (`src/llama_server.rs`,
  commit `59631f5`): added `SLOT_HTTP_TIMEOUT` (60s -- generous for
  multi-GB transfers, but finite) to the client `open()` builds.
  Mutation-proofed with a short-timeout test variant, not the real 60s
  value. `aivyx-coder`'s own pinned rev is unchanged by this entry --
  bumping it to pick up the fix is separate follow-on work.
- ~~The `hit` pre-check in `ensure_kv_slot_checked_out` (a `store.find()`
  call) is now fully redundant with `restore_into_slot`'s own internal
  `find()`~~ — **Fixed 2026-08-26.** Collapsed to one lookup; `Ok(true)`/
  `Ok(false)`/`Err` from `restore_into_slot` alone now drives the
  warm-up decision. No behavior change, one fewer HTTP request per
  session start.
- ~~No regression test exercises the three behavioral fixes from the final
  review (`Ok(false)` reaching the warm-up path, the `/props` probe
  client carrying a timeout, `kv_slot_id` being set before the first
  `.await`) — verified by code reading at review time, not by an
  automated test. `agent_builder.rs`'s kvcache-construction block also has
  zero test coverage of its own, since it's inline in `build_agent`~~ —
  same structural gap shape as the MCP-server tier-boundary finding from
  the previous chapter. **Fixed 2026-08-26.** Added two wiremock-driven
  regression tests in `aivyx-core/src/agent/tests.rs` (restore Ok(false)
  → warm-up; kv_slot_id recorded before the first await, no leak on
  cancellation) and extracted+tested `kv_cache_props_client()` in
  `agent_builder.rs`. `parse_llama_slots_info` (the JSON half of the
  block) already had its own tests in `aivyx-llm`; a full wiremock-driven
  end-to-end test of `build_agent`'s entire block would need a complete
  `Settings`/`Cli` fixture this crate has no harness for, left as a
  heavier follow-up. All three new tests mutation-proofed.

**New backlog, from specialist deny-paths attenuation's
(`crates/aivyx-core/src/specialist_enforcement.rs`,
`delegate_to_specialist.rs`, `specialist_sessions.rs`,
`crates/aivyx/src/agent_builder.rs`) own final review** — deliberately
deferred, not overlooked:
- `scoped_gate_and_confiner` does a real (potentially slow) filesystem
  walk to build the Landlock ruleset on *every* `delegate_to_specialist`/
  `spawn_specialist` call, unlike the lead's own gate/confiner, built
  once per process — synchronous inside an async fn, no `spawn_blocking`.
  Memoizing the `(gate, confiner)` pair per `(member name, cwd)` would
  fix this and also fix the next item in one change.
- A specialist's Always-Allow cache resets on every delegation call, not
  just once per session, since a fresh `ConfirmationGate` is built each
  time — meaning the same command re-prompts (interactive mode) or gets
  denied (autonomous mode, since a previously-lead-earned approval no
  longer applies) on every single delegation, not just the first.
- No test exercises the confiner (OS-level Landlock) side of this
  feature directly — only the gate side, via `write_file`. A Linux-gated
  test asserting a specialist's `run_command` genuinely cannot read/write
  a file under its own `extra_deny_paths` would be the highest-value
  addition.
- `GrepTool`/`GlobTool`/`RepoMap` still carry the lead's `deny_paths`,
  not the specialist's narrower union — and this is a real content leak,
  not just an existence leak: `GrepTool` returns full matched line text,
  so a specialist holding `grep` in its `tool_allowlist` can read the
  complete content of any small file (under `MAX_MATCHES = 200` lines)
  inside its own `extra_deny_paths`. See the design spec's "What this
  spec does not decide" section for the corrected severity writeup.
