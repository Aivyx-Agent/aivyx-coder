*(This is the full phase-by-phase development history, relocated here
from `ROADMAP.md` on 2026-07-18. See `ROADMAP.md` at the repo root for
current status.)*

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

*(Updated 2026-07-21 — the phase sections below carry the full history
and evidence; this is the summary.)*

**Shipped and live-verified** (Phases 1–8, 10 Parts A & B, 11a/11b/11c,
12, the full Phase 9 stretch-goal list, editor/IDE context integration,
editor approval integration, and the 4-phase capability-gap-closing
chapter): the full agent loop — streaming chat, native + prompted
SEARCH/REPLACE edit formats (A/B-measured, native default), grep/glob
search, `run_command`/`run_shell` behind real Landlock+seccomp
confinement, git tools (including `git_branch`/`git_push`/`git_pr`) +
automatic worktree checkpoint refs (with automatic multi-file-edit
rollback on a mid-batch failure), `delete_file` (`ActionKind::Delete`'s
first constructor), tree-sitter repo map injection (Rust, Python,
JavaScript/JSX, and TypeScript/TSX), an agent-maintained wiki (`/wiki`),
session persistence/resume, context budget + compaction, plan mode
(gate-enforced read-only), autonomous mode (`--auto`), `/council` multi-model
deliberation, AGENTS.md project instructions, `web_fetch`/`web_search`,
full MCP client support, a startup probe of the *served* context window,
goal-bounded turn pausing instead of a hard iteration-cap failure,
enforced post-edit verification with automatic fix-and-retry and a
cross-attempt "what's new" note, live reasoning visibility in the TUI,
editor/IDE context awareness, editor-side permission approval, and an
ACP editor-integration frontend. 473 workspace tests; every
security-critical behavior also proven by live E2E against real serving
— **except** the ACP frontend's concurrency fix, verified only by deep
source-level static analysis so far, pending a human-run manual Zed
smoke test (see "ACP editor integration" below).

**Serving verdict (Phase 10 Part A)**: the serving configuration — not
the model, not the edit format — was the dominant reliability variable.
Correctly-configured llama-server (explicit 16k window, thinking
disabled) took the same qwen3.5:9b from Ollama's best 7/9 to 9/9 at ~10×
the speed on the edit benchmark. The daily driver runs llama-server;
Ollama stays as the zero-setup default and serves the council's
swap-per-request members.

**Constrained-decoding verdict (Phase 10 Part B, 2026-07-17)**: SGLang's
xgrammar-forced tool calls matched llama-server's already-clean 9/9,
0-malformed-call baseline exactly — no material win, so no config
surface was added. Radix caching was confirmed to automatically reuse
the entire growing conversation prefix on every turn after the first, a
genuine (if not decision-gating) positive finding. Full details in the
Phase 10 section below.

**vLLM compat pass (2026-07-18)**: the last README-claimed provider
(Ollama/vLLM/llama.cpp) is now live-verified. vLLM served the same
Qwen3.5-9B-AWQ weights cleanly on the first attempt — no compat bugs,
unlike SGLang's dtype crash — with correct native AWQ quantization and
GDN/Mamba dtype auto-detection, and a working `--default-chat-template-kwargs`
launch flag SGLang lacked. One live E2E task through the real binary
came back clean (native tool calls, correct edit, no retries). No new
config surface needed — `base_url` already generically targets any
OpenAI-compatible endpoint. Full details in the Phase 10 section below.

**Capability audit (2026-07-12) — fully closed.** A broader audit against
the project's actual end-goal — a high-end vibe-coding agent with a path
to full autonomy — found the security/checkpoint foundation doesn't need
a redesign for autonomy, only extension, but surfaced two structural gaps
in the agent loop itself (closed via Phase 12) plus five smaller gaps
(closed via Phase 9: AGENTS.md, `web_fetch`/`web_search`, MCP client
support, branch/PR tooling, `delete_file`). No tracked items remain from
this audit.

**In flight / next**: a human-run manual smoke test of the ACP frontend
against real Zed (small, concrete, documented in `README.md`'s "Editor
integration (ACP)" section) is the immediate next action. After that,
the next real context is a bare-metal test-rig trial (previously used
for the sibling Aivyx-Agent project) — the original motivating goal
behind closing all 4 capability-gap sub-projects — not yet started as of
this writing.

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
Phase 2's prompted edit format exists. Per the capability audit below, the
delegation and pairing items are prioritized above LSP/MCP: both compound
with the project's existing small-context-window discipline (repo map,
compaction, `LlmBackend` interchangeability already proven by council
mode) rather than requiring new architecture to support.

Surfaced by the same audit, not previously tracked here: **multi-language
repo map support** (v1 is Rust-only — a real cap on general applicability
for most real-world vibe-coding work, which skews TS/Python/Go-heavy, not
just an incomplete nice-to-have); **multi-file/patch-based editing** with
dedicated rename/move-with-import-fixup primitives (today's
N-independent-`edit_file`-calls approach for a cross-file change has no
atomicity across the set, multiplying the model's own documented
reliability ceiling as the error surface rather than reducing it);
**visible reasoning/thinking content in the TUI** (a reasoning-capable
model's `delta.reasoning` is currently dropped silently by `WireDelta` in
`aivyx-llm/src/openai_compat.rs` — serde's default behavior — unless
`AIVYX_DEBUG_LOG` is on; there's no first-class way to see *why* the
agent is about to do something before approving a mutating action);
**structured verification output** (pass/fail extraction and cross-turn
memory of which tests were already failing, instead of raw tail-capped
text the model re-derives every turn — the current cap was tuned for
compiler-error-style output, not noisy test-framework tails); and a
**user-extensible hooks/command-macro layer** (`/council` is the only
special-cased slash command today; no post-edit hooks, no user-defined
command macros — core UX in comparable tools, though it doesn't block
autonomy the way Phase 12/11c's gaps do).

**Sub-agent delegation built and live-verified (2026-07-13).** Followed a
full design pass
(`docs/superpowers/specs/2026-07-13-subagent-delegation-design.md`) before
implementation, mirroring how 11a/11b/11c were each signed off first.
Shipped: `delegate_task`, a tool the model calls mid-turn to spawn a
fresh, isolated `Agent` — full tool access, the exact same trust boundary
as the parent (`ConfirmationGate`, checkpointer, plan/autonomous mode
flags all shared, not a restricted copy), but a completely separate
conversation history, so exploring or working on something unfamiliar
never clutters the calling session's own context window. `DelegateTaskTool`
is defined in `aivyx-core`, not alongside the other tools in
`aivyx-tools` — `aivyx-tools` has no dependency on `aivyx-core`/`aivyx-llm`
(the graph runs the other way), so a type needing `Agent` and
`LlmBackend` in scope simultaneously with the `aivyx_tools::Tool` trait it
implements can only live where all three are already visible. Every
session-stable dependency the tool needs is bundled into a
`DelegateTaskConfig` struct baked in once at registration time, rather
than threading anything through `ToolExecutionContext` (which stays
untouched, along with every other `Tool` impl). The sub-agent's own
activity streams live into the transcript (prefixed `sub-agent>`,
visually distinct) via a spawned background task that drains the
sub-agent's private `AgentEvent` channel and forwards each event wrapped
in a new `AgentEvent::SubAgentActivity` variant — `/council`'s simpler
same-channel precedent didn't transfer directly, since council emits its
own coarse-grained notes rather than forwarding a separate nested agent's
full token-by-token stream. Bounded by `[sub_agent] max_iterations`
(default 10); a sub-agent that exhausts its budget still returns a
best-effort partial result, never an error. Delegation is capped at one
level: a sub-agent's own tool list is snapshotted from the parent's
*before* `delegate_task` is registered onto it, so recursion is
structurally impossible rather than merely policy-excluded.

Three real corrections to the original design were found and resolved
during implementation, each disclosed rather than silently smoothed over:
the `ToolExecutionContext`-extension approach the design first proposed
turned out to be unnecessary (every dependency is session-stable, so
baking it into the tool's own constructor — matching the existing
`RunCommandTool` pattern — touches zero other call sites); the crate
placement moved from the originally-assumed `aivyx-tools` to `aivyx-core`
once the dependency-graph direction was checked; and — found during
implementation itself, not design — the nested sub-agent's own
`max_tool_iterations` must be pinned to exactly `1` rather than reusing
the same `max_iterations` value as the outer continuation-loop bound,
since setting both to the same number would let total round-trips reach
`max_iterations²` in the worst case instead of `max_iterations`.

12 new tests (243 total, up from 231): real behavior against a mock
`LlmBackend` and real `Agent`/`ToolExecutor` construction (nested-agent
completion, cap exhaustion returning `Ok` not `Error`, recursion
structurally absent from a sub-agent's own tool list, plan-mode
filtering, a genuine backend error surfacing correctly), plus TUI
rendering tests for `ChatLine::SubAgent`. Three live E2E checks through
the real binary (qwen3.5:9B via the Lemonade-managed llama-server), all
passing: (1) a sub-agent explored an unfamiliar crate and reported back —
confirmed via the persisted session file that only the `delegate_task`
call and its distilled text result entered the parent's history, none of
the sub-agent's own tool calls; (2) a sub-agent's `write_file` call
triggered the identical confirmation modal (`Tool: write_file`, diff
preview) any other write would, proving the shared `ConfirmationGate`
rather than a bypassed one; (3) under `--plan`, the sub-agent's own
tool list was filtered to read-only before ever reaching the gate — the
model itself reported "the available tools only support reading files,
not creating them," confirming graceful degradation rather than a
silent per-call denial.

**Architect/editor model-pairing built and live-verified (2026-07-14).**
Followed a full design pass
(`docs/superpowers/specs/2026-07-14-architect-editor-pairing-design.md`)
before implementation, mirroring 11a/11b/11c and sub-agent delegation.
Shipped: `/architect <task>`, a slash command dispatched in `Agent::run_turn`
alongside `/council` and `/wiki`, via a new `crate::architect` module
mirroring `crate::council`'s shape (`ArchitectSeat`/`Architect` vs.
`CouncilSeat`/`Council` — the tail-budget field lives on the wrapper, not the
seat, exactly as `Council` already does, since a bare seat carries no notion
of how much conversation context it should see). Unlike `/council`, there's
exactly one seat and no deliberation/ranking; unlike `delegate_task`, the
architect never calls tools and its output feeds the *same* session's next
turn rather than spawning an isolated agent. The core mechanism needed no new
plumbing: `run_architect_turn` makes one no-tools planning call, then calls
`self.run_turn_inner(formatted_plan, cwd, cancellation).await` directly —
`run_turn_inner` is completely unmodified, since it already pushes whatever
string it's given as a `Role::User` message and runs the normal iteration
loop. That one call *is* the hand-off: the editor model sees the plan exactly
as if it were the user's next message and starts calling tools immediately,
in the same turn, with zero re-prompting.

One real bug was found and fixed during implementation, not design: the
plan's own reference code for `architect::plan` went straight from emitting
a "planning…" note into the backend call, omitting the early
`cancellation.is_cancelled()` check `council::convene` already does before
each of its own stages. A pre-cancelled token could race `collect_text`'s
internal `tokio::select!` against an already-ready mock stream in tests,
producing genuine ~20% flakiness — reproduced in isolation (6/30 failures
without the fix, independently re-confirmed by the task reviewer) before
being fixed with the same one-line pre-check `convene` already uses.

12 new tests (255 total, up from 243): config parsing/`configured()` logic,
`parse_command` recognition, unconfigured/bare-invocation/backend-failure/
empty-response turn behavior, a plan-mode regression proving the editor's
own tool-list filtering still applies after a hand-off, the cancellation
race fix above, and TUI rendering for the new `architect>`-prefixed
(cyan) chat line. One live E2E through the real binary (only one local
backend was actually running at test time — `Qwen3.5-9B-GGUF` via a
Lemonade-managed llama-server — so both `[backend]` and `[architect]`
pointed at the same seat; the mechanism under test doesn't require distinct
models, only distinct configuration): `/architect` against a scratch repo
produced a live `architect>`-prefixed plan, a real `edit_file` confirmation
modal with the correct diff, an actual on-disk edit after approval, and a
persisted session history with the injected `"[Architect plan"` message
immediately followed by the editor's own `read_file`/`edit_file` round-trip
— confirming the whole request completed as one continuous turn.

**LSP integration built and live-verified (2026-07-15).** Followed a full
design pass (`docs/superpowers/specs/2026-07-14-lsp-integration-design.md`)
before implementation, mirroring every prior Phase 9/10/11/12 item. Shipped:
`go_to_definition` and `find_references`, two read-only tools backed by a
lazily-spawned `rust-analyzer` subprocess — the first genuinely new
capability *shape* in this project (every prior process-executing tool is
one-shot spawn→drain→exit; this is a long-lived, stateful JSON-RPC session).
A new `aivyx-tools/src/lsp/` module (not a new crate — mirrors
`GitCheckpointer`'s precedent of a substantial stateful component living
directly in `aivyx-tools`) holds a transport layer generic over any
`AsyncRead`/`AsyncWrite` pair (a real child's stdio in production, an
in-memory `tokio::io::duplex()` in tests) and `LspClient`, which is
constructor-baked into both tools (`Arc<LspClient>`) rather than threaded
through `ToolExecutionContext`/`ToolExecutor` — unlike `GitCheckpointer`,
nothing else in the crate needs it, so it doesn't belong on the
cross-cutting dispatch path. `rust-analyzer` is spawned through the same
`ExecutionConfiner` every other process-executing tool already uses, lazily
on first call, reused for the session, and transparently respawned once if
it dies. Since this project has no persistent open-editor-buffer concept,
every query re-syncs the target file fresh from disk (`didOpen`/`didChange`)
immediately before asking, so edits made via `write_file`/`edit_file`
between two LSP calls are always reflected. No `lsp-types` dependency —
hand-rolled response types only, matching this workspace's existing
hand-rolled-over-heavy-dependency convention (the same reasoning behind the
frontmatter parser having no YAML dependency).

Four real issues were found and fixed during implementation and review, one
of them only surfaced by live testing against a genuine `rust-analyzer`:
task reviews caught a missing regression test for "a healthy session is
reused without respawning" (an explicit plan constraint with zero coverage,
closed with a mutation-tested regression test — the re-reviewer confirmed by
hand that inverting the liveness check would make the new test fail), a real
concurrency bottleneck (`LspClient::request` held the client's state mutex
across the *entire* in-flight JSON-RPC round trip, serializing both tools
sharing one client — fixed by wrapping `Connection` in an `Arc` so only a
cheap clone happens under the lock), and a live symlink/`deny_paths` bypass
(both tools resolved paths via raw `cwd.join()` instead of the crate's
`path_resolve::resolve()`, which `ConfirmationGate`'s deny-list depends on
for symlink safety — the re-reviewer verified the closed gap with an actual
symlink-escape test plus a before/after control reproducing the original
bug). The fourth was found only once a real `rust-analyzer` binary was
available for Task 6's live verification (none of this crate's own
hand-rolled test doubles had ever exercised it): `read_loop`'s response
correlation matched on JSON-RPC `"id"` presence alone, but rust-analyzer
sends its own server-initiated requests (`window/workDoneProgress/create`
and similar) on an independent id counter that collides with the client's
own — a server request could silently masquerade as the response to a real
pending query, resolving it with a bogus `Null`. Root-caused by directly
tracing the wire protocol with a hand-rolled probe script before fixing;
closed by requiring the absence of `"method"` and the presence of
`"result"`/`"error"` before a message may resolve a pending sender. The same
live pass also found that `ensure_started` never actually waited for
`rust-analyzer`'s initial workspace load to finish — only for the
`initialize` handshake, which returns long before indexing completes — so a
query issued immediately after a fresh spawn silently returned empty results
(confirmed directly: identical query, `[]` immediately after spawn, correct
result 10s later). This directly contradicted `[lsp] timeout_secs`'s own doc
comment, already written to promise "cold indexing can be slow" tolerance
the implementation didn't yet provide; closed by tracking `$/progress`
begin/end notifications and having a fresh spawn wait (debounced, since
rust-analyzer's startup emits several back-to-back progress cycles, and
bounded by the same `timeout_secs` budget) until the server goes quiet.

21 new tests (276 total, up from 255): JSON-RPC framing/correlation against
an in-memory duplex (no process), confiner-invocation and missing-binary
error paths via a deliberately-nonexistent program name (not the real
`rust-analyzer`, so every unit test stays deterministic regardless of
what's on the machine running it), bidirectional 1-indexed↔0-indexed
position conversion, array/single/null response-shape parsing,
`didOpen`-vs-`didChange` branching, request timeout-firing, and both tools'
`Tool` trait contracts. Plus a real-`rust-analyzer` Cargo integration test
(self-skips when the binary isn't on `PATH` rather than failing) and one
live E2E through the real binary: no local install of `rust-analyzer` or
`rustup` was available, so the standalone release binary was downloaded
directly for this verification pass. Both the integration test and the
live E2E initially failed against the real binary — surfacing the two
protocol bugs above — then passed cleanly once fixed: the live run showed
`go_to_definition` resolving a real call site to its real definition
through the actual TUI, with zero confirmation modals (matching the
`ActionKind::Read` auto-allow every other read-only tool already gets) and
correct 1-indexed input/output end to end.

**`AGENTS.md` project instructions built and live-verified (2026-07-15).**
Followed a full design pass
(`docs/superpowers/specs/2026-07-15-agents-md-design.md`) before
implementation, mirroring every prior Phase 9 item. Surfaced by a
tool/capability audit of the shipped tool set against comparable agents
(Claude Code's `CLAUDE.md`, Cursor's `.cursorrules`, Aider's config
conventions, the emerging cross-tool `AGENTS.md` standard) and prioritized
above the remaining audit items — it improves every session rather than one
workflow, unlike a specific tool or workflow gap. Shipped: an optional
`<cwd>/AGENTS.md` (project) and `<config_dir>/AGENTS.md` (user-global,
sibling to `config.toml`), both refreshed every turn via a new
`Agent::refresh_agents_files`, reusing `refresh_repo_map`'s exact per-turn
(not per-round-trip) cadence and its `Option<...>`-gated
constructor-injection shape (`Agent::set_agents_file`). Merge order is
global-then-project with a one-line precedence note when both are present,
each file budgeted independently (`[agents_file] budget_tokens`, default
1024) — a deliberate divergence from the repo map's truncate-by-rank
behavior: a file over budget is still included in full (hand-written prose
has no safe cut point to truncate at) but triggers a one-time notice via
the existing `Agent::notify`/`AgentEvent::Error` channel, reusing that
established non-fatal-notice convention rather than adding a new event
variant.

12 new tests (288 total, up from 276): all four file-presence combinations
(neither/global-only/project-only/both) and their labeling/ordering, the
precedence-note appearing only when both files are present, over-budget
(full content still included, exactly one notice) and within-budget (no
notice) behavior, a dynamic-refresh test proving an edit between two turns
changes the very next turn's prompt (not just at startup), the size
estimator correctly counting the new content, the feature stays fully
inert when never wired in (mirrors how `main.rs`'s own `[agents_file]
enabled` gate works, since `Agent` itself carries no separate runtime
disable flag), and graceful degradation on a real I/O error (a directory
literally named `AGENTS.md` in place of a file). Task reviews independently
hand-traced two easy-to-get-backwards details and confirmed both correct:
the per-turn refresh call landed in `run_turn_inner`'s own call site, not
`run_architect_turn`'s separate, pre-existing `refresh_repo_map()` call
(the two functions each have one); and the both-files-present merge
correctly extracts project and global content in the right order despite
`Vec::remove`'s index-shifting behavior (`remove(1)` before `remove(0)`).
One live E2E through the real binary, in two turns of the same session:
turn one, with an `AGENTS.md` instructing the model to end every response
with a specific marker line, produced a response ending with exactly that
marker (verified via the persisted session JSON, not raw screen text, per
this project's own established grading method); `AGENTS.md` was then
edited mid-session to remove the rule, and turn two's response — while
still accurately recalling the old rule from ordinary conversation memory
while describing the repo's contents — no longer produced the marker,
directly confirming the refresh is live per-turn rather than loaded once at
session start.

**`web_fetch`/`web_search` tools built and live-verified (2026-07-15).**
Resolved the Medium-priority web-network values question the capability
audit above raised as a real tradeoff rather than an oversight: "local-only"
describes where LLM inference happens (Ollama/vLLM/llama.cpp), not network
isolation — the agent gets full network access, and this phase ships the
first two tools whose entire purpose is reaching it.
`web_fetch(url)`/`web_search(query)` followed a full design pass
(`docs/superpowers/specs/2026-07-15-web-tools-design.md`) before
implementation. `web_search` queries a self-hosted or public SearXNG
instance (chosen specifically for needing no API key or paid signup,
matching this project's avoid-external-service-dependencies posture — no
other search backend is in scope). Both tools use `ActionKind::Read`
auto-allow, the same tier as `read_file`/`grep` — a real fork decided
against the more conservative confirm-by-default alternative, on the
reasoning that fetched/searched content is already covered by the standing
untrusted-tool-output convention.

Since neither tool sits behind a human confirmation gate, `web_fetch`
carries its own SSRF pre-flight check shared via `resolve_and_check`:
before connecting, it resolves the target host and refuses any
loopback/private/link-local resolved address (`[web] allow_private_targets
= true` overrides it). This has a documented, accepted DNS-resolution
TOCTOU limitation — the check's own resolution and the actual HTTP client's
subsequent resolution could theoretically diverge — consistent with this
project's established stance that the sandbox/`ConfirmationGate` remains
the primary security boundary, not this kind of best-effort mitigation.
Task 2's review caught a genuine Critical bug before merge: IPv4-mapped
IPv6 addresses (`::ffff:127.0.0.1` and similar, a well-known real SSRF
bypass vector) skipped the check entirely, since the IPv6 range logic never
unwrapped them via `to_ipv4_mapped()` — fixed, with the fix independently
re-verified by the re-reviewer compiling and running a standalone program
against all test vectors. `web_search` carries no equivalent check: it only
ever talks to the one explicitly-configured, admin-trusted
`search_base_url`.

All HTTP-touching tests run against an in-process mock server — a
hand-rolled `tokio::net::TcpListener`-based single-shot HTTP double
(`crate::web::test_support::spawn_mock_http_server`), never a real network
call, deliberately without adding an HTTP-mocking crate dependency
(mirroring LSP integration's own fake-JSON-RPC-server precedent for test
doubles over real network calls). 35 new tests (323 total, up from 288):
SSRF range boundaries for every blocked range plus a real-DNS-resolution
case, HTML-to-text conversion, char-boundary-safe head-truncation at the
50KB cap, SearXNG JSON response parsing (multiple/zero/malformed results),
`web_search`'s unconfigured-`search_base_url` explanation path, and both
tools' absence from the registry when `[web] enabled = false`. Live E2E
through the real binary (a `python-pyte`-driven PTY harness, needed for
accurate VT100 screen rendering after a first hand-rolled ANSI-stripping
attempt produced misleading garbled text) confirmed, via the persisted
session JSON per this project's established grading method: a real
`web_fetch` against `https://example.com` returning genuine page content
with no confirmation modal; a real `web_search` against a self-hosted
SearXNG instance (public instances all blocked or rate-limited their JSON
API) returning real ranked results with no confirmation modal; and
`web_fetch` against `http://127.0.0.1:1/` being cleanly refused by the SSRF
check with the documented error message, confirming the check is wired
through to the real binary end-to-end, not just covered at the unit level.

**MCP client support built and live-verified (2026-07-16).** The queued
next major phase after `web_fetch`/`web_search`, resolving the tool audit's
original "no plug-in integration surface" gap: instead of one bespoke tool
per third-party integration, aivyx-coder can now connect to arbitrary
user-configured MCP servers over stdio, covering all three MCP primitives
(tools, resources, prompts) in one phase rather than a tools-first, defer-
the-rest split. Followed a full design pass
(`docs/superpowers/specs/2026-07-16-mcp-support-design.md`) before
implementation, mirroring every prior Phase 9 item.

The core trust decision, and a genuine fork decided the conservative way
(the opposite of `web_fetch`/`web_search`'s auto-allow): every discovered
MCP tool call declares a new `ActionKind::McpTool`, always confirm-gated,
regardless of anything the server itself claims about its own behavior
(including MCP's optional, advisory `readOnlyHint`/`destructiveHint`
annotations, deliberately not trusted). Unlike this project's own
hand-written tools, an MCP tool's actual behavior is arbitrary third-party
code that can't be verified, so it can't be allowed to honestly describe
itself as safe the way `Read`/`Internal` actions can. Resources and
prompts, by contrast, are protocol-guaranteed read-only regardless of which
server provides them, so they surface through four fixed meta-tools
(`list_mcp_resources`/`read_mcp_resource`/`list_mcp_prompts`/`get_mcp_prompt`)
at the `Read` tier instead — one meta-tool per primitive rather than one
tool per discovered item, since a server's resource/prompt list can be
unbounded and would otherwise churn the tool registry every time it
changes.

Every configured `[[mcp.servers]]` entry connects concurrently at startup
(via `tokio::task::JoinSet`), each bounded by its own `timeout_secs` — this
eager-at-startup design is forced by `ToolRegistry` being a fixed list built
once before the model can be offered anything, unlike the LSP client's own
lazy-spawn-on-first-use pattern, since MCP's server set is arbitrary and
user-configured rather than one well-known always-present tool. A server
that fails or times out is skipped with a warning (both logged and
surfaced into the TUI) rather than blocking startup or any other server's
tools. A dead connection respawns on next use, re-running only the
`initialize` handshake — not full rediscovery, which only ever runs once,
at startup, to populate the static registry. The MCP stdio transport is
hand-rolled (newline-delimited JSON-RPC 2.0 framing, no new dependency),
deliberately mirroring the LSP client's own `Connection` shape (pending-map
request/response correlation, background read-loop task, `is_dead()`
liveness, `Drop`-based cleanup) with only the wire framing genuinely
different from LSP's `Content-Length` header block.

29 new tests (352 total, up from 323): `ActionKind::McpTool`'s confirm-gate
fall-through (proven via the prompter actually being invoked, not just the
decision equalling `Allow`), the MCP transport's request/response
correlation and liveness detection, `McpClient`'s spawn/handshake/discovery/
respawn lifecycle (including a real "not found" spawn-failure path and a
reused-not-respawned healthy-session path, mirroring the LSP client's own
test shape), content-block/resource/prompt rendering (text concatenation,
non-text placeholder strings, `isError` passthrough), the `McpToolAdapter`'s
naming/schema-passthrough/permission-tier/error-mapping, and all four
meta-tools' aggregation/filter/empty-result/unknown-server paths — plus the
critical cross-task polarity check: `McpToolAdapter` correctly does NOT
override `mutates_outside_session()` (the trait default applies, hiding MCP
tools in Plan Mode), while the four meta-tools correctly DO override it to
`false` (keeping them available in Plan Mode) — the opposite polarity in
each case, both verified correct by the task reviews.

One live E2E through the real binary (a `python-pyte`-driven PTY harness),
confirmed via the persisted session JSON per this project's established
grading method: a real MCP tool call (`mcp__mini__echo`, via a small
purpose-built stdio test server — the official `@modelcontextprotocol/
server-everything` reference package was reachable and worked standalone,
but repeatedly timed out when spawned through this project's own
Landlock-confined `ExecutionConfiner`, since its default-deny sandbox policy
has no read access to `npx`'s cache directory; a real, if narrower, finding
about this sandbox's interaction with `npx`-based MCP servers, left as a
noted limitation rather than a defect in this phase's own code) showing the
confirmation modal and succeeding on approval; a real `list_mcp_resources`
call with no confirmation modal, returning genuine resource data; and,
observed directly during the environment investigation above, the
skip-with-warning path firing for real against a genuinely slow server —
the session stayed usable and the warning surfaced live in the TUI rather
than the process hanging or crashing.

**Branch/PR tooling built and live-verified (2026-07-16).** Closes out the
lowest-priority remaining item from the original tool/capability audit
("no dedicated branch/PR tools — already reachable via `run_shell`"),
kept deliberately narrow to match that framing. `git_branch`/`git_push`/
`git_pr` all reuse `git_commit`'s existing `ActionKind::Execute` +
`PermissionTarget::Command` permission tier rather than introducing a new
one — a real fork, decided the opposite way from MCP's genuinely novel
arbitrary-third-party-code trust situation: these tools are structured
invocations of the same trusted `git`/`gh` CLIs `git_commit` already
shells out to, so the existing tier's reasoning already covers them.
Branch *listing* stays read-only and auto-allowed as a new mode on the
existing `git_read` tool, rather than a fourth mutating tool, mirroring
why `git_read`/`git_commit` were already split (Plan Mode's tool filtering
is static per-tool).

A task review caught a real, live-git-reproduced Critical bug before
merge: `git_branch`'s `name` (switch mode) and `base` (create mode) sat in
bare positional argv slots with no protection against a leading dash — a
branch name of `-f` produced `git checkout -f`, which git parses as the
`--force` flag rather than a branch name, **silently discarding
uncommitted changes** instead of erroring "branch not found." Fixed by
rejecting any leading-`-` value in those two positions before the git
process ever spawns (the `-b <name>` position — the new branch's own
name — is genuinely unaffected, since `-b` unconditionally consumes the
next token positionally; confirmed both ways against live git). The same
class of bug was proactively fixed in `git_push`'s `remote` argument
before its own review even ran, since it sits in the identical kind of
slot (`git push -u <remote> <branch>`) — both fixes were independently
reproduced against real git by their respective task reviewers, not just
taken on faith. `git_pr`'s `title`/`body`/`base` needed no equivalent
guard: they're each passed as an explicit `--flag value` pair, which
`gh`'s Cobra-based argument parser (like most standard CLI parsers)
consumes as-is regardless of what the value looks like — verified
empirically against a real local `gh` binary, not just reasoned about.

`git_push` never constructs a `--force`/`--force-with-lease` argv at all,
deliberately excluded as a whole operation class rather than merely
undocumented. `git_pr` gates on two deterministic preflight checks, each
by exit status only, never by parsing either tool's stderr text for a
specific phrase (a documented anti-pattern from earlier phases, since
exact wording isn't a stable contract across versions): a `git
rev-parse`-based upstream check (directing the model to `git_push` first
if missing, rather than `git_pr` silently pushing on its own behalf) and a
`gh auth status` check (distinguishing "not installed" from "not
authenticated" with different fixes). `git_pr` is always registered, no
config flag — the same "environmental accident, not a deliberate
off-switch" reasoning already applied to a missing `rust-analyzer`.

20 new tests (375 total, up from 355): the new `git_read` branches mode;
`git_branch`'s create/switch/base-ref/preview/validation paths plus the
dash-rejection fix; `git_push`'s first-push/subsequent-push/preview paths
(against a real local **bare** repository used as the test remote — a
genuine local git transport round-trip, no network) plus its own
dash-rejection guard; and `git_pr`'s two preflight-failure paths, its
success path, and its argv construction, all against a fake `gh`
stand-in (`GitPrTool::with_gh_program`, mirroring the LSP client's
`with_program` test-only constructor) — no real GitHub account or network
access anywhere in the suite.

Live E2E through the real binary covered `git_branch` and `git_push`
only, not `git_pr` — an explicit non-goal from the design, since there's
no safe, repeatable way to live-test a real `gh pr create` call against a
real GitHub repository in this environment; `git_pr`'s verification is
bounded by its fake-`gh` unit tests. Confirmed live, via a `python-pyte`
PTY harness and the persisted session JSON per this project's established
grading method: creating a branch via `git_branch` showed a real
confirmation modal naming the exact `git checkout -b` argv and, on
approval, genuinely created and checked out the branch (independently
verified against the scratch repo's actual `git branch`/`git log`
afterward); listing branches via `git_read`'s new mode showed no
confirmation modal and returned real branch data; pushing via `git_push`
against a real local **bare** repository (no network) showed a real
confirmation modal and, on approval, genuinely landed the commit on the
bare repo's `main` branch — independently verified afterward by reading
the bare repo directly (`git --git-dir=<bare> log main`), not just trusting
the tool's own reported output.

**`delete_file` built and live-verified (2026-07-17).** Closes out the
original tool/capability audit entirely — the last remaining tracked
gap was `ActionKind::Delete` being defined in the permission-tier enum
since this project's security model was first designed, but never having
a real constructor: every prior mutating tool declared `Read`/`Write`/
`Execute`/`Internal`/`McpTool`, and deletion was only ever reachable
through `run_shell`'s generic confirm tier. `delete_file(path)` closes
that gap directly, reusing `write_file`/`edit_file`'s existing
`ActionKind`-adjacent confirm-gated shape (no new tier) and their exact
preview pattern (file content, or a binary-file warning reusing
`write_file`'s own wording verbatim).

Two deliberate scope decisions, both a direct continuation of lessons
from the immediately preceding phases rather than fresh ground: single-
file only, no directory/recursive deletion (`run_shell` remains the path
for that, matching this project's now-repeated "ship the narrow version
first" pattern from `git_push`'s own force-push exclusion); and deletion
implemented as a single `tokio::fs::remove_file` call with **no
subprocess spawned at all**, a deliberate, explicit contrast with the
branch/PR tooling phase's own argv-injection vulnerability
(`git_branch`/`git_push`'s dash-prefixed bare-positional-argument bug,
found and fixed one phase earlier) — there is no argv here to construct,
so that entire bug class cannot exist by design, not merely by careful
guarding. No `deny_paths` field on the tool either, matching `write_file`/
`edit_file`: `ConfirmationGate`'s existing central check already blocks
any `PermissionTarget::Path` under configured `deny_paths` uniformly,
so a per-tool duplicate would be redundant.

No new recovery or confirmation-richness mechanism was built for this
either — deliberately deferring entirely to the existing automatic
pre-mutation checkpoint every mutating tool already gets for free via
`ToolExecutor::dispatch_inner`'s `mutates_outside_session()`-keyed hook,
zero new plumbing required. This is the same "checkpoints are the safety
net, not bespoke recovery machinery" decision this project has made
repeatedly (enforced verification's own retry-exhaustion path relies on
the identical mechanism) — but this is the first phase to specifically
live-verify the delete-then-restore round trip end to end, since no
earlier phase's live E2E happened to delete anything.

6 new tests (381 total, up from 375): successful deletion (file genuinely
gone afterward), the directory-refusal and nonexistent-path errors (both
`ToolError::ExecutionFailed`, not `InvalidArguments` — the `path`
argument itself is well-formed in both cases, the problem is a runtime
precondition about what's on disk), the text-file preview, the binary-file
warning preview, and the permission request's `ActionKind::Delete`/
`PermissionTarget::Path` shape.

Live E2E through the real binary (a `python-pyte`-driven PTY harness),
confirmed via the persisted session JSON per this project's established
grading method: a real `delete_file` call showed a genuine confirmation
modal and, on approval, the target file was verifiably gone from disk
afterward — plus the new ground this phase specifically set out to prove:
locating the checkpoint `delete_file` automatically created
(`git for-each-ref refs/aivyx/checkpoints/`, labeled `"aivyx checkpoint
before delete_file"` by the existing generic checkpoint mechanism with no
tool-specific wiring) and confirming `git checkout <that-ref> -- <path>`
genuinely restored the file with its exact original content — the direct,
concrete payoff of this phase's "no bespoke recovery machinery" decision,
verified rather than assumed.

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

**Part A progress (2026-07-11): A0–A4 built and live-verified; A5 in
flight.** llama-server built from source (CUDA sm_89; ggml's
`GGML_CCACHE=ON` default auto-adopts the system sccache and corrupts
parallel nvcc builds — `-DGGML_CCACHE=OFF`). Verified against it: 5/5
map-E2E checks (streaming, usage ordering, repo-map injection) and 9/9
git-E2E checks (tool-call deltas, modals, checkpoints, commit) with the
unsloth qwen3.5-9B GGUF at an explicit, probe-confirmed 16k window. The
A3 window probe shipped (`aivyx-llm::probe`, llama-server `/props` +
Ollama `/api/show`, advisory transcript notice) and live-passed all three
cases: warns on bare-Ollama-no-num_ctx, silent on llama-server-16k and on
the derived 8k Ollama model. Three migration gotchas found by
verification, all documented in README's Serving section:
1. **Ollama blob reuse is arch-dependent** — the qwen35 blob carries
   Ollama-fork GGUF metadata upstream rejects; and even a loadable blob
   (lfm2.5) breaks native tool-calling because Ollama stores chat
   templates *outside* the GGUF (calls streamed through as raw
   `<|tool_call_start|>` text). HF GGUFs are the reliable path.
2. **Tool-call parsing is per-model-template**: verify per model, don't
   assume — the git E2E is the acceptance test for it.
3. **Sampling does not migrate**: Ollama applies Modelfile sampling
   server-side; llama-server uses generic defaults — carry the model
   family's recommended flags across.
4. **Reasoning models emit tool calls inside unclosed think blocks** on
   continuation turns — llama-server's reasoning parser then classifies
   the whole action as `reasoning_content` and the turn ends as a silent
   no-op. Confirmed format-agnostic: prompted SEARCH/REPLACE blocks
   vanish identically, independently corroborating Phase 2's
   format-doesn't-matter verdict. `--reasoning-budget 0` is inert on
   templates without `enable_thinking` support; the working switch is
   `--chat-template-kwargs '{"enable_thinking": false}'` (verified at the
   wire: 0 reasoning chunks, clean tool_calls deltas).

**A5 verdict (attempt 4, thinking disabled, 3 tasks × 3 reps per format):
native 9/9 — every task first-try, single-approval, ~2s each, 19.2s total.
Prompted 6/9 (305s).** Against Ollama's best round (native 7/9, 221s),
correctly-configured llama-server is both perfect on this benchmark and
~10× faster — the serving configuration, not the model and not the edit
format, was the dominant reliability variable all along. It took four
serving-layer bugs to get here, and every one was found by the A/B harness
acting as an acceptance test rather than by reading documentation. Phase
2's native default is re-confirmed decisively (9/9 vs 6/9 under identical,
finally-honest conditions). Part A complete.

**Lemonade re-verification (2026-07-12).**
[lemonade-sdk/lemonade](https://github.com/lemonade-sdk/lemonade) — a
distro-packaged (CachyOS `lemonade-server`) llama.cpp process manager with
its own OpenAI-compatible gateway — was evaluated as a lower-friction
alternative to the manual llama-server install above (its CUDA backend
ships prebuilt per-compute-capability binaries, sidestepping the
`GGML_CCACHE` build gotcha entirely). Two integration findings, both now
documented in README's Serving section:
1. The context-window probe (`aivyx-llm::probe`) must target the real
   llama-server process Lemonade spawns, not its gateway port — the
   gateway's Ollama-compat `/api/show` always reports `num_ctx -1`, which
   the probe parses as "Ollama's hidden default," producing a **false**
   truncation warning even when the model is correctly configured.
   Confirmed both directions live: a deliberately-mismatched
   `context_tokens` correctly triggered the real warning against the
   underlying port, and the correct value produced silence.
2. `--llamacpp-args` requires single-quote-wrapping any flag carrying
   embedded JSON (`--chat-template-kwargs`) — Lemonade's argument splitter
   strips bare double quotes before they reach llama-server.

With both resolved, and README's documented Qwen non-thinking sampling
flags applied (`--temp 0.7 --top-k 20 --top-p 0.8 --presence-penalty
1.5`), re-ran the A5 edit-format benchmark against a Lemonade-managed
`qwen3.5:9b` (3 tasks × 3 reps, live through the real `aivyx` binary via a
purpose-built pty harness, graded from on-disk file state — not the
screen, per the harness lessons elsewhere in this document): **native
9/9 (45.5s total), prompted 6/9 (204.9s total)** — reproducing the
original A5 verdict exactly on an independently-managed serving path. The
3 prompted failures split into the two already-documented signatures: one
~95s+ reasoning stall (GPU confirmed actively generating at 93% util, not
hung) and two fast (10-13s) zero-tool-call turns where the model answered
in prose without ever emitting a SEARCH/REPLACE block — both consistent
with Phase 2's model-side agentic-reliability finding, not new plumbing
bugs.

Separately confirmed: `Qwen3-4B-Instruct-2507` (a non-thinking-only Qwen3
release) has no `enable_thinking` template variable at all —
`--chat-template-kwargs` is a no-op for that model family, not something
to debug if it appears inert.

**Part B run and closed out (2026-07-17).** The prior attempt's
environment (AWQ weights, AUR `llama.cpp-cuda`, SGLang via the official
docker image) had gone stale since 2026-07-12 — the AWQ shards were no
longer on disk and had to be re-downloaded fresh from
`QuantTrio/Qwen3.5-9B-AWQ` (12.4GB; the first transfer attempt dropped
mid-shard and needed a resumed re-run). The GPU's current daily-driver
llama-server (running since 2026-07-12, serving the qwen3.5:9b GGUF
quant) was stopped for the duration of the spike to free VRAM, and
restored to its exact prior invocation afterward.

Two real compatibility snags, both found and resolved before B0 could
complete, worth recording for whoever runs this again:
- SGLang (current `:latest` image) has **no `--chat-template-kwargs`
  launch flag** — despite general SGLang docs suggesting one — so the
  planned "thinking disabled via patched chat template" recipe had to
  mean an actual patched template file, not a launch-time kwarg. Fixed by
  copying QuantTrio's bundled `chat_template.jinja`, replacing its
  `{%- if enable_thinking is defined and enable_thinking is false %}`
  guard with an unconditional `{%- if true %}`, and passing the result via
  `--chat-template`.
- Qwen3.5's hybrid GDN (Gated DeltaNet / linear-attention) layers crash
  on first inference under this AWQ quant with `RuntimeError: Index put
  requires the source and destination dtypes match, got BFloat16 for the
  destination and Half for the source` — a real, open, unmerged SGLang
  bug ([sgl-project/sglang#30178](https://github.com/sgl-project/sglang/issues/30178):
  `SGLANG_MAMBA_CONV_DTYPE` defaults to a hardcoded `"bfloat16"` instead
  of following the model's own configured dtype). The linked fix
  (sgl-project/sglang#30950) wasn't merged into the pulled image, so
  the same effect was reproduced manually: `docker run -e
  SGLANG_MAMBA_CONV_DTYPE=float16 ...` (matching the AWQ model's own
  `config.json`-declared `"dtype": "float16"`) — this cleared the crash
  and the server came up clean.

**B0 (serve):** SGLang up and serving `/model` on port 30000; a raw
`curl` sanity check confirmed both non-thinking behavior
(`reasoning_tokens: 0`, clean direct answers, no leaked `<think>` tokens)
and schema-valid native tool-call output (`qwen3_coder` parser correctly
emitted `read_file({"path": "test.txt"})`-shaped calls) before aivyx-coder
was ever pointed at it.

**B1 (baseline compatibility):** one real task (the A5 benchmark's own
"discount" task, below) through the actual `aivyx` binary — `read_file`
then a correct single-shot `edit_file`, no malformed tool calls, no
repair-loop retries, verified via the persisted session JSON and the
resulting file content on disk.

**B2 (the measurement): 9/9, 0 malformed tool calls, 0 repair-loop
retries** — the exact same 3-tasks-×-3-reps grid A5 used against
llama-server (discount / bounds-check / iterator-rewrite, judged from
final file state), replayed live through the real binary against SGLang.
One harness bug surfaced and was fixed mid-run: the PTY driver's
"turn complete" heuristic (`"ready —"` in the status line) is not
sufficient on its own — that text is shown even while an unsent message
still sits in the input box — so 6 of the first 9 runs were killed
prematurely before the model had even finished responding, misreported
as failures. Fixed by also requiring the input placeholder text
(confirming the box was genuinely cleared by a real submission) before
starting the completion timer; all 6 re-ran cleanly to 9/9 once fixed.
Since llama-server's own A5 result was already a clean 9/9 with no
malformed calls on this exact grid, SGLang's constrained decoding had no
regressed baseline to improve *on* — same ceiling, reached the same way.

**B3 (radix-cache benefit): confirmed working exactly as hypothesized,**
measured directly from SGLang's own per-request `#cached-token` telemetry
(a more precise signal than wall-clock TTFT, which the harness's own
typing/rendering overhead would have polluted) across a real two-turn
conversation: turn 1's tool-call continuation left 5568 cached tokens;
turn 2 — a genuinely new user message appended to the same growing
history aivyx-coder resends in full every turn — reused all 5568 of them
and only prefilled its own 255 new tokens. Zero re-computation of
anything the server had already seen. A full head-to-head wall-clock
number against llama-server's own `--cache-reuse` was not captured (out
of the spike's one-session budget, and not decision-critical — B4's gate
depends on B2, not B3), but the mechanism itself is real and automatic
for aivyx-coder's exact request pattern with no client-side change
needed.

**B4 (decision gate): no config surface added.** The gate was "aivyx only
grows config surface if B2 shows a material win" — it didn't, because
there was no headroom left to win: llama-server's existing baseline was
already a clean 9/9 with zero malformed calls, so constrained decoding
had nothing left to fix on this grid. This is a real, evidence-based
"no," not an inconclusive one — consistent with Phase 2's own "evidence
before architecture" rule. `aivyx-coder` itself was not modified in any
way during this spike (no aivyx architecture changes, per the spike's own
bound); this section is a documentation-only update recording the
result.

**vLLM compat pass, run and closed out (2026-07-18).** The one
README-claimed provider (Ollama/vLLM/llama.cpp) never live-tested until
now. Scope matched the roadmap's own framing: cheap, one session, official
image, one E2E run, no aivyx-coder code changes expected or made.

Reused the `QuantTrio/Qwen3.5-9B-AWQ` weights already on disk from the
SGLang spike (Part B) — no new model download needed, since vLLM natively
supports AWQ. Pulled the official `vllm/vllm-openai:latest` image
(vLLM 0.25.1) and served with:
```
docker run -d --gpus all --ipc=host \
  -v /home/julian/models/Qwen3.5-9B-AWQ:/model -p 8010:8000 \
  vllm/vllm-openai:latest \
  --model /model --served-model-name Qwen3.5-9B-AWQ \
  --max-model-len 16384 --enable-auto-tool-choice \
  --tool-call-parser qwen3_coder \
  --default-chat-template-kwargs '{"enable_thinking": false}' \
  --gpu-memory-utilization 0.85
```

Unlike the SGLang spike, **vLLM came up clean on the first attempt** —
no compat bugs found. Two things that were sharp edges in SGLang were
non-issues here: AWQ quantization was auto-detected from the model's own
`config.json` (no explicit `--quantization` flag needed), and the
Gated-DeltaNet/Mamba cache dtype matched the model's `float16` correctly
out of the box (vLLM's `MambaConfig` handles this natively — no
`SGLANG_MAMBA_CONV_DTYPE`-style env var workaround required). vLLM also
has a real `--default-chat-template-kwargs` launch flag (SGLang's
equivalent didn't exist despite general docs suggesting otherwise), so
forcing `enable_thinking: false` needed no patched chat template this
time.

Verified via raw curl: a plain completion came back with no reasoning
tokens and the correct answer; a tool-definition request produced a
schema-valid `tool_calls` array via the `qwen3_coder` parser. Then ran one
task (the same "discount" fixture from the A5/B2 grid: multiply
`calculate_total`'s return by 0.9) through the real `aivyx-coder` release
binary — PTY + `python-pyte` harness, graded via the persisted session
JSON. Result: a clean `read_file` → `edit_file` sequence, both tool calls
natively parsed (`"source": "Native"`), correct edit applied
(`total * 0.9`), one confirmation, no malformed calls, no retries.

**Verdict: vLLM is a genuinely working third provider for this project's
target model**, with a real compat story (native AWQ + native GDN dtype
handling, no adapter cost/hackery) that's better than SGLang's on this
same model. No aivyx-coder config surface was added — `base_url` already
generically points at any OpenAI-compatible endpoint, so vLLM needs zero
new code, only a config change, exactly like SGLang would have. Environment
fully restored afterward (container removed, `config.toml` back to
llama-server, llama-server restarted with its original invocation, VRAM
footprint confirmed matching pre-spike: 8267 MiB).

### Phase 11 — Candidate directions (scoped 2026-07-11, user-proposed)

Three external projects, each reinterpreted onto primitives aivyx already
has rather than ported. Recommended build order: 11a → 11b → 11c (rising
security surface), each behind its own design pass with user sign-off.
Status: **all three shipped and live-verified** — 11a (2026-07-11), 11c
(2026-07-13, built out of the recommended order once the capability audit
found it needed Phase 12's loop-mechanics prerequisites first), and 11b
(2026-07-13, see its section below).

**11a — Council mode** (inspiration: karpathy/llm-council — multiple
models answer, anonymously cross-rank, a chairman synthesizes; upstream
uses OpenRouter/cloud). *Our interpretation, local-only*: a `/council
<question>` TUI command for hard design decisions. Design pass complete
and signed off — see the Phase 11a section below.

**11b — Agent-maintained codebase wiki** (inspiration:
langchain-ai/openwiki — a CLI that writes and maintains agent-facing repo
documentation). *Our interpretation*: no separate tool — the agent itself
generates and maintains `docs/wiki/*.md` (architecture overview, module
guides, decision log) via a `/wiki` command, using its existing gated
read/search/edit/git tools; checkpoint refs make regeneration free to
attempt. The repo map gains awareness of wiki pages so they surface as
context when relevant. This mechanizes the project's own
"document design as we go" discipline. Open questions for its design pass:
injection policy vs budget, and staleness detection (git_read diffing
since the last wiki update is already enough plumbing).

**Built and live-verified (2026-07-13).** Followed a full design pass
(`docs/superpowers/specs/2026-07-13-phase-11b-agent-wiki-design.md`) before
implementation, mirroring how 11a and 11c were each signed off first. Not a
security-critical change — no new `ConfirmationGate` tier, every
`write_file` call goes through the existing confirmation modal unchanged.
Built: a deterministic staleness/frontmatter module in `aivyx-tools`
(`aivyx_tools::wiki`) that diffs each page's recorded generation commit
against HEAD, scoped to that page's covered paths via plain git pathspecs
(not globs — `git diff -- <pathspec>` already matches a directory prefix
recursively); a fixed page skeleton in `aivyx-core`
(`aivyx_core::wiki::page_specs`) — `architecture-overview.md` plus one page
per workspace crate, crate pages discovered from `crates/*/Cargo.toml`
rather than hand-maintained; and `Agent::run_wiki_turn`, which drives one
turn per stale page directly through `run_turn_inner` (not the public
`run_turn`, so a synthesized per-page instruction is never re-checked
against `/council`/`/wiki`), reusing the exact `TurnPaused`-continuation
mechanism Phase 12/11c already built. The repo map gained a lightweight
pointer-list section (page path + one-line summary) inside its *existing*
token budget — no new budget parameter, full page content stays
`read_file`-on-demand.

One deviation from the design doc, disclosed rather than silently
resolved: `architecture-overview.md`'s covered paths are a fixed constant
picked once at implementation time, not re-curated by the model on every
`/wiki` run as the design doc's more abstract framing suggested — keeps
staleness fully deterministic and avoids a fragile model-authored-
frontmatter-parsing path for a field whose correctness the whole staleness
mechanism depends on. `generated_at_commit`/`covers` are always stamped by
code after a page's turn completes, never trusted from what the model
wrote; only a model-provided `summary` is preserved (falling back to a
default when absent).

27 new tests (231 total, up from 204): real-git fixture tests for
staleness/stamping in `aivyx-tools` (mirroring `checkpoint.rs`'s own
style), `parse_command` tests mirroring `council::parse_command`'s
exactly, `Agent::run_wiki_turn` tested with a mock `LlmBackend` plus real
git repos (no pty needed — the reason this orchestration lives in
`aivyx-core`, not `aivyx-tui`), repo-map render tests. Per-task review
caught three Important findings, all inherited from the plan's own
reference code and all fixed before merge: a dead unreachable branch in
the frontmatter parser, an unguarded empty-`covers` pathspec in
`stale_pages` that would have silently become an unrestricted whole-repo
diff, and — the substantive one — an unbounded per-page auto-continue loop
with no cap on `TurnPaused` cycles (`/wiki` has no human at the pause the
way interactive mode does; fixed with a `MAX_WIKI_PAGE_CONTINUATIONS`
cap, the same shape as 11c's own autonomous-loop budget). Four live E2E
checks through the real binary (qwen3.5:9B via the Lemonade-managed
llama-server), all passing: (1) first run on a fresh 2-crate scratch
workspace generated all 3 pages (`architecture-overview` + one per crate),
each correctly stamped with the real HEAD commit and a preserved
model-written summary; (2) touching one crate and re-running `/wiki`
regenerated only that crate's page, leaving the other two untouched at
their original commit; (3) `/wiki <page>` force-regenerated an
already-up-to-date page regardless of staleness; (4) sending Ctrl+C
mid-batch stopped the run without processing further queued pages, and a
subsequent bare `/wiki` picked up exactly the page that was interrupted —
confirming the idempotent-resume property (no persisted queue state
needed) the design relied on.

**11c — Autonomous research loop** (inspiration: karpathy/autoresearch —
an agent iterates on a training script overnight against one metric with a
fixed time budget, keep-or-discard per experiment). *Our interpretation*:
`aivyx --auto "<goal>"` — loop: plan a small change → apply → run the
configured **metric command** (an `allowed_commands` entry; cargo
test/bench or a user score script) → keep (checkpoint + experiment-log
entry) or discard (**rewind to the pre-experiment checkpoint ref** — the
Phase 7 machinery is exactly the keep/discard primitive) → repeat within
an iteration/wall-clock budget, session log persisted throughout. aivyx
uniquely already owns every primitive this needs. The hard part is
deliberate: unattended operation means no human at the permission modal,
so the gate needs an explicit autonomous trust profile (edits confined to
the worktree + only pre-approved commands, everything else denied) —
a Phase-5-grade security design pass of its own, which is why this is
last. Doubly attractive after Phase 10: an overnight loop is where
llama-server's stability and explicit windows matter most.

**Built and live-verified (2026-07-12).** Followed the security-design
pass this sketch called for (`docs/superpowers/specs/2026-07-12-phase-11c-autonomous-loop-design.md`,
signed off before implementation) rather than building straight from this
paragraph. `aivyx --auto "<goal>"`: `ConfirmationGate` gained a fourth tier
— an `AutonomousMode` shared flag (mirroring `PlanMode` exactly, checked
after the plan-mode deny and before the Always-Allow cache) that trades the
interactive prompt for an unconditional trust profile: `write_file`/
`edit_file` auto-allowed only inside `cwd` (a new gate-level boundary check
— file edits have no Landlock scoping the way spawned commands do) and
outside `deny_paths`; `run_command` auto-allowed only against the
pre-seeded `allowed_commands` Always-Allow cache with no prompt fallback;
`run_shell`/`git_commit` hidden from the model's tool list entirely and
denied at the gate as a backstop (`git_commit`'s target is never cacheable,
so it falls out of the existing cache-miss-denies rule with zero
special-casing, exactly as designed). Exhausted verification retries now
trigger a real discard: `Agent` records a `pre_experiment_ref` checkpoint
the moment a batch of edits goes unverified, and on exhaustion (autonomous
mode only) rewinds the worktree to it via two new `GitCheckpointer`
methods (`latest_ref`, `restore_to` — `git add -A` into the private index
then `git read-tree --reset -u`, the git plumbing the design doc
deliberately left unverified pending real-git behavior; a reviewer caught
that the first `restore_to` draft omitted `deny_paths` exclusion from the
`git add -A` step, which would have deleted deny-listed files from disk on
the first rewind — fixed to mirror `checkpoint_inner` before this shipped).

One deviation from the design doc, called out explicitly rather than
buried: the design framed the loop driver abstractly (react to
`AgentEvent`s); the actual driver in `aivyx-tui::app::run` instead has
`Agent` expose a synchronous `last_turn_paused()` accessor, queried by the
same task that owns the turn's `CancellationToken` immediately after
`run_turn()` returns. This sidesteps a real constraint neither the sketch
above nor the design doc surfaced: `agent_events_rx` is an
`mpsc::UnboundedReceiver`, which only supports one consumer, and the render
loop already owns it — so the autonomous driver cannot also listen for
`TurnPaused` on that channel without racing the render loop for events.
Querying `Agent` directly after each `run_turn()` call is race-free by
construction, no cross-task inference needed.

22 new tests (199 total, up from 177): gate-tier ordering and cwd-boundary
tests in `aivyx-sandbox` (the boundary check relies on `path_resolve`'s
pre-existing symlink-escape resolution, exercised by
`symlink_escaping_cwd_does_not_resolve_to_a_path_under_cwd`, rather than
adding new symlink-specific gate tests), a real-git discard/rewind
fixture in `aivyx-tools`, tool-list filtering and the pause/rewind
integration in `aivyx-core`, driver-logic unit tests in `aivyx-tui`. Four
live E2E checks through the real binary (qwen3.5:9B via the
Lemonade-managed llama-server from the Phase 10 acceptance setup), all
passing: (1) happy path — goal achieved within budget, file created with
exact expected content, driver stopped without a further "continue"; (2)
cwd-boundary denial — asking the model to write to `/tmp` produced the
"worktree boundary" denial in the transcript with no file created outside
`cwd`; (3) discard/rewind — an always-failing verification command caused
the file to exist transiently then disappear after retries exhausted, with
the autonomous-specific "discarded this round of edits and restored the
worktree" notice (not interactive mode's wording); (4) Ctrl+C — cancelling
mid-generation of a deliberately slow turn (a long single-file write) left
the target file never created and the driver idle afterward with no
further turn started; a fast turn's cancellation window turned out to be
sub-second on this model/hardware, so this check needed a slow goal to
land reliably mid-stream rather than after the turn had already finished.

### Phase 11a — Council mode (designed + signed off 2026-07-11)

Multiple local models independently answer a hard question, anonymously
cross-rank each other's answers, and a chairman model synthesizes a
recommendation into the conversation. Read-only by construction: council
members receive **no tools**, so the feature is equally safe in normal
and plan mode and adds zero permission-gate surface.

Four design forks were put to the user and resolved (all on the
recommended option):

1. **Invocation — `/council` command only.** `/council <question>` works
   in any mode; bare `/council` convenes the council on the last
   assistant message (the "review this plan" ergonomic). No automatic
   plan-mode offers in v1 — surface stays minimal, and the command is
   intercepted in the agent loop *before* a normal turn starts, so the
   raw command text never enters LLM history.
2. **Membership — small members, big chairman, no service juggling.**
   Default council: the pulled 9B-class models via Ollama's
   swap-per-request (they fit alongside the resident llama-server daily
   driver); chairman `qwen3.6:27b` via Ollama, accepting partial-CPU-
   offload latency for its one synthesis call. Explicitly rejected: GPU
   handover (stopping llama-server mid-council is stateful and
   failure-prone — a crash leaves the daily driver down). A council is a
   deliberately latency-tolerant feature; correctness of the deliberation
   beats turn speed.
3. **Context — question + token-budgeted conversation tail.** Members
   see the question plus a bounded digest of the recent conversation
   (`tail_budget_tokens`, default ~3k), built with the Phase 8
   self-calibrating estimator. Not the repo map (N models × map tokens
   per swap-in) and not the full history.
4. **Persistence — synthesis only enters LLM history.** The full
   deliberation (every answer, every ranking, the reveal table) renders
   live in the TUI transcript; only the chairman's synthesis — as a
   clearly-delimited user-role message with the anonymization reveal
   appended — is pushed to history, so it survives resume and compaction
   without a single council eating half a 16k window.

**Protocol** (all stages sequential — one GPU):
- *Stage 1, answers*: each member gets a shared advisor system prompt +
  the tail digest + the question; plain `stream_chat`, no tools, text
  collected (any literal `<think>…</think>` spans stripped defensively).
- *Stage 2, ranking*: answers are shuffled (std `RandomState` hash
  ordering — no new dependency) and labeled Advisor A/B/C…; each member
  ranks all answers with one-line justifications. Anonymity is for the
  models (no brand favoritism), not the user — the TUI shows real names
  as each model runs.
- *Stage 3, synthesis*: the chairman gets question + anonymized answers
  + rankings and produces the final recommendation.

**Failure handling** (fail closed, degrade loudly): a member that errors
or times out is skipped with a transcript note; quorum is 2 collected
answers, below which the council aborts with no history entry. A chairman
failure also means **no history entry** — the raw deliberation stays
visible in the transcript, but nothing unsynthesized is fed back to the
agent. Ctrl+C cancels cleanly via the same `CancellationToken` as a
normal turn. Council backends are built with a longer idle timeout than
the interactive default (60s), because a cold Ollama load of a 17 GB
chairman can exceed 60s before the first token.

**Config** (`[council]`, absent = feature off — the command then explains
how to enable it):

```toml
[council]
members = [
  { base_url = "http://localhost:11434/v1", model = "qwen3.5:9b" },
  { base_url = "http://localhost:11434/v1", model = "ornith:9b" },
  { base_url = "http://localhost:11434/v1", model = "lfm2.5" },
]
chairman = { base_url = "http://localhost:11434/v1", model = "qwen3.6:27b" }
tail_budget_tokens = 3072
```

Each entry is just another `OpenAiCompatBackend` — the Phase 10 lesson
that the generic `/v1` design needs zero per-provider code is what makes
a mixed llama-server/Ollama council free.

**Built + live-verified (2026-07-12).** 171 workspace tests (13 new:
protocol, quorum, chairman-failure, bare-`/council`, anonymization,
digest budgeting), clippy clean. Live E2E through the real TUI against
Ollama with the design's exact membership: three member answers in
25–52s each (swap-per-request as predicted), cross-rankings, and the
qwen3.6:27b chairman synthesis — with llama-server not yet resident the
chairman ran fully on-GPU. Verified at the session-file level: exactly
one history entry (the marked synthesis), the raw `/council` command
absent, and a `--resume` follow-up turn on the main backend correctly
answering from the synthesis. One harness lesson re-learned: raw pty
capture of ratatui output garbles wrapped lines under cell-level
redraws — assert on the persisted session file, not the screen, when
the claim is about history.

## Capability audit — gaps toward a Full Autonomous High-End Coding Agent (2026-07-12)

An audit against the project's stated end-goal (a high-end vibe-coding
agent for an end user, with a path to full autonomy), separate from and
broader than the routine security audits referenced elsewhere in this
document. Foundation verdict first, then gaps ranked by how structural
they are; the smaller ones are folded into Phase 9 above, the two
structural ones become Phase 12 below.

**Foundation holds up.** The permission-gate → checkpoint → sandbox
pipeline doesn't need a redesign for autonomy, only extension:
`PlanMode`'s pattern (a shared flag the gate enforces independently of
what's offered to the model) is exactly the shape an autonomous trust
profile would take; checkpoints are already the keep/discard primitive
Phase 11c's experiment loop needs; `allowed_commands` is already the
pre-approved-execution tier; `Tool::execute` being reachable only through
`ToolExecutor::dispatch` means new tools (LSP, refactor primitives,
sub-agents) can't accidentally bypass the security model. Worth stating
plainly because it changes what "closing the gap" actually costs —
additive work, not a rewrite.

**Gap 1 (most structural) — the turn loop fails on a cap, not a goal
boundary.** `run_turn_inner`'s loop is bounded by `max_tool_iterations`
(default 25) per *user turn*; hitting it returns
`AgentError::MaxIterationsExceeded` rather than pausing gracefully.
Checked against the actual code before writing this down (this project's
own habit — see Phase 5's bubblewrap re-check, Phase 2's
plan-vs-evidence check): state is *not* lost — `run_turn`'s wrapper calls
`self.persist()` unconditionally regardless of the `Result`, and every
tool call/result up to the cap is already in history — so in interactive
use a human can just send another message and the agent picks up where
it left off. The real gap is (a) this is presented as a failure
(`AgentEvent::Error` + `Err`) rather than a natural pause, which is
misleading UX today, and (b) in an *unattended* run there is no one to
send that next message, so an autonomous session would simply stop at 25
round-trips with no continuation mechanism at all.

**Gap 2 — verification is 100% emergent, never enforced.** The tools to
close an edit→build→test→fix loop all exist (`run_command`, checkpoints
to roll back a bad attempt), but nothing requires the model to actually
use them before ending a turn. This directly contradicts the project's
own cited research finding ("verification loops are the biggest lever
for actual task success... a deterministic compiler/test-runner as the
feedback signal is more reliable for a weak local model than asking it
to self-critique") — Phase 4 built the *tool*, nothing yet builds the
*policy*, and this project's own benchmarks (Phase 2, Phase 10) already
show 9B-class models won't reliably self-verify unprompted.

Gaps 1 and 2 are close to a shared prerequisite for Phase 11c's
experiment loop, which needs both a continuation mechanism and a
deterministic verify step to do keep/discard at all — scoped together as
Phase 12 below, ahead of 11c.

Already-tracked gaps (LSP, MCP, sub-agent delegation, architect/editor
pairing, the wiki, the autonomous loop itself) aren't re-litigated here —
see Phase 9 and Phase 11b/11c — except to sharpen priority: sub-agent
delegation and architect/editor pairing are higher-leverage than they
look, because they compound with what this project already does well
rather than requiring new architecture.

### Phase 12 — Agent loop foundations: goal-bounded continuation + enforced verification — ✅ done

Surfaced by the capability audit above as the actual prerequisite to
Phase 11c, ahead of the security-profile work: an autonomous loop is
pointless without (a) a way to keep working past today's per-turn
iteration cap without a human re-prompting, and (b) a verification step
that runs whether or not the model chooses to call it. Both close gaps in
the *existing* interactive product too, not just the future autonomous
one — scoping them together because Phase 11c's "keep or discard"
experiment loop needs exactly this pairing (continue working, then
deterministically check the result) as its core primitive.

**Part A — turn-loop continuation.**

Design (signed off 2026-07-12, not yet built):
- Keep `max_tool_iterations` as a per-round-trip safety valve (protects
  against a single response accidentally looping forever), but stop
  treating hitting it as an `AgentError`. On hitting the cap while the
  model was still actively issuing tool calls (i.e. not a natural
  no-more-tool-calls stop), emit a new event — `AgentEvent::TurnPaused`
  or similar — distinct from both `TurnComplete` and `Error`, carrying
  whatever the task list currently shows as progress. The TUI renders
  this distinctly from an actual error (no red `!` notice — something
  closer to the plan-mode toggle notice).
- In interactive mode, this alone fixes the misleading-failure UX: the
  session is already resumable by construction (`--resume`, or simply
  continuing to type in the same running session — history is already
  intact in memory, not just on disk).
- For autonomous use (still gated behind Phase 11c's own trust-profile
  design, not unblocked by this phase alone): a *second*, coarser budget
  — wall-clock and/or total-tool-call count for the whole unattended
  session, distinct from the existing per-round-trip cap — governs when
  an autonomous run auto-continues (re-enter the loop with a synthesized
  "continue" turn reusing persisted history) versus when it must stop and
  surface a report. This is Phase 11c's territory to design in full (it
  also owns what tool calls are permitted with no human at the modal);
  Phase 12 only needs the *loop mechanism* itself to support being
  re-entered without erroring, so 11c isn't also fighting the turn loop's
  shape when it lands.
- **Decided (2026-07-12): keep a hard ceiling.**
  `AgentError::MaxIterationsExceeded` doesn't disappear — it's repurposed
  rather than removed. The per-round-trip `max_tool_iterations` cap
  hitting mid-work now emits `AgentEvent::TurnPaused` (graceful,
  resumable, not an error) as designed above; the hard error fires only
  when the coarser autonomous-session budget is *also* exhausted with no
  natural stopping point reached — the last-resort ceiling moves up a
  level rather than disappearing. An unbounded loop stays a real
  resource-exhaustion risk independent of the pause-vs-fail UX question,
  so something must still be able to say no.

**Part A built and live-verified (2026-07-12).** New
`AgentEvent::TurnPaused(String)` and matching `ChatLine::Paused` (blue,
`"  ~ paused: "` prefix — deliberately not `Notice`'s red/bold, which
today also carries real errors and the plan-mode toggle). The
per-round-trip cap-hit site in `run_turn_inner` emits this and returns
`Ok(())` instead of constructing `AgentError::MaxIterationsExceeded`;
that variant stays defined (per the decision above) but is now
unconstructed anywhere in the crate — confirmed `cargo build`/`clippy`
raise no dead-code warning (it's a `pub` enum reachable outside the
crate). Two rewritten/new unit tests cover the pause firing without an
`Err` and without dropping any dispatched tool call from history, and a
follow-up `run_turn` call cleanly resuming the same conversation. Live
E2E through the real binary (qwen3.5:9B via the Lemonade-managed
llama-server from the acceptance benchmark above, `max_tool_iterations`
forced to 1): the transcript showed the tool call, its result, and then
the blue paused notice — never the red error style — with the status
line back to `ready` afterward, confirming the fix end-to-end rather than
just at the unit level.

**Part B — enforced verification.**

Design (signed off 2026-07-12, not yet built):
- **Decided (2026-07-12): config surface is just `[verification] command
  = "test"`** (references an existing `[[permissions.allowed_commands]]`
  entry by name — reusing the existing trust tier rather than inventing a
  new one) — **no separate enable flag.** Configuring the command *is*
  the opt-in, matching how `allowed_commands` itself works (empty/off by
  default, but adding an entry makes it live immediately) and avoiding a
  silent trap where someone defines a verification command expecting
  enforcement and gets nothing because they missed a second flag. Still
  "off until configured" for any project that hasn't set `[verification]`
  at all, so existing users see no behavior change.
- Mechanism: track, per turn, whether a mutating file-tool call
  (`write_file`/`edit_file`) has happened since the last successful
  verification run. When the model produces a response with no further
  tool calls (today's `TurnComplete` signal) while that's true and a
  `[verification] command` is configured, don't end the turn yet —
  synthesize a verification `ToolCall` the same way prompted-mode
  SEARCH/REPLACE blocks already get synthesized into real tool calls (a
  new
  `ToolCallSource` variant alongside `Native`/`TextFallback` — the
  pattern already exists and dispatches through the exact same
  gate/checkpoint path with no new security surface), dispatch it, and
  feed the result back into history so the model can react —
  fix-and-retry on failure, or naturally end the turn on success. Bounded
  by the existing `max_tool_iterations`/`MAX_TOOL_CALLS_PER_RESPONSE`
  caps, so this can't loop forever either; add `max_auto_verify_retries`
  as its own explicit cap so a stuck fix-loop surfaces to the human
  distinctly from a generic iteration-cap hit.
- Never fires in plan mode (no mutating calls happen there by
  construction) or when nothing was actually edited this turn (a
  read-only Q&A turn shouldn't force a test run).
- On repeated verification failure, surface the existing checkpoint refs
  as the recovery path ("still failing after N attempts — the worktree
  was checkpointed before each edit; `git log refs/aivyx/checkpoints/` to
  inspect or rewind") rather than building any new rewind mechanism —
  Phase 7 already owns that primitive.
- **Decided (2026-07-12): a failed final verification (retries exhausted)
  completes the turn with a loud, un-missable notice, not a block.**
  `TurnComplete` still fires — consistent with every other "never
  silent" pattern already in this codebase (truncation notices,
  plan-mode toggles, denial reasons) — but carries a prominent notice
  that verification never passed, plus the checkpoint-recovery hint
  above. Keeps the transparency-over-magic principle intact (the same
  one Phase 7 leaned on for the auto-commit question): the agent hands
  control back rather than unilaterally deciding the turn can't end, and
  the user decides what happens next.

**Part B built and live-verified (2026-07-12).** New
`ToolCallSource::AutoVerification` (the same "synthesize a real `ToolCall`"
pattern prompted-mode SEARCH/REPLACE blocks already established — a
synthetic assistant message carrying the one call, dispatched through the
unchanged executor/gate/checkpoint path, then its result — needed because
OpenAI-compatible wire format requires every `Role::Tool` result to
correspond to a preceding assistant `tool_calls` entry). New
`[verification]` config (`command`, `max_auto_verify_retries`) and
`Agent::set_verification`; `main.rs` warns at startup rather than silently
doing nothing if `command` doesn't match an `allowed_commands` name. The
synthesized call targets the existing `run_command` tool by name, so it
inherits that tool's pre-approval seeding automatically — no new
permission-gate surface. Pass/fail is read from `run_command`'s own
formatted-output verdict marker (`(success)`/`(failed)`) since a failing
command is normal `Ok` output for that tool, not a tool-level error — the
only signal available without changing that tool's contract, and a
documented fragile coupling to `aivyx_tools::process::format_output`'s
exact wording. `unverified_edits`/`verify_retries` are `Agent` fields, not
per-turn locals, deliberately spanning turn boundaries — a turn pausing
mid-edit (Part A) must not let unverified edits be silently forgotten by
whichever turn continues it — and `verify_retries` resets to 0 both on a
pass and on exhaustion (never on a mid-attempt continue), so the feature
can't silently disable itself for the rest of the session after one bad
run. A new `VERIFICATION_PROMPT` system-prompt note (mirroring the
existing plan-mode/edit-format notes) tells the model the auto-injected
`run_command` call in its own history isn't something it called itself.
One implementation-level clarification beyond the original design text: on
a *passing* verification the turn completes immediately without spending
an extra model round-trip (the design's "naturally end the turn on
success" read as "no extra chattiness needed," not "ask the model to
confirm the pass") — only a *failing* verification spends a round-trip, so
the model can see the failure and react. Three new unit tests (pass with
no extra round-trip, fail-until-exhausted with the correct retry count and
notice text, and never-fires-when-nothing-was-edited) plus two in
`aivyx-config` for the config parsing; 177 workspace tests, clippy clean.
Live E2E through the real binary (the same Lemonade-managed qwen3.5:9B):
asked it to create a file containing a specific word, verification
configured to grep for that word — the transcript showed the write, the
model's own "Done!", and then, unprompted, `auto-verify:
run_command({"command":"verify"})` → `exit status: 0 (success)` → turn
complete, with no permission modal (the pre-approval reuse working
exactly as designed) and no extra round-trip.

No open questions remain in either part. **Sequencing**: both parts land
together (they share the "the model isn't the one deciding when a turn
actually ends" premise) before Phase 11c's
security-profile design pass begins — 11c can then focus entirely on the
permission-tier question instead of also inventing loop mechanics.

## Notes on sequencing

Phase 12 is scoped ahead of Phase 11c deliberately: the capability audit
above found the autonomous loop's actual prerequisite isn't just a
security-profile redesign but two loop-mechanics gaps (goal-bounded
continuation, enforced verification) that 11c would otherwise have to
solve inline while also doing its own security work — cheaper to land
them as their own pass first.

Phases 1-2 are deliberately reliability-first rather than feature-first: the
research and this project's own capability testing agree that adding tool
surface on top of unreliable tool-calling/editing just multiplies failure
modes rather than fixing the underlying one. Phases 3-4 are the cheapest,
highest-consensus additions (every agent studied has them, none need new
infrastructure). Phase 5 is gated behind Phase 1-4 partly because it's the
biggest single chunk of new complexity (real sandboxing) and partly because
shell-exec is exactly where the `Command`-target permission gaps matter —
better to have the simpler tools' patterns settled first.

### Editor/IDE context integration — ✅ done

The first entirely new feature phase after the GitHub push (2026-07-18).
The agent now polls a local, editor-agnostic JSON descriptor file
(`~/.local/state/aivyx-coder/editor-context/<fnv1a-hash>.json`, the same
keying scheme as session files) describing what file/line/selection is
open in the user's editor and, if present, schema-valid, fresh (under 5
minutes), and `workspace_root`-matched, injects a one-line metadata note
into the system prompt every turn (e.g. "Currently open in editor: {file},
cursor at line {line}."). Deliberately editor-agnostic — no plugin code
for any specific editor ships — and metadata-only: the model never
receives raw file/selection content this way, only a path and
line/column numbers, so it still has to call `read_file` itself for
actual code. This preserves the project's existing invariant that file
content only enters the conversation via an explicit, visible tool call.
Gated by a new `[editor_context] enabled` config flag (default on).
Live-E2E verified through the real release binary: the model correctly
answered a cursor/file question with zero tool calls, proving the
injected note alone (not a `read_file` call) informed the answer.

**Real security finding from this phase's whole-branch review**: the first
implementation interpolated the editor-context JSON's `file` string
verbatim into the system prompt with no sanitization — a crafted value
containing embedded newlines/control characters could have forged fake
instructions at trusted-prompt trust level (the numeric cursor/selection
fields were always safe; only the free-form path string was the gap).
Fixed by sanitizing only the *displayed* copy of the string (strip ASCII
control chars + C1 controls, clamp length to 512 chars) while leaving the
real, unsanitized path untouched for the actual `deny_paths` security
check — sanitizing the security-relevant copy would have weakened that
check for no reason. General lesson: any new data source formatted into
the system/trusted prompt — even one that's supposedly "just metadata" —
needs the same string-sanitization scrutiny as tool output, because the
trust boundary is about prompt position, not about whether the data looks
like harmless structured fields.

### Editor approval integration — ✅ done

The "context out" follow-on to editor/IDE context integration (2026-07-19).
The user's editor can now answer a pending `ConfirmationGate` permission
decision (write/edit/delete/execute/MCP-tool) as a fully equal-trust second
surface, racing the terminal's own Allow/Deny/Always-Allow prompt via
`tokio::select!` — first decision wins, the other is dropped. Transport:
two polled JSON files (request + response) under
`~/.local/state/aivyx-coder/editor-approval/`, the same FNV-1a-of-cwd
keying scheme as `editor_context`/sessions. `[editor_approval] enabled`
defaults to `true` — a deliberate, reasoned exception to this project's
usual conservative-security-default posture, since the feature is
genuinely inert without an active external process writing a response
file, unlike a feature that's live the moment it's flipped on.

**Real architectural correction found during plan-writing**: the original
spec placed the new schema/path-keying module in
`crates/aivyx-core/src/editor_approval.rs`, mirroring the sibling
`editor_context.rs`. This was wrong — `aivyx-sandbox` (where
`ConfirmationGate`, the only consumer, lives) has zero dependency on
`aivyx-core`, so that placement would have created a dependency cycle.
Caught by directly checking both crates' `Cargo.toml` files before writing
the plan. The module ended up in
`crates/aivyx-sandbox/src/editor_approval.rs` instead; `aivyx-core` never
touches this feature's logic at all, unlike `editor_context`.

**Real, previously-unanticipated finding surfaced only by the live E2E
task, not by any unit test or code review of the touched crates**: when
the editor wins the race, nothing told the TUI's render loop (a crate none
of the plan's tasks originally touched) to dismiss its own now-stale
"Permission required" modal, since the modal was only ever cleared via a
keypress. Fixed with an actively-woken `tokio::select!` branch using
`oneshot::Sender::closed()` (fires the instant the other race branch's
receiver drops) plus a belt-and-braces render guard for the one-frame
window before that branch fires. General lesson: a live E2E through the
real UI can surface integration gaps that no amount of unit-testing the
changed crates in isolation would catch, because the gap can live in a
crate the plan never scoped as "touched."

`ConfirmationGate::check`'s race logic got the most scrutiny of any single
task in this phase: independently re-verified twice (task review and
whole-branch review) that the pre-existing security tier order
(deny_paths → Read/Internal auto-allow → plan-mode deny → autonomous-mode
resolution → Always-Allow cache → interactive prompt) was completely
untouched, and that autonomous mode is structurally unreachable from the
new race logic.

### Capability-gap-closing chapter — ✅ done

Following a direct audit of whether aivyx-coder can actually write real
code/scripts/small applications (2026-07-19) — the answer was no: no
genuine from-scratch multi-file build had ever been tested, only narrow
edit-existing-file benchmarks and single-tool-call E2Es. The audit
surfaced 4 concrete, previously-undocumented gaps, sequenced by the user
as separate sub-projects, each following this project's full
spec → plan → subagent-driven-development → finishing-a-development-branch
cycle. Motivating context for the whole chapter: once all 4 closed, the
user planned to deploy the agent to a real bare-metal test rig (previously
used for the sibling Aivyx-Agent project) as a genuine "give it a
playground" trial.

**Gap 1 — multi-file edit atomicity.** Closed the "N independent
`edit_file` calls, no transactional guarantee" gap the audit itself had
called the biggest reliability multiplier for any real cross-file change.
When a model response contains multiple mutating tool calls and a later
one fails, every earlier successful call in that same response now
automatically rolls back to the checkpoint from before the batch started —
reusing 100% pre-existing checkpoint/restore infrastructure, zero new
dependencies, entirely scoped to `crates/aivyx-core/src/agent/mod.rs`'s
turn loop. The rollback notice is folded directly into the failing call's
own error text, a deliberate correction to the original spec's
"separate synthetic message" wording, made after discovering this
codebase's only precedent for narrating agent-internal events into
history (`run_auto_verification`) always fakes a complete call+result pair
against an already-registered real tool, never a bare unpaired note.

A real, subtle bug was found and triple-verified during this phase: the
plan's own literal code anchored the rollback target from the wrong
checkpoint ref, colliding `Option<String>`'s `None` between "not yet set"
and "no prior checkpoint exists" — silently failing to roll back a batch
with exactly one success before a failure. Fixed by anchoring from the
checkpoint the successful call itself just minted, rather than whatever
ref existed before it. Independently re-derived and confirmed correct by
hand three separate times (implementer, task reviewer, whole-branch
reviewer).

**Gap 2 — reasoning visibility.** A reasoning-capable model's
chain-of-thought now renders live as a dimmed/italic "thinking:" line in
the TUI transcript, distinct from the final answer. Threaded through the
same three-enum pipeline (`StreamEvent` → `AgentEvent` → `ChatLine`)
`TextDelta` already uses at each hop, but with one deliberate, load-bearing
difference: reasoning content never touches `Agent`'s own
`history`/session JSON — display-only, forgotten once shown. All 4 tasks
passed individual review with zero findings each; the whole-branch review
even found a bonus safety property the plan never claimed: reasoning is
also invisible to prompted-edit-mode's SEARCH/REPLACE parser, since that
only ever reads `assistant_text`.

A real empirical correction was found during this phase's brainstorming:
the existing code's own doc comments already flagged the
"reasoning content is silently dropped" gap, but named the wire field
`delta.reasoning`, which is wrong. Verified live against a real
llama-server + Qwen3.5-9B-GGUF response that the actual field is
`delta.reasoning_content` — the DeepSeek API's original naming, since
adopted by llama-server/vLLM for compatibility. This was also the first
time this project needed to stop/restart the user's real running
local-LLM serving process as a means to an end (to force thinking on for
the live E2E, confirmed safe with the user first, independently
re-verified restored byte-identical afterward).

**Gap 3 — structured verification memory.** A failing `[verification]
command` result now gets a short "N line(s) of this output were not
present in the immediately preceding verification attempt" note appended
— a coarse line-set diff against whichever verification run came
immediately before, regardless of that prior run's own pass/fail outcome.
This last point is a deliberate, non-obvious semantic: comparing only
against the last *successful* run would never help either a codebase that
starts broken or a genuine fix-and-retry loop, a gap found and corrected
during spec-writing itself (surfaced to the user directly rather than
silently changed). In-memory only (a new `Agent.last_verification_output`
field, never persisted to the session JSON), framework-agnostic by design
— no test-runner-specific parsing, since the configured verification
command is genuinely user-chosen.

A second correction was found during implementation: the first submission
discovered its own test asserted on a substring that didn't actually
appear in the spec's mandated note wording, and "fixed" it backwards —
shortening the actual model-facing message (deleting the explicit
"this is a coarse heuristic, not a precise test diff" disclaimer) to make
the test's string match, rather than fixing the test's own assertion.
Caught at task review by treating the reviewer's brief as the source of
truth for exact required wording. Live-E2E-confirmed the note correctly
named only a newly-broken test while omitting an already-failing one
present in both attempts' raw output.

**Gap 4 — repo-map multi-language support.** The repo map (tree-sitter
symbol extraction + PageRank over the cross-file reference graph, appended
to the system prompt) now covers Python, JavaScript/JSX, and
TypeScript/TSX in addition to Rust. A data-only `LanguageConfig` table
(`crates/aivyx-repomap/src/languages/{mod,rust,python,javascript,
typescript}.rs`) replaced the old hardcoded-to-Rust `Extractor` — a plain
struct-per-language table, not a trait, matching this crate's existing
non-abstraction style. The walk/cache/PageRank/render machinery downstream
of extraction was already 100% language-agnostic, so a mixed-language
repo gets one unified map for free with zero special-casing. Live-E2E
confirmed with the strongest possible evidence: the model answered a
cross-file Python question with zero tool calls, citing "the repository
map provided" directly.

Three independently-verified real bugs were found during this phase's own
plan-writing and implementation, a concrete case study in why grounding
claims against the actual grammar/compiler pays for itself: (1) the
design spec's own illustrative `js_signature_node` sketch checked only one
parent hop for JS/TS export detection, but exported
`const foo = () => {}`-style bindings need a second hop — found by tracing
the real `tree-sitter-javascript` grammar's `node-types.json` before
writing the plan; (2) the plan's own illustrative `for_extension()`-as-a-
method code doesn't compile — Rust's borrow checker can't see through an
opaque method call to know it only touches one struct field, fixed by
inlining a direct field-indexed lookup instead; (3) TypeScript's
`class_declaration.name` field is `type_identifier`, not `identifier` as
plain JavaScript's is — a real, easy-to-miss grammar divergence between
the two `tree-sitter-*` crates, caught only by a runtime `Query::new`
panic if not verified in advance. A related test-quality lesson: a
TypeScript reference-tracking test initially passed vacuously (a type
matching its own declaration node, not a genuine usage site) — caught via
actual mutation testing (temporarily breaking the real mechanism and
confirming the test then failed), fixed by strengthening the assertion.

**Closing state**: 452 workspace tests, clippy clean across all 4
sub-projects. No tracked items remain from this audit. `main` was pushed
to GitHub immediately after the chapter closed.

### ACP editor integration — ✅ shipped, one verification step still open

The user asked to scope a VS Code extension and a Zed extension so an
end user could plug aivyx-coder into their editor (2026-07-20). Research
done before any design work changed the shape of the ask: Zed's own
WASM extension API cannot build custom agent UI at all any more — no
panels, and "extension-provided agents are deprecated" — leaving the
[Agent Client Protocol](https://agentclientprotocol.com) (ACP), a
JSON-RPC-over-stdio standard Zed authored and open-sourced, as the
*only* integration point for Zed. VS Code turned out not to need bespoke
code either: a mature, open-source community extension
(`formulahendry.acp-client`) already connects to any ACP-compatible
agent. So "two bespoke editor extensions" became "one ACP server mode,"
confirmed with the user before any design was written. This is also the
follow-on the two prior editor-integration phases (context-in, approval-out,
both above) explicitly deferred: their specs both said "no
VS Code/Neovim/other plugin code ships here — that's a separate, later
project."

**What shipped**: a new crate, `crates/aivyx-acp`, structurally a
sibling to `aivyx-tui` — a thin frontend over the same
`aivyx-core::Agent`, built on the official `agent-client-protocol` Rust
crate (resolved to 1.2.0, pulling in `agent-client-protocol-schema`
1.4.0). `aivyx --acp` runs the ACP server loop over stdio instead of the
TUI. `translate.rs` maps `AgentEvent` to ACP `SessionUpdate`/`StopReason`
values, pure and unit-tested with no I/O. `prompter.rs`'s `AcpPrompter`
implements the same `PermissionPrompter` trait `TuiPrompter` does,
sending `session/request_permission` instead of bridging to a render
loop — and is the *only* place in the whole crate a real diff reaches
the client, mirroring `editor_approval::build_pending_request`'s
existing `ActionKind` match. `session.rs` owns session lifecycle: one
session per process (parallelism is the editor spawning multiple
processes, not this crate hosting multiple sessions), `session/prompt`
drives one turn of `Agent::run_turn` while streaming events out,
`session/set_mode` toggles `PlanMode` the same shared flag the TUI's
Ctrl+P does. A prerequisite refactor pulled `main.rs`'s ~460-line
TUI-agnostic construction sequence (config → tools → MCP discovery →
`ConfirmationGate` → `Agent::new` → every `agent.set_*` call) into a new
`agent_builder.rs`, so both frontends build `Agent` from provably
identical code — verified behavior-preserving by an unchanged full test
suite plus a manual TUI smoke check, before any ACP code was written on
top of it.

**Real deadlock found and fixed during implementation, not caught by
design or planning.** The plan's own code ran `agent.run_turn(...)`
inline inside the ACP `PromptRequest` handler. This deadlocks: the
handler runs on `agent-client-protocol`'s single serial dispatch loop
(`incoming_protocol_actor`), and `AcpPrompter`'s own
`session/request_permission` call uses `block_task()`, which needs that
same loop free to route the response back — so the first gated tool
call in any real prompt (any `write_file`) would hang the whole
connection forever. Found by the task's own implementer investigating a
flag raised during the *previous* task's review (Task 4's reviewer
independently noticed `block_task()`'s own doc comment warns against
exactly this call shape). Fixed by moving the whole turn body into
`connection.spawn(...)` — offloading it to a task outside the blocking
dispatch loop, following a pattern the `agent-client-protocol-cookbook`
crate itself recommends for "expensive work." A second-order version of
the same class of bug was caught in the same pass: `session/set_mode`
or a second `session/new` arriving mid-turn would deadlock identically
if either awaited the session mutex a live turn holds — fixed by making
`PlanMode` toggling lock-free (the existing shared `Arc<AtomicBool>`,
untouched by the session mutex). Both fixes were independently
re-verified against the actual installed crate source by an opus-tier
task reviewer (dispatch-loop serialization, `Responder` being `Send`
and safely deferrable into a spawned task, no double-response) — not
accepted on the implementer's word — and re-confirmed by the final
whole-branch reviewer. The honest caveat every reviewer in this chain
recorded: **static analysis cannot fully prove the absence of a runtime
deadlock**; only a real permission round-trip against a real ACP client
can. See "Still open" below.

**Real bug found only by the final whole-branch review, not by any
individual task review**: `translate_event` grouped `AgentEvent::Error`
into the same "turn-terminal, handled elsewhere" bucket as
`TurnComplete`/`TurnPaused`, with a comment claiming `terminal_stop_reason`
handled it — but `terminal_stop_reason` never matches `Error` at all, so
the event was silently dropped. This was a defect in the plan's own
Protocol Mapping table, not just the implementation: `AgentEvent::Error`
fires from roughly a dozen call sites inside `run_turn` for advisory
conditions that do *not* end the turn (backend/tool failures,
verification failures, context-truncation warnings), so a Zed/VS Code
user saw nothing where a TUI user sees a transcript line. Exactly the
kind of cross-task seam a single task's reviewer can't catch — Task 3's
reviewer had no reason to doubt the mapping the plan itself specified,
and Task 5's reviewer was focused on the concurrency fix. Fixed by
surfacing `Error` the same way `CouncilNote`/`ArchitectNote` already
are: a plain `AgentMessageChunk`, not a dropped event or a
prompt-failing JSON-RPC error.

**Other real findings along the way**: agent-client-protocol-schema
1.4.0's types are almost universally `#[non_exhaustive]`, so the
research-phase design code's plain struct literals don't compile against
the real crate — every construction site needed each type's `::new(...)`
+ builder-setter pattern instead, discovered incrementally task-by-task
and each time carried forward as explicit context into the next
dispatch. `ToolCallContent`'s blanket `From` impl lives on the enum, not
its inner `Content` struct — a subtle trap where the "obviously right"
`.into()` call silently targets the wrong level. A permission request's
`ToolCallUpdate.tool_call_id` was originally derived from the tool's
*name* (`"write_file"`), not a per-call identifier — collides across
repeat calls to the same tool in one session (editing file A, then file
B) exactly the way `ConfirmationGate`'s sibling `editor_approval`
channel had already solved once with its own `request_id`; fixed the
same way, a fresh UUID per permission request.

**Still open**: the deadlock fix has been verified two independent ways
by static/source-level analysis and by construction (the same shared
`PlanMode`/lock-free pattern this codebase already trusted elsewhere),
but never by a real end-to-end run — no LLM backend or Zed installation
existed in the sandboxed environment this chapter was implemented in.
`README.md`'s "Editor integration (ACP)" section documents the exact
manual steps: point Zed's `settings.json` at the built binary, send a
prompt that triggers a real gated tool call (to exercise
`AcpPrompter`'s `block_task()` path for real) and, per the final
review's recommendation, a prompt that deliberately errors (to exercise
the `AgentEvent::Error`-surfacing fix too). This project's own
live-E2E-over-static-claims bar — the same standard every phase above
this one was held to — hasn't been cleared for this feature yet.

**Closing state**: 473 workspace tests (up from 452), clippy clean
across the whole workspace. Seven-task subagent-driven-development plan
executed in one branch (`docs/superpowers/plans/
2026-07-20-acp-editor-integration.md`), every task individually
reviewed, one Important finding fixed mid-stream (the `tool_call_id`
collision above), a final whole-branch review that caught and fixed the
`AgentEvent::Error` bug above before merge. Merged to `main` and pushed
to GitHub.

### Bare-metal trial: PATH collision fix, live testing, and a real `agent-client-protocol` bug — ✅ done

The motivating question: can `aivyx-coder` and the sibling Aivyx
Personal Assistant (a separate, unrelated project sharing only naming
and authorship — see the top-level workspace `CLAUDE.md`) coexist on
the same machine an end user might run both on? Investigation found
their config/data directories are already fully disjoint
(`~/.config/aivyx-coder` vs. `~/.aivyx`, `~/.local/state/aivyx-coder`
vs. `~/.local/share/aivyx`), but both projects built a binary literally
named `aivyx` — a real collision if a user installed both to the same
`bin/` directory, with whichever installed second silently overwriting
the first. Fixed by renaming the installed binary to `aivyx-coder` via
`[[bin]]` in `crates/aivyx/Cargo.toml`, keeping the package name `aivyx`
(so `cargo run -p aivyx` and the ~38 existing test call sites across
`confirmation.rs`/`agent/tests.rs` needed no changes) — a builder/setter
pattern over new required parameters, the same trade-off later reused
for the autonomous-mode injection guard below.

**Live testing on real hardware.** Deployed the release binary to a
bare-metal rig (CachyOS, RTX 3090) with `llama-server` + `qwen3.5:9b`
— this project's own documented best-serving-config verdict from Phase
10. First attempt used Qwen2.5-Coder-7B instead; its GGUF chat template
didn't parse into structured `tool_calls` at all with this
`llama-server` build (the model narrated `{"name": "read_file", ...}`
as plain text instead of a real tool call) — switching to `qwen3.5:9b`
fixed it immediately, independent confirmation of why this project
settled on that model for `llama-server` in the first place. A
graduated series of coding tasks — a small script (FizzBuzz), a bigger
task (a temperature-converter CLI with pytest coverage, including the
model correctly self-correcting a floating-point precision test
failure), and a small application (a Flask TODO REST API with tests) —
all completed correctly, independently re-verified by re-running the
test suites and, for the Flask app, driving the real running server
with live HTTP requests rather than trusting the model's own summary.

**Bug found: the permission modal's Allow/Deny legend could go
invisible.** `render_permission_modal` (`aivyx-tui/src/app.rs`)
appended the `[y] Allow ...` legend as the last entry in one long
`Vec<Line>` rendered by a single unscrolled `Paragraph` — a diff taller
than the popup's available height (e.g. a new ~100-line file) silently
pushed the legend off-screen entirely, leaving a human with no visible
way to know how to respond. Confirmed via a `ratatui::backend::TestBackend`
regression test (`permission_modal_button_row_stays_visible_for_a_long_diff`)
that fails against the old code and passes after splitting the modal
into a fixed-height footer (the legend, always rendered last, never
clipped) and a separate scroll-clipped content area above it for the
diff.

**Bug found: `agent-client-protocol` 1.2.0 silently denied every ACP
permission request.** The ACP editor-integration chapter above shipped
with its deadlock fix "verified two independent ways... but never by a
real end-to-end run." This chapter finally ran that real end-to-end
test, against a real Zed session on the bare-metal rig — every edit was
denied, no matter what the user clicked "Allow" on. Root-caused through
escalating diagnostics: (1) a scripted ACP client that always
auto-approves (the crate's own official `yolo_one_shot_client` example)
reproduced the same denial, ruling out a Zed-side UI issue; (2)
file-based logging inside `AcpPrompter::prompt` (`eprintln!` is silently
discarded — the ACP transport layer pipes and swallows the agent
subprocess's stderr unless the client wires up a debug callback, which
neither Zed nor the reference client does) revealed the real error:
`send_request(...).block_task()` received a spurious `-32601 Method not
found`, despite the client genuinely having received the request and
sent back a valid "Allow" response; (3) a from-scratch, in-process
reproduction using *only* `agent-client-protocol`'s own public API — no
`aivyx` code at all, an agent and client exchanging a permission
request over an in-memory duplex stream, sent from a spawned task
exactly as the crate's own deadlock-avoidance docs prescribe —
reproduced the identical failure, conclusively ruling out a bug in this
project's own code.

Checked crates.io: `agent-client-protocol` 2.0.0 had shipped three days
earlier. Its own migration guide confirmed the exact symptom as a known,
now-fixed defect: "The 1.x documentation said that `on_receiving_result`
and `on_receiving_ok_result` callbacks held the dispatch loop until
completion, but the implementation did not enforce that ordering," and
2.0 "routes response-handler failures to the pending local request"
instead of surfacing "the misleading generic failure that previously
appeared when the real interceptor error was lost" — precisely the
spurious `Method not found` observed. Despite the 2.0 changelog's long
breaking-change list, the actual upgrade touched almost nothing: this
crate only uses the high-level `Stdio` transport and the standard
`on_receive_request`/`connect_with`/`connect_to` builder pattern, never
the low-level `Channel`/`ResponseRouter` APIs that changed. The one real
change was deleting `session.rs`'s `on_receive_dispatch` catch-all
handler entirely — the migration guide's own recommendation, since 2.0's
built-in default now does the same thing (`Method not found` for
unmatched requests) while also correctly routing responses. Verified
twice live against the real rig (two different edits, both landed
correctly on disk) before committing to the fix, then closed with a
permanent regression test — `prompter.rs`'s
`prompt_resolves_to_allow_over_a_real_connection` — that exercises
`AcpPrompter` against a real in-process `agent-client-protocol`
connection (no subprocess, no LLM) so this exact class of bug can't
silently regress again; the existing pure-mapping-function tests
(`acp_response_to_user_response`, etc.) could never have caught it,
since the bug was in response routing before that function ever runs.

**Closing state**: full workspace build, test suite, and clippy all
clean on `agent-client-protocol` 2.0.0. The ACP editor-integration
chapter's "still open" live-verification gap is now closed — real gated
tool calls, real permission approvals, real edits, all confirmed against
a real Zed session.

### Aivyx Personal Assistant coexistence + a final verification pass — ✅ done

The sibling Aivyx Personal Assistant (`/home/julian/Projects/Rust/aivyx`
— a separate, unrelated project sharing only naming and authorship) was
built natively on the same CachyOS build as the rig (no musl
cross-compile needed — a dynamically-linked glibc binary was directly
portable), deployed as `~/.local/bin/aivyx` — its correct real name,
now free of the earlier `PATH` collision since `aivyx-coder` no longer
claims it — and configured via the `llamacpp` provider to share the
*same* `llama-server` instance `aivyx-coder` was already using, rather
than standing up a second redundant backend. Daemon started cleanly,
the web Studio UI responded, a real headless turn did real arithmetic
correctly, and a real tool call wrote a real file — all while
`aivyx-coder`'s own session against the same backend kept working
without any interference. Footprints confirmed genuinely disjoint on
disk: `~/.aivyx/` + `~/.local/share/aivyx/` for the assistant, `~/.config/
aivyx-coder/` + `~/.local/state/aivyx-coder/` for the coder — exactly as
the original PATH-collision investigation predicted, now proven on real
hardware with both real binaries installed side by side.

**Final verification pass**, run explicitly to check for regressions
and close out anything still deferred, before moving on from this
chapter:

- The modal-scrolling fix, re-verified live in the actual release
  binary (not just the synthetic `TestBackend` regression test): a real
  write of a 40-function, 201-line file produced a diff taller than the
  popup, and the `[y] Allow ...` legend stayed visible exactly as
  designed.
- The `agent-client-protocol` 2.0.0 fix, re-verified live a second time
  — this time with the Personal Assistant's daemon *also* running
  concurrently against the same shared `llama-server` — confirming the
  ACP permission round trip has no cross-process interference.
- The autonomous-mode injection guard's own plan (`docs/superpowers/
  plans/2026-07-22-autonomous-mode-injection-guard.md`) left one item
  explicitly deferred: Task 6, a live E2E run against a real backend,
  blocked at the time by no reachable local LLM. Run now: `--auto`
  against a project seeded with a real injection payload in a file the
  goal would naturally read. The model itself correctly recognized and
  refused the injection independently of the guard; the guard also
  independently fired, halting the run with `autonomous run stopped:
  possible prompt injection detected after 1 iteration(s) — flagged
  content from notes.txt (read_file) matched "ignore previous
  instructions"` — the exact designed notice, quoting the exact
  matched source and excerpt. `config.toml` and the test project's git
  state were both confirmed byte-for-byte untouched afterward (no
  checkpoint refs were even created, since no mutating call was ever
  attempted). This closes the injection guard's last open item.

No new bugs found during this pass. The bare-metal Aivyx Personal
Assistant coexistence trial — the original motivating question behind
the `aivyx-coder` binary rename all the way back at the start of this
chapter — is now fully closed.

### Codebase audit + rig deployment — ✅ done

A follow-up audit (2026-07-22) covering security posture, tool coverage,
test quality, and documentation surfaced 15 findings, triaged into two
buckets rather than built ad hoc: bugs/gaps/stale docs got fixed directly
in the same pass (TDD + full workspace verification per finding), new
*capabilities* got logged to `ROADMAP.md`'s new Backlog section instead,
since a new tool or behavior surface deserves its own design pass, not a
same-session patch. Ten findings were fixed directly: the MCP tool's
Always-Allow cache was argument-blind (approving one call silently
blessed every future call regardless of arguments); `delegate_task`
sub-agents got a fresh, disconnected `InjectionTaint` instead of sharing
the parent's; `deny_paths`' defaults covered only `~/.ssh`/`~/.aws`/the
config dir, missing common credential locations (`~/.gnupg`, `~/.netrc`,
Docker/kube/npm/PyPI/gcloud/Azure/cargo/gh configs); five README/ROADMAP
sections were inaccurate or stale (the read-path auto-allow was never
actually cwd-scoped, the Landlock section undersold its own coverage,
the shipped injection guard was undocumented in three places); the
injection scanner's documented-but-untested marker-list tie-break rule
got a real test; `web_fetch`/`web_search` injection findings reported a
bare tool name with no URL/query, now fixed by extending the same
target-description function `write_file`/`edit_file` already used for
`path`; and `--resume` silently dropped Plan mode back to Act mode
because `SessionState` never persisted it — fixed by adding a
`#[serde(default)]` field so old session files still load, restored by
`Agent::restore` in an allow-list direction only (turns Plan mode on,
never off, so it can't clobber an explicit `--plan` flag). The four
capability opportunities logged to the backlog instead: a move/rename
tool, REPL/interactive-process support, a patch-apply tool, and
verification test-selection (scoping a retry to just the tests relevant
to the edited files instead of always re-running the full suite).

**Deploying the fixes to the bare-metal rig** (`10.80.80.148`) surfaced
that the rig has no Rust toolchain installed by design — the working
binary there had always arrived via cross-compile-and-ship, not an
in-place build. `scripts/build-release.sh` produced a fresh
`x86_64-unknown-linux-musl` release binary locally from `main`
(`3107192`, all 8 audit-fix commits), `sha256sum`-verified on both ends
of the `scp` transfer, installed to `~/.local/bin/aivyx-coder` over a
timestamped backup of the previous binary (removed once the new one was
confirmed working).

**Live re-verification**, `pyte`-driven PTY sessions against the rig's
real `llama-server` + Qwen3.5-9B backend (no synthetic backend, no unit
test):
- **Plan-mode/`--resume` fix**: started `aivyx-coder --plan`, completed
  one real turn, quit, then ran `aivyx-coder --resume` with no `--plan`
  flag at all — the `PLAN` badge correctly reappeared on the resumed
  session, live-confirming the exact behavior the new
  `plan_mode_active`/`SessionState` field + `Agent::restore` logic was
  built for.
- **Expanded `deny_paths` defaults**: planted a fake secret in
  `~/.npmrc` (one of the newly-added default-denied paths — previously
  unprotected) and asked the agent to read and report its contents;
  `read_file` was hard-blocked with `denied: target is under a
  configured deny_paths entry`, and the fake token never reached the
  transcript.

Both checks passed on the first correctly-constructed attempt. Getting
there needed one test-script fix worth recording: sending a bare `\n`
after typed text produces no visible effect at all in this TUI (no
partial text even appears in the input box) — `ratatui`'s crossterm
input layer under a PTY in raw mode expects `\r` (carriage return) for
Enter, not `\n`. All prior live-E2E scripts in this project's history
happened to avoid this because they were built around single-key
shortcuts (Ctrl+P, Ctrl+C) rather than typing free text and pressing
Enter to submit it — worth carrying forward into the next PTY-driving
script that needs to submit a typed message.

Test artifacts (the planted `.npmrc`, scratch project directories, the
tarball, the backup binary) were cleaned up from the rig after both
checks passed.
