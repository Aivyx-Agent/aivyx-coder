# aivyx-coder Roadmap

_Last updated: 2026-07-20_

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
autonomous mode (`--auto`), `/council` multi-model deliberation, AGENTS.md
project instructions, `web_fetch`/`web_search`, full MCP client support, a
startup probe of the *served* context window, goal-bounded turn pausing
instead of a hard iteration-cap failure, and enforced post-edit
verification with automatic fix-and-retry. 452 workspace tests; every
security-critical behavior also proven by live E2E against real serving.

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

**In flight / next**: nothing pre-scoped remains. The next real context is
a bare-metal test-rig trial (previously used for the sibling Aivyx-Agent
project) — the original motivating goal behind closing all 4
capability-gap sub-projects — not yet started as of this writing. See
`docs/HISTORY.md` for the full phase-by-phase narrative behind every item
above.
