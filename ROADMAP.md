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

### Phase 2 — Fix editing reliability at the root cause — revisited, building scoped (2026-07-11)

**Revisit rationale** (design pass with the user, after Phases 3–8 shipped):
the original investigation below stands — ornith:9b's degradation happens
before any edit is attempted and no edit format can reach it. What the
revisit targets is the *other* claim from the research, which the
investigation never tested: that multiline edit *content* travels badly
through JSON tool arguments (escaping, whitespace fidelity) even on models
whose tool-calling transport works. Our E2Es so far only ever exercised
trivial one-line writes under hyper-directive prompts — the native edit
path has never been stress-tested on real multiline edits.

**User-confirmed forks:** scope is **edits only** (SEARCH/REPLACE blocks
replace how edit_file/write_file payloads travel; every other tool keeps
native calling, which demonstrably works — full text-mode tool invocation
remains unbuilt), and the default format is **decided by A/B evidence**:
the same real multiline-edit tasks run through both paths on `qwen3.5:9b`,
wire-logged, winner becomes the config default, loser stays selectable via
`[backend] edit_format = "native" | "prompted"`.

**Design:**
- Blocks are parsed from the assistant's plain text (Aider's format: a
  path line, `<<<<<<< SEARCH`, old text, `=======`, new text,
  `>>>>>>> REPLACE`; fence-tolerant; empty SEARCH = create the file) and
  **synthesized into ordinary `ToolCall`s** (`ToolCallSource::TextFallback`
  — the variant has been waiting for this since the foundation pass) that
  dispatch through the normal executor. One security path: same gate, same
  diff-preview modal, same plan-mode denial, same checkpoints. No parallel
  edit machinery.
- Malformed blocks get a synthetic call + error result (via the existing
  skipped-result mechanism), so the model receives precise in-history
  feedback and the call/result balance invariant holds.
- In prompted mode, edit_file/write_file stay registered (the synthetic
  calls need them) but leave the model-facing tool definitions; the system
  prompt teaches the block format instead.
- The repair loop already exists (tool errors feed back, iterations retry);
  it gets sharpened for both modes by adding a nearest-miss hint to
  edit_file's zero-match error (locate the closest partial match, quote the
  surrounding lines).

**A/B results (2026-07-11, qwen3.5:9b, 3 tasks × 3 reps per format,
auto-approved modals, success judged from final file state): native 7/9,
prompted 6/9 — format is not the bottleneck.** Every success in *both*
formats was a first-try, single-approval correct edit: no JSON mangling in
native payloads, no match failures in prompted blocks. Every failure in
both formats shared one signature — a ~95s reasoning stall ending without
any action — i.e. the same model-side agentic limit the original
investigation identified, unreachable by either format. The default stays
`native` (it also ran ~45% faster overall); `prompted` remains fully
supported via `[backend] edit_format` / `--edit-format` for models that
genuinely mangle tool-call JSON, which qwen3.5:9b, measured, does not.

It took four rounds to get an honest measurement, and the discards were
more valuable than the verdict:
- Round 1 (pilot, discarded): a 25s quiet-window in the harness was
  killing runs mid-reasoning (the thinking phase streams `delta.reasoning`,
  which the client drops — silence looks like a hang), and the system
  prompt still steered prompted-mode runs toward the withheld edit tools.
- Round 2 (aborted on discovery): **Ollama had been serving a 4096-token
  window all along** — no `num_ctx` in the model, no
  `OLLAMA_CONTEXT_LENGTH` on the service, not settable via `/v1` — so
  `context_tokens = 8192` was fiction and reasoning phases truncated
  mid-think (`finish_reason: length`) across *both* formats, masquerading
  as model unreliability. Fixed with a derived `qwen35-8k` model; README
  documents the trap; the truncation error now explains it; Phase 10's
  window-mismatch probe is the permanent fix.
- Round 3 (discarded): the harness matched the modal's `[y] Allow` hint,
  which ratatui can splice across cell runs — at least one fully-correct
  native run sat unapproved at a perfect diff and was scored FAIL.
- Round 4 (clean, reported above), after two product fixes the rounds
  surfaced: `edit_file` now rejects no-op edits (a confused model looped
  through 11 approved no-change edits believing each worked) and its
  zero-match error quotes the nearest-miss region.
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

### Phase 5 — Shell execution behind real sandboxing — ✅ done
Implemented `LandlockConfiner` (`ExecutionConfiner`, replacing `NoopConfiner`
by default), and a general `run_shell` tool (arbitrary `sh -c` commands,
unlike Phase 4's fixed-menu `run_command`).

Before implementing, checked this section's own claim of "following the
pattern validated by Codex CLI" — stale: Codex CLI's *current* Linux sandbox
is bubblewrap (namespaces), with Landlock kept only as a legacy fallback, and
its seccomp policy is a narrow denylist, not a broad allowlist. Presented the
corrected tradeoff to the user (bubblewrap, matching Codex exactly but adding
a new external binary dependency and materially more complexity, vs. Landlock
+ a seccomp denylist, fitting the existing trait with no new binary
dependency) — user chose Landlock + seccomp denylist. Verified the concrete
integration against the real `landlock` 0.4.5 / `seccompiler` 0.5.0 crate
source before writing any code (not `sandlock-core`, which looked convenient
but is 3 months old with under 500 downloads and no docs — too unproven for
a kernel security boundary): confinement is applied via `tokio::process::Command::pre_exec`
(the forked child, after `fork()` before `exec()` — not the crate's own
one-shot-wrapper example pattern, since aivyx-coder must keep running after
spawning a subprocess), with the full ruleset and compiled BPF program built
in the parent (all allocation front-loaded) so the `pre_exec` closure itself
only calls two verified-allocation-free syscall wrappers.

Filesystem policy (informed by, not copied from, Codex's lesson that
cwd-only reads break real toolchains): write access scoped to cwd + tmp;
read access to cwd + a built-in list of common system/toolchain paths
(`/usr`, `/lib`, `/bin`, `/etc`, `~/.cargo`, `~/.rustup`) plus a config
escape hatch (`sandbox.extra_read_paths`) — deliberately not Codex's
"read everything" default, since Landlock has no negative/deny rule and
carving `deny_paths` entries out of a broad grant would need fragile
sibling-enumeration; a bounded allowlist sidesteps the problem entirely.
Seccomp is a narrow denylist (`ptrace`, `io_uring_*`, `mount`, `reboot`,
module-loading, etc.) with default-allow otherwise — network is
unrestricted by default, matching Codex's own default preset.

This closes the remaining piece of audit finding #3 (`deny_paths` not
covering `Command` targets) via a **better** mechanism than originally
planned: real kernel enforcement instead of a heuristic string-match check,
so the heuristic was deliberately not built at all. The other half of
finding #3 (Always-Allow caching by program name only, dropping args) was
already fixed in Phase 4.

"Command-level allowlisting as an additional trust tier": `run_shell`
reuses Phase 4's `permissions.allowed_commands` list — an exact match
skips confirmation entirely (pre-seeded into `ConfirmationGate`'s
Always-Allow cache at construction, in both the direct `(program, args)`
form and the `sh -c "..."` form, since the two tools have different
invocation shapes) — rather than building a new "is this command pattern
safe" classifier, which is real security engineering in its own right and
easy to get subtly wrong.

Verified live, not just unit-tested: with a Landlock ABI 9 kernel active on
the dev machine, real (not mocked) tests confirm a write inside the granted
root succeeds, a write outside it fails, a read outside the allowlist fails,
and the seccomp filter doesn't break normal commands. A pty-driven E2E run
demonstrated the actual defense-in-depth property this phase exists for: a
shell command that tried to write outside the working directory was
approved by the (simulated) human at the confirmation prompt, and the
**kernel still blocked it** — the write never happened, and the agent
correctly reported the permission failure back rather than silently
succeeding or hanging.

_[unverified] Worth a short research spike before Phase 2/5 lock in their
designs: whether Ollama/llama.cpp's grammar-constrained decoding (GBNF /
JSON-schema-to-grammar, long supported by llama.cpp; vLLM has an equivalent
via outlines/xgrammar) can be made to constrain native tool-call syntax
specifically, not just freeform JSON mode — if so, it could reduce or remove
the need for Phase 2's prompted-format fallback for some backends. Not
confirmed this session; check before committing engineering time either way._

### Phase 6 — Codebase understanding (repo map) — ✅ done
Build an Aider-style repo map: extract per-file symbol signatures (functions,
types) via `tree-sitter`, rank by a dependency/reference graph, inject a
token-budgeted slice into the system prompt. This is the best-fit context
strategy for small local context windows per the research (cheap, no server,
degrades gracefully) — prioritize it over embeddings/RAG, which the field has
moved away from for code specifically.

#### Phase 6 design (agreed 2026-07-11)

**User-confirmed forks:** the map is **injected** as a token-budgeted slice
appended to the system prompt every request (Aider's proven shape — small
local models won't proactively call an optional map tool, and first-turn
orientation is where the map earns its keep), and v1 parses **Rust only**
(one grammar and one tags query to get right; other languages become
mechanical follow-ups once the pipeline is proven; unknown-language files
simply contribute no symbols).

**Mechanism — a new `aivyx-repomap` crate**, dependency-free of the rest of
the workspace (pure input → string):
- **Walk**: `ignore::WalkBuilder` rooted at the cwd — gitignore-aware, no
  symlink following, `deny_paths` excluded — collecting `.rs` files under a
  per-file size cap.
- **Extract** per file via tree-sitter queries: definitions (fn, struct,
  enum, trait, mod, const, static, type alias, macro) each carrying its
  signature line, plus references (called identifiers, used type names).
- **Cache** per-file extraction keyed by `(mtime, size)` — a render pass
  re-parses only changed files; the walk itself is cheap. In-memory only
  (an on-disk cache is a later optimization, not v1).
- **Rank** files by PageRank (plain power iteration, damping 0.85) over the
  cross-file graph: an edge from referencing file to defining file per
  matched symbol name, weighted by reference count. This is the part that
  makes the map *relevant* rather than alphabetical.
- **Render** top-ranked files as `path:` + indented signature lines until
  the token budget (chars/4, same estimator convention as the agent) is
  spent; per-file signature cap so one huge module can't hog the budget.
- **Agent integration**: rendered once per *turn* (not per iteration) via
  `spawn_blocking`, appended to the system prompt like the plan-mode and
  truncation notes — and **counted by the context estimator** (a ~1k-token
  map that compaction can't see would silently eat the window's headroom).
  Active in plan mode: orientation is most valuable while planning.
- **Config**: `[repo_map] enabled = true, budget_tokens = 1024`. The map
  is skipped when the repo yields no symbols (no noise for non-Rust
  projects in v1).

**Deliberately not in v1:** ranking personalization toward
recently-touched/chat-mentioned files (Aider does this; a clean follow-up
once the base map proves itself), on-disk tag caching, and additional
languages.

**Built (2026-07-11), as designed** — new `aivyx-repomap` crate (9 unit
tests: extraction kinds, references, ranking, budget, deny/gitignore
exclusion, cache invalidation, deleted-file eviction), agent integration
with the map's weight visible to the compaction estimator, and a live E2E
that verified through the raw wire capture (`AIVYX_DEBUG_LOG`) that the map
— with the hub file correctly outranking an unreferenced one — was inside
the actual request's system prompt. E2E harness lesson (again): a "model
replied" check that pattern-matches the reply text also matches the typed
prompt's own echo; the `ctx` usage indicator is the honest
request-completed signal.

### Phase 7 — Git integration and checkpointing — ✅ done
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

#### Phase 7 design (agreed 2026-07-11)

**The flagged fork resolved — checkpoint refs, not auto-commit.** The
analysis moved off the original auto-commit lean: a fourth option gives the
same audit trail and rewind capability without its costs. Before each
approved mutating tool call, the worktree is snapshotted to a commit object
stored under `refs/aivyx/checkpoints/<timestamp>` via plumbing (a private
persistent index at `.git/aivyx/index` + `write-tree`/`commit-tree`/
`update-ref`) — never touching HEAD, the user's index, or the worktree.
Fully transparent (plain git objects, inspectable with
`git log refs/aivyx/checkpoints/...`, restorable with `git checkout <ref>
-- <path>`), zero pollution of deliberate branch history, no dirty-worktree
policy needed. Auto-commit was rejected because it interleaves agent noise
into the user's own commit discipline; the shadow repo because hidden state
cuts against this project's transparency ethos. (User confirmed both this
and the commit-tool question below.)

**Checkpoint details:**
- Trigger: in `ToolExecutor::dispatch`, after the gate allows and before
  `Tool::execute`, for any tool with `mutates_outside_session() == true` —
  so arbitrary `run_shell` effects are covered, not just file tools.
- **Security-critical: checkpoints exclude `deny_paths`** via
  `:(exclude)` pathspecs on the internal `git add -A`. Without this, a
  denied subpath inside the worktree (which Landlock carves out of the
  kernel sandbox) would get copied into readable `.git` objects — and
  `git show <checkpoint>:<denied-file>` would read denied content straight
  through the kernel sandbox. Gets its own regression test.
- Checkpoint commits use a fixed synthetic identity (env-forced
  author/committer), so they work on machines with no git identity
  configured; `.gitignore` is respected (no `target/` snapshots).
- Dedup (skip if the tree oid matches the previous checkpoint) and
  retention (keep the newest 50 refs, delete older — objects then age out
  via normal gc). Best-effort: a checkpoint failure logs a warning and
  never blocks the tool; a non-repo cwd disables checkpointing at startup
  with one log line. Config: `[git] checkpoints = true`.
- Restore UX is deliberately v1-manual (documented git commands); a TUI
  rewind can come later — the objects are just git.

**Tools — two, not one, because plan mode's tool filtering is static:**
- `git_read` (`mode: status|diff|log`, optional `path`/`staged`/`count`):
  read-only, auto-allowed, **available in plan mode** — exactly when
  status/diff matter most. Fixed argv shapes built by the tool
  (`status --short --branch`, `diff [--cached] [-- <path>]`,
  `log --oneline -n <count≤50>`); never raw passthrough. Path arguments are
  resolved to absolute paths first (immune to option/pathspec-magic
  injection) and covered by the gate's deny_paths check; status/diff also
  exclude deny_paths via pathspecs so a repo-internal denied subpath's
  contents/names don't leak into model context.
- `git_commit` (`message`, optional `paths`): stages (`git add -A
  [-- paths]`) then commits. `PermissionTarget::Command` with the real argv
  — deliberately NOT a `Path` target, so an Always-Allow can never blanket-
  approve future commits (each message is a distinct cache key). The modal
  preview is `git diff HEAD --stat` plus the untracked-file list — the
  reviewer sees what's being committed at file granularity.
- Both run through the existing confined `process::run` path (timeouts,
  output caps, process-group kill). The confiner's default home read grants
  gain `.gitconfig` and `.config/git` — commit needs the user's identity,
  and this also fixes latent `git` breakage under `run_shell`. GPG signing
  and hooks needing paths outside the sandbox are documented limitations.

**Not doing in v1:** TUI rewind/restore UI; push/pull/branch tools (network
+ far larger blast radius — later phase, if at all); auto-generated commit
messages (the model writes the message in the tool call; with slow local
inference, per-edit message generation would be a UX tax anyway).

**Built (2026-07-11), as designed.** Live-verified end-to-end with
`qwen3.5:9b`: `git_read` status with no modal, an approved `write_file`
leaving a checkpoint ref that held the *pre*-edit content, and `git_commit`
through the preview modal landing in `git log` with a clean worktree.
Findings from the live E2E, all fixed en route:
- **The sandbox never granted `/dev`** — every `2>/dev/null` in a confined
  shell command died with EACCES, a latent Phase 5 gap no unit test caught.
  Fixed by granting exactly `/dev/null`/`zero`/`urandom`/`random` (not
  `/dev` wholesale).
- **The system prompt induced narrate-instead-of-call**: "explain what
  you're about to do before calling" made the small model explain and end
  its turn waiting for permission in chat. Reworded to "say what you're
  doing, then call the tool in the same response — the approval UI only
  appears once you call." Also added a "prefer dedicated tools over
  run_shell" steer after watching the model reach for `run_shell git ...`
  despite `git_read` being available.
- Test-harness lesson recorded for future E2Es: PTY screen-scrape matching
  must be scoped per-turn (a pattern happily matches the *previous* turn's
  modal or the echo of the typed prompt), and a stale binary from a
  `cd`-affected `cargo build` cost one full debugging round — build from
  the workspace root and `strings`-check the binary when tool lists look
  impossible.

### Phase 8 — Agentic UX — ✅ done
A hard **plan mode** (read-only enforced at the permission layer — trivial on
top of the existing gate, just withhold write grants until the user confirms
a plan), a **persisted, visible task list** (more valuable here than in cloud
tools since local sessions are interrupted more often — crashes, slow
inference), **session persistence/resume**, and **context-budget visibility**
(percentage of context window consumed) in place of the cost-tracking cloud
tools show, since local inference has no per-token cost but silently degrades
on context overflow instead — a worse failure mode to be blind to.

**Built (2026-07):** three of the four, brought forward ahead of Phases 6–7
because context overflow and interrupted sessions were the failure modes
actually being hit in local testing. Design decisions worth recording:

- **Context budget**: `backend.context_tokens` config (not auto-detected —
  the OpenAI-compat `/v1` surface doesn't expose the window reliably across
  Ollama/vLLM/llama.cpp), a live colored `ctx used/limit (%)` status-line
  indicator fed by the backend's real `prompt_tokens`, and a
  self-calibrating chars-per-token estimator (starts at ~4, re-calibrated
  from each response's actual usage) that decides when to compact.
- **Compaction** at 80% of the window, down to 60% (hysteresis so it doesn't
  re-fire every turn): first elide oversized tool results to head+tail
  excerpts (one huge `read_file` output is usually the dominant consumer,
  and trimming it is far less lossy than dropping turns), then drop the
  oldest whole turn-groups, cutting only at user-message boundaries so a
  `tool_call` is never separated from its result. Truncation is surfaced to
  both the user (notice line) and the model (system-prompt note) — never
  silent.
- **Session persistence**: saved after every turn (crash-safe), owner-only
  0o600 (the session embeds file contents and command output), one file per
  project directory keyed by FNV-1a of the canonicalized cwd, under
  `~/.local/state/aivyx-coder/sessions/` — deliberately *outside* the
  project so conversation data can't end up committed to its repo. Restore
  is opt-in via `--resume`; the TUI rebuilds the visible transcript from
  the restored history.
- **Task list**: a `set_tasks` tool using whole-list replacement
  (TodoWrite-style) rather than incremental add/update ops — no id
  bookkeeping across turns for small local models to get wrong, and
  self-healing after a malformed call. Verified live: `qwen3.5:9b` used it
  correctly on the first try. Rendered as a bordered TUI panel that only
  takes vertical space while non-empty; persisted with the session. Gated
  behind a new honest `ActionKind::Internal` (auto-allowed — it mutates
  nothing outside the agent's own session state) instead of mislabeling
  the write as a `Read`.
- Fixed en route: the agent loop `break`-ing on `finish_reason` lost the
  usage chunk OpenAI-compatible servers send *after* it
  (`stream_options.include_usage` semantics) — caught by live E2E testing,
  now locked in by a regression test.

#### Plan mode design (agreed and built 2026-07-11)

Built exactly as designed below, and live-verified end-to-end through the
real binary with `qwen3.5:9b`: started with `--plan`, a change request
produced a `set_tasks` plan with no permission modal and no file change;
Ctrl+P flipped to Act, and the same request then flowed through a real
confirmation modal onto disk. One deviation worth noting: grouping
`Agent::new`'s scalar knobs into an `AgentConfig` struct (the constructor
had grown to 8 args across the phases — clippy was right).

A user-controlled read-only stance, enforced at the permission layer — not a
prompt-level suggestion, because the whole premise of this project is that
the model may be wrong or manipulated.

**Decisions made with the user:**
- **Exit is a user toggle only** (Ctrl+P flips Plan ⇄ Act, plus a `--plan`
  flag to start in plan mode). No model-signaled `exit_plan_mode` tool in v1:
  it adds tool surface that small local models may never call or call
  prematurely, and a manual toggle is always needed as the escape hatch
  anyway. The model is instructed to record its plan via `set_tasks` (the
  task panel *is* the reviewable plan — direct synergy with the Phase 8
  task list) and announce when it's ready for review. Revisit the tool
  later only if the manual flow proves annoying.
- **Write/execute tools are hidden while planning**: filtered out of the
  per-request `ChatRequest.tools` (the agent already rebuilds definitions
  every iteration), with the gate still blocking as the hard backstop.
  Rationale: small local models loop badly on unavailable actions (the
  `ornith:9b` read-loop finding), so not offering a tool beats letting it
  fail repeatedly.

**Mechanism** — one shared flag, three consumers (same wiring pattern as the
Phase 8 tasks handle):
- A `PlanMode` newtype over `Arc<AtomicBool>` defined in `aivyx-sandbox`
  (the lowest crate all three consumers can reach), `Relaxed` ordering —
  nothing hangs off the flag; a toggle racing one in-flight check by a
  single tool call is acceptable and documented.
- **`ConfirmationGate`**: check order becomes `deny_paths` → Read/`Internal`
  auto-allow → **plan-mode deny of everything else** → Always-Allow cache →
  prompt. Critically the plan check sits *before* the cache and the
  pre-approved `allowed_commands` tier, so prior approvals cannot leak
  through plan mode; that ordering gets its own regression test, like
  `deny_paths_wins_over_read_auto_allow` locks in the existing order.
  Plan-mode denials are logged (audit trail) like every other decision.
- **`Agent`**: when active, sends the filtered tool definitions and appends
  a plan-mode note to the system prompt per-request (same pattern as the
  existing history-truncation note): explore read-only, record the plan
  with `set_tasks`, tell the user to press Ctrl+P to approve. Toggling
  mid-turn is safe by construction: *into* plan mode, the gate cuts off
  writes on the very next tool call; *out of* it, the current iteration
  already holds the filtered tool list, so full capability returns on the
  next request — acceptable, documented.
- **TUI**: Ctrl+P intercept in the key loop (swallowed while a permission
  modal is up, like everything else); a colored `PLAN` badge in the status
  line with the exit hint; a transcript notice line on each toggle so the
  mode change is part of the visible record.

**Supporting type changes, done honestly rather than minimally:**
- `Tool` gains a coarse static classification (default: *mutating*, i.e.
  fail-closed — a new tool is hidden in plan mode unless explicitly marked
  otherwise) overridden by `read_file`/`grep`/`glob`/`set_tasks`. This is
  the tool-level analogue of `ActionKind`, needed because filtering
  definitions must happen before any arguments exist for
  `permission_request` to classify.
- `PermissionDecision::Deny` grows an optional reason string (surfaced in
  `ToolOutput::Denied`), because "permission denied for tool X" gives the
  model nothing to adapt to — a plan-mode denial should say it's plan mode,
  and a `deny_paths` denial can say that too. Value beyond plan mode;
  `ToolOutput::Denied`'s own doc comment already promises exactly this.

**Deliberately not doing:** persisting the mode into the session file
(sessions always resume in Act mode — the mode is a live UI stance, not
conversation data); a per-tool plan-mode allowlist in config (YAGNI until a
real need shows up); any relaxation for "read-only-ish" shell commands
(`Execute` is `Execute` — a shell can read anything its sandbox grants, and
plan mode's promise is *no side effects*, kernel-independent).

**Verification bar:** gate unit tests (plan blocks Write/Execute/Delete even
when cached or pre-approved; Read/Internal pass; toggle restores; reason
surfaces), an agent test that definitions are filtered and restored, and a
live E2E through the real binary: start with `--plan`, request a change,
confirm the model plans via `set_tasks` instead of editing (and that a
forced write attempt is denied with the plan-mode reason), Ctrl+P, confirm
execution then proceeds through the normal confirmation flow.

### Phase 9 — Stretch goals
LSP integration for exact symbol resolution (complements, doesn't replace,
the repo map and grep tools), MCP support for external tool integration,
sub-agent delegation for context-isolated exploration, and an
architect/editor model-pairing mode (a stronger model plans in prose, a
faster local model executes the mechanical edit) — directly enabled once
Phase 2's prompted edit format exists.

### Phase 10 — Serving layer: llama-server migration + constrained-decoding spike (scoped 2026-07-11)

The Phase 2 A/B diagnosis promoted the serving layer to a first-class
reliability component: Ollama's hidden 4096-token serving default (not
settable via `/v1`, invisible until a reasoning phase burns through it)
silently truncated responses across *both* edit formats and masqueraded as
model unreliability. Direction confirmed with the user; hardware verified:
RTX 4090 / 24 GB — comfortable for qwen3.5:9b-class GGUFs at 16k context,
and CUDA means the SGLang spike is fully feasible.

**Part A — llama-server as the recommended serving path.** Same GGUF
models, same kernels as Ollama, but everything explicit — the trap class
this phase was born from is structurally impossible when `-c` is on the
command line.
- A0 Install (AUR `llama.cpp-cuda` / ggml-org prebuilt CUDA binaries /
  source build) and obtain GGUFs — reuse Ollama's existing blobs (the
  `FROM` path in `ollama show --modelfile`) or pull from HuggingFace.
- A1 Bring-up: `llama-server -m <gguf> -c 16384 --jinja --cache-reuse 256`
  with aivyx pointed at it via `--base-url`; `--jinja` is load-bearing for
  native tool-call templating, `--cache-reuse` for agent-loop prompt reuse.
- A2 Compatibility verification through the existing live-E2E harnesses,
  wire-log inspected. Watch specifically: streaming tool-call delta shape,
  usage-chunk ordering (Phase 8's drain-to-stream-end fix should hold —
  verify, don't assume), and finish_reason semantics. Any quirks get fixed
  in `aivyx-llm` with regression tests.
- A3 **Window-mismatch probe** (small new code): at startup, best-effort
  query the server's real context window (llama-server exposes `/props`
  n_ctx; Ollama exposes model params via `/api/show`) and warn loudly when
  it's smaller than `backend.context_tokens`. Converts this phase's silent
  trap into a startup warning; silently skipped on servers exposing
  neither endpoint. Decision (flagged for veto): build it — it's the
  permanent fix for the diagnosis class, not just a doc note.
- A4 Docs: a README "Serving" section — recommended invocation, a systemd
  user-unit example, the context-matching rule (`context_tokens` ≤ `-c`).
  Decision (flagged for veto): the *shipped* default base_url stays Ollama
  (the 5-minute quick start is real value); the recommended serious setup
  is llama-server, and the user's own config migrates.
- A5 Acceptance test: re-run the Phase 2 edit-format A/B against
  llama-server — doubles as evidence on whether edit-format results are
  provider-sensitive.

**Part B — SGLang constrained-decoding spike** (bounded: one session, no
aivyx architecture changes inside the spike). Answers the research pass's
standing `[unverified]` question: can grammar-constrained decoding
(xgrammar) force *native tool-call* syntax — function name plus
schema-valid arguments — at generation time? If yes, that attacks edit
reliability at a level neither prompt format reaches.
- B0 Serve a qwen3.5-9B quant (AWQ/FP8) via SGLang on the 4090; aivyx
  pointed at its `/v1` unmodified.
- B1 Baseline compatibility through the same E2E subset.
- B2 The measurement: malformed-tool-call incidence and edit-task success
  with vs without constrained decoding, against the llama-server baseline
  from A5 — same harness, same tasks, wire-logged.
- B3 Radix-cache benefit: time-to-first-token on iteration ≥2 of
  multi-tool turns vs llama-server's `--cache-reuse`.
- B4 Decision gate: aivyx only grows config surface (structured-output
  request knobs) if B2 shows a material win — evidence before
  architecture, same rule Phase 2 set.

**Sequencing:** after the Phase 2 revisit lands (A/B round 3 in flight as
this is written; default decision + commit pending). Part A first; Part B
uses A5's numbers as its baseline.

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
