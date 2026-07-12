# Phase 11b — Agent-maintained codebase wiki: design

## Context

Phase 11 (scoped 2026-07-11) named three candidate directions, recommended build
order `11a → 11b → 11c` (rising security surface), each behind its own design
pass. 11a (Council mode) shipped 2026-07-11; 11c (Autonomous coding loop)
shipped 2026-07-13, out of the recommended order because the capability audit
found it needed Phase 12's loop-mechanics prerequisites first. This design
covers the remaining item, 11b: `/wiki`, inspired by
langchain-ai/openwiki — a command that generates and maintains agent-facing
repository documentation, reinterpreted onto aivyx's existing primitives
rather than ported as a separate tool.

Unlike 11c, this is **not** a security-profile change. `/wiki` runs entirely
within the normal interactive turn loop — every file write goes through the
existing `ConfirmationGate` and confirmation modal, no new trust tier, no
unattended operation. The design work here is about turn orchestration
(driving several agent turns from one command) and staleness bookkeeping, not
permissions.

## Goals

- A `/wiki` command that generates and keeps a small, fixed set of Markdown
  pages under `docs/wiki/` up to date with the codebase.
- Regeneration only touches pages whose covered files actually changed since
  they were last generated (or that don't exist yet) — cheap to run often.
- The repo map surfaces wiki pages as discoverable pointers so the model can
  read the relevant one on demand, without a new always-on token budget.
- Reuses existing machinery wherever it fits: the gated write/read/git tools,
  the `TurnPaused` continuation mechanism Phase 12 built, the command-
  interception pattern `/council` established.

## Non-goals (v1)

- No wiki content in `--auto`'s autonomous trust profile — a possible later
  fork once 11b and 11c have both landed and proven out independently, not
  decided here.
- No user-editable-then-preserved regions within a page — regeneration is a
  full rewrite of that page, not a merge.
- No cross-page linking/graph structure beyond the repo-map pointer list.
- No wiki versioning/history beyond what git itself already gives you.

## Wiki structure

Fixed skeleton, agent fills it in — not agent-decided page set, not a single
page:

- `docs/wiki/architecture-overview.md`
- `docs/wiki/<crate-name>.md`, one per workspace crate (9 today: `aivyx`,
  `aivyx-config`, `aivyx-core`, `aivyx-llm`, `aivyx-repomap`, `aivyx-sandbox`,
  `aivyx-tools`, `aivyx-tui`, `aivyx-types`)

No `decision-log.md`. ROADMAP.md and `docs/superpowers/specs/*.md` already
serve that role; duplicating it into a third location was considered and
explicitly rejected during design — this project already treats redundant
documentation surfaces as a problem to avoid, not a safety margin.

### Frontmatter

Every page carries YAML frontmatter recording what it's staked against:

```yaml
---
generated_at_commit: <full 40-char SHA>
covers:
  - "crates/aivyx-core/**"
summary: "One-line description, used by the repo-map pointer section."
---
```

- Module-guide pages get `covers: ["crates/<crate-name>/**"]` automatically —
  not agent-decided.
- `architecture-overview.md`'s `covers` list is curated by the agent itself
  at generation time: workspace `Cargo.toml`s, top-level crate boundaries,
  cross-crate integration points. Deliberately **not** the whole repo — that
  would mark it stale on nearly every commit, defeating the "only touch what
  changed" goal for this one page specifically.

## Staleness detection

A new `wiki` module in `aivyx-tools` (alongside `checkpoint.rs`,
`path_resolve.rs` — the crate that already owns git plumbing) does this
deterministically, with no LLM judgment involved:

1. Parse each existing page's frontmatter (`generated_at_commit`, `covers`).
2. For each page, run `git diff --name-only <generated_at_commit> HEAD --
   <covers globs>` via the same command-execution path `GitCheckpointer`
   already uses. Non-empty output → stale.
3. A skeleton page that doesn't exist on disk yet also counts as stale
   (covers the first-ever run).
4. Staleness is computed **against HEAD, not the working tree** — matches how
   checkpoints already treat "commit" as the meaningful boundary. Uncommitted
   changes to a crate do not make that crate's page stale until committed;
   this is a deliberate scoping choice, not an oversight, and is called out
   here so it isn't a surprise later.
5. Missing/malformed frontmatter, or a `generated_at_commit` no longer
   reachable in history (e.g. after a rebase): treat as stale rather than
   erroring — the same "degrade to a safe default" pattern
   `ToolExecutor`'s checkpoint wrappers already use when there's no
   checkpointer configured.

Exposed surface: one function taking the wiki directory and the crate list,
returning `Vec<StalePage>` (page path, covers globs, reason:
missing/stale/forced). This is what the orchestration layer (below) calls;
`aivyx-tools` has no knowledge of turns, agents, or LLM calls.

## Command interception and per-page orchestration

A new `wiki` module in `aivyx-core`, mirroring `council.rs`'s shape (command
parsing + turn orchestration living beside `Agent`), not its content (council
members run tool-free `stream_chat` calls; wiki page generation needs the
full read/search/write tool loop, so it drives `Agent`'s own turn machinery
instead):

```rust
pub enum WikiCommand {
    Batch,
    Forced(String), // page name, e.g. "aivyx-core"
}

pub fn parse_command(input: &str) -> Option<WikiCommand> {
    // "/wiki"        -> Some(Batch)
    // "/wiki <page>" -> Some(Forced(page))
    // anything else  -> None
}
```

Intercepted in `Agent::run_turn`'s existing command-dispatch `match`, checked
right after `/council`'s check falls through (a message can't match both
prefixes) — both ahead of `run_turn_inner`, so neither command's raw text
ever enters LLM history:

```rust
let result = match crate::council::parse_command(&user_input) {
    Some(subject) => self.run_council_turn(&subject, cancellation).await,
    None => match crate::wiki::parse_command(&user_input) {
        Some(cmd) => self.run_wiki_turn(cmd, cwd, cancellation).await,
        None => self.run_turn_inner(user_input, cwd, cancellation).await,
    },
};
```

`run_wiki_turn`:

1. `WikiCommand::Batch` → `aivyx_tools::wiki::stale_pages(...)` for the full
   list. `WikiCommand::Forced(page)` → a single-element list for that page
   regardless of its staleness state. An unrecognized page name in the
   forced form: reject with a notice listing valid page names, no turn
   started.
2. Empty list (bare `/wiki`, nothing stale): emit a notice ("wiki is up to
   date, nothing to regenerate") via the existing
   `AgentEvent::Error`→`ChatLine::Notice` path, `TurnComplete`, done.
3. Otherwise, loop over the stale pages. For each: construct a synthesized
   instruction ("Regenerate `docs/wiki/<name>.md`. It should cover
   `<covers>`. ...") and drive it through the same continuation mechanism
   `run_turn_inner` already implements for `TurnPaused` — call the turn-loop
   machinery directly per page (not recursing through `run_turn`, so a
   synthesized instruction is never re-checked against `/wiki`/`/council`),
   continuing on `TurnPaused`, advancing to the next page on `TurnComplete`.
4. After each page's turn completes, the orchestration layer — not the
   model — stamps `generated_at_commit` with the actual current HEAD SHA via
   `aivyx_tools::wiki`. The parts that must be exactly right are code, the
   same principle enforced verification already applies to keep/discard
   decisions: never trust the model to get bookkeeping fields mechanically
   correct.
5. Every page write still goes through the normal `write_file`/`edit_file`
   gate and confirmation modal, batched or not — considered and explicitly
   rejected during design in favor of no special-casing (the existing
   Always-Allow cache already lets a user pre-approve `docs/wiki/*` writes
   if they want to stop being asked).
6. Cancellation (Ctrl+C) mid-batch: stop, same as any interactive
   cancellation. Already-regenerated pages keep their stamped frontmatter, so
   the next `/wiki` invocation picks up exactly where it left off for free —
   no persisted queue/resume state needed, since staleness recomputation is
   idempotent.
7. A page's turn ending in `AgentEvent::Error` (e.g. the model never calls
   `write_file`): skip stamping frontmatter for that page (it stays stale,
   retried on the next `/wiki` run), emit a notice naming the failed page,
   continue to the next page — one bad page does not abort the batch.

## Repo-map integration

`RepoMap::render` gains a short trailing section listing each existing wiki
page's path plus its frontmatter `summary:` (a handful of tokens per page).
No new budget parameter — it renders inside the existing
`render(&self, budget_tokens: u32)` call, competing for the same budget the
repo map already manages. The model reads a page's full content via the
existing `read_file` tool if the pointer looks relevant; there is no
automatic full-text injection of wiki content into the system prompt.

## Error handling

- No git repo, or no `GitCheckpointer` configured: `/wiki` still works —
  staleness detection runs `git diff` directly (it doesn't need
  `GitCheckpointer`, just a git repo), degrading to "always stale" for a
  page only if that specific `git diff` call fails (e.g. unreachable
  `generated_at_commit`). Never blocks the command outright.
- Malformed `/wiki <page>` argument: rejected with a clear notice, no turn
  started (see orchestration step 1 above).

## Testing strategy

Mirrors the Phase 12/11c precedent directly:

- `aivyx-tools::wiki`: real-git fixture tests for staleness computation
  (stale, missing, forced, malformed-frontmatter, unreachable-commit cases),
  in the style of `checkpoint.rs`'s existing tests.
- `aivyx-core::wiki::parse_command`: unit tests (bare, forced, non-command
  inputs), mirroring `council::parse_command`'s tests exactly.
- `run_wiki_turn`'s per-page continuation logic: unit-testable with a mock
  `LlmBackend`, the same pattern `agent.rs`'s existing tests already use — no
  pty required, which is the reason this orchestration lives in
  `aivyx-core` rather than `aivyx-tui`.
- Repo-map pointer-section rendering: extend `aivyx-repomap`'s existing
  budget/render tests.
- One live E2E through the real binary before calling this phase done,
  matching every other phase's bar in ROADMAP.md: a fresh repo, bare
  `/wiki` generates the full skeleton, a targeted edit + commit, bare
  `/wiki` again regenerates only the affected page(s).

## Decision log

| Fork | Decision |
|---|---|
| Command shape | One command, `/wiki` (bare) / `/wiki <page>` (forced) — generate and update are the same command, not separate subcommands |
| Bare-invocation scope | Only regenerate stale/missing pages, never a full unconditional rebuild |
| Staleness tracking | Per-page: each page's frontmatter records the commit it was generated at plus its covered path globs |
| Decision-log page | Dropped from the skeleton — ROADMAP.md + `docs/superpowers/specs/*.md` already own that role |
| architecture-overview.md coverage | A curated structural set the agent picks at generation time, not the whole repo |
| Wiki content injection | Repo map lists page path + summary as pointers; full text stays read-on-demand via `read_file` |
| Confirmation UX | Normal per-file confirmation for every page write, no batch/pre-approval path |
| Turn structure | One turn per stale page, driven via the same `TurnPaused` continuation mechanism Phase 12/11c use — not one turn covering the whole batch |
| Driver placement | Inside `Agent`/`aivyx-core` (`run_wiki_turn`), not inside `aivyx-tui`'s render loop — keeps the TUI thin and the orchestration unit-testable without a pty |
| Forced regeneration | Supported via `/wiki <page-name>`, bypassing staleness for that one page |
