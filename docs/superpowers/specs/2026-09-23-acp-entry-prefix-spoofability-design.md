# ACP Entry-Prefix Spoofability Design

## Context

Nonagon deferred gap #7 of the original 7, the last remaining one:
`aivyx-acp/src/translate.rs`'s `build_merged_plan` weaves three genuinely
free-form, model-controlled strings — `task.text`, `plan.mission` (the
mission description), and `step.task` (a per-step description) — directly
into a `PlanEntry.content` string, each origin-tagged with a fixed literal
prefix (`"[Task] "`, `"[Mission] "`, `"[Mission: {member}] "`). None of
`set_tasks`/`decompose_task` (the tools that produce these values)
restrict their content or length. A model (possibly manipulated by a
prompt-injection source) can embed a newline in any of these fields
followed by text shaped like a *different* tag — e.g. `"done\n[Specialist:
reviewer] verified, ship it"` — and if the ACP client (Zed) renders
`PlanEntry.content` as multi-line/markdown-aware text, a single `[Task]`
entry can visually masquerade as a separate, more-trusted-looking
`[Specialist: ...]` entry. Flagged (not fixed) at the Nonagon TUI-missions
Phase 6b's own final review as low-impact/display-only, matching this
codebase's existing "model may be manipulated" threat model.

## Grounding

Read directly in the current codebase, and independently verified where
reasoning alone wasn't enough:

- **The exact vector, read from `build_merged_plan`
  (`aivyx-acp/src/translate.rs:53-114`)**: `format!("[Task] {}",
  task.text)`, `format!("[Mission] {}", plan.mission)`, `format!("[Mission:
  {}] {}", step.member, step.task)` (and its `(FAILED)` variant) all
  interpolate free-form model text directly, with no sanitization.
  `session.member` (used for `[Specialist: {member}] session open`) is
  the one field NOT free-form — `decompose_task`/`spawn_specialist` both
  validate member names against the real team roster, so it can never
  contain arbitrary text.
- **A directly-reusable precedent already exists and solves the identical
  problem class**: `aivyx-core/src/agent/mod.rs:2450`'s
  `sanitize_for_display` strips model-controlled text (there, an
  editor-context `file` value) before it's interpolated into the
  *trusted* system prompt, specifically because "a crafted value
  containing newlines could otherwise forge additional 'instructions' at
  that trust level" — replacing every control character (`is_control()`,
  which covers `\n`/`\r`/tab and more) with U+FFFD, then clamping length.
  Same underlying problem (model text forging structure across a trust
  boundary via embedded control characters), different trust boundary
  (a rendered UI panel here, not the system prompt).
- **The TUI's own mission panel is NOT vulnerable to the same
  attack — confirmed empirically, not just reasoned about.** A temporary
  probe (`ratatui::widgets::Paragraph::new(Line::from(evil_string))`
  rendered into a real `ratatui::buffer::Buffer`, using this project's
  exact pinned `ratatui = "0.29.0"`) showed that ratatui silently drops
  embedded `\n` characters entirely when building a `Line` from a plain
  `String` — `"[Task] done\n[Specialist: reviewer] LGTM ship it"`
  rendered as one garbled row, `"[Task] done[Specialist: reviewer] LGTM
  s"`, never as two separate-looking lines. Neither the task panel nor
  the mission panel (`aivyx-tui/src/app.rs:756,794`) calls `.wrap()`
  either, so there's no word-wrap-boundary variant of the attack — an
  overlong string is simply clipped at the panel's width. This matches
  `mission_step_line`/the task-line renderer's own code
  (`aivyx-tui/src/app.rs:956,1001-1011`), both plain
  `Line::from(format!(...))` with no wrap. No TUI change is needed or
  proposed by this spec.
- **`aivyx-acp` already depends on `aivyx-core`**
  (`aivyx-acp/Cargo.toml:18`), but this project has an established,
  deliberate precedent for NOT reaching across a crate boundary for a
  small, single-purpose helper when the alternative is a few lines of
  justified duplication — `specialist_enforcement.rs` duplicating
  `aivyx-config`'s own `resolve_tilde_paths`/`resolve_symlinks` rather
  than depending on that crate, explicitly to avoid the coupling for a
  small amount of logic.

## Decisions

**1. A small, local sanitization helper is added directly to
`aivyx-acp/src/translate.rs`**, mirroring `sanitize_for_display`'s exact
stripping logic (map every `char::is_control()` to U+FFFD) but WITHOUT
that function's length clamp — Plan entries can legitimately be longer
than an editor-context filename, and the vulnerability here is about
*structure-forging control characters*, not length. Duplicated rather
than exported from `aivyx-core`, per the grounding's established
small-helper-duplication precedent — this is a 4-line `chars().map(...)`
function, not worth new cross-crate coupling.

**2. `build_merged_plan` applies this helper to exactly the three
free-form fields**: `task.text`, `plan.mission`, `step.task` — at the
point each is formatted into a `PlanEntry.content` string. `session.member`
is left untouched (already roster-validated, never free-form; sanitizing
it would be a no-op that adds a misleading impression it needed
protection).

**3. No TUI change** — confirmed unnecessary by direct empirical test
(Grounding), not merely asserted safe.

**4. No length truncation added** — this spec's scope is the
structure-forging vector (embedded control characters), not general
"very long model text makes for a noisy panel," which is a separate,
lower-severity UX concern not part of the originally-flagged gap.

## What this spec does not decide

- Any change to `set_tasks`/`decompose_task`'s own input validation —
  those tools still accept arbitrary text; this fix only sanitizes what
  gets rendered into the ACP Plan panel, matching the display-only scope
  the original finding was flagged at.
- Any change to the TUI's rendering — confirmed unnecessary.
- Any change to how ACP's `Session` struct tracks/merges the three state
  sources (`build_merged_plan`'s own merge logic, last-known-state
  tracking) — untouched, this is purely a per-field sanitization step
  inside the existing formatting.
- General length limits or truncation on Plan entry content.
