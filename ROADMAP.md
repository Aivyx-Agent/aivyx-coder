# aivyx-coder Roadmap

_Last updated: 2026-07-18_

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
verification with automatic fix-and-retry. 381 workspace tests; every
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

**In flight / next**: nothing pre-scoped remains. All previously-tracked
threads (including the vLLM compat pass) are closed. See `docs/HISTORY.md`
for the full phase-by-phase narrative behind every item above.
