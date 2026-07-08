# aivyx-coder Development Roadmap

_Last updated: 2026-07-08_

This roadmap synthesizes two things: (1) an honest inventory of what aivyx-coder
can and can't do today, based on the audit and capability testing already done
against this codebase, and (2) deep research into how existing coding-agent
frameworks (Claude Code, Aider, OpenHands/SWE-agent, Codex CLI, Cline,
Cursor/Windsurf) are architected, what tools they converge on, and why. Where
a claim below comes from that research it's cited; where it's carried over
from this project's own testing it says so; a couple of items on local-model
tool-calling internals (grammar-constrained decoding, exact Landlock ABI
version numbers) are marked **[unverified]** because the research pass that
was covering them was cut off by a session limit mid-way — treat those as
directional, not committed fact, until spot-checked.

## Where things stand today

**Built and working** (foundation + ConfirmationGate passes, commits
`e958cdb`, `146dd22`, `23cfbd8`):
- Streaming chat against any OpenAI-compatible local backend (Ollama/vLLM/llama.cpp), multi-turn history, cancellation (partially — see gaps).
- `read_file` / `write_file` / `edit_file`, each behind `ConfirmationGate`: `deny_paths` hard-block (now symlink-safe), reads auto-allow, everything else prompts with a per-exact-target Always-Allow cache, diff preview via `similar`.
- A ratatui TUI with a live-streaming chat view and a permission-confirmation modal.

**Verified real-world behavior** (see memory: `project_aivyx_coder_ornith_capability_test`):
- A 9B model (`ornith:9b`) completes one clean read→act tool cycle, then reliably degrades — gets stuck re-reading the same file and eventually hallucinates content that isn't there, rather than taking further action.
- A 27B model (`qwen3.6:27b`) completes a full 5-turn edit task correctly, ~7x slower per call.
- Ollama itself can panic under sustained multi-tool-call load (external bug, not ours) — our error handling degraded gracefully when it did.

**Phase 1 hardening complete** (this pass): cancellation now propagates through the tool-dispatch loop and the next LLM request; `max_tool_iterations_per_turn` is wired from config (clamped to a minimum of 1); the LLM client has a connect timeout, an idle-stream timeout, and a size cap on accumulated response text; a truncated (`length`/`error` finish reason) response now surfaces as a visible error instead of looking like a normal empty turn; `TerminalGuard` no longer leaks raw mode on partial init failure and `restore_terminal` attempts both cleanup steps independently; tool-result/tool-call/notice transcript lines preserve embedded newlines instead of rendering as one concatenated wall of text; the auto-scroll offset is computed from ratatui's own post-wrap row count instead of pre-wrap logical line count; `write_file` now shows an explicit warning instead of a silent/ambiguous prompt when overwriting an existing binary file; and `SYSTEM_PROMPT`'s tool list is derived from `ToolExecutor::definitions()` so it can't drift. Deliberately deferred (narrow/theoretical, documented in the plan rather than fixed): the `edit_file` TOCTOU, case-sensitive path comparison, relative `deny_paths` entries, the `path_resolve`/`aivyx-config` home-dir divergence, `Tool::execute`'s type-system bypassability, and a couple of efficiency-only findings (history-cloning cost, full-transcript-rebuild-per-redraw). A follow-up review of this same pass caught 5 real regressions it had introduced (a cancellation/history-corruption bug, an off-by-2 in the new scroll fix, an empty-tool-result rendering bug, a `TerminalGuard` alt-screen leak, and an unbounded pre-first-byte hang) — all fixed and live-verified, see memory `project-aivyx-coder-phase1-hardening`.

**Phase 2 investigated, not built** (see the Phase 2 section below and memory `project-aivyx-coder-phase2-investigation`): confirmed via a new raw wire-traffic logger that the `ornith:9b` degradation is a model-generation failure (hallucinated procedural rules, indecision spiraling, then a bare `finish_reason: stop` with no tool call) rather than a client bug or an edit-format problem — no engineering fix identified at this layer.

## What the research says a coding agent needs

Full findings are in the research agent's output from this session; the load-bearing conclusions:

1. **Tool set**: every serious agent converges on the same short list beyond file read/write/edit — content search (grep-equivalent), path search (glob-equivalent), shell execution, git operations, and (for anything beyond toy tasks) a way to run the project's own build/test/lint and read the result. ([Claude Code tool system](https://callsphere.ai/blog/claude-code-tool-system-explained), [Aider docs](https://aider.chat/docs/usage/lint-test.html))
2. **The single most transferable lesson for this project**: Aider deliberately does **not** rely on native function-calling for file edits, even on models that support it — it found structured tool-calling is often *worse* than a well-specified prompted text format (SEARCH/REPLACE-style blocks) for editing quality, and pairs that with a repair loop (on a failed match, show the model the actual nearby content and re-prompt, capped at ~3 retries). This is close to an exact description of the `ornith:9b` failure mode already observed here. ([edit formats](https://aider.chat/docs/more/edit-formats.html), [troubleshooting](https://aider.chat/docs/troubleshooting/edit-errors.html))
3. **Context/codebase understanding for small-context local models**: the field has moved *away* from embeddings/RAG for code (stale indexes, worse precision than exact search — ["grep beat embeddings"](https://jxnl.co/writing/2025/09/11/why-grep-beat-embeddings-in-our-swe-bench-agent-lessons-from-augment/)) and toward two complementary techniques: agentic grep (no index, always fresh) and Aider's tree-sitter **repo map** (compressed, ranked symbol signatures for the whole repo within a token budget, no server required). Both fit a local-first, no-daemon Rust tool far better than embeddings would.
4. **Verification loops are the biggest lever for actual task success**, not prompt cleverness — SWE-bench analyses consistently show iterative edit→test→fix beats single-shot generation. A deterministic compiler/test-runner as the feedback signal is *more* reliable for a weak local model than asking it to self-critique in prose.
5. **Sandboxing**: for a single-binary, no-daemon-assumed local CLI, the field's answer is not Docker — it's kernel-level primitives. Notably, **OpenAI's Codex CLI is itself a Rust codebase** that ships exactly the architecture this project already planned (`ExecutionConfiner` → real Landlock/seccomp instead of `NoopConfiner`): Landlock for filesystem scoping, seccomp-bpf for syscall allow-listing, no privileged daemon. This directly validates the existing design and gives a concrete reference implementation to study.
6. **UX patterns worth adopting cheaply**: a hard read-only "plan mode" (just withhold write-tool grants at the permission layer — trivial on top of the existing `ConfirmationGate`), a persisted/visible task list (more valuable here than in cloud tools, since local sessions get interrupted more often), and context-window-budget visibility in place of the cost tracking cloud tools show (a local model silently degrades on overflow instead of costing money — a worse failure mode to be blind to).
7. **Tool design hygiene**: Anthropic's own ["Writing Effective Tools for Agents"](https://www.anthropic.com/engineering/writing-tools-for-agents) argues for fewer, higher-level tools with explicit descriptions and *actionable* error messages rather than raw errors — directly relevant to revising `read_file`/`write_file`/`edit_file` before the tool surface grows.

## Roadmap

Phases are ordered by dependency and leverage, not by ease. Each closes a
specific gap identified above.

### Phase 1 — Harden what's already shipped — ✅ done
Fixed the audit backlog before adding surface area: cancellation propagation
through the tool-dispatch loop, `max_tool_iterations_per_turn` wired from
config, HTTP/idle timeouts + size caps on `OpenAiCompatBackend`, truncated-response
surfacing, `TerminalGuard`/`restore_terminal` robustness, the tool-result
newline-stripping and scroll-math rendering bugs, `write_file`'s binary-overwrite
prompt clarity, a regression test locking in deny-before-read-auto-allow
ordering, and `SYSTEM_PROMPT` derived from tool definitions. The `edit_file`
TOCTOU and a handful of narrow/theoretical findings were deliberately deferred
rather than fixed — see the "Phase 1 hardening complete" note above. Not yet
done from the original Phase 1 idea: revising the three tool descriptions/error
messages per Anthropic's tool-design guidance — still worth doing before the
tool surface grows, folded into whichever of Phase 2/3 lands next.

### Phase 2 — Fix editing reliability at the root cause — investigated, not building this
Before implementing the prompted SEARCH/REPLACE rework described below, reviewed
the plan against the actual evidence and found it doesn't target the observed
failure: a repair loop already exists for free (`ToolExecutor::dispatch` already
feeds tool errors back into history, and `Agent::run_turn`'s multi-iteration loop
already lets the model retry within a turn), and the `ornith:9b` degradation
happens *before or without* any edit ever being attempted, so an edit-format
change can't reach it. Built an opt-in raw wire-traffic logger
(`AIVYX_DEBUG_LOG`, `crates/aivyx-llm/src/openai_compat.rs`) and re-ran the
original capability-test scenario with it on. Root cause, confirmed from the
raw log: a genuine model-generation failure, not a client bug — history
assembly is correct through every captured request; the model itself sometimes
hallucinates procedural rules that were never in the system prompt, argues with
itself about them in its reasoning trace, and ends the turn with
`finish_reason: "stop"` and zero tool calls instead of executing what it just
described. This looks like an inherent small-model (9B) agentic-reliability
limit, not something fixable at the edit-format or plumbing layer. No further
engineering action planned here — see memory `project-aivyx-coder-phase2-investigation`
for the full findings with log excerpts. Mitigation remains what the original
capability test already showed: prefer a larger model (`qwen3.6:27b`) for
multi-step work.

_Original plan, kept for reference, not being built_: adopt a prompted
SEARCH/REPLACE-style edit format for `edit_file` (Aider's pattern) instead of
leaning solely on native tool-calling for the edit payload, with a bounded
repair loop — on a non-matching edit, feed back the actual nearby file content
and let the model retry, capped at ~3 attempts. Native tool-calling stays the
mechanism for *invoking* tools (that part works fine); this was specifically
about how edit *content* gets communicated and validated — but per the
investigation above, edit content format was never the actual problem.

### Phase 3 — Search and navigation tools — ✅ done
Added `grep` (content search) and `glob` (path search) tools, named to match
Claude Code's own tool names deliberately — well-represented in training
data, which matters for small local models. Built on `ignore` + `grep-searcher`/
`grep-regex`/`grep-matcher`/`globset` (ripgrep's own library crates), so both
respect `.gitignore` and don't follow symlinks by default, same as ripgrep
itself. Both are read-only/auto-allow like `read_file`, extending the
existing `ToolRegistry`/`ConfirmationGate` framework as planned.

Planning this surfaced a real gap before it shipped: `ConfirmationGate::is_denied`
checks `path.starts_with(denied)`, correct for a single-file target but
insufficient for a recursive search — if the search root is merely an
*ancestor* of a `deny_paths` entry, the top-level check passes and a naive
walk would read into the denied subtree anyway. Closed by threading
`deny_paths` into both tools directly (constructor argument, not a broader
`ToolExecutionContext` change) and skipping any walked entry under a denied
path via a new shared `path_resolve::is_denied` helper. Verified this holds
end-to-end: a live pty-driven run against a real local model, with a `secret/`
directory configured as `deny_paths` and containing the *same* search string
as a legitimate file elsewhere in the tree, confirmed `grep`/`glob` only ever
returned the legitimate match — no trace of the denied file's name or content
anywhere in the session, alongside confirming `.gitignore`-excluded content
was also correctly absent. 11 new unit tests across both tools: real matches,
`.gitignore` respected, denied-subtree-as-ancestor-root closed, symlink
escape closed, output capped and clearly reported when truncated.

### Phase 4 — A verification loop — ✅ done
Added `run_command`: the model selects a name from a fixed, user-configured
allowlist (`[[permissions.allowed_commands]]` in config.toml — empty by
default, fail-closed) and gets back exit status plus stdout/stderr, tail-truncated
(not head-truncated, unlike `grep`/`glob`) since the actionable signal in
build/test output is almost always at the end. The model never supplies a
program or arbitrary args — that's what makes auto-caching repeated runs via
the normal Always-Allow flow safe without waiting on Phase 5's real
sandboxing. Spawns through `ExecutionConfiner::confine` (currently
`NoopConfiner`), so Phase 5's `LandlockConfiner` slots in later with no tool
changes needed. Process execution races a concurrent stdout/stderr drain
against a timeout and the existing `CancellationToken`, killing the child on
either — draining concurrently with waiting, not after, avoids a deadlock if
the child fills its pipe buffer before exiting.

Planning surfaced and closed a real bug before it ever shipped:
`PermissionKey::Command` (the Always-Allow cache key) only hashed `program`,
dropping `args` — audit finding #3, previously dead code since no
command-executing tool existed to exercise it. Two allowlist entries sharing
a program (e.g. both using `cargo`) would have had approving one silently
auto-approve the other. Fixed by keying on `(program, args)`; live-verified
end-to-end (see `project-aivyx-coder-phase4-run-command` memory) that a
second command sharing a program with an already-approved one still prompts
independently, while re-running the same command doesn't re-prompt.
`deny_paths` still doesn't cover `Command` targets — deliberately left for
Phase 5, where the model chooses arbitrary commands/args and it actually
matters; this phase's allowlist is fully user-fixed, so it doesn't need it.

### Phase 5 — Shell execution behind real sandboxing
Implement the `ExecutionConfiner` this project's trait already anticipates:
Landlock (filesystem scope) + seccomp-bpf (syscall allow-list), replacing
`NoopConfiner`, following the pattern validated by Codex CLI's own Rust
implementation. Rust crates: `landlock` (already a dependency), `seccompiler`
(new). This phase **must** also close the `ConfirmationGate`/`PermissionKey`
gaps already found for `PermissionTarget::Command` (deny_paths doesn't cover
it, Always-Allow caches by program name only, dropping args) — those are
currently dead code but become live risk the moment a shell tool exists, so
fix them as part of shipping the tool, not after. Add command-level
allowlisting as an additional trust tier beyond raw confirm/deny.

_[unverified] Worth a short research spike before Phase 2/5 lock in their
designs: whether Ollama/llama.cpp's grammar-constrained decoding (GBNF /
JSON-schema-to-grammar, long supported by llama.cpp; vLLM has an equivalent
via outlines/xgrammar) can be made to constrain native tool-call syntax
specifically, not just freeform JSON mode — if so, it could reduce or remove
the need for Phase 2's prompted-format fallback for some backends. Not
confirmed this session; check before committing engineering time either way._

### Phase 6 — Codebase understanding (repo map)
Build an Aider-style repo map: extract per-file symbol signatures (functions,
types) via `tree-sitter`, rank by a dependency/reference graph, inject a
token-budgeted slice into the system prompt. This is the best-fit context
strategy for small local context windows per the research (cheap, no server,
degrades gracefully) — prioritize it over embeddings/RAG, which the field has
moved away from for code specifically.

### Phase 7 — Git integration and checkpointing
A git-ops tool (status/diff/log/commit), shelling out to the `git` CLI to
match the user's real config/credentials (consistent with this project's
existing choice for tool-vs-library tradeoffs). Open design question worth
deciding deliberately rather than defaulting: auto-commit every accepted edit
into the user's real history with an AI-authored message (Aider's approach —
transparent, doubles as an audit trail) vs. a separate shadow git repo for
step-by-step rewind (Cline's approach — less invasive to the user's actual
history). Given this project's emphasis on transparency over magic, auto-commit
to the real repo is the more consistent default, but flag it for confirmation
before building.

### Phase 8 — Agentic UX
A hard **plan mode** (read-only enforced at the permission layer — trivial on
top of the existing gate, just withhold write grants until the user confirms
a plan), a **persisted, visible task list** (more valuable here than in cloud
tools since local sessions are interrupted more often — crashes, slow
inference), **session persistence/resume**, and **context-budget visibility**
(percentage of context window consumed) in place of the cost-tracking cloud
tools show, since local inference has no per-token cost but silently degrades
on context overflow instead — a worse failure mode to be blind to.

### Phase 9 — Stretch goals
LSP integration for exact symbol resolution (complements, doesn't replace,
the repo map and grep tools), MCP support for external tool integration,
sub-agent delegation for context-isolated exploration, and an
architect/editor model-pairing mode (a stronger model plans in prose, a
faster local model executes the mechanical edit) — directly enabled once
Phase 2's prompted edit format exists.

## Notes on sequencing

Phases 1-2 are deliberately reliability-first rather than feature-first: the
research and this project's own capability testing agree that adding tool
surface on top of unreliable tool-calling/editing just multiplies failure
modes rather than fixing the underlying one. Phases 3-4 are the cheapest,
highest-consensus additions (every agent studied has them, none need new
infrastructure). Phase 5 is gated behind Phase 1-4 partly because it's the
biggest single chunk of new complexity (real sandboxing) and partly because
shell-exec is exactly where the `Command`-target permission gaps matter —
better to have the simpler tools' patterns settled first.
