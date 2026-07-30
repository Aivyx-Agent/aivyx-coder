# aivyx-coder Roadmap

_Last updated: 2026-07-30_

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
(see below). 473 workspace tests; every security-critical behavior also
proven by live E2E against real serving.

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

**Docker Model Runner serving support — documented, pending live
verification.** A new `README.md` "Serving" subsection covers Docker
Model Runner (DMR) as another local-LLM backend option — no code
changes needed, since `base_url` already targets any OpenAI-compatible
endpoint generically. Unlike every other backend documented in this
project, none of this subsection's technical claims (the exact `base_url`
path, the context-window default behavior, whether tool-calling works
end-to-end through aivyx's native edit format) have been confirmed
against a real running instance yet — see `docs/HISTORY.md` for the full
account of what's confirmed vs. still open.

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
