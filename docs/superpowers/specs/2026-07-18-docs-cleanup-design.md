# Docs Cleanup (pre-GitHub-push, sub-project 1 of 3) — Design

**Status:** Approved by user 2026-07-18.

## Context

This is the first of three independent sub-projects preparing aivyx-coder
for its first push to GitHub (the repo exists but has never been connected;
see ROADMAP.md's "In flight / next" history). The user chose build order
**Docs → Codebase → Release**. This spec covers docs only. Codebase cleanup
and release-build strategy are separate, later specs.

Current state, established by direct inspection before this design was
written:

- `ROADMAP.md` (repo root, 124KB) is a full phase-by-phase development
  journal — comprehensive but not what a first-time visitor wants as "the
  roadmap."
- `README.md` (40KB) is accurate and well-written; it needs a light
  accuracy pass, not a rewrite. Its final section ("See `ROADMAP.md` for
  what's planned next (repo map, git integration, richer agentic UX)") is
  stale — repo map and git integration both shipped in earlier phases.
- No `LICENSE` file exists despite `Cargo.toml`'s `[workspace.package]`
  declaring `license = "MIT OR Apache-2.0"`.
- No `SECURITY.md` exists.
- `CLAUDE.md` (repo root) is accurate and current; scanned for
  anything sensitive/internal-only — found nothing (only `api_key` is
  mentioned as a config field name, no actual secrets, no internal-only
  content).
- `docs/superpowers/specs/` and `docs/superpowers/plans/` hold every past
  phase's design spec and implementation plan (some individual plan files
  are 60–100KB). All are already tracked in git.
- `README.md` contains 7 cross-references to `ROADMAP.md` (lines 12, 46,
  126, 142, 437, 659, 724 as of this writing); `CLAUDE.md` contains 2
  (lines 19, 171). Several point at specific phase narratives (e.g. "See
  ROADMAP.md Phase 2 for the A/B measurements") — content that this spec
  moves out of `ROADMAP.md`.

## Decisions (confirmed with user via one-at-a-time questions before this doc was written)

1. **`ROADMAP.md` restructuring**: keep the full journal as history, add a
   lean summary on top, at a new location — not trimmed in place, not left
   as-is.
2. **`docs/superpowers/specs/`+`plans/`**: keep tracked, link from the new
   history doc, don't delete or exclude.
3. **License**: keep `MIT OR Apache-2.0` (already declared in
   `Cargo.toml`), add the two standard license text files.
4. **Copyright holder**: "Julian" (matches `git config user.name`).
5. **`CLAUDE.md`**: stays public, no content rewrite — only the
   `ROADMAP.md` cross-reference fix from decision 1.
6. **Repo-meta files**: add `SECURITY.md` only. Explicitly defer
   `CONTRIBUTING.md` and issue/PR templates — no contributor traffic yet
   to justify them.
7. **`README.md`**: light pass — fix staleness and the cross-references,
   no structural rewrite, no badges/screenshots/install-instructions yet
   (those belong with the release-build sub-project, once release
   artifacts actually exist to install).

## Changes

### 1. `docs/HISTORY.md` (new)

Move `ROADMAP.md`'s entire current content here, verbatim, unmodified —
this preserves the phase-by-phase narrative exactly as-is; it is history,
not something to edit for this pass. File starts with one added sentence
at the very top identifying it as the relocated full history, pointing
back to the root `ROADMAP.md` for current status.

### 2. `ROADMAP.md` (rewritten, repo root)

Replace entirely with a short (target: under 100 lines) document covering:
- One-paragraph description of what aivyx-coder is (can draw from
  README's opening or `docs/HISTORY.md`'s own summary — should not
  contradict either).
- Current status: all phases shipped and live-verified, 381 workspace
  tests, nothing pre-scoped remains (this is the accurate, current state
  per `docs/HISTORY.md`'s own final "Where things stand today" section —
  carry that content forward, don't re-derive it).
- A pointer to `docs/HISTORY.md`: "Full phase-by-phase history, every
  design decision and its evidence, is in `docs/HISTORY.md`."
- A pointer to `docs/superpowers/specs/` and `docs/superpowers/plans/`:
  "Every phase's original design spec and implementation plan is tracked
  there, for the curious."
- No phase-specific detail lives in the new `ROADMAP.md` — any reader
  wanting phase detail is sent to `docs/HISTORY.md`.

### 3. Cross-reference fixes

In `README.md`, each of the 7 `ROADMAP.md` references gets evaluated
individually and repointed to `docs/HISTORY.md` if it references
phase-specific narrative content (all of lines 12, 46, 126, 142, 437, 659
qualify — they cite specific phases/measurements now living in
`docs/HISTORY.md`). Line 724's "See `ROADMAP.md` for what's planned next"
sentence is rewritten as part of the README staleness fix (item 5 below),
since "what's planned next" is no longer an accurate framing once nothing
is pre-scoped — replace with something like "See `ROADMAP.md` for current
status and `docs/HISTORY.md` for the full phase-by-phase history."

In `CLAUDE.md`, both references (lines 19, 171) get the same treatment:
repoint to `docs/HISTORY.md` if they reference phase-specific narrative,
otherwise repoint to whichever of the two files (`ROADMAP.md` or
`docs/HISTORY.md`) actually now holds the referenced content.

### 4. `LICENSE-MIT` and `LICENSE-APACHE` (new, repo root)

Standard, unmodified license texts for the two licenses. Copyright line:
`Copyright (c) 2026 Julian`.

### 5. `README.md` accuracy pass

- Fix the stale final section (line ~724): repo map and git integration
  are both shipped; rewrite this section to reflect actual current status
  (nothing pre-scoped) rather than a "what's next" framing, consistent
  with `ROADMAP.md`'s own new framing.
- Apply the cross-reference fixes from item 3.
- Read the rest of the file fully during implementation and fix any other
  stale claims found (e.g. feature descriptions that no longer match
  current behavior) — this is a full-file accuracy read, not limited to
  the two spots already known to be stale.
- No structural changes: no badges, no screenshots, no install
  instructions in this pass.

### 6. `SECURITY.md` (new, repo root)

Short document covering:
- Scope: this is a local-only tool (no server, no multi-tenant
  deployment); the security boundary that matters is the sandbox/
  permission model (`ActionKind`, `ConfirmationGate`, Landlock/seccomp
  confinement, `deny_paths`) — vulnerabilities in scope are things like
  sandbox escapes, permission-gate bypasses, or confinement gaps, not
  e.g. "the LLM said something wrong."
- How to report: email jccorbett67@gmail.com. No formal SLA (small
  solo-maintained project), but reports will be read and acknowledged.
- Pointer to the "Known limitations and non-goals" section of `README.md`
  (already exists — the `AIVYX_DEBUG_LOG`, TOCTOU, `Tool::execute` bypass,
  git-specifics content quoted in this spec's Context section) as
  already-documented, known, accepted risk surface — not something to
  report as a new finding.

### 7. `CLAUDE.md`

No content rewrite. Only the cross-reference fix from item 3.

## Out of scope for this spec

- Any change to code (`crates/`) — that's the codebase-cleanup sub-project.
- Any CI/release/packaging work — that's the release-build sub-project.
- `CONTRIBUTING.md`, issue/PR templates (explicitly deferred).
- README structural additions (badges, install instructions, screenshots)
  — revisit once the release-build sub-project produces actual
  downloadable artifacts.

## Testing / verification

This is a docs-only change; there's no test suite to run. Verification is
manual:
- `docs/HISTORY.md` diffed against the original `ROADMAP.md` to confirm
  the move was verbatim (aside from the one added pointer sentence at the
  top).
- Every `ROADMAP.md` / `docs/HISTORY.md` cross-reference in `README.md`
  and `CLAUDE.md` grepped for and manually confirmed to resolve to content
  that actually exists at the target location.
- `LICENSE-MIT` / `LICENSE-APACHE` diffed against the canonical published
  texts (opensource.org / apache.org) to confirm no accidental
  modification.
- Final `git status` / `git diff --stat` review before commit, confirming
  only the expected files changed.
