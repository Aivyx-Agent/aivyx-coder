# Post-Capability-Gap Documentation Sync — Design

**Status:** Approved by user 2026-07-20. Requested once the 4-phase
capability-gap-closing chapter (multi-file edit atomicity, reasoning
visibility, structured verification memory, repo-map multi-language
support) had fully shipped, merged, and been pushed to GitHub — while the
user prepares a bare-metal test rig for the next phase of work.

## Context

**Problem this spec solves:** `README.md`, `ROADMAP.md`, `docs/HISTORY.md`,
and `CLAUDE.md` are the project's only documentation, and none of them
reflect the 6 phases shipped since `ROADMAP.md` was last updated
(2026-07-18): editor/IDE context integration, editor approval integration,
and the 4 capability-gap-closing sub-projects. This isn't a hypothetical
staleness concern — it was verified directly against the actual files and
git history before this spec was written, not assumed:

- `docs/HISTORY.md` (1972 lines) ends at `### Phase 12 — ... — ✅ done`
  (line 1788) plus a trailing "Notes on sequencing" section. It has **zero**
  sections for editor-context-integration, editor-approval-integration, or
  any of the 4 capability-gap phases — confirmed via `git log --oneline --
  docs/HISTORY.md`, which shows no commit touching this file since
  `5be16d6` (the original Phase-6 relocation, part of the pre-GitHub-push
  docs-cleanup phase).
- `ROADMAP.md` (92 lines) is dated `_Last updated: 2026-07-18_`, cites
  "381 workspace tests" (actual current count: 452+), documents
  editor-context-integration as shipped but **not** editor-approval-
  integration (which shipped afterward, per `git log`, but only ever got
  README coverage — commits `90636d5`/`569d0a7`/`8da325f` — never a
  ROADMAP "shipped" paragraph), and its "In flight / next" section (lines
  84-92) describes a state ("nothing pre-scoped remains... likely next
  directions... none yet scoped") that predates all 6 of the missing
  phases and is now inaccurate.
- `README.md` (855 lines) has zero mentions of the 4 capability-gap
  phases' actual user-facing behavior — confirmed via direct grep for
  "rollback"/"reasoning"-as-a-TUI-feature/"new_lines_note"-style content:
  none found outside unrelated contexts (e.g. "reasoning models" used
  generically to describe local model behavior, not this project's own
  reasoning-*display* feature). Additionally, its "Repository map" section
  (lines 62-69) is now **self-contradictory**: line 64 already says "Rust,
  Python, JavaScript/JSX, and TypeScript/TSX today" (fixed during the
  repo-map-multi-language phase's own live-E2E doc-fix commit `a3246e5`),
  but line 69, five lines later in the same paragraph, still says
  "non-Rust projects simply get no map and pay no cost" — a leftover the
  earlier fix missed.
- `CLAUDE.md` (173 lines, checked into the repo) still describes
  `aivyx-repomap` in its crate table (line 59) as "Rust-only today; other
  languages degrade gracefully to no map" — the same claim `README.md` and
  the crate's own module doc comment already had fixed elsewhere.

**Scope, confirmed with the user**: sync existing docs to current reality
only — no new document types (no `CONTRIBUTING.md`, no architecture
diagrams, nothing the project doesn't already have a slot for). This
mirrors the project's own established precedent: the original
pre-GitHub-push "docs cleanup" phase (`642fb68`/`a387259`) did exactly this
kind of sync, using the identical spec → plan → subagent-driven-development
cycle.

## Decisions

1. **`docs/HISTORY.md` gets 3 new top-level sections, appended after the
   current end of file** (after "Notes on sequencing"), matching the
   file's existing `### <Title> — ✅ done` header convention exactly (see
   e.g. `### Phase 12 — Agent loop foundations... — ✅ done`, line 1788).
   Not numbered as further "Phase N"s — the file's own most recent
   sections (Phase 11a/11b/11c, Phase 12) are the last strictly-numbered
   items, and `ROADMAP.md` already refers to the post-Phase-12 work by
   descriptive name ("Editor/IDE context integration — shipped," not
   "Phase 13") — this spec follows that established informal-naming
   precedent rather than inventing new phase numbers unprompted:
   - `### Editor/IDE context integration — ✅ done`
   - `### Editor approval integration — ✅ done`
   - `### Capability-gap-closing chapter — ✅ done` (one section, bundling
     all 4 sub-projects as subsections — mirroring how `### Phase 9 —
     Stretch goals` already bundles multiple independent items under one
     heading, since these 4 sub-projects share one originating audit and
     one user-directed sequencing, exactly like Phase 9's stretch goals
     share one list).
   Each section's content is sourced from this project's own memory
   records of each phase (already written contemporaneously with enough
   detail — real bugs found, evidence, test counts) cross-checked against
   the actual final whole-branch review verdicts and commit messages for
   accuracy, not re-derived from scratch. Depth matches existing entries:
   what shipped, why, the real correction(s) found along the way (this
   project's `HISTORY.md` consistently narrates these, e.g. Phase 7's
   design section, Phase 8's plan-mode section), and the closing evidence
   (test counts, live E2E outcome).
2. **`ROADMAP.md` gets four kinds of edits**, all within its existing
   structure (no restructuring):
   - Bump `_Last updated:_` to today's date.
   - Refresh the shipped-summary paragraph's test count from "381" to the
     actual current total.
   - Add a "shipped" paragraph for editor-approval-integration (matching
     the existing editor-context-integration paragraph's style/depth,
     lines 64-82) — the one phase that has README coverage but never got
     ROADMAP status text.
   - Replace the "Capability audit — fully closed" paragraph's framing
     (lines 55-62, which describes an *older*, already-closed audit from
     before this session) is untouched — it's accurate and refers to a
     different, already-resolved audit. Add a **new** paragraph
     summarizing the 4-phase capability-gap chapter as shipped (this is
     new content, not an edit to the existing "fully closed" paragraph,
     since that one is about a different, earlier audit and conflating
     them would be inaccurate).
   - Rewrite "In flight / next" (lines 84-92) to reflect actual current
     state: nothing else pre-scoped, the bare-metal test-rig trial is the
     next real context (per the audit's own original motivating framing),
     not yet started.
3. **`README.md` gets targeted additions + one contradiction fix**, no
   restructuring:
   - Fix the self-contradictory line 69 ("non-Rust projects simply get no
     map and pay no cost") to match line 64's already-correct multi-language
     framing — e.g. "a project in an unsupported language simply gets no
     map and pays no cost."
   - Add feature-description prose for the 3 capability-gap phases with
     genuinely new user-facing behavior not yet mentioned anywhere in
     `README.md` (multi-file edit atomicity doesn't need its own bullet —
     it's a refinement of the existing multi-file-edit/checkpoint
     behavior already described, expressed as an addition to that
     existing section rather than a new one):
     - Multi-file edit rollback: add to the existing "Worktree
       checkpoints" section (README.md, right after line 69's fixed
       repo-map paragraph) describing the batch-rollback behavior.
     - Reasoning visibility: new short bullet near wherever the TUI's
       transcript rendering is otherwise described, noting the dimmed
       "thinking:" line and that it's display-only (never enters
       history/session JSON).
     - Structured verification memory: extend the existing "Enforced
       verification" section (mentioned via `docs/HISTORY.md`'s Phase 12
       cross-reference already in `README.md`) with the new-lines-note
       behavior.
   - Each addition matches this file's existing prose style (the
     established bullets like "Repository map"/"Worktree checkpoints" are
     the template: one bold lead-in phrase, then 2-4 sentences).
4. **`CLAUDE.md` gets one line fixed**: line 59's `aivyx-repomap` crate-
   table entry, replacing "(Rust-only today; other languages degrade
   gracefully to no map)" with the same multi-language framing already
   used elsewhere ("Rust, Python, JavaScript/JSX, and TypeScript/TSX
   today; other languages degrade gracefully to no map").

## Out of scope

- Any new document (`CONTRIBUTING.md`, architecture diagrams, a test-rig
  prep guide) — explicitly deferred per the user's own scope confirmation.
- Any code change — this is documentation-only; no `crates/*` files are
  touched.
- Restructuring `ROADMAP.md`'s or `README.md`'s existing sections beyond
  the targeted additions/fixes above — this is a sync, not a rewrite.
- Cutting a `v0.1.0` release tag or flipping GitHub visibility to public —
  `ROADMAP.md`'s existing "In flight / next" language about this remains
  as-is (still not yet done, still not this spec's concern).

## Testing / verification

Documentation-only change — no code tests apply. Verification is:
- Each new/edited section is grep-able (e.g. `grep -n "Editor/IDE context
  integration" docs/HISTORY.md` finds the new section).
- No remaining "Rust-only"/"Rust files only"/"non-Rust projects simply get
  no map" stale phrasing anywhere in `README.md`, `CLAUDE.md`, or
  `docs/HISTORY.md`'s newly-added text (a final grep pass across all 4
  touched files for these exact stale phrases must return zero hits
  outside of the *historical* HISTORY.md entries that correctly narrate
  what used to be true at the time, e.g. Phase 6's own original "Rust
  only" framing when the feature first shipped — that historical framing
  is accurate for its own moment in time and must not be rewritten).
- `ROADMAP.md`'s test count matches a fresh `cargo test --workspace` run's
  actual total at the time of writing.

## Sequencing

One sub-project, executed directly following this project's established
spec → plan → subagent-driven-development → finishing-a-development-branch
cycle (matching the original pre-GitHub-push "docs cleanup" phase's own
precedent for a documentation-only change). Not decomposed further — the
4 file edits are independent enough to be separate plan tasks but small
enough that they don't need separate specs.
