# Post-Capability-Gap Documentation Sync Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Bring `README.md`, `ROADMAP.md`, `docs/HISTORY.md`, and `CLAUDE.md`
current with the 6 phases shipped since `ROADMAP.md`'s last update
(editor/IDE context integration, editor approval integration, and the
4-phase capability-gap-closing chapter), and fix a handful of now-stale
"Rust-only" claims.

**Architecture:** Pure documentation edit — no code, no tests in the usual
sense. Each task edits one file, appending or fixing prose content whose
exact final text is given in full below (per this project's "no
placeholders" convention, applied to prose the same way it applies to
code).

**Tech Stack:** Markdown only.

## Global Constraints

- Documentation-only — no `crates/*` file is touched by this plan.
- Every new/edited passage matches its file's existing voice and heading
  conventions exactly (see each task's own notes on this).
- Do **not** touch any *historical* framing already correct for its own
  moment in time (e.g. Phase 6's original "Rust only" language in
  `docs/HISTORY.md`, describing what was true when Phase 6 shipped) — only
  the specific *currently-stale* claims identified in the spec's Context
  section are in scope.
- `cargo test --workspace` must stay clean throughout (it will be
  unaffected by this plan, but re-run once at the end to confirm nothing
  else in the worktree drifted).
- Final verification: a grep across `README.md`, `CLAUDE.md`, and
  `docs/HISTORY.md`'s *newly-added* text for the stale phrases "Rust-only
  today", "Rust files only for now", and "non-Rust projects simply get no
  map" must return zero hits.

---

### Task 1: `docs/HISTORY.md` — add the 3 missing sections

**Files:**
- Modify: `docs/HISTORY.md` (append after the current end of file, i.e.
  after the existing "Notes on sequencing" section)

**Interfaces:** None — pure prose append, no other file depends on this
task's exact wording.

- [ ] **Step 1: Append the three new sections**

Open `docs/HISTORY.md`, go to the very end of the file (after the
"Notes on sequencing" section), and append the following, verbatim:

```markdown

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
```

- [ ] **Step 2: Verify the new sections landed correctly**

Run: `grep -n "^### Editor/IDE context integration\|^### Editor approval integration\|^### Capability-gap-closing chapter" docs/HISTORY.md`
Expected: all three headers found, each exactly once.

Run: `grep -c "^### " docs/HISTORY.md`
Expected: 3 more than the count before this step (confirm by running
`git show HEAD:docs/HISTORY.md | grep -c "^### "` for the before-count and
comparing).

- [ ] **Step 3: Commit**

```bash
git add docs/HISTORY.md
git commit -m "Add editor-context, editor-approval, and capability-gap-chapter history"
```

---

### Task 2: `ROADMAP.md` — refresh status, add missing shipped notes, rewrite next-steps

**Files:**
- Modify: `ROADMAP.md`

**Interfaces:**
- Consumes: Task 1's new `docs/HISTORY.md` section titles, for the
  cross-reference at the end of this file (`ROADMAP.md` already ends with
  "See `docs/HISTORY.md` for the full phase-by-phase narrative behind
  every item above" — no change needed to that sentence itself, just
  confirm it still reads correctly once this task's new paragraphs are
  added above it).

- [ ] **Step 1: Get the current exact test count**

Run: `cargo test --workspace 2>&1 | grep -oP '\d+(?= passed)' | awk '{s+=$1} END {print s}'`
Record the number this prints — it replaces "381" in Step 2 below (do not
hardcode 452 from this plan's own writing time; re-derive it fresh, since
drift is possible).

- [ ] **Step 2: Bump the date and test count**

In `ROADMAP.md`, change line 3 from:
```markdown
_Last updated: 2026-07-18_
```
to (using today's actual date):
```markdown
_Last updated: 2026-07-20_
```

In the "Shipped and live-verified" paragraph (currently ending "...
enforced post-edit verification with automatic fix-and-retry. 381
workspace tests; every security-critical behavior also proven by live E2E
against real serving."), replace `381` with the number from Step 1.

- [ ] **Step 3: Add the editor-approval-integration shipped paragraph**

Immediately after the existing "Editor/IDE context integration — shipped."
paragraph (the one ending "...while leaving the real path untouched for
the security-relevant `deny_paths` check."), insert a new paragraph:

```markdown

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
```

- [ ] **Step 4: Add the capability-gap-closing chapter shipped paragraph**

Immediately after the paragraph added in Step 3 (and before the existing
"**In flight / next**" paragraph), insert:

```markdown

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
```

- [ ] **Step 5: Rewrite "In flight / next"**

Replace the entire existing "**In flight / next**" paragraph (from
"**In flight / next**: nothing pre-scoped remains..." through
"...See `docs/HISTORY.md` for the full phase-by-phase narrative behind
every item above.") with:

```markdown

**In flight / next**: nothing pre-scoped remains. The next real context is
a bare-metal test-rig trial (previously used for the sibling Aivyx-Agent
project) — the original motivating goal behind closing all 4
capability-gap sub-projects — not yet started as of this writing. See
`docs/HISTORY.md` for the full phase-by-phase narrative behind every item
above.
```

- [ ] **Step 6: Verify**

Run: `grep -n "Last updated\|Editor approval integration — shipped\|Capability-gap-closing chapter — shipped\|In flight / next" ROADMAP.md`
Expected: all four found, in that order, each once.

Run: `grep -n "381 workspace tests" ROADMAP.md`
Expected: no output (the stale count is gone).

- [ ] **Step 7: Commit**

```bash
git add ROADMAP.md
git commit -m "Refresh ROADMAP.md: editor-approval + capability-gap chapter shipped, current status"
```

---

### Task 3: `README.md` — fix the repo-map contradiction, document the 3 new user-facing behaviors

**Files:**
- Modify: `README.md`

**Interfaces:** None — pure prose edit.

- [ ] **Step 1: Fix the self-contradictory repo-map line**

In `README.md`'s "Repository map" paragraph, change:
```markdown
or disable under `[repo_map]`; non-Rust projects simply get no map and pay
no cost.
```
to:
```markdown
or disable under `[repo_map]`; a project in an unsupported language simply
gets no map and pays no cost.
```

- [ ] **Step 2: Add multi-file edit rollback to the existing checkpoints section**

Find the "**Worktree checkpoints**" paragraph in `README.md` (the one
starting "when the working directory is a git repository..."). Read its
full existing text first (it describes the checkpoint-before-mutation
mechanism this new behavior builds on). Immediately after that paragraph's
existing final sentence, append (staying in the same paragraph — do not
start a new bolded heading, this is a refinement of the existing
mechanism, not a separate feature):

```markdown
When a single model response contains multiple mutating tool calls and a
later one fails, every earlier successful call in that same response is
automatically rolled back to the checkpoint from before the batch started
— the model doesn't have to notice and manually undo a partial multi-file
change itself. The rollback notice is folded directly into the failing
call's own error text, so the model sees exactly what happened and what
was undone in the same turn.
```

- [ ] **Step 3: Add a reasoning-visibility bullet**

Find where `README.md` describes the TUI transcript/streaming behavior
(search for the existing "native + prompted SEARCH/REPLACE edit formats"
or streaming-related paragraph near the top of the feature list — read
the surrounding context first to place this consistently with the file's
existing bullet ordering). Add a new bullet in the same style as
neighboring ones:

```markdown

**Reasoning visibility**: a reasoning-capable model's chain-of-thought
renders live as a dimmed, italicized "thinking:" line in the terminal
transcript, distinct from its final answer. Display-only — reasoning
content never enters the agent's own history or the session JSON, and is
invisible to prompted-edit-mode's SEARCH/REPLACE parser.
```

- [ ] **Step 4: Extend the enforced-verification section with the new-lines-note behavior**

Find the "**Enforced verification**" paragraph in `README.md` (references
`docs/HISTORY.md` Phase 12 Part B). Read its full existing text first.
Immediately after its existing final sentence, append (same paragraph):

```markdown
A failing verification result also gets a short note appended listing
which lines are new since the immediately preceding verification attempt
— whatever that prior attempt's own outcome was — so the model can tell a
newly-introduced regression apart from an already-known failure without
re-deriving that context from raw output each time. This is a coarse,
framework-agnostic line-set comparison, not real test-parsing, and says so
explicitly in the note itself.
```

- [ ] **Step 5: Verify**

Run: `grep -n "unsupported language simply\|automatically rolled back to the checkpoint\|Reasoning visibility\|new since the immediately preceding verification attempt" README.md`
Expected: all four found, each once.

Run: `grep -n "non-Rust projects simply get no map" README.md`
Expected: no output.

- [ ] **Step 6: Commit**

```bash
git add README.md
git commit -m "Document multi-file rollback, reasoning visibility, and verification-memory notes in README"
```

---

### Task 4: `CLAUDE.md` — fix the stale repo-map crate description

**Files:**
- Modify: `CLAUDE.md`

**Interfaces:** None.

- [ ] **Step 1: Fix the crate table row**

In `CLAUDE.md`'s crate table, find the `aivyx-repomap` row (currently
reading: `... token-budgeted and appended to the system prompt each turn
(Rust-only today; other languages degrade gracefully to no map). ...`).
Change `(Rust-only today; other languages degrade gracefully to no map)`
to `(Rust, Python, JavaScript/JSX, and TypeScript/TSX today; other
languages degrade gracefully to no map)`.

- [ ] **Step 2: Verify**

Run: `grep -n "Rust-only today" CLAUDE.md`
Expected: no output.

Run: `grep -n "Rust, Python, JavaScript/JSX, and TypeScript/TSX today" CLAUDE.md`
Expected: one match, in the `aivyx-repomap` row.

- [ ] **Step 3: Commit**

```bash
git add CLAUDE.md
git commit -m "Fix stale Rust-only claim in aivyx-repomap's CLAUDE.md description"
```

---

### Task 5: Final cross-file verification

**Files:**
- None modified — verification only.

**Interfaces:**
- Consumes: Tasks 1-4's committed changes.

- [ ] **Step 1: Confirm no stale phrasing remains anywhere touched**

Run: `grep -rn "Rust-only today\|Rust files only for now\|non-Rust projects simply get no map" README.md CLAUDE.md`
Expected: no output.

Run: `grep -n "Rust-only today\|Rust files only for now\|non-Rust projects simply get no map" docs/HISTORY.md`
Expected: this MAY legitimately match text inside a section describing
what was true *at the time* that section's phase originally shipped (e.g.
Phase 6's own original framing) — if it matches, read the surrounding
context and confirm it's genuinely historical narration, not a leftover
in the 3 new sections added by Task 1. Do not edit historical entries.

- [ ] **Step 2: Confirm the test suite is still clean**

Run: `cargo test --workspace 2>&1 | grep -E "^test result|FAILED"`
Expected: every crate `ok`, 0 failed (unaffected by this plan's doc-only
changes — this just confirms nothing else in the worktree drifted).

- [ ] **Step 3: Report**

No commit for this task (verification only). Report the grep results from
Step 1 and confirm the test suite is clean.
